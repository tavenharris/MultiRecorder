use crate::config::Config;
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use crate::tui::TuiApp;
use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct RecordingEngine {
    config: Config,
    config_path: PathBuf,
    managers: Vec<RingBufferManager>,
}

impl RecordingEngine {
    pub fn new(config: Config) -> Self {
        Self::with_config_path(config, PathBuf::from("config.toml"))
    }

    pub fn with_config_path(config: Config, config_path: PathBuf) -> Self {
        let mut managers = Vec::new();
        for source in &config.sources {
            managers.push(RingBufferManager::new(
                &source.name,
                &config.buffer_dir,
                config.segment_duration_secs,
            ));
        }

        Self {
            config,
            config_path,
            managers,
        }
    }

    /// Launches the interactive full-screen Terminal User Interface (TUI)
    pub async fn run(&mut self) -> Result<()> {
        let mut tui = TuiApp::new(self.config.clone(), self.config_path.clone());
        tui.run_loop()
    }

    /// Executes instantaneous lossless flashback save across all active streams
    pub fn trigger_flashback(&self, duration_secs: u64) -> Result<()> {
        let start_time = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        println!("\r\n{}", "⚡ FLASHBACK TRIGGERED! Exporting synchronized clips...".magenta().bold());

        let target_duration = Duration::from_secs(duration_secs);
        let mut exported_files = Vec::new();

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
                let start_time = chrono::Local::now() - chrono::Duration::seconds(duration_secs as i64);
                Some(start_time.format("%H:%M:%S:00").to_string())
            })
        } else {
            None
        };

        for (idx, mgr) in self.managers.iter().enumerate() {
            let segments = mgr.get_segments_for_duration(target_duration)?;
            if segments.is_empty() {
                println!(
                    "\r  ⚠ No buffered segments ready yet for stream [{}] (wait ~5s).",
                    mgr.source_name()
                );
                continue;
            }

            let out_ext = if self.config.output_format.trim().is_empty() {
                "mp4"
            } else {
                &self.config.output_format
            };
            let stream_output = self.config.output_dir.join(format!(
                "Flashback_{}_{}.{}",
                timestamp,
                mgr.source_name(),
                out_ext
            ));
            Remuxer::stitch_segments(&segments, &stream_output, shared_timecode.as_deref())?;
            exported_files.push(stream_output.clone());

            if idx == 0 {
                if let Ok(wav_files) = Remuxer::export_separate_audio_tracks(
                    &stream_output,
                    &self.config.output_dir,
                    "Flashback",
                    &timestamp,
                    &self.config.audio_tracks,
                    shared_timecode.as_deref(),
                ) {
                    exported_files.extend(wav_files);
                }
            }
        }

        let elapsed = start_time.elapsed();
        println!(
            "\r{}",
            format!(
                "✔ Flashback complete in {:.2}s! Synced files saved to {:?}",
                elapsed.as_secs_f32(),
                self.config.output_dir
            )
            .green()
            .bold()
        );

        Ok(())
    }
}
