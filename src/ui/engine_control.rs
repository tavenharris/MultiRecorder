use super::{ExportRecord, MultiRecordingApp, RecordingMode};
use crate::config::SourceConfig;
use crate::devices::{Device, DeviceManager, DeviceType};
use crate::recorder::{AudioTrackMonitor, PreviewStream, StreamRecorder};
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

impl MultiRecordingApp {
    pub fn refresh_devices(&mut self) {
        match DeviceManager::list_devices() {
            Ok(devices) => {
                let count = devices.len();
                self.discovered_devices = devices;
                self.has_loopback = DeviceManager::has_loopback_device(&self.discovered_devices);
                self.sync_device_selections();
                self.sync_audio_monitors();
                self.stop_preview_streams();
                self.ensure_preview_streams();
                if count == 0 {
                    self.status_message = Some((
                        "⚠️ 0 devices discovered. Please ensure FFmpeg is installed and accessible in PATH.".to_string(),
                        Instant::now(),
                        true,
                    ));
                } else {
                    self.status_message = Some((
                        format!(
                            "Devices refreshed: {} sources found (Loopback: {})",
                            count,
                            if self.has_loopback { "Active" } else { "Not Installed" }
                        ),
                        Instant::now(),
                        false,
                    ));
                }
            }
            Err(e) => {
                self.status_message = Some((
                    format!("⚠️ Device discovery error: {e}"),
                    Instant::now(),
                    true,
                ));
            }
        }
    }

    pub fn sync_audio_monitors(&mut self) {
        #[cfg(target_os = "macos")]
        if self.is_recording {
            for i in 0..5 {
                if let Some(mut old) = self.audio_monitors[i].take() {
                    old.stop();
                }
                self.audio_levels[i] = (0.0, 0.0);
            }
            return;
        }

        for i in 0..5 {
            let track = self.config.audio_tracks.get(i);
            if let Some(track) = track {
                if track.enabled {
                    let found_dev = self
                        .discovered_devices
                        .iter()
                        .find(|d| d.id == track.device_id)
                        .or_else(|| {
                            let clean = track
                                .device_id
                                .strip_prefix("out:")
                                .or_else(|| track.device_id.strip_prefix("in:"))
                                .or_else(|| track.device_id.strip_prefix("audio="))
                                .unwrap_or(&track.device_id);
                            self.discovered_devices
                                .iter()
                                .find(|d| d.id == clean || d.name.contains(clean))
                        })
                        .or_else(|| {
                            if track.device_id == "wasapi"
                                || track.device_id == "default"
                                || track.name.to_lowercase().contains("desktop")
                            {
                                self.discovered_devices
                                    .iter()
                                    .find(|d| d.device_type == DeviceType::AudioOutput)
                            } else {
                                None
                            }
                        });

                    if let Some(dev) = found_dev {
                        let monitor_id = if let Some(av_idx) = dev.av_index {
                            av_idx.to_string()
                        } else {
                            dev.id.clone()
                        };
                        let need_spawn = match &self.audio_monitors[i] {
                            Some(m) => m.device_identifier() != monitor_id,
                            None => true,
                        };
                        if need_spawn {
                            if let Some(mut old) = self.audio_monitors[i].take() {
                                old.stop();
                            }
                            self.audio_monitors[i] = Some(AudioTrackMonitor::new(monitor_id));
                        }
                        continue;
                    }
                }
            }
            if let Some(mut old) = self.audio_monitors[i].take() {
                old.stop();
            }
            self.audio_levels[i] = (0.0, 0.0);
        }
    }

    pub fn ensure_preview_streams(&mut self) {
        if self.is_recording {
            self.stop_preview_streams();
            return;
        }

        // Preview stream 1
        let dev1 = self
            .video_devices()
            .into_iter()
            .find(|d| d.index == self.game_video_idx)
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.game_video_idx.to_string());
        if self.preview_stream_1.is_none() {
            let is_screen = self
                .video_devices()
                .iter()
                .find(|d| d.index == self.game_video_idx)
                .map(|d| d.name.to_lowercase().contains("screen") || d.id.eq_ignore_ascii_case("desktop"))
                .unwrap_or(false);
            let mut ps1 = PreviewStream::new(dev1, is_screen);
            let _ = ps1.start();
            self.preview_stream_1 = Some(ps1);
        }

