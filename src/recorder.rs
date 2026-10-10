use crate::config::{AudioTrackConfig, SourceConfig};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct StreamRecorder {
    source: SourceConfig,
    audio_tracks: Vec<AudioTrackConfig>,
    child: Option<Child>,
    segment_duration_secs: u64,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
    log_file_path: PathBuf,
}

impl StreamRecorder {
    pub fn new(
        source: SourceConfig,
        audio_tracks: Vec<AudioTrackConfig>,
        segment_duration_secs: u64,
    ) -> Self {
        let log_dir = PathBuf::from("/tmp/multirecording_logs");
        let _ = fs::create_dir_all(&log_dir);
        let log_file_path = log_dir.join(format!("{}.log", source.name));

        Self {
            source,
            audio_tracks,
            child: None,
            segment_duration_secs,
            recent_logs: Arc::new(Mutex::new(VecDeque::with_capacity(50))),
            log_file_path,
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

    pub fn log_file_path(&self) -> &Path {
        &self.log_file_path
    }

    pub fn start(&mut self, buffer_dir: &Path) -> Result<()> {
        let segment_pattern = buffer_dir
            .join(format!("{}_%Y%m%d_%H%M%S.mp4", self.source.name))
            .to_string_lossy()
            .to_string();

        let fps = self.source.fps.unwrap_or(30);
        let gop_size = fps * self.segment_duration_secs as u32;

        let enabled_tracks: Vec<&AudioTrackConfig> = self
            .audio_tracks
            .iter()
            .filter(|t| t.enabled)
            .collect();

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y"]);

        // Input 0: Video device + first enabled audio device (if any)
        if let Some(first_track) = enabled_tracks.first() {
            cmd.args([
                "-f",
                "avfoundation",
                "-i",
                &format!("{}:{}", self.source.video_device, first_track.device_index),
            ]);
        } else {
            cmd.args([
                "-f",
                "avfoundation",
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

        // Map video from input 0
        cmd.args(["-map", "0:v"]);

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

        // Optional scaling filter if resolution requested
        if let Some(res) = &self.source.resolution {
            if let Some((w, h)) = res.split_once('x') {
                cmd.args(["-vf", &format!("scale={}:{}", w, h)]);
            }
        }

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
            cmd.args(["-c:a", "aac", "-b:a", "320k"]);
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

        // Setup pipes
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::null());
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

        // Background thread to continuously drain stderr, avoid pipe stall, and capture live logs
        if let Some(stderr) = child.stderr.take() {
            let logs_arc = Arc::clone(&self.recent_logs);
            let log_path = self.log_file_path.clone();

            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                let mut file_out = File::create(&log_path).ok();

                for line in reader.lines().flatten() {
                    if let Some(f) = file_out.as_mut() {
                        let _ = writeln!(f, "{}", line);
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
