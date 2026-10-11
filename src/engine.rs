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
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);

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

        // Spawn recorders (primary stream captures audio tracks, secondary streams capture video + optional sync track)
        for (i, src) in self.config.sources.clone().into_iter().enumerate() {
            let tracks = if i == 0 {
                self.config.audio_tracks.clone()
            } else if i == 1 && self.config.sync_audio_to_secondary {
                if let Some(t1) = self.config.audio_tracks.first().cloned() {
                    if t1.enabled {
                        vec![t1]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            let mut recorder = StreamRecorder::new(
                src,
                tracks,
                self.config.segment_duration_secs,
                self.config.audio_bitrate.clone(),
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

        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }

        println!("{}", "All recorders stopped. Buffer cleared.".green());
        Ok(())
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

    pub fn print_stats(&self) -> Result<()> {
        println!("\r\n{}", "--- Live Buffer Health ---".blue().bold());
        for mgr in &self.managers {
            let stats = mgr.get_stats()?;
            println!(
                "\r  Stream [{}]: count: {} segments | size: {:.1} MB | duration: ~{}s",
                mgr.source_name().yellow().bold(),
                stats.segment_count,
                stats.total_size_mb,
                stats.estimated_duration_secs
            );
        }
        println!();
        Ok(())
    }
}
