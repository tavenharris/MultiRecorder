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

pub const PREVIEW_WIDTH: usize = 480;
pub const PREVIEW_HEIGHT: usize = 270;
pub const PREVIEW_FRAME_SIZE: usize = PREVIEW_WIDTH * PREVIEW_HEIGHT * 4;
pub const PREVIEW_FPS: u32 = 15;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamHealth {
    pub current_fps: f32,
    pub speed_ratio: f32,
    pub is_lagging: bool,
    pub frames_processed: u64,
}

impl Default for StreamHealth {
    fn default() -> Self {
        Self {
            current_fps: 0.0,
            speed_ratio: 1.0,
            is_lagging: false,
            frames_processed: 0,
        }
    }
}

pub fn parse_progress_metrics(line: &str) -> Option<StreamHealth> {
    if !line.contains("fps=") || !line.contains("speed=") {
        return None;
    }

    let segment = line.rsplit('\r').next().unwrap_or(line);

    // Extract fps
    let fps_idx = segment.find("fps=")?;
    let after_fps = segment[fps_idx + 4..].trim_start();
    let fps_end = after_fps.find(|c: char| c.is_whitespace() || c == 'q' || c == 's' || c == 'L').unwrap_or(after_fps.len());
    let fps: f32 = after_fps[..fps_end].parse().ok()?;

    // Extract speed
    let speed_idx = segment.find("speed=")?;
    let after_speed = segment[speed_idx + 6..].trim_start();
    let speed_end = after_speed.find(|c: char| c == 'x' || c.is_whitespace()).unwrap_or(after_speed.len());
    let speed: f32 = after_speed[..speed_end].parse().ok()?;

    // Extract frame count
    let frames = if let Some(frame_idx) = segment.find("frame=") {
        let after_frame = segment[frame_idx + 6..].trim_start();
        let frame_end = after_frame.find(|c: char| !c.is_ascii_digit()).unwrap_or(after_frame.len());
        after_frame[..frame_end].parse().unwrap_or(0)
    } else {
        0
    };

    Some(StreamHealth {
        current_fps: fps,
        speed_ratio: speed,
        is_lagging: speed > 0.0 && speed < 0.98,
        frames_processed: frames,
    })
}

static RES_REGEX: OnceLock<regex::Regex> = OnceLock::new();
static FPS_REGEX: OnceLock<regex::Regex> = OnceLock::new();

fn get_res_regex() -> &'static regex::Regex {
    RES_REGEX.get_or_init(|| regex::Regex::new(r"(\d{3,5}x\d{3,5})").expect("valid resolution regex"))
}

fn get_fps_regex() -> &'static regex::Regex {
    FPS_REGEX.get_or_init(|| regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:fps|tbr)").expect("valid fps regex"))
}

