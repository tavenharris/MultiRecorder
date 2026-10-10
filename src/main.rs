mod config;
mod devices;
mod engine;
mod recorder;
mod remuxer;
mod ring_buffer;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use config::Config;
use devices::{DeviceManager, DeviceType};
use engine::RecordingEngine;
use ring_buffer::RingBufferManager;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "multirecording",
    about = "High-Performance Multi-Source Video & Flashback Recording for Content Creators",
    version = "0.1.0"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the native graphical interface (Flat Design)
    Gui {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },

    /// List all available AVFoundation video capture devices (capture cards, webcams) and audio inputs
    Devices,

    /// Generate a template config.toml configuration file
    Init {
        #[arg(short, long, default_value = "config.toml")]
        path: PathBuf,
    },

    /// Start the continuous multi-source recording engine in terminal mode
    Run {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },

    /// Show current buffer disk metrics and duration
    Status {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },

    /// Save a flashback clip from the current buffer (lossless remux)
    Save {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        #[arg(short, long, default_value_t = 300)]
        duration: u64,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Gui { config: PathBuf::from("config.toml") }) {
        Commands::Gui { config: config_path } => {
            let native_options = eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_inner_size([900.0, 760.0])
                    .with_min_inner_size([800.0, 600.0])
                    .with_title("MultiRecording Pro"),
                ..Default::default()
            };

            eframe::run_native(
                "MultiRecording Pro",
                native_options,
                Box::new(|cc| Ok(Box::new(ui::MultiRecordingApp::new(cc, config_path)))),
            )
            .map_err(|e| anyhow::anyhow!("UI error: {}", e))?;
        }

        Commands::Devices => {
            println!("{}", "════════════════════════════════════════════════════════════════".cyan().bold());
            println!("{}", "               DISCOVERED AVFOUNDATION DEVICES                  ".green().bold());
            println!("{}", "════════════════════════════════════════════════════════════════".cyan().bold());

            let devices = DeviceManager::list_devices()?;

            let video_devs: Vec<_> = devices.iter().filter(|d| d.device_type == DeviceType::Video).collect();
            let audio_in_devs: Vec<_> = devices.iter().filter(|d| d.device_type == DeviceType::AudioInput).collect();
            let audio_out_devs: Vec<_> = devices.iter().filter(|d| d.device_type == DeviceType::AudioOutput).collect();

            println!("\n{}", "📹 VIDEO SOURCES (Capture Cards, Cameras, Screens):".yellow().bold());
            if video_devs.is_empty() {
                println!("  No video devices found.");
            } else {
                for dev in video_devs {
                    println!("  [{}] {}", dev.index.to_string().cyan().bold(), dev.name);
                }
            }

            println!("\n{}", "🎙 AUDIO INPUTS (Mics, HDMI Audio, Interfaces):".yellow().bold());
            if audio_in_devs.is_empty() {
                println!("  No audio inputs found.");
            } else {
                for dev in audio_in_devs {
                    println!("  [{}] {}", dev.index.to_string().cyan().bold(), dev.name);
                }
            }

            println!("\n{}", "🔊 AUDIO OUTPUTS (Speakers, AirPods, Headphones):".yellow().bold());
            if audio_out_devs.is_empty() {
                println!("  No audio outputs found.");
            } else {
                for dev in audio_out_devs {
                    println!("  • {}", dev.name.cyan().bold());
                }
            }
            println!("\nTip: To route audio outputs (Speakers/AirPods) into a recording track, use BlackHole loopback or Multi-Output Device.");
        }

        Commands::Init { path } => {
            if path.exists() {
                println!("Config file already exists at {:?}", path);
            } else {
                let default_cfg = Config::default();
                default_cfg.save_to_file(&path)?;
                println!("✔ Created default configuration at {:?}", path);
                println!("Edit `config.toml` to select your capture card and webcam device indices.");
            }
        }

        Commands::Run { config: config_path } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let cfg = if config_path.exists() {
                    Config::load_from_file(&config_path)?
                } else {
                    println!(
                        "{} Config file not found at {:?}. Generating default...",
                        "⚠".yellow(),
                        config_path
                    );
                    let default_cfg = Config::default();
                    default_cfg.save_to_file(&config_path)?;
                    default_cfg
                };

                let mut engine = RecordingEngine::new(cfg);
                engine.run().await
            })?;
        }

        Commands::Status { config: config_path } => {
            let cfg = Config::load_from_file(&config_path)?;
            println!("Buffer Directory: {:?}", cfg.buffer_dir);
            for src in &cfg.sources {
                let mgr = RingBufferManager::new(&src.name, &cfg.buffer_dir, cfg.segment_duration_secs);
                let stats = mgr.get_stats()?;
                println!(
                    "Stream [{}]: {} segments | {:.2} MB | ~{}s buffered",
                    src.name.yellow().bold(),
                    stats.segment_count,
                    stats.total_size_mb,
                    stats.estimated_duration_secs
                );
            }
        }

        Commands::Save { config: config_path, duration } => {
            let cfg = Config::load_from_file(&config_path)?;
            let engine = RecordingEngine::new(cfg);
            engine.trigger_flashback(duration)?;
        }
    }

    Ok(())
}
