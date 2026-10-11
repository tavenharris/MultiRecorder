use crate::config::{AudioTrackConfig, SourceConfig};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

pub const PREVIEW_WIDTH: usize = 960;
pub const PREVIEW_HEIGHT: usize = 540;
pub const PREVIEW_FRAME_SIZE: usize = PREVIEW_WIDTH * PREVIEW_HEIGHT * 4;
pub const PREVIEW_FPS: u32 = 30;

static RES_REGEX: OnceLock<regex::Regex> = OnceLock::new();
static FPS_REGEX: OnceLock<regex::Regex> = OnceLock::new();

fn get_res_regex() -> &'static regex::Regex {
    RES_REGEX.get_or_init(|| regex::Regex::new(r"(\d{3,5}x\d{3,5})").expect("valid resolution regex"))
}

fn get_fps_regex() -> &'static regex::Regex {
    FPS_REGEX.get_or_init(|| regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:fps|tbr)").expect("valid fps regex"))
}

fn parse_detected_format(line: &str, default_fps: u32) -> Option<String> {
    if line.contains("Stream #0:0") && line.contains("Video:") {
        let res = get_res_regex().captures(line).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
        let fps = get_fps_regex().captures(line).and_then(|c| c.get(1)).map(|m| {
            let n: f32 = m.as_str().parse().unwrap_or(default_fps as f32);
            format!("{}fps", n.round() as u32)
        }).unwrap_or_else(|| format!("{}fps", default_fps));
        if let Some(res) = res {
            return Some(format!("{}  {}", res, fps));
        }
    }
    None
}

