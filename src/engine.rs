use crate::config::Config;
use crate::recorder::StreamRecorder;
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use anyhow::Result;
use colored::Colorize;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct RecordingEngine {
    config: Config,
    recorders: Vec<StreamRecorder>,
    managers: Vec<RingBufferManager>,
}

impl RecordingEngine {
    pub fn new(config: Config) -> Self {
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
            recorders: Vec::new(),
            managers,
        }
    }

    /// Starts all recorders and runs the interactive flashback loop
    pub async fn run(&mut self) -> Result<()> {
        fs::create_dir_all(&self.config.buffer_dir)?;
        fs::create_dir_all(&self.config.output_dir)?;

        println!("{}", "════════════════════════════════════════════════════════════════".cyan().bold());
        println!("{}", "             MULTI-RECORDING & FLASHBACK ENGINE                 ".green().bold());
        println!("{}", "════════════════════════════════════════════════════════════════".cyan().bold());
        println!("Buffer Directory : {:?}", self.config.buffer_dir);
        println!("Output Directory : {:?}", self.config.output_dir);
        println!("Buffer Duration  : {}s ({} minutes)", self.config.buffer_duration_secs, self.config.buffer_duration_secs / 60);
        println!("Active Sources   : {}", self.config.sources.len());

        for src in &self.config.sources {
            println!(
                "  • [{}] Video: Device {} | Encoder: {} ({})",
                src.name.yellow().bold(),
                src.video_device,
                src.encoder,
                src.video_bitrate
            );
        }

        println!("{}", "Audio Tracks (Multi-Track):".white().bold());
        for (i, track) in self.config.audio_tracks.iter().enumerate() {
            if track.enabled {
                println!(
                    "  • Track {}: {} (Audio Device {})",
                    i + 1,
                    track.name.green().bold(),
                    track.device_index
                );
            }
        }

        println!("{}", "────────────────────────────────────────────────────────────────".cyan());
        println!("{}", "Controls:".white().bold());
        println!("  [{}] : Save Flashback (last {} mins)", "F / Space".green().bold(), self.config.buffer_duration_secs / 60);
        println!("  [{}] : Display live buffer statistics", "S".blue().bold());
        println!("  [{}] : Graceful exit / stop recording", "Q / Esc".red().bold());
        println!("{}", "────────────────────────────────────────────────────────────────".cyan());

        // Spawn recorders
        for src in self.config.sources.clone() {
            let mut recorder = StreamRecorder::new(
                src,
                self.config.audio_tracks.clone(),
                self.config.segment_duration_secs,
            );
            recorder.start(&self.config.buffer_dir)?;
            self.recorders.push(recorder);
        }

        // Enable terminal raw mode for instant keypress handling
        let _ = crossterm::terminal::enable_raw_mode();
        let running = Arc::new(AtomicBool::new(true));

        let mut last_eviction = Instant::now();
        let eviction_interval = Duration::from_secs(5);
        let max_age = Duration::from_secs(self.config.buffer_duration_secs);

        println!("\n{}", "● Continuous Flashback Recording is LIVE...".bright_green().bold());

        while running.load(Ordering::SeqCst) {
            // Periodic Ring Buffer Eviction
            if last_eviction.elapsed() >= eviction_interval {
                for mgr in &self.managers {
                    let _ = mgr.evict_older_than(max_age);
                }
                last_eviction = Instant::now();
            }

            // Health check active recorders
            for rec in &mut self.recorders {
                if let Err(e) = rec.check_health() {
                    println!("\r\n{}", format!("❌ {}", e).red().bold());
                    running.store(false, Ordering::SeqCst);
                    break;
                }
            }

            // Non-blocking key event check
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key_event) = event::read()? {
                    match key_event.code {
                        KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Char(' ') => {
                            self.trigger_flashback(self.config.buffer_duration_secs)?;
                        }
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            self.print_stats()?;
                        }
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                            println!("\r\n{}", "Exiting... Stopping streams gracefully.".yellow());
                            running.store(false, Ordering::SeqCst);
                            break;
                        }
                        KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            println!("\r\n{}", "Ctrl+C received. Stopping streams gracefully.".yellow());
                            running.store(false, Ordering::SeqCst);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        let _ = crossterm::terminal::disable_raw_mode();

        // Stop all recorders gracefully
        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }

        println!("{}", "All recorders stopped. Buffer intact.".green());
        Ok(())
    }

    /// Executes instantaneous lossless flashback save across all active streams
    pub fn trigger_flashback(&self, duration_secs: u64) -> Result<()> {
        let start_time = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        println!("\r\n{}", "⚡ FLASHBACK TRIGGERED! Exporting synchronized clips...".magenta().bold());

        let target_duration = Duration::from_secs(duration_secs);
        let mut exported_files = Vec::new();

        for mgr in &self.managers {
            let segments = mgr.get_segments_for_duration(target_duration)?;
            if segments.is_empty() {
                println!(
                    "\r  ⚠ No buffered segments ready yet for stream [{}] (wait ~5s).",
                    mgr.get_stats()?.segment_count
                );
                continue;
            }

            let stream_output = self.config.output_dir.join(format!(
                "Flashback_{}_{}.mp4",
                timestamp,
                mgr.source_name()
            ));
            Remuxer::stitch_segments(&segments, &stream_output)?;
            exported_files.push(stream_output);
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

    pub fn print_stats(&self) -> Result<()> {
        println!("\r\n{}", "--- Live Buffer Health ---".blue().bold());
        for mgr in &self.managers {
            let stats = mgr.get_stats()?;
            println!(
                "\r  Stream: count: {} segments | size: {:.1} MB | duration: ~{}s",
                stats.segment_count,
                stats.total_size_mb,
                stats.estimated_duration_secs
            );
        }
        println!();
        Ok(())
    }
}
