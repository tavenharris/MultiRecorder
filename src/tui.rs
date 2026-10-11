use crate::config::{Config, SourceConfig};
use crate::devices::{Device, DeviceManager, DeviceType};
use crate::recorder::{AudioTrackMonitor, StreamRecorder};
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use crate::ui::{ExportRecord, MultiRecordingApp, RecordingMode};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use std::fs;
use std::io::{stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// RAII guard that ensures raw mode and alternate screen are always restored on exit.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut out = stdout();
        terminal::enable_raw_mode()?;
        execute!(out, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, ResetColor, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub struct TuiApp {
    pub config: Config,
    pub config_path: PathBuf,
    pub discovered_devices: Vec<Device>,
    pub has_loopback: bool,

    // Runtime recording state
    pub is_recording: bool,
    pub recording_mode: Option<RecordingMode>,
    recorders: Vec<StreamRecorder>,
    managers: Vec<RingBufferManager>,
    last_eviction: Instant,
    recording_started_at: Option<Instant>,

    // Hardware audio monitors for live TUI stereo VU meters
    audio_monitors: [Option<AudioTrackMonitor>; 5],
    pub audio_levels: [(f32, f32); 5],

    // Selected video device indices & focused audio track
    pub game_video_idx: usize,
    pub cam_video_idx: usize,
    pub selected_audio_track: usize,
    pub selected_flashback_secs: u64,

    // Status banner & export history
    pub status_message: Option<(String, Instant, bool)>,
    pub recent_exports: Vec<ExportRecord>,
    pub should_quit: bool,
}

impl TuiApp {
    pub fn new(mut config: Config, config_path: PathBuf) -> Self {
        let devices = DeviceManager::list_devices().unwrap_or_default();
        let has_loopback = DeviceManager::has_loopback_device(&devices);

        let fixes = config.sanitize_for_platform(&devices);
        if !fixes.is_empty() {
            let _ = config.save_to_file(&config_path);
        }

        let selected_secs = config.buffer_duration_secs;
        let initial_status = if devices.is_empty() {
            Some((
                "⚠️ No capture devices found. Ensure FFmpeg is installed and in PATH.".to_string(),
                Instant::now(),
                true,
            ))
        } else if !fixes.is_empty() {
            Some((
                format!(
                    "✔ Auto-configured {} setting{} for this machine's hardware.",
                    fixes.len(),
                    if fixes.len() == 1 { "" } else { "s" }
                ),
                Instant::now(),
                false,
            ))
        } else {
            Some((
                "Ready. Press [R] to Record forward or [B] to start Flashback buffer.".to_string(),
                Instant::now(),
                false,
            ))
        };

        let mut app = Self {
            config,
            config_path,
            discovered_devices: devices,
            has_loopback,
            is_recording: false,
            recording_mode: None,
            recorders: Vec::new(),
            managers: Vec::new(),
            last_eviction: Instant::now(),
            recording_started_at: None,
            audio_monitors: [None, None, None, None, None],
            audio_levels: [(0.0, 0.0); 5],
            game_video_idx: 0,
            cam_video_idx: 0,
            selected_audio_track: 0,
            selected_flashback_secs: selected_secs,
            status_message: initial_status,
            recent_exports: Vec::new(),
            should_quit: false,
        };

        app.sync_video_indices_from_config();
        app.sync_audio_monitors();
        app
    }

    pub fn video_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Video)
            .collect()
    }

    pub fn audio_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| {
                d.device_type == DeviceType::AudioInput || d.device_type == DeviceType::AudioOutput
            })
            .collect()
    }

    fn sync_video_indices_from_config(&mut self) {
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
                if track.enabled && track.device_id != "none" {
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

    fn stop_audio_monitors_for_recording(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let mut stopped_any = false;
            for i in 0..5 {
                if let Some(mut old) = self.audio_monitors[i].take() {
                    old.stop();
                    stopped_any = true;
                }
                self.audio_levels[i] = (0.0, 0.0);
            }
            if stopped_any {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }

    pub fn refresh_devices(&mut self) {
        match DeviceManager::list_devices() {
            Ok(devices) => {
                let count = devices.len();
                self.discovered_devices = devices;
                self.has_loopback = DeviceManager::has_loopback_device(&self.discovered_devices);
                let _ = self.config.sanitize_for_platform(&self.discovered_devices);
                let _ = self.config.save_to_file(&self.config_path);
                self.sync_video_indices_from_config();
                self.sync_audio_monitors();
                self.status_message = Some((
                    format!("🔄 Refreshed hardware: {} devices discovered.", count),
                    Instant::now(),
                    false,
                ));
            }
            Err(e) => {
                self.status_message = Some((
                    format!("⚠️ Failed to refresh devices: {}", e),
                    Instant::now(),
                    true,
                ));
            }
        }
    }

    pub fn cycle_video_device(&mut self, stream_idx: usize) {
        if self.is_recording {
            self.status_message = Some((
                "⚠️ Stop recording before changing video input devices.".to_string(),
                Instant::now(),
                true,
            ));
            return;
        }

        let v_indices: Vec<(usize, String, String)> = self
            .video_devices()
            .into_iter()
            .map(|d| (d.index, d.id.clone(), d.name.clone()))
            .collect();
        if v_indices.is_empty() {
            return;
        }

        let current_idx = if stream_idx == 0 {
            self.game_video_idx
        } else {
            self.cam_video_idx
        };

        let cur_pos = v_indices
            .iter()
            .position(|(idx, _, _)| *idx == current_idx)
            .unwrap_or(0);
        let next_pos = (cur_pos + 1) % v_indices.len();
        let (next_idx, next_id, next_name) = v_indices[next_pos].clone();

        if stream_idx == 0 {
            self.game_video_idx = next_idx;
        } else {
            self.cam_video_idx = next_idx;
        }

        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            src.video_device = next_id;
            let _ = self.config.save_to_file(&self.config_path);
        }

        self.status_message = Some((
            format!(
                "📹 Stream {} video input set to [{}] {}",
                stream_idx + 1,
                next_idx,
                next_name
            ),
            Instant::now(),
            false,
        ));
    }

    pub fn cycle_audio_track_device(&mut self, track_idx: usize, forward: bool) {
        if self.is_recording {
            self.status_message = Some((
                "⚠️ Stop recording before changing audio track devices.".to_string(),
                Instant::now(),
                true,
            ));
            return;
        }

        let audio_devs: Vec<Device> = self.audio_devices().into_iter().cloned().collect();
        // Option 0 = None (Disabled), Option 1..=N = discovered audio devices
        let total_options = audio_devs.len() + 1;

        let Some(track) = self.config.audio_tracks.get_mut(track_idx) else {
            return;
        };

        let cur_opt = if !track.enabled || track.device_id == "none" {
            0
        } else {
            audio_devs
                .iter()
                .position(|d| d.id == track.device_id)
                .map(|p| p + 1)
                .unwrap_or(0)
        };

        let next_opt = if forward {
            (cur_opt + 1) % total_options
        } else if cur_opt == 0 {
            total_options - 1
        } else {
            cur_opt - 1
        };

        if next_opt == 0 {
            track.enabled = false;
            track.device_id = "none".to_string();
            track.name = format!("Track {}", track_idx + 1);
        } else {
            let dev = &audio_devs[next_opt - 1];
            track.enabled = true;
            track.device_id = dev.id.clone();
            track.device_index = dev.av_index.unwrap_or(0);
            track.name = dev.name.clone();
        }

        let _ = self.config.save_to_file(&self.config_path);
        self.sync_audio_monitors();
    }

    pub fn adjust_selected_track_volume(&mut self, delta: f32) {
        if let Some(track) = self.config.audio_tracks.get_mut(self.selected_audio_track) {
            track.volume = (track.volume + delta).clamp(0.0, 2.0);
            let _ = self.config.save_to_file(&self.config_path);
        }
    }

    pub fn toggle_selected_track_mute(&mut self) {
        if let Some(track) = self.config.audio_tracks.get_mut(self.selected_audio_track) {
            track.volume = if track.volume > 0.01 { 0.0 } else { 1.0 };
            let _ = self.config.save_to_file(&self.config_path);
        }
    }

    fn get_device_input_fps(&self, dev_idx: usize) -> u32 {
        self.video_devices()
            .into_iter()
            .find(|d| d.index == dev_idx)
            .and_then(|d| d.fps)
            .unwrap_or(60)
    }

    pub fn start_engine(&mut self, mode: RecordingMode) {
        if self.is_recording {
            return;
        }

        if let Err(warning) = crate::platform::CurrentPlatform.validate_audio_tracks(
            &self.config.audio_tracks,
            &self.discovered_devices,
        ) {
            self.status_message = Some((warning, Instant::now(), true));
            return;
        }

        let in_fps_1 = self.get_device_input_fps(self.game_video_idx);
        let in_fps_2 = self.get_device_input_fps(self.cam_video_idx);

        self.stop_audio_monitors_for_recording();

        let _ = fs::create_dir_all(&self.config.buffer_dir);
        let _ = fs::create_dir_all(&self.config.output_dir);
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);

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

        for track in &mut self.config.audio_tracks {
            if let Some(dev) = self
                .discovered_devices
                .iter()
                .find(|d| d.id == track.device_id)
            {
                if let Some(av_idx) = dev.av_index {
                    track.device_index = av_idx;
                }
            }
        }

        let _ = self.config.save_to_file(&self.config_path);

        self.managers.clear();
        self.recorders.clear();

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

            // In TUI mode, use headless StreamRecorder::new (no rawvideo RGBA preview pipe overhead)
            let mut recorder = StreamRecorder::new(
                src.clone(),
                tracks,
                self.config.segment_duration_secs,
                self.config.audio_bitrate.clone(),
            );
            if let Err(e) = recorder.start(&self.config.buffer_dir) {
                self.status_message = Some((
                    format!("❌ Failed to start stream [{}]: {}", src.name, e),
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
            let msg = match mode {
                RecordingMode::Continuous => {
                    "🎬 Continuous Recording LIVE! Press [R] to stop and save.".to_string()
                }
                RecordingMode::Flashback => {
                    "⚡ Flashback Buffer LIVE! Press [F/Space] or [1..4] to save clips, [B] to stop."
                        .to_string()
                }
            };
            self.status_message = Some((msg, Instant::now(), false));
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

        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.sync_audio_monitors();
        self.status_message = Some((
            "■ Flashback stopped. Temporary ring buffer cleared.".to_string(),
            Instant::now(),
            false,
        ));
    }

    pub fn stop_continuous_recording(&mut self) {
        if !self.is_recording {
            return;
        }

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

        for (idx, mgr) in self.managers.iter().enumerate() {
            let segments: Vec<PathBuf> = match mgr.list_segments() {
                Ok(segs) => segs.into_iter().map(|s| s.path).collect(),
                Err(_) => continue,
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

            if Remuxer::stitch_segments(&segments, &out_file, shared_timecode.as_deref()).is_ok() {
                if let Ok(meta) = fs::metadata(&out_file) {
                    total_size += meta.len() as f64 / (1024.0 * 1024.0);
                }
                exported_files.push(out_file.clone());

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
        }

        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.sync_audio_monitors();

        if let Some(first) = exported_files.first() {
            self.recent_exports.insert(
                0,
                ExportRecord {
                    timestamp,
                    path: first.clone(),
                    duration_secs: elapsed.as_secs(),
                    size_mb: total_size,
                },
            );
        }

        if !exported_files.is_empty() {
            self.status_message = Some((
                format!(
                    "🎬 Recording saved! ({}:{:02}, {:.1} MB, {} file{})",
                    elapsed.as_secs() / 60,
                    elapsed.as_secs() % 60,
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
        if !self.is_recording || self.recording_mode != Some(RecordingMode::Flashback) {
            self.status_message = Some((
                "⚠️ Flashback buffer is not active. Press [B] to start Flashback first.".to_string(),
                Instant::now(),
                true,
            ));
            return;
        }

        let start = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        let target_duration = Duration::from_secs(duration_secs);
        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

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
                        format!("Error reading buffer segments: {}", e),
                        Instant::now(),
                        true,
                    ));
                    return;
                }
            };

            if segments.is_empty() {
                self.status_message = Some((
                    format!(
                        "⏳ No segments ready yet for [{}] — wait {}s.",
                        mgr.source_name(),
                        self.config.segment_duration_secs + 1
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
                    timestamp,
                    path: first.clone(),
                    duration_secs,
                    size_mb: total_size,
                },
            );
        }

        let label = MultiRecordingApp::format_interval_label(duration_secs);
        self.status_message = Some((
            format!(
                "⚡ Saved last {} Flashback in {:.2}s! ({} files, {:.1} MB)",
                label,
                elapsed.as_secs_f32(),
                exported_files.len(),
                total_size
            ),
            Instant::now(),
            false,
        ));
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match key.code {
            // Quit
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                self.should_quit = true;
            }

            // Toggle Continuous Forward Recording [R]
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if self.is_recording {
                    if self.recording_mode == Some(RecordingMode::Continuous) {
                        self.stop_continuous_recording();
                    } else {
                        self.status_message = Some((
                            "⚠️ Flashback is currently running. Press [B] to stop Flashback first."
                                .to_string(),
                            Instant::now(),
                            true,
                        ));
                    }
                } else {
                    self.start_engine(RecordingMode::Continuous);
                }
            }

            // Toggle Flashback Ring Buffer [B]
            KeyCode::Char('b') | KeyCode::Char('B') => {
                if self.is_recording {
                    if self.recording_mode == Some(RecordingMode::Flashback) {
                        self.stop_engine();
                    } else {
                        self.status_message = Some((
                            "⚠️ Continuous recording is active. Press [R] to stop Recording first."
                                .to_string(),
                            Instant::now(),
                            true,
                        ));
                    }
                } else {
                    self.start_engine(RecordingMode::Flashback);
                }
            }

            // Save Flashback Clip [F / Space]
            KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Char(' ') => {
                let secs = self.selected_flashback_secs;
                self.trigger_flashback(secs);
            }

            // Quick Save Interval Hotkeys [1..9]
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                let intervals = if self.config.clip_intervals.is_empty() {
                    crate::config::default_clip_intervals()
                } else {
                    self.config.clip_intervals.clone()
                };
                if let Some(&secs) = intervals.get(idx) {
                    self.selected_flashback_secs = secs;
                    if self.is_recording && self.recording_mode == Some(RecordingMode::Flashback) {
                        self.trigger_flashback(secs);
                    } else {
                        let lbl = MultiRecordingApp::format_interval_label(secs);
                        self.status_message = Some((
                            format!("⏱ Selected default flashback clip duration: {}", lbl),
                            Instant::now(),
                            false,
                        ));
                    }
                }
            }

            // Cycle Video Input 1 [V] & Video Input 2 [C]
            KeyCode::Char('v') | KeyCode::Char('V') => {
                self.cycle_video_device(0);
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.cycle_video_device(1);
            }

            // Refresh Hardware Devices [D]
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.refresh_devices();
            }

            // Audio Track Selection [Up / Down]
            KeyCode::Up => {
                if self.selected_audio_track == 0 {
                    self.selected_audio_track = 4;
                } else {
                    self.selected_audio_track -= 1;
                }
            }
            KeyCode::Down => {
                self.selected_audio_track = (self.selected_audio_track + 1) % 5;
            }

            // Cycle Audio Device on Selected Track [Left / Right]
            KeyCode::Right => {
                let idx = self.selected_audio_track;
                self.cycle_audio_track_device(idx, true);
            }
            KeyCode::Left => {
                let idx = self.selected_audio_track;
                self.cycle_audio_track_device(idx, false);
            }

            // Adjust Selected Track Volume [- / + / M]
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.adjust_selected_track_volume(0.05);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.adjust_selected_track_volume(-0.05);
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.toggle_selected_track_mute();
            }

            _ => {}
        }
    }

    fn tick(&mut self) {
        // Periodic Ring Buffer Eviction (Flashback mode only)
        if self.is_recording
            && self.recording_mode == Some(RecordingMode::Flashback)
            && self.last_eviction.elapsed() >= Duration::from_secs(5)
        {
            let max_age = Duration::from_secs(self.config.buffer_duration_secs);
            for mgr in &self.managers {
                let _ = mgr.evict_older_than(max_age);
            }
            self.last_eviction = Instant::now();
        }

        // Recorder process health check
        if self.is_recording {
            let mut failed_msg = None;
            for recorder in &mut self.recorders {
                if let Err(e) = recorder.check_health() {
                    failed_msg = Some(e.to_string());
                    break;
                }
            }
            if let Some(err) = failed_msg {
                self.stop_engine();
                self.status_message = Some((err, Instant::now(), true));
            }
        }

        // Sample Stereo (L/R) Audio VU Levels
        for i in 0..5 {
            let (track_enabled, gain) = self
                .config
                .audio_tracks
                .get(i)
                .map(|t| (t.enabled, t.volume.max(0.0)))
                .unwrap_or((false, 1.0));
            if track_enabled {
                let (raw_l, raw_r) = self.audio_monitors[i]
                    .as_ref()
                    .map(|m| m.get_peaks())
                    .unwrap_or((0.0, 0.0));
                let peak_l = (raw_l * gain).clamp(0.0, 1.0);
                let peak_r = (raw_r * gain).clamp(0.0, 1.0);

                let (mut cur_l, mut cur_r) = self.audio_levels[i];
                if peak_l > cur_l {
                    cur_l = cur_l * 0.25 + peak_l * 0.75;
                } else {
                    cur_l *= 0.85;
                }
                if peak_r > cur_r {
                    cur_r = cur_r * 0.25 + peak_r * 0.75;
                } else {
                    cur_r *= 0.85;
                }
                self.audio_levels[i] = (cur_l, cur_r);
            } else {
                self.audio_levels[i] = (0.0, 0.0);
            }
        }
    }

    pub fn format_ascii_vu_bar(level: f32, width: usize, enabled: bool) -> String {
        if width == 0 {
            return String::new();
        }
        if !enabled {
            return "·".repeat(width);
        }
        let filled = ((level.clamp(0.0, 1.0) * (width as f32)).round() as usize).min(width);
        let empty = width.saturating_sub(filled);
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    }

    fn render(&self, out: &mut impl Write) -> Result<()> {
        queue!(out, MoveTo(0, 0))?;

        let border = "═".repeat(86);
        let thin_sep = "─".repeat(86);

        // 1. Top Header Banner
        queue!(
            out,
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold),
            Print(format!("╔{}╗", border)),
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        )?;

        let mode_str = match self.recording_mode {
            Some(RecordingMode::Continuous) => "● RECORDING LIVE (CONTINUOUS)",
            Some(RecordingMode::Flashback) => "⚡ FLASHBACK RING BUFFER LIVE",
            None => "○ STANDBY / IDLE",
        };
        let mode_color = match self.recording_mode {
            Some(RecordingMode::Continuous) => Color::Red,
            Some(RecordingMode::Flashback) => Color::Green,
            None => Color::DarkGrey,
        };

        let elapsed_secs = if self.is_recording {
            self.recording_started_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0)
        } else {
            0
        };
        let timer_str = if elapsed_secs >= 3600 {
            format!(
                "{}:{:02}:{:02}",
                elapsed_secs / 3600,
                (elapsed_secs % 3600) / 60,
                elapsed_secs % 60
            )
        } else {
            format!("{:02}:{:02}s", elapsed_secs / 60, elapsed_secs % 60)
        };

        let total_size_mb: f64 = if self.is_recording {
            self.managers
                .iter()
                .filter_map(|m| m.get_stats().ok())
                .map(|s| s.total_size_mb)
                .sum()
        } else {
            0.0
        };
        let size_str = if total_size_mb >= 1024.0 {
            format!("{:.1} GB", total_size_mb / 1024.0)
        } else {
            format!("{:.0} MB", total_size_mb.max(0.0))
        };

        let bitrate_str = if self.is_recording {
            let mbps: u64 = self
                .config
                .sources
                .iter()
                .filter_map(|s| {
                    s.video_bitrate
                        .trim_end_matches(['M', 'm'])
                        .parse::<u64>()
                        .ok()
                })
                .sum();
            format!("{} Mb/s", if mbps > 0 { mbps } else { 40 })
        } else {
            "0 Mb/s".to_string()
        };

        queue!(
            out,
            SetForegroundColor(Color::White),
            SetAttribute(Attribute::Bold),
            Print("  MULTI-RECORDING PRO (TUI)   "),
            SetForegroundColor(mode_color),
            Print(format!("{:<31}", mode_str)),
            SetForegroundColor(Color::White),
            Print(format!("⏱ {}   {} ({})", timer_str, bitrate_str, size_str)),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold),
            Print(format!("╚{}╝", border)),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        )?;

        // 2. Status / Notification Line
        if let Some((msg, _, is_err)) = &self.status_message {
            let color = if *is_err {
                Color::Red
            } else {
                Color::Green
            };
            queue!(
                out,
                SetForegroundColor(color),
                SetAttribute(Attribute::Bold),
                Print(format!("  {}", msg)),
                ResetColor,
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            )?;
        } else {
            queue!(out, Clear(ClearType::UntilNewLine), Print("\r\n"))?;
        }

        // 3. Video Sources Section
        queue!(
            out,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print("📹 VIDEO SOURCES  "),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            Print("([V] Cycle Video 1 Input  •  [C] Cycle Video 2 Input)"),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("  {}", thin_sep)),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        )?;

        let video_devs = self.video_devices();
        for stream_idx in 0..2 {
            let role_label = if stream_idx == 0 {
                "Stream 1 (Gameplay)"
            } else {
                "Stream 2 (Facecam) "
            };
            let sel_idx = if stream_idx == 0 {
                self.game_video_idx
            } else {
                self.cam_video_idx
            };

            let dev_obj = video_devs.iter().find(|d| d.index == sel_idx).copied();
            let dev_label = dev_obj
                .map(|d| format!("[{}] {}", d.index, d.name))
                .unwrap_or_else(|| format!("[{}] Unknown Video Device", sel_idx));
            let in_res = dev_obj
                .and_then(|d| d.resolution.clone())
                .unwrap_or_else(|| "1920x1080".to_string());
            let in_fps = dev_obj.and_then(|d| d.fps).unwrap_or(60);

            let src_cfg = self.config.sources.get(stream_idx);
            let out_res = src_cfg
                .and_then(|s| s.resolution.clone())
                .unwrap_or_else(|| format!("Native ({})", in_res));
            let out_fps = src_cfg
                .and_then(|s| s.fps)
                .unwrap_or(in_fps)
                .min(in_fps);
            let encoder = src_cfg
                .map(|s| s.encoder.as_str())
                .unwrap_or("h264_videotoolbox");
            let bitrate = src_cfg.map(|s| s.video_bitrate.as_str()).unwrap_or("30M");

            let health_str = if self.is_recording {
                if let Some(rec) = self.recorders.get(stream_idx) {
                    let h = rec.get_health();
                    if h.frames_processed > 0 {
                        if h.is_lagging {
                            format!("⚠️ {:.1} fps ({:.2}x) LAGGING", h.current_fps, h.speed_ratio)
                        } else {
                            format!("⚡ {:.1} fps ({:.2}x) Real-time", h.current_fps, h.speed_ratio)
                        }
                    } else {
                        "⚡ Starting encoder...".to_string()
                    }
                } else {
                    "ℹ️ Standby (Shared device)".to_string()
                }
            } else {
                "Standby".to_string()
            };

            queue!(
                out,
                SetForegroundColor(Color::White),
                SetAttribute(Attribute::Bold),
                Print(format!("  • {} : ", role_label)),
                SetForegroundColor(Color::Cyan),
                Print(format!("{:<32}", MultiRecordingApp::format_fixed_audio_label(&dev_label, 32))),
                ResetColor,
                SetForegroundColor(Color::Grey),
                Print(format!(
                    " │ In: {} {}fps → Out: {} {}fps │ {} @ {} │ {}",
                    in_res, in_fps, out_res, out_fps, encoder, bitrate, health_str
                )),
                ResetColor,
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            )?;
        }

        queue!(out, Clear(ClearType::UntilNewLine), Print("\r\n"))?;

        // 4. Audio Tracks Section (5 Tracks + Live Stereo L/R VU Meters)
        queue!(
            out,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print("🎙 AUDIO TRACKS & STEREO METERS  "),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            Print("([↑/↓] Select Track  •  [←/→] Change Device  •  [-/+] Gain  •  [M] Mute)"),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("  {}", thin_sep)),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        )?;

        let audio_devs = self.audio_devices();
        for i in 0..5 {
            let is_selected = i == self.selected_audio_track;
            let cursor = if is_selected { "▸" } else { " " };
            let track = &self.config.audio_tracks[i];

            let dev_display = if !track.enabled || track.device_id == "none" {
                "None (Disabled)".to_string()
            } else {
                audio_devs
                    .iter()
                    .find(|d| d.id == track.device_id)
                    .map(|d| d.display_label(self.has_loopback))
                    .unwrap_or_else(|| track.name.clone())
            };
            let fixed_dev = MultiRecordingApp::format_fixed_audio_label(&dev_display, 28);

            let pct = (track.volume * 100.0).round().clamp(0.0, 200.0) as i32;
            let gain_str = if !track.enabled {
                "  --- ".to_string()
            } else if pct == 0 {
                " MUTE ".to_string()
            } else {
                format!("{:>4}% ", pct)
            };

            let (lvl_l, lvl_r) = self.audio_levels[i];
            let bar_l = Self::format_ascii_vu_bar(lvl_l, 12, track.enabled);
            let bar_r = Self::format_ascii_vu_bar(lvl_r, 12, track.enabled);

            let bar_color = |lvl: f32, enabled: bool| -> Color {
                if !enabled {
                    Color::DarkGrey
                } else if lvl > 0.85 {
                    Color::Red
                } else if lvl > 0.65 {
                    Color::Yellow
                } else {
                    Color::Green
                }
            };

            let row_fg = if is_selected {
                Color::Cyan
            } else if track.enabled {
                Color::White
            } else {
                Color::DarkGrey
            };

            queue!(
                out,
                SetForegroundColor(row_fg),
                SetAttribute(if is_selected {
                    Attribute::Bold
                } else {
                    Attribute::Reset
                }),
                Print(format!("  {} Track {} │ {} │ Gain: {}", cursor, i + 1, fixed_dev, gain_str)),
                ResetColor,
                Print("│ L ["),
                SetForegroundColor(bar_color(lvl_l, track.enabled)),
                Print(bar_l),
                ResetColor,
                Print("]  R ["),
                SetForegroundColor(bar_color(lvl_r, track.enabled)),
                Print(bar_r),
                ResetColor,
                Print("]"),
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            )?;
        }

        queue!(out, Clear(ClearType::UntilNewLine), Print("\r\n"))?;

        // 5. Quick Clip Intervals & Output Settings Summary
        let intervals = if self.config.clip_intervals.is_empty() {
            crate::config::default_clip_intervals()
        } else {
            self.config.clip_intervals.clone()
        };
        let mut interval_badges = String::new();
        for (idx, &secs) in intervals.iter().take(9).enumerate() {
            let lbl = MultiRecordingApp::format_interval_label(secs);
            if secs == self.selected_flashback_secs {
                interval_badges.push_str(&format!("[{}] *⚡ {}*   ", idx + 1, lbl));
            } else {
                interval_badges.push_str(&format!("[{}] ⚡ {}   ", idx + 1, lbl));
            }
        }

        queue!(
            out,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print("⚡ FLASHBACK PRESETS : "),
            ResetColor,
            SetForegroundColor(Color::White),
            Print(&interval_badges),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            SetForegroundColor(Color::DarkGrey),
            Print(format!(
                "  Output Folder: {:?}   │   Format: .{}   │   Max Buffer Window: {}m   │   NLE Timecode Sync: {}",
                self.config.output_dir,
                self.config.output_format,
                self.config.buffer_duration_secs / 60,
                if self.config.embed_timecode { "ON" } else { "OFF" }
            )),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n\r\n"),
        )?;

        // 6. Keybinds Controls Box
        queue!(
            out,
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold),
            Print(format!("┌{}┐", thin_sep)),
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            Print("│ "),
            SetForegroundColor(Color::Red),
            Print("[R]"),
            SetForegroundColor(Color::White),
            Print(" Start/Stop Recording    "),
            SetForegroundColor(Color::Green),
            Print("[B]"),
            SetForegroundColor(Color::White),
            Print(" Start/Stop Flashback    "),
            SetForegroundColor(Color::Magenta),
            Print("[F / Space]"),
            SetForegroundColor(Color::White),
            Print(" Save Flashback Clip  "),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            Print("│ "),
            SetForegroundColor(Color::Yellow),
            Print("[1..4]"),
            SetForegroundColor(Color::White),
            Print(" Quick Save Clip      "),
            SetForegroundColor(Color::Yellow),
            Print("[V / C]"),
            SetForegroundColor(Color::White),
            Print(" Cycle Video 1/2     "),
            SetForegroundColor(Color::Yellow),
            Print("[↑/↓ ←/→ -/+]"),
            SetForegroundColor(Color::White),
            Print(" Audio Track & Gain "),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            Print("│ "),
            SetForegroundColor(Color::Blue),
            Print("[D]"),
            SetForegroundColor(Color::White),
            Print(" Refresh Devices         "),
            SetForegroundColor(Color::Red),
            Print("[Q / Esc]"),
            SetForegroundColor(Color::White),
            Print(" Quit Studio                                        "),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
            Print(format!("└{}┘", thin_sep)),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        )?;

        // 7. Recent Exports (if any)
        if !self.recent_exports.is_empty() {
            queue!(
                out,
                SetForegroundColor(Color::Green),
                SetAttribute(Attribute::Bold),
                Print("  Recent Saved Files:"),
                ResetColor,
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            )?;
            for exp in self.recent_exports.iter().take(3) {
                queue!(
                    out,
                    SetForegroundColor(Color::Grey),
                    Print(format!(
                        "    ✔ {} ({}s, {:.1} MB) -> {:?}",
                        exp.timestamp, exp.duration_secs, exp.size_mb, exp.path
                    )),
                    ResetColor,
                    Clear(ClearType::UntilNewLine),
                    Print("\r\n"),
                )?;
            }
        }

        queue!(out, Clear(ClearType::FromCursorDown))?;
        out.flush()?;
        Ok(())
    }

    pub fn run_loop(&mut self) -> Result<()> {
        let _guard = TerminalGuard::enter()?;
        let mut out = stdout();

        while !self.should_quit {
            self.tick();
            self.render(&mut out)?;

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key_event) = event::read()? {
                    self.handle_key_event(key_event);
                }
            }
        }

        // Cleanly stop any active recorders or audio monitors before leaving TUI
        if self.is_recording {
            self.stop_engine();
        }
        for i in 0..5 {
            if let Some(mut mon) = self.audio_monitors[i].take() {
                mon.stop();
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_vu_bar_formatting() {
        let off_bar = TuiApp::format_ascii_vu_bar(0.5, 10, false);
        assert_eq!(off_bar.chars().count(), 10);
        assert_eq!(off_bar, "··········");

        let zero_bar = TuiApp::format_ascii_vu_bar(0.0, 10, true);
        assert_eq!(zero_bar.chars().count(), 10);
        assert_eq!(zero_bar, "░░░░░░░░░░");

        let half_bar = TuiApp::format_ascii_vu_bar(0.5, 10, true);
        assert_eq!(half_bar.chars().count(), 10);
        assert_eq!(half_bar, "█████░░░░░");

        let full_bar = TuiApp::format_ascii_vu_bar(1.2, 10, true);
        assert_eq!(full_bar.chars().count(), 10);
        assert_eq!(full_bar, "██████████");
    }
}