#[allow(dead_code)]
fn stop_child_gracefully(mut child: Child, name: &str, timeout: Duration) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q\n");
        let _ = stdin.flush();
    }
    let start = std::time::Instant::now();
    let mut exited = false;
    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::info!("Process for [{}] exited with status: {}", name, status);
                exited = true;
                break;
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                tracing::warn!("Error waiting on process for [{}]: {}", name, e);
                break;
            }
        }
    }
    if !exited {
        tracing::warn!("Process for [{}] did not exit within {:?}; forcing termination...", name, timeout);
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub struct PreviewStream {
    video_device: String,
    #[allow(dead_code)]
    is_screen: bool,
    child: Option<Child>,
    latest_frame: Arc<Mutex<Option<Vec<u8>>>>,
    frame_seq: Arc<AtomicU64>,
    detected_format: Arc<Mutex<Option<String>>>,
}

impl PreviewStream {
    pub fn new(video_device: String, is_screen: bool) -> Self {
        Self {
            video_device,
            is_screen,
            child: None,
            latest_frame: Arc::new(Mutex::new(None)),
            frame_seq: Arc::new(AtomicU64::new(0)),
            detected_format: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&mut self) -> Result<()> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-fflags", "nobuffer", "-flags", "low_delay"]);
        cmd.args(crate::platform::CurrentPlatform.build_preview_input_args(&self.video_device));
        let vf = format!(
            "scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}",
            PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_FPS
        );
        cmd.args([
            "-vf", &vf,
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-flush_packets", "1",
            "pipe:1",
        ]);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn preview ffmpeg for device {}", self.video_device)
        })?;

        if let Some(stdout) = child.stdout.take() {
            let frame_arc = Arc::clone(&self.latest_frame);
            let frame_seq = Arc::clone(&self.frame_seq);
            thread::spawn(move || {
                use std::io::Read;
                let mut reader = BufReader::with_capacity(PREVIEW_FRAME_SIZE, stdout);
                let mut buf = vec![0u8; PREVIEW_FRAME_SIZE];
                while reader.read_exact(&mut buf).is_ok() {
                    if let Ok(mut lock) = frame_arc.lock() {
                        if let Some(existing) = lock.as_mut() {
                            std::mem::swap(existing, &mut buf);
                        } else {
                            *lock = Some(buf.clone());
                        }
                    }
                    frame_seq.fetch_add(1, Ordering::Release);
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let fmt_arc = Arc::clone(&self.detected_format);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut found = false;
                for line in reader.lines().flatten() {
                    if !found {
                        if let Some(fmt_str) = parse_detected_format(&line, 30) {
                            if let Ok(mut lock) = fmt_arc.lock() {
                                *lock = Some(fmt_str);
                                found = true;
                            }
                        }
                    }
                }
            });
        }

        self.child = Some(child);
        Ok(())
    }

    pub fn get_latest_frame(&self) -> Option<Vec<u8>> {
        self.latest_frame.lock().ok().and_then(|f| f.clone())
    }

    pub fn frame_seq(&self) -> u64 {
        self.frame_seq.load(Ordering::Acquire)
    }

    pub fn get_detected_format(&self) -> Option<String> {
        self.detected_format.lock().ok().and_then(|f| f.clone())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"q\n");
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for PreviewStream {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct StreamRecorder {
    source: SourceConfig,
    audio_tracks: Vec<AudioTrackConfig>,
    audio_bitrate: String,
    child: Option<Child>,
    segment_duration_secs: u64,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
    log_file_path: PathBuf,
    latest_frame: Arc<Mutex<Option<Vec<u8>>>>,
    frame_seq: Arc<AtomicU64>,
    detected_format: Arc<Mutex<Option<String>>>,
}

impl StreamRecorder {
    pub fn new(
        source: SourceConfig,
        audio_tracks: Vec<AudioTrackConfig>,
        segment_duration_secs: u64,
        audio_bitrate: String,
    ) -> Self {
        let log_dir = std::env::temp_dir().join("multirecording_logs");
        let _ = fs::create_dir_all(&log_dir);
        let log_file_path = log_dir.join(format!("{}.log", source.name));

        Self {
            source,
            audio_tracks,
            audio_bitrate,
            child: None,
            segment_duration_secs,
            recent_logs: Arc::new(Mutex::new(VecDeque::with_capacity(50))),
            log_file_path,
            latest_frame: Arc::new(Mutex::new(None)),
            frame_seq: Arc::new(AtomicU64::new(0)),
            detected_format: Arc::new(Mutex::new(None)),
        }
    }

    pub fn source_name(&self) -> &str {
        &self.source.name
    }

    pub fn get_recent_logs(&self) -> Vec<String> {
        if let Ok(logs) = self.recent_logs.lock() {
            logs.iter().cloned().collect()
        } else {
            vec![]
        }
    }

    pub fn get_latest_frame(&self) -> Option<Vec<u8>> {
        self.latest_frame.lock().ok().and_then(|f| f.clone())
    }

    pub fn frame_seq(&self) -> u64 {
        self.frame_seq.load(Ordering::Acquire)
    }

    pub fn log_file_path(&self) -> &Path {
        &self.log_file_path
    }

    pub fn start(&mut self, buffer_dir: &Path) -> Result<()> {
        let segment_pattern = buffer_dir
            .join(format!("{}_%Y%m%d_%H%M%S.mp4", self.source.name))
            .to_string_lossy()
            .to_string();

        let fps = self.source.fps.unwrap_or(60);
        let gop_size = fps * self.segment_duration_secs as u32;

        let is_camera = self.source.name.to_lowercase().contains("cam")
            || self.source.name.to_lowercase().contains("face");
        let in_fps = if is_camera { 30 } else { fps.min(60) };

        let enabled_tracks: Vec<&AudioTrackConfig> = self
            .audio_tracks
            .iter()
            .filter(|t| t.enabled)
            .collect();

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y"]);

        let enabled_configs: Vec<AudioTrackConfig> = enabled_tracks.iter().map(|&t| t.clone()).collect();
        cmd.args(crate::platform::CurrentPlatform.build_recording_input_args(
            &self.source.video_device,
            in_fps,
            &enabled_configs,
        ));

        // Filter complex: split video into [rec] and [prev] with locked 16:9 aspect ratio and padding
        let filter_str = if let Some(res) = &self.source.resolution {
            if let Some((w, h)) = res.split_once('x') {
                format!(
                    "[0:v]split=2[rec_raw][prev_raw];\
                     [rec_raw]scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[rec];\
                     [prev_raw]scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[prev]",
                    w, h, w, h, fps,
                    PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_FPS
                )
            } else {
                format!(
                    "[0:v]split=2[rec_raw][prev_raw];\
                     [rec_raw]fps={}[rec];\
                     [prev_raw]scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[prev]",
                    fps,
                    PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_FPS
                )
            }
        } else {
            format!(
                "[0:v]split=2[rec_raw][prev_raw];\
                 [rec_raw]fps={}[rec];\
                 [prev_raw]scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[prev]",
                fps,
                PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_WIDTH, PREVIEW_HEIGHT, PREVIEW_FPS
            )
        };

        cmd.args(["-filter_complex", &filter_str]);

        // Output 0: Segments
        cmd.args(["-map", "[rec]"]);

        // Map each audio stream with its dedicated track title metadata
        for (i, track) in enabled_tracks.iter().enumerate() {
            let input_idx = crate::platform::CurrentPlatform.audio_input_index(i);
            cmd.args(["-map", &format!("{}:a", input_idx)]);
            cmd.args([
                &format!("-metadata:s:a:{}", i),
                &format!("title={}", track.name),
            ]);
        }

        // Output framerate & GOP boundary alignment (Crucial for segmenting)
        cmd.args(["-r", &fps.to_string()]);
        cmd.args(["-g", &gop_size.to_string()]);

        // Video encoder settings
        cmd.args([
            "-c:v",
            &self.source.encoder,
            "-b:v",
            &self.source.video_bitrate,
            "-pix_fmt",
            "yuv420p",
        ]);

        // Audio encoder settings (applied to all mapped audio tracks)
        if !enabled_tracks.is_empty() {
            cmd.args(["-c:a", "aac", "-b:a", &self.audio_bitrate]);
        }

        // Segment muxer settings for rolling buffer
        cmd.args([
            "-f",
            "segment",
            "-segment_time",
            &self.segment_duration_secs.to_string(),
            "-segment_format",
            "mp4",
            "-reset_timestamps",
            "1",
            "-strftime",
            "1",
            &segment_pattern,
        ]);

        // Output 1: Live Preview Pipe (RGBA rawvideo to stdout)
        cmd.args([
            "-map", "[prev]",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-flush_packets", "1",
            "pipe:1",
        ]);

        // Setup pipes
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::info!(
            "Starting recorder for source [{}] (audio tracks: {}) with cmd: {:?}",
            self.source.name,
            enabled_tracks.len(),
            cmd
        );

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn ffmpeg for source {}", self.source.name))?;

        // Background thread to continuously read live preview frames from stdout
        if let Some(stdout) = child.stdout.take() {
            let frame_arc = Arc::clone(&self.latest_frame);
            let frame_seq = Arc::clone(&self.frame_seq);
            thread::spawn(move || {
                use std::io::Read;
                let mut reader = BufReader::with_capacity(PREVIEW_FRAME_SIZE, stdout);
                let mut buf = vec![0u8; PREVIEW_FRAME_SIZE];
                while reader.read_exact(&mut buf).is_ok() {
                    if let Ok(mut lock) = frame_arc.lock() {
                        if let Some(existing) = lock.as_mut() {
                            std::mem::swap(existing, &mut buf);
                        } else {
                            *lock = Some(buf.clone());
                        }
                    }
                    frame_seq.fetch_add(1, Ordering::Release);
                }
            });
        }

        // Background thread to continuously drain stderr, avoid pipe stall, and capture live logs
        if let Some(stderr) = child.stderr.take() {
            let logs_arc = Arc::clone(&self.recent_logs);
            let log_path = self.log_file_path.clone();
            let fmt_arc = Arc::clone(&self.detected_format);

            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut file_out = File::create(&log_path).ok();

                for line in reader.lines().flatten() {
                    if let Some(f) = file_out.as_mut() {
                        let _ = writeln!(f, "{}", line);
                    }

                    if let Some(fmt_str) = parse_detected_format(&line, 60) {
                        if let Ok(mut lock) = fmt_arc.lock() {
                            *lock = Some(fmt_str);
                        }
                    }

                    if let Ok(mut logs) = logs_arc.lock() {
                        if logs.len() >= 40 {
                            logs.pop_front();
                        }
                        logs.push_back(line);
                    }
                }
            });
        }

        self.child = Some(child);
        Ok(())
    }

    pub fn get_detected_format(&self) -> Option<String> {
        self.detected_format.lock().ok().and_then(|f| f.clone())
    }

    /// Check if child process is healthy. If it exited, returns error with recent logs.
    pub fn check_health(&mut self) -> Result<()> {
        if let Some(child) = &mut self.child {
            match child.try_wait()? {
                Some(status) => {
                    let recent = self.get_recent_logs();
                    let start = recent.len().saturating_sub(5);
                    let err_summary = recent[start..].join("\n");

                    bail!(
                        "Stream [{}] stopped unexpectedly (exit: {}).\nLast log lines:\n{}",
                        self.source.name,
                        status,
                        if err_summary.is_empty() { "No stderr captured" } else { &err_summary }
                    );
                }
                None => Ok(()),
            }
        } else {
            bail!("Recorder not running");
        }
    }

    #[allow(dead_code)]
    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => true,
                _ => false,
            }
        } else {
            false
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        if let Some(child) = self.child.take() {
            tracing::info!("Stopping recorder for source [{}]...", self.source.name);
            stop_child_gracefully(child, &self.source.name, Duration::from_secs(3));
        }
        Ok(())
    }
}