        // Preview stream 2 (only if distinct video device)
        let dev2 = self
            .video_devices()
            .into_iter()
            .find(|d| d.index == self.cam_video_idx)
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.cam_video_idx.to_string());
        if self.cam_video_idx != self.game_video_idx && self.preview_stream_2.is_none() {
            let is_screen = self
                .video_devices()
                .iter()
                .find(|d| d.index == self.cam_video_idx)
                .map(|d| d.name.to_lowercase().contains("screen") || d.id.eq_ignore_ascii_case("desktop"))
                .unwrap_or(false);
            let mut ps2 = PreviewStream::new(dev2, is_screen);
            let _ = ps2.start();
            self.preview_stream_2 = Some(ps2);
        }

        self.sync_audio_monitors();
    }

    pub fn stop_preview_streams(&mut self) {
        if let Some(mut ps1) = self.preview_stream_1.take() {
            ps1.stop();
        }
        if let Some(mut ps2) = self.preview_stream_2.take() {
            ps2.stop();
        }
        #[cfg(target_os = "macos")]
        for i in 0..5 {
            if let Some(mut old) = self.audio_monitors[i].take() {
                old.stop();
            }
            self.audio_levels[i] = (0.0, 0.0);
        }
        self.preview_texture_1 = None;
        self.preview_texture_2 = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        std::thread::sleep(Duration::from_millis(250));
    }

    pub(crate) fn sync_device_selections(&mut self) {
        let fixes = self.config.sanitize_for_platform(&self.discovered_devices);
        if !fixes.is_empty() {
            for fix in &fixes {
                tracing::info!("Startup hardware/OS compatibility auto-heal: {}", fix);
            }
            let _ = self.config.save_to_file(&self.config_path);
            if self.status_message.is_none() {
                self.status_message = Some((
                    format!(
                        "✔ Auto-configured {} setting{} for this machine's hardware & OS.",
                        fixes.len(),
                        if fixes.len() == 1 { "" } else { "s" }
                    ),
                    Instant::now(),
                    false,
                ));
            }
        }

        let v_devs: Vec<(usize, String, String)> = self
            .video_devices()
            .into_iter()
            .map(|d| (d.index, d.id.clone(), d.name.clone()))
            .collect();
        let valid_v_indices: Vec<usize> = v_devs.iter().map(|(idx, _, _)| *idx).collect();

        let find_v_idx = |device_str: &str, fallback_pos: usize| -> usize {
            if let Some((idx, _, _)) = v_devs.iter().find(|(_, id, _)| id == device_str) {
                return *idx;
            }
            let clean = device_str.strip_prefix("video:").unwrap_or(device_str).trim();
            if let Some((idx, _, _)) = v_devs
                .iter()
                .find(|(_, id, name)| name == device_str || name == clean || id == clean)
            {
                return *idx;
            }
            if let Ok(num) = clean.parse::<usize>() {
                if valid_v_indices.contains(&num) {
                    return num;
                }
            }
            valid_v_indices
                .get(fallback_pos)
                .or_else(|| valid_v_indices.first())
                .copied()
                .unwrap_or(0)
        };

        if let Some(src) = self.config.sources.first() {
            self.game_video_idx = find_v_idx(&src.video_device, 0);
        }
        if self.config.sources.len() > 1 {
            let src2 = &self.config.sources[1];
            self.cam_video_idx = find_v_idx(&src2.video_device, 1);
        }
    }

    pub(crate) fn video_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Video)
            .collect()
    }

    pub(crate) fn audio_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| {
                d.device_type == DeviceType::AudioInput || d.device_type == DeviceType::AudioOutput
            })
            .collect()
    }

    pub fn format_interval_label(secs: u64) -> String {
        if secs < 60 {
            format!("{}s", secs)
        } else if secs % 3600 == 0 {
            format!("{}h", secs / 3600)
        } else if secs % 60 == 0 {
            format!("{}m", secs / 60)
        } else if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        }
    }

    /// Formats an audio device label to a fixed character length (truncating with '…' if longer,
    /// or right-padding to `max_chars` so all device selector buttons maintain identical width).
    pub fn format_fixed_audio_label(raw: &str, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }
        let char_count = raw.chars().count();
        if char_count > max_chars {
            let truncated: String = raw.chars().take(max_chars.saturating_sub(1)).collect();
            format!("{}…", truncated)
        } else {
            format!("{:<width$}", raw, width = max_chars)
        }
    }

    pub fn start_engine(&mut self, mode: RecordingMode) {
        if self.is_recording {
            return;
        }

        // Pre-validate audio tracks via HAL
        if let Err(warning) = crate::platform::CurrentPlatform.validate_audio_tracks(
            &self.config.audio_tracks,
            &self.discovered_devices,
        ) {
            self.status_message = Some((warning, Instant::now(), true));
            return;
        }

        let (_, in_fps_1) = self.parse_input_res_fps(0);
        let (_, in_fps_2) = self.parse_input_res_fps(1);

        self.stop_preview_streams();

        let _ = fs::create_dir_all(&self.config.buffer_dir);
        let _ = fs::create_dir_all(&self.config.output_dir);

        // Wipe any previous un-saved cached segments in buffer_dir when starting a fresh session
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);

        // Sync dropdown selections and hardware-clamped framerates into config
        let dev_id1 = self
            .video_devices()
            .into_iter()
            .find(|d| d.index == self.game_video_idx)
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.game_video_idx.to_string());
        let dev_id2 = self
            .video_devices()
            .into_iter()
            .find(|d| d.index == self.cam_video_idx)
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.cam_video_idx.to_string());

        if let Some(src1) = self.config.sources.first_mut() {
            src1.video_device = dev_id1;
            let cur = src1.fps.unwrap_or(in_fps_1);
            src1.fps = Some(if cur < 24 { in_fps_1 } else { cur.min(in_fps_1) });
        }
        if self.config.sources.len() > 1 {
            let src2 = &mut self.config.sources[1];
            src2.video_device = dev_id2;
            let cur = src2.fps.unwrap_or(in_fps_2);
            src2.fps = Some(if cur < 24 { in_fps_2 } else { cur.min(in_fps_2) });
        }

        // Map audio device indices
        for track in &mut self.config.audio_tracks {
            if let Some(dev) = self.discovered_devices.iter().find(|d| d.id == track.device_id) {
                if let Some(av_idx) = dev.av_index {
                    track.device_index = av_idx;
                }
            }
        }

        let _ = self.config.save_to_file(&self.config_path);

        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        self.managers.clear();
        self.recorders.clear();

        // If both sources point to the same device index (e.g. only 1 webcam/screen available),
        // only launch Stream 1 to avoid AVFoundation hardware device contention errors.
        let sources_to_record: Vec<SourceConfig> =
            if self.config.sources.len() > 1 && self.game_video_idx == self.cam_video_idx {
                vec![self.config.sources[0].clone()]
            } else {
                self.config.sources.clone()
            };

        for src in &sources_to_record {
            self.managers.push(RingBufferManager::new(
                &src.name,
                &self.config.buffer_dir,
                self.config.segment_duration_secs,
            ));
        }

        let mut success = true;
        for (i, src) in sources_to_record.iter().enumerate() {
            // Primary video stream attaches all discrete audio tracks (1..5);
            // Secondary video stream attaches Track 1 if sync_audio_to_secondary is enabled for NLE waveform sync.
            let tracks = if i == 0 {
                self.config.audio_tracks.clone()
            } else if i == 1 && self.config.sync_audio_to_secondary {
                if let Some(t1) = self
                    .config
                    .audio_tracks
                    .iter()
                    .find(|t| t.enabled && t.device_id != "none")
                    .cloned()
                {
                    vec![t1]
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            let mut recorder = StreamRecorder::with_preview(
                src.clone(),
                tracks,
                self.config.segment_duration_secs,
                self.config.audio_bitrate.clone(),
                self.config.enable_recording_preview,
            );
            if let Err(e) = recorder.start(&self.config.buffer_dir) {
                self.status_message = Some((
                    format!("Failed to start stream [{}]: {}", src.name, e),
                    Instant::now(),
                    true,
                ));
                success = false;
                break;
            }
            self.recorders.push(recorder);
        }

        if success {
            self.is_recording = true;
            self.recording_mode = Some(mode);
            self.recording_started_at = Some(Instant::now());
            let status_text = match mode {
                RecordingMode::Continuous => {
                    "🎬 Continuous recording LIVE! Forward recording until you click Stop.".to_string()
                }
                RecordingMode::Flashback => {
                    if sources_to_record.len() == 1 && self.config.sources.len() > 1 {
                        "Flashback active! Stream 1 live (Stream 2 standby: select a 2nd video device).".to_string()
                    } else {
                        "Flashback active! Live multi-source buffer running...".to_string()
                    }
                }
            };
            self.status_message = Some((status_text, Instant::now(), false));
        } else {
            self.stop_engine();
        }
    }

    pub fn stop_engine(&mut self) {
        if self.recording_mode == Some(RecordingMode::Continuous) {
            self.stop_continuous_recording();
            return;
        }

        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }
        self.recorders.clear();

        // Wipe all cached segments from the buffer on stop (only saved clips are kept)
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        self.ensure_preview_streams();
        self.status_message = Some((
            "Flashback stopped. Buffer reset & cleared.".to_string(),
            Instant::now(),
            false,
        ));
    }

    /// Stops continuous forward recording and losslessly concatenates all segments into the final video file(s)
    pub fn stop_continuous_recording(&mut self) {
        if !self.is_recording {
            return;
        }

        // 1. Stop all recorders to ensure all segments are flushed to disk
        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }
        self.recorders.clear();

        let elapsed = self
            .recording_started_at
            .map(|t| t.elapsed())
            .unwrap_or(Duration::from_secs(0));
        let timestamp = Remuxer::generate_session_timestamp();
        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

        let out_ext = if self.config.output_format.trim().is_empty() {
            "mp4"
        } else {
            &self.config.output_format
        };

        // Extract shared start timecode
        let shared_timecode = if self.config.embed_timecode {
            let mut found_tc = None;
            for mgr in &self.managers {
                if let Ok(segs) = mgr.list_segments() {
                    if !segs.is_empty() {
                        let paths: Vec<PathBuf> = segs.iter().map(|s| s.path.clone()).collect();
                        found_tc = Some(Remuxer::extract_start_timecode_from_segments(&paths));
                        break;
                    }
                }
            }
            found_tc
        } else {
            None
        };

        // 2. Stitch all segments for each manager
        for (idx, mgr) in self.managers.iter().enumerate() {
            let segments: Vec<PathBuf> = match mgr.list_segments() {
                Ok(segs) => segs.into_iter().map(|s| s.path).collect(),
                Err(e) => {
                    self.status_message = Some((
                        format!("Error listing segments: {}", e),
                        Instant::now(),
                        true,
                    ));
                    continue;
                }
            };

            if segments.is_empty() {
                continue;
            }

            let out_file = self.config.output_dir.join(format!(
                "Recording_{}_{}.{}",
                timestamp,
                mgr.source_name(),
                out_ext
            ));

            if let Err(e) =
                Remuxer::stitch_segments(&segments, &out_file, shared_timecode.as_deref())
            {
                self.status_message = Some((
                    format!("Remuxing recording failed: {}", e),
                    Instant::now(),
                    true,
                ));
                continue;
            }

            if let Ok(meta) = fs::metadata(&out_file) {
                total_size += meta.len() as f64 / (1024.0 * 1024.0);
            }
            exported_files.push(out_file.clone());

            // Export standalone synchronized Broadcast WAV (.wav) files for each enabled audio track from Stream 1
            if idx == 0 {
                if let Ok(wav_files) = Remuxer::export_separate_audio_tracks(
                    &out_file,
                    &self.config.output_dir,
                    "Recording",
                    &timestamp,
                    &self.config.audio_tracks,
                    shared_timecode.as_deref(),
                ) {
                    for wav in wav_files {
                        if let Ok(meta) = fs::metadata(&wav) {
                            total_size += meta.len() as f64 / (1024.0 * 1024.0);
                        }
                        exported_files.push(wav);
                    }
                }
            }
        }

        // 3. Clear temporary buffer directory now that continuous file has been created
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        self.ensure_preview_streams();

        if let Some(first) = exported_files.first() {
            self.recent_exports.insert(
                0,
                ExportRecord {
                    timestamp: timestamp.clone(),
                    path: first.clone(),
                    duration_secs: elapsed.as_secs(),
                    size_mb: total_size,
                },
            );
        }

        let dur_str = if elapsed.as_secs() >= 3600 {
            format!(
                "{}:{:02}:{:02}",
                elapsed.as_secs() / 3600,
                (elapsed.as_secs() % 3600) / 60,
                elapsed.as_secs() % 60
            )
        } else {
            format!("{}:{:02}", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
        };

        if !exported_files.is_empty() {
            self.status_message = Some((
                format!(
                    "🎬 Recording saved! ({}, {:.1} MB, {} file{})",
                    dur_str,
                    total_size,
                    exported_files.len(),
                    if exported_files.len() > 1 { "s" } else { "" }
                ),
                Instant::now(),
                false,
            ));
        } else {
            self.status_message = Some((
                "Recording stopped (no segments captured).".to_string(),
                Instant::now(),
                false,
            ));
        }
    }

    pub fn trigger_flashback(&mut self, duration_secs: u64) {
        let start = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        let target_duration = Duration::from_secs(duration_secs);

        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

        // Compute synchronized session start timecode across both streams
        let shared_timecode = if self.config.embed_timecode {
            let mut found_tc = None;
            for mgr in &self.managers {
                if let Ok(segs) = mgr.get_segments_for_duration(target_duration) {
                    if !segs.is_empty() {
                        found_tc = Some(Remuxer::extract_start_timecode_from_segments(&segs));
                        break;
                    }
                }
            }
            found_tc.or_else(|| {
                let start_time =
                    chrono::Local::now() - chrono::Duration::seconds(duration_secs as i64);
                Some(start_time.format("%H:%M:%S:00").to_string())
            })
        } else {
            None
        };

        for (idx, mgr) in self.managers.iter().enumerate() {
            let segments = match mgr.get_segments_for_duration(target_duration) {
                Ok(s) => s,
                Err(e) => {
                    self.status_message = Some((
                        format!("Error getting segments: {}", e),
                        Instant::now(),
                        true,
                    ));
                    return;
                }
            };

            if segments.is_empty() {
                self.status_message = Some((
                    format!(
                        "No segments ready yet for stream [{}]! Wait 3-5 seconds.",
                        mgr.source_name()
                    ),
                    Instant::now(),
                    true,
                ));
                return;
            }

            let out_ext = if self.config.output_format.trim().is_empty() {
                "mp4"
            } else {
                &self.config.output_format
            };
            let out_file = self.config.output_dir.join(format!(
                "Flashback_{}_{}.{}",
                timestamp,
                mgr.source_name(),
                out_ext
            ));

            if let Err(e) =
                Remuxer::stitch_segments(&segments, &out_file, shared_timecode.as_deref())
            {
                self.status_message =
                    Some((format!("Remuxing failed: {}", e), Instant::now(), true));
                return;
            }

            if let Ok(meta) = fs::metadata(&out_file) {
                total_size += meta.len() as f64 / (1024.0 * 1024.0);
            }
            exported_files.push(out_file.clone());

            // Export standalone synchronized Broadcast WAV (.wav) files for each enabled audio track from Stream 1
            if idx == 0 {
                if let Ok(wav_files) = Remuxer::export_separate_audio_tracks(
                    &out_file,
                    &self.config.output_dir,
                    "Flashback",
                    &timestamp,
                    &self.config.audio_tracks,
                    shared_timecode.as_deref(),
                ) {
                    for wav in wav_files {
                        if let Ok(meta) = fs::metadata(&wav) {
                            total_size += meta.len() as f64 / (1024.0 * 1024.0);
                        }
                        exported_files.push(wav);
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        if let Some(first) = exported_files.first() {
            self.recent_exports.insert(
                0,
                ExportRecord {
                    timestamp: timestamp.clone(),
                    path: first.clone(),
                    duration_secs,
                    size_mb: total_size,
                },
            );
        }

        self.status_message = Some((
            format!(
                "⚡ Flashback saved in {:.2}s! ({} files exported)",
                elapsed.as_secs_f32(),
                exported_files.len()
            ),
            Instant::now(),
            false,
        ));
    }
}