fn parse_detected_format(line: &str, default_fps: u32, force_default_fps: bool) -> Option<String> {
    if line.contains("Stream #0:0") && line.contains("Video:") {
        let res = get_res_regex().captures(line).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
        let fps = if force_default_fps {
            format!("{}fps", default_fps)
        } else {
            get_fps_regex().captures(line).and_then(|c| c.get(1)).map(|m| {
                let n: f32 = m.as_str().parse().unwrap_or(default_fps as f32);
                format!("{}fps", n.round() as u32)
            }).unwrap_or_else(|| format!("{}fps", default_fps))
        };
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
            let is_screen = self.is_screen
                || self.video_device.eq_ignore_ascii_case("desktop")
                || self.video_device.to_lowercase().contains("screen");
            let default_fps = if is_screen { 60 } else { 30 };
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut found = false;
                for line in reader.lines().map_while(Result::ok) {
                    if !found {
                        if let Some(fmt_str) = parse_detected_format(&line, default_fps, is_screen) {
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
    enable_preview: bool,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
    log_file_path: PathBuf,
    latest_frame: Arc<Mutex<Option<Vec<u8>>>>,
    frame_seq: Arc<AtomicU64>,
    detected_format: Arc<Mutex<Option<String>>>,
    health_metrics: Arc<Mutex<StreamHealth>>,
}

impl StreamRecorder {
    pub fn new(
        source: SourceConfig,
        audio_tracks: Vec<AudioTrackConfig>,
        segment_duration_secs: u64,
        audio_bitrate: String,
    ) -> Self {
        Self::with_preview(source, audio_tracks, segment_duration_secs, audio_bitrate, true)
    }

    pub fn with_preview(
        source: SourceConfig,
        audio_tracks: Vec<AudioTrackConfig>,
        segment_duration_secs: u64,
        audio_bitrate: String,
        enable_preview: bool,
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
            enable_preview,
            recent_logs: Arc::new(Mutex::new(VecDeque::with_capacity(50))),
            log_file_path,
            latest_frame: Arc::new(Mutex::new(None)),
            frame_seq: Arc::new(AtomicU64::new(0)),
            detected_format: Arc::new(Mutex::new(None)),
            health_metrics: Arc::new(Mutex::new(StreamHealth::default())),
        }
    }

    pub fn get_health(&self) -> StreamHealth {
        self.health_metrics.lock().map(|h| *h).unwrap_or_default()
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

        let raw_fps = self.source.fps.unwrap_or(60);
        let fps = if raw_fps < 24 { 30 } else { raw_fps };
        let gop_size = fps * self.segment_duration_secs as u32;

        let is_camera = self.source.name.to_lowercase().contains("cam")
            || self.source.name.to_lowercase().contains("face");
        let in_fps = if is_camera { 30 } else { fps.min(60) };

        let enabled_tracks: Vec<&AudioTrackConfig> = self
            .audio_tracks
            .iter()
            .filter(|t| t.enabled && t.device_id != "none")
            .collect();

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y"]);

        let enabled_configs: Vec<AudioTrackConfig> = enabled_tracks.iter().map(|&t| t.clone()).collect();
        cmd.args(crate::platform::CurrentPlatform.build_recording_input_args(
            &self.source.video_device,
            in_fps,
            &enabled_configs,
        ));

        if self.enable_preview {
            // Filter complex: split video into [rec] and [prev] with locked 16:9 aspect ratio, padding, and fast bilinear scaling
            let filter_str = if let Some(res) = &self.source.resolution {
                if let Some((w, h)) = res.split_once('x') {
                    format!(
                        "[0:v]split=2[rec_raw][prev_raw];\
                         [rec_raw]scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[rec];\
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
            cmd.args(["-map", "[rec]"]);
        } else {
            // Preview bypassed: direct single-pass filter with zero split and zero preview pipe overhead
            let filter_str = if let Some(res) = &self.source.resolution {
                if let Some((w, h)) = res.split_once('x') {
                    format!(
                        "[0:v]scale={}:{}:flags=fast_bilinear:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[rec]",
                        w, h, w, h, fps
                    )
                } else {
                    format!("[0:v]fps={}[rec]", fps)
                }
            } else {
                format!("[0:v]fps={}[rec]", fps)
            };

            cmd.args(["-filter_complex", &filter_str]);
            cmd.args(["-map", "[rec]"]);
        }

        // Map each audio stream as a discrete track with title/handler metadata and enabled disposition for Premiere Pro / DaVinci Resolve
        for (i, track) in enabled_tracks.iter().enumerate() {
            let input_idx = crate::platform::CurrentPlatform.audio_input_index(i);
            cmd.args(["-map", &format!("{}:a", input_idx)]);
            if (track.volume - 1.0).abs() > 0.005 {
                cmd.args([
                    &format!("-filter:a:{}", i),
                    &format!("volume={:.3}", track.volume.clamp(0.0, 4.0)),
                ]);
            }
            cmd.args([
                &format!("-metadata:s:a:{}", i),
                &format!("title={}", track.name),
                &format!("-metadata:s:a:{}", i),
                &format!("handler_name={}", track.name),
                &format!("-disposition:a:{}", i),
                "default",
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

        // Platform-specific real-time hardware encoder optimizations to eliminate frame drops
        if self.source.encoder.contains("videotoolbox") {
            cmd.args(["-realtime", "1", "-prio_speed", "1"]);
        } else if self.source.encoder.contains("nvenc") {
            cmd.args(["-preset", "p1", "-tune", "ll", "-zerolatency", "1"]);
        } else if self.source.encoder.contains("qsv") {
            cmd.args(["-preset", "veryfast", "-low_power", "1"]);
        } else if self.source.encoder.contains("amf") {
            cmd.args(["-usage", "lowlatency"]);
        }

        // Audio encoder settings (applied to all mapped audio tracks; uniform 48kHz stereo for NLE multitrack import)
        if !enabled_tracks.is_empty() {
            cmd.args([
                "-c:a",
                "aac",
                "-b:a",
                &self.audio_bitrate,
                "-ar",
                "48000",
                "-ac",
                "2",
            ]);
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

        // Output 1: Live Preview Pipe (only when enable_preview is true)
        if self.enable_preview {
            cmd.args([
                "-map", "[prev]",
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-flush_packets", "1",
                "pipe:1",
            ]);
            cmd.stdout(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
        }

        // Setup pipes
        cmd.stdin(Stdio::piped());
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
            let health_arc = Arc::clone(&self.health_metrics);

            thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut file_out = File::create(&log_path).ok();
                let mut line_buf = Vec::with_capacity(256);

                let mut process_line = |line: &str| {
                    if let Some(f) = file_out.as_mut() {
                        let _ = writeln!(f, "{}", line);
                    }

                    if let Some(health) = parse_progress_metrics(line) {
                        if let Ok(mut h) = health_arc.lock() {
                            *h = health;
                        }
                    }

                    if let Some(fmt_str) = parse_detected_format(line, 60, false) {
                        if let Ok(mut lock) = fmt_arc.lock() {
                            *lock = Some(fmt_str);
                        }
                    }

                    if let Ok(mut logs) = logs_arc.lock() {
                        if logs.len() >= 40 {
                            logs.pop_front();
                        }
                        logs.push_back(line.to_string());
                    }
                };

                loop {
                    let available = match reader.fill_buf() {
                        Ok(buf) if buf.is_empty() => break,
                        Ok(buf) => buf,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };

                    let mut consumed = 0;
                    for &b in available {
                        consumed += 1;
                        if b == b'\r' || b == b'\n' {
                            if !line_buf.is_empty() {
                                let line = String::from_utf8_lossy(&line_buf).to_string();
                                line_buf.clear();
                                process_line(&line);
                            }
                        } else {
                            line_buf.push(b);
                        }
                    }
                    reader.consume(consumed);
                }

                if !line_buf.is_empty() {
                    let line = String::from_utf8_lossy(&line_buf).to_string();
                    process_line(&line);
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
            matches!(child.try_wait(), Ok(None))
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
    peak_left: Arc<AtomicU32>,  // (0..10000)
    peak_right: Arc<AtomicU32>, // (0..10000)
    device_identifier: String,
    #[allow(dead_code)]
    device_index: usize,
}

impl AudioTrackMonitor {
    pub fn new(device_identifier: impl Into<String>) -> Self {
        let identifier = device_identifier.into();
        let device_index = identifier.parse::<usize>().unwrap_or(0);
        let peak_left = Arc::new(AtomicU32::new(0));
        let peak_right = Arc::new(AtomicU32::new(0));

        #[cfg(target_os = "windows")]
        let stream = crate::platform::windows::start_cpal_monitor(
            &identifier,
            Arc::clone(&peak_left),
            Arc::clone(&peak_right),
        );

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
                    let peak_l = Arc::clone(&peak_left);
                    let peak_r = Arc::clone(&peak_right);
                    thread::spawn(move || {
                        use std::io::Read;
                        let mut reader = BufReader::with_capacity(1024, stdout);
                        let mut buf = [0u8; 1024]; // 128 stereo frames of 8kHz f32le = 16ms
                        while reader.read_exact(&mut buf).is_ok() {
                            let mut max_l: f32 = 0.0;
                            let mut max_r: f32 = 0.0;
                            for frame in buf.chunks_exact(8) {
                                let l = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]).abs();
                                let r = f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]).abs();
                                if l > max_l {
                                    max_l = l;
                                }
                                if r > max_r {
                                    max_r = r;
                                }
                            }
                            // Scale response slightly so natural speech is well-represented in the green/amber zones
                            let boost_l = (max_l * 2.5).clamp(0.0, 1.0);
                            let boost_r = (max_r * 2.5).clamp(0.0, 1.0);
                            peak_l.store((boost_l * 10000.0) as u32, Ordering::Relaxed);
                            peak_r.store((boost_r * 10000.0) as u32, Ordering::Relaxed);
                        }
                        peak_l.store(0, Ordering::Relaxed);
                        peak_r.store(0, Ordering::Relaxed);
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
            peak_left,
            peak_right,
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

    pub fn get_peaks(&self) -> (f32, f32) {
        #[cfg(target_os = "windows")]
        let (val_l, val_r) = (
            self.peak_left.swap(0, Ordering::Relaxed),
            self.peak_right.swap(0, Ordering::Relaxed),
        );
        #[cfg(not(target_os = "windows"))]
        let (val_l, val_r) = (
            self.peak_left.load(Ordering::Relaxed),
            self.peak_right.load(Ordering::Relaxed),
        );
        ((val_l as f32) / 10000.0, (val_r as f32) / 10000.0)
    }

    #[allow(dead_code)]
    pub fn get_peak(&self) -> f32 {
        let (l, r) = self.get_peaks();
        l.max(r)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress_metrics_realtime() {
        let sample = "frame=   26 fps= 26.5 q=-0.0 size=N/A time=00:00:01.07 bitrate=N/A speed=1.07x elapsed=0:00:01.01";
        let metrics = parse_progress_metrics(sample).expect("should parse");
        assert_eq!(metrics.frames_processed, 26);
        assert!((metrics.current_fps - 26.5).abs() < 0.01);
        assert!((metrics.speed_ratio - 1.07).abs() < 0.01);
        assert!(!metrics.is_lagging);
    }

    #[test]
    fn test_parse_progress_metrics_lagging() {
        let sample = "frame=  124 fps= 24.0 q=-0.0 Lq=-0.0 size=N/A time=00:00:04.16 bitrate=N/A speed=0.85x elapsed=0:00:04.04";
        let metrics = parse_progress_metrics(sample).expect("should parse");
        assert_eq!(metrics.frames_processed, 124);
        assert!((metrics.current_fps - 24.0).abs() < 0.01);
        assert!((metrics.speed_ratio - 0.85).abs() < 0.01);
        assert!(metrics.is_lagging);
    }

    #[test]
    fn test_parse_progress_metrics_carriage_returns() {
        let sample = "frame=   10 fps=10.0 speed=1.00x\rframe=   20 fps=30.0 speed=0.92x";
        let metrics = parse_progress_metrics(sample).expect("should parse last update");
        assert_eq!(metrics.frames_processed, 20);
        assert!((metrics.current_fps - 30.0).abs() < 0.01);
        assert!((metrics.speed_ratio - 0.92).abs() < 0.01);
        assert!(metrics.is_lagging);
    }

    #[test]
    fn test_reader_splits_on_carriage_returns_and_newlines() {
        use std::io::Cursor;
        let data = b"line 1\rframe= 10 fps=30.0 speed=1.00x\rframe= 20 fps=60.0 speed=1.02x\nline 4\r\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut lines = Vec::new();
        let mut line_buf = Vec::new();

        loop {
            let available = match reader.fill_buf() {
                Ok(buf) if buf.is_empty() => break,
                Ok(buf) => buf,
                Err(_) => break,
            };
            let mut consumed = 0;
            for &b in available {
                consumed += 1;
                if b == b'\r' || b == b'\n' {
                    if !line_buf.is_empty() {
                        lines.push(String::from_utf8_lossy(&line_buf).to_string());
                        line_buf.clear();
                    }
                } else {
                    line_buf.push(b);
                }
            }
            reader.consume(consumed);
        }

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "line 1");
        assert_eq!(lines[1], "frame= 10 fps=30.0 speed=1.00x");
        assert_eq!(lines[2], "frame= 20 fps=60.0 speed=1.02x");
        assert_eq!(lines[3], "line 4");
    }
}