impl Drop for StreamRecorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct AudioTrackMonitor {
    #[cfg(target_os = "windows")]
    _stream: Option<cpal::Stream>,
    #[cfg(not(target_os = "windows"))]
    child: Option<Child>,
    peak_level: Arc<AtomicU32>, // (0..10000)
    device_identifier: String,
    #[allow(dead_code)]
    device_index: usize,
}

impl AudioTrackMonitor {
    pub fn new(device_identifier: impl Into<String>) -> Self {
        let identifier = device_identifier.into();
        let device_index = identifier.parse::<usize>().unwrap_or(0);
        let peak_level = Arc::new(AtomicU32::new(0));

        #[cfg(target_os = "windows")]
        let stream = crate::platform::windows::start_cpal_monitor(&identifier, Arc::clone(&peak_level));

        #[cfg(not(target_os = "windows"))]
        let child = {
            let mut cmd = Command::new("ffmpeg");
            cmd.arg("-y");
            cmd.args(crate::platform::CurrentPlatform.build_audio_monitor_args(&identifier));
            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::null());

            let mut child = cmd.spawn().ok();

            if let Some(ch) = child.as_mut() {
                if let Some(stdout) = ch.stdout.take() {
                    let peak_clone = Arc::clone(&peak_level);
                    thread::spawn(move || {
                        use std::io::Read;
                        let mut reader = BufReader::with_capacity(1024, stdout);
                        let mut buf = [0u8; 1024]; // 256 samples of 8kHz f32 = 32ms
                        while reader.read_exact(&mut buf).is_ok() {
                            let mut max_abs: f32 = 0.0;
                            for chunk in buf.chunks_exact(4) {
                                let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).abs();
                                if sample > max_abs {
                                    max_abs = sample;
                                }
                            }
                            // Scale response slightly so natural speech is well-represented in the green/amber zones
                            let boost = (max_abs * 2.5).clamp(0.0, 1.0);
                            let int_val = (boost * 10000.0) as u32;
                            peak_clone.store(int_val, Ordering::Relaxed);
                        }
                        peak_clone.store(0, Ordering::Relaxed);
                    });
                }
            }
            child
        };

        Self {
            #[cfg(target_os = "windows")]
            _stream: stream,
            #[cfg(not(target_os = "windows"))]
            child,
            peak_level,
            device_identifier: identifier,
            device_index,
        }
    }

    #[allow(dead_code)]
    pub fn device_index(&self) -> usize {
        self.device_index
    }

    pub fn device_identifier(&self) -> &str {
        &self.device_identifier
    }

    pub fn get_peak(&self) -> f32 {
        #[cfg(target_os = "windows")]
        let val = self.peak_level.swap(0, Ordering::Relaxed);
        #[cfg(not(target_os = "windows"))]
        let val = self.peak_level.load(Ordering::Relaxed);
        (val as f32) / 10000.0
    }

    pub fn stop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            self._stream = None;
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Drop for AudioTrackMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

