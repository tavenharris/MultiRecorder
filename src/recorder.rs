use crate::config::{AudioTrackConfig, SourceConfig};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct PreviewStream {
    video_device: String,
    is_screen: bool,
    child: Option<Child>,
    latest_frame: Arc<Mutex<Option<Vec<u8>>>>,
    detected_format: Arc<Mutex<Option<String>>>,
}

impl PreviewStream {
    pub fn new(video_device: String, is_screen: bool) -> Self {
        Self {
            video_device,
            is_screen,
            child: None,
            latest_frame: Arc::new(Mutex::new(None)),
            detected_format: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&mut self) -> Result<()> {
        let mut cmd = Command::new("ffmpeg");
        let framerate = if self.is_screen { "60" } else { "30" };
        cmd.args([
            "-y",
            "-f", "avfoundation",
            "-framerate", framerate,
            "-i", &format!("{}:none", self.video_device),
            "-vf", "scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2:black,fps=15",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
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
            thread::spawn(move || {
                use std::io::Read;
                let mut reader = BufReader::with_capacity(320 * 180 * 4 * 2, stdout);
                let mut buf = vec![0u8; 320 * 180 * 4];
                while reader.read_exact(&mut buf).is_ok() {
                    if let Ok(mut lock) = frame_arc.lock() {
                        *lock = Some(buf.clone());
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let fmt_arc = Arc::clone(&self.detected_format);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let res_re = regex::Regex::new(r"(\d{3,5}x\d{3,5})").ok();
                let fps_re = regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:fps|tbr)").ok();
                for line in reader.lines().flatten() {
                    if line.contains("Stream #0:0") && line.contains("Video:") {
                        let res = res_re.as_ref().and_then(|r| r.captures(&line)).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
                        let fps = fps_re.as_ref().and_then(|r| r.captures(&line)).and_then(|c| c.get(1)).map(|m| {
                            let n: f32 = m.as_str().parse().unwrap_or(30.0);
                            format!("{}fps", n.round() as u32)
                        }).unwrap_or_else(|| "30fps".to_string());
                        if let Some(res) = res {
                            if let Ok(mut lock) = fmt_arc.lock() {
                                *lock = Some(format!("{}  {}", res, fps));
                                break;
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
    detected_format: Arc<Mutex<Option<String>>>,
}

impl StreamRecorder {
    pub fn new(
        source: SourceConfig,
        audio_tracks: Vec<AudioTrackConfig>,
        segment_duration_secs: u64,
        audio_bitrate: String,
    ) -> Self {
        let log_dir = PathBuf::from("/tmp/multirecording_logs");
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

        // Input 0: Video device + first enabled audio device (if any)
        // Pass exact hardware-supported framerate (30 for webcams, up to 60 for capture cards/screen)
        if let Some(first_track) = enabled_tracks.first() {
            cmd.args([
                "-f",
                "avfoundation",
                "-framerate",
                &in_fps.to_string(),
                "-i",
                &format!("{}:{}", self.source.video_device, first_track.device_index),
            ]);
        } else {
            cmd.args([
                "-f",
                "avfoundation",
                "-framerate",
                &in_fps.to_string(),
                "-i",
                &format!("{}:none", self.source.video_device),
            ]);
        }

        // Inputs 1..N: Additional audio devices (Tracks 2..5)
        for track in enabled_tracks.iter().skip(1) {
            cmd.args([
                "-f",
                "avfoundation",
                "-i",
                &format!("none:{}", track.device_index),
            ]);
        }

        // Filter complex: split video into [rec] and [prev] with locked 16:9 aspect ratio and padding
        let filter_str = if let Some(res) = &self.source.resolution {
            if let Some((w, h)) = res.split_once('x') {
                format!(
                    "[0:v]split=2[rec_raw][prev_raw];\
                     [rec_raw]scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:black,fps={}[rec];\
                     [prev_raw]scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2:black,fps=15[prev]",
                    w, h, w, h, fps
                )
            } else {
                format!(
                    "[0:v]split=2[rec_raw][prev_raw];\
                     [rec_raw]fps={}[rec];\
                     [prev_raw]scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2:black,fps=15[prev]",
                    fps
                )
            }
        } else {
            format!(
                "[0:v]split=2[rec_raw][prev_raw];\
                 [rec_raw]fps={}[rec];\
                 [prev_raw]scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2:black,fps=15[prev]",
                fps
            )
        };

        cmd.args(["-filter_complex", &filter_str]);

        // Output 0: Segments
        cmd.args(["-map", "[rec]"]);

        // Map each audio stream with its dedicated track title metadata
        for (i, track) in enabled_tracks.iter().enumerate() {
            cmd.args(["-map", &format!("{}:a", i)]);
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

        // Output 1: Live Preview Pipe (RGBA 320x180 rawvideo to stdout)
        cmd.args([
            "-map", "[prev]",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
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
            thread::spawn(move || {
                use std::io::Read;
                let mut reader = BufReader::with_capacity(320 * 180 * 4 * 2, stdout);
                let mut buf = vec![0u8; 320 * 180 * 4];
                while reader.read_exact(&mut buf).is_ok() {
                    if let Ok(mut lock) = frame_arc.lock() {
                        *lock = Some(buf.clone());
                    }
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
                let res_re = regex::Regex::new(r"(\d{3,5}x\d{3,5})").ok();
                let fps_re = regex::Regex::new(r"(\d+(?:\.\d+)?)\s*(?:fps|tbr)").ok();

                for line in reader.lines().flatten() {
                    if let Some(f) = file_out.as_mut() {
                        let _ = writeln!(f, "{}", line);
                    }

                    if line.contains("Stream #0:0") && line.contains("Video:") {
                        let res = res_re.as_ref().and_then(|r| r.captures(&line)).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
                        let fps = fps_re.as_ref().and_then(|r| r.captures(&line)).and_then(|c| c.get(1)).map(|m| {
                            let n: f32 = m.as_str().parse().unwrap_or(60.0);
                            format!("{}fps", n.round() as u32)
                        }).unwrap_or_else(|| "60fps".to_string());
                        if let Some(res) = res {
                            if let Ok(mut lock) = fmt_arc.lock() {
                                *lock = Some(format!("{}  {}", res, fps));
                            }
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
                    let err_summary = recent
                        .iter()
                        .rev()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");

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
        if let Some(mut child) = self.child.take() {
            tracing::info!("Stopping recorder for source [{}]...", self.source.name);
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"q\n");
            }
            let _ = child.wait();
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
    child: Option<Child>,
    peak_level: Arc<AtomicU32>, // (0..10000)
    device_index: usize,
}

impl AudioTrackMonitor {
    pub fn new(device_index: usize) -> Self {
        let peak_level = Arc::new(AtomicU32::new(0));

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-f", "avfoundation",
            "-i", &format!("none:{}", device_index),
            "-vn",
            "-ac", "1",
            "-ar", "8000",
            "-f", "f32le",
            "pipe:1",
        ]);
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

        Self {
            child,
            peak_level,
            device_index,
        }
    }

    pub fn device_index(&self) -> usize {
        self.device_index
    }

    pub fn get_peak(&self) -> f32 {
        let val = self.peak_level.load(Ordering::Relaxed);
        (val as f32) / 10000.0
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AudioTrackMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

