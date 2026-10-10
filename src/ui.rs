#![allow(deprecated)]

use crate::config::Config;
use crate::devices::{Device, DeviceManager, DeviceType};
use crate::recorder::StreamRecorder;
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Rounding, Stroke, Vec2};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ExportRecord {
    pub timestamp: String,
    pub path: PathBuf,
    pub duration_secs: u64,
    pub size_mb: f64,
}

pub struct MultiRecordingApp {
    config: Config,
    config_path: PathBuf,
    discovered_devices: Vec<Device>,

    // Engine runtime state
    is_recording: bool,
    recorders: Vec<StreamRecorder>,
    managers: Vec<RingBufferManager>,
    last_eviction: Instant,

    // Stats & Feedback
    status_message: Option<(String, Instant, bool)>, // (msg, time, is_error)
    recent_exports: Vec<ExportRecord>,
    selected_flashback_secs: u64,

    // Selected device indices for UI dropdowns
    game_video_idx: usize,
    game_audio_idx: usize,
    cam_video_idx: usize,
    cam_audio_idx: usize,
}

impl MultiRecordingApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config_path: PathBuf) -> Self {
        Self::apply_flat_theme(&cc.egui_ctx);

        let cfg = if config_path.exists() {
            Config::load_from_file(&config_path).unwrap_or_default()
        } else {
            let default_cfg = Config::default();
            let _ = default_cfg.save_to_file(&config_path);
            default_cfg
        };

        let devices = DeviceManager::list_devices().unwrap_or_default();
        let selected_secs = cfg.buffer_duration_secs;

        let mut app = Self {
            config: cfg,
            config_path,
            discovered_devices: devices,
            is_recording: false,
            recorders: Vec::new(),
            managers: Vec::new(),
            last_eviction: Instant::now(),
            status_message: None,
            recent_exports: Vec::new(),
            selected_flashback_secs: selected_secs,
            game_video_idx: 0,
            game_audio_idx: 0,
            cam_video_idx: 0,
            cam_audio_idx: 0,
        };

        app.sync_device_selections();
        app
    }

    fn apply_flat_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();

        visuals.dark_mode = true;
        visuals.override_text_color = Some(Color32::from_rgb(243, 244, 246));
        visuals.window_fill = Color32::from_rgb(24, 24, 27);
        visuals.panel_fill = Color32::from_rgb(20, 20, 22);
        visuals.faint_bg_color = Color32::from_rgb(34, 34, 38);

        // Flat surfaces and minimalist edges
        visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52));
        visuals.window_rounding = Rounding::same(6.0);

        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(28, 28, 32);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(42, 42, 48));
        visuals.widgets.noninteractive.rounding = Rounding::same(4.0);

        visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 36, 42);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(52, 52, 60));
        visuals.widgets.inactive.rounding = Rounding::same(4.0);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 48, 56);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(75, 75, 88));
        visuals.widgets.hovered.rounding = Rounding::same(4.0);

        visuals.widgets.active.bg_fill = Color32::from_rgb(56, 56, 68);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(110, 110, 130));
        visuals.widgets.active.rounding = Rounding::same(4.0);

        ctx.set_visuals(visuals);
    }

    fn sync_device_selections(&mut self) {
        if let Some(src) = self.config.sources.first() {
            self.game_video_idx = src.video_device.parse().unwrap_or(0);
            self.game_audio_idx = src.audio_device.as_ref().and_then(|a| a.parse().ok()).unwrap_or(0);
        }
        if self.config.sources.len() > 1 {
            let src2 = &self.config.sources[1];
            self.cam_video_idx = src2.video_device.parse().unwrap_or(0);
            self.cam_audio_idx = src2.audio_device.as_ref().and_then(|a| a.parse().ok()).unwrap_or(0);
        }
    }

    fn video_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Video)
            .collect()
    }

    fn audio_devices(&self) -> Vec<&Device> {
        self.discovered_devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Audio)
            .collect()
    }

    pub fn start_engine(&mut self) {
        if self.is_recording {
            return;
        }

        let _ = fs::create_dir_all(&self.config.buffer_dir);
        let _ = fs::create_dir_all(&self.config.output_dir);

        // Sync dropdown selections into config
        if let Some(src1) = self.config.sources.first_mut() {
            src1.video_device = self.game_video_idx.to_string();
            src1.audio_device = Some(self.game_audio_idx.to_string());
        }
        if self.config.sources.len() > 1 {
            self.config.sources[1].video_device = self.cam_video_idx.to_string();
            self.config.sources[1].audio_device = Some(self.cam_audio_idx.to_string());
        }
        let _ = self.config.save_to_file(&self.config_path);

        self.managers.clear();
        self.recorders.clear();

        for src in &self.config.sources {
            self.managers.push(RingBufferManager::new(
                &src.name,
                &self.config.buffer_dir,
                self.config.segment_duration_secs,
            ));
        }

        let mut success = true;
        for src in self.config.sources.clone() {
            let mut recorder = StreamRecorder::new(src.clone(), self.config.segment_duration_secs);
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
            self.status_message = Some((
                "Recording active! Rolling buffer is buffering...".to_string(),
                Instant::now(),
                false,
            ));
        } else {
            self.stop_engine();
        }
    }

    pub fn stop_engine(&mut self) {
        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }
        self.recorders.clear();
        self.is_recording = false;
        self.status_message = Some(("Recording stopped. Buffer preserved.".to_string(), Instant::now(), false));
    }

    pub fn trigger_flashback(&mut self, duration_secs: u64) {
        let start = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        let target_duration = Duration::from_secs(duration_secs);

        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

        for mgr in &self.managers {
            let segments = match mgr.get_segments_for_duration(target_duration) {
                Ok(s) => s,
                Err(e) => {
                    self.status_message = Some((format!("Error getting segments: {}", e), Instant::now(), true));
                    return;
                }
            };

            if segments.is_empty() {
                self.status_message = Some((
                    format!("No segments ready yet for stream [{}]! Wait 5 seconds.", mgr.source_name()),
                    Instant::now(),
                    true,
                ));
                return;
            }

            let out_file = self.config.output_dir.join(format!(
                "Flashback_{}_{}.mp4",
                timestamp,
                mgr.source_name()
            ));

            if let Err(e) = Remuxer::stitch_segments(&segments, &out_file) {
                self.status_message = Some((format!("Remuxing failed: {}", e), Instant::now(), true));
                return;
            }

            if let Ok(meta) = fs::metadata(&out_file) {
                total_size += meta.len() as f64 / (1024.0 * 1024.0);
            }
            exported_files.push(out_file);
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
            format!("⚡ Flashback saved in {:.2}s! ({} files exported)", elapsed.as_secs_f32(), exported_files.len()),
            Instant::now(),
            false,
        ));
    }
}

impl eframe::App for MultiRecordingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(500));

        // Background Ring Buffer Eviction
        if self.is_recording && self.last_eviction.elapsed() >= Duration::from_secs(5) {
            let max_age = Duration::from_secs(self.config.buffer_duration_secs);
            for mgr in &self.managers {
                let _ = mgr.evict_older_than(max_age);
            }
            self.last_eviction = Instant::now();
        }

        // Health check active recorders
        if self.is_recording {
            let mut failed_msg = None;
            for recorder in &mut self.recorders {
                if let Err(e) = recorder.check_health() {
                    failed_msg = Some(e.to_string());
                    break;
                }
            }
            if let Some(err) = failed_msg {
                self.status_message = Some((err, Instant::now(), true));
                self.stop_engine();
            }
        }

        // Global hotkey check (Space or 'F' triggers flashback)
        if ctx.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::F)) {
            if self.is_recording {
                self.trigger_flashback(self.selected_flashback_secs);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            // ==========================================
            // TOP BAR: Brand & Engine State
            // ==========================================
            ui.horizontal(|ui| {
                ui.heading(RichText::new("MULTI-RECORDING").font(FontId::new(20.0, FontFamily::Proportional)).strong().color(Color32::from_rgb(244, 244, 245)));
                ui.label(RichText::new("PRO").size(11.0).background_color(Color32::from_rgb(39, 39, 42)).color(Color32::from_rgb(161, 161, 170)));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.is_recording {
                        if ui.add(egui::Button::new(RichText::new("■ STOP ENGINE").color(Color32::from_rgb(244, 63, 94)).strong()).min_size(Vec2::new(120.0, 32.0))).clicked() {
                            self.stop_engine();
                        }
                        ui.label(RichText::new("● BUFFERING LIVE").color(Color32::from_rgb(16, 185, 129)).strong());
                    } else {
                        if ui.add(egui::Button::new(RichText::new("▶ START ENGINE").color(Color32::from_rgb(16, 185, 129)).strong()).min_size(Vec2::new(120.0, 32.0))).clicked() {
                            self.start_engine();
                        }
                        ui.label(RichText::new("○ IDLE").color(Color32::from_rgb(113, 113, 122)));
                    }
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            // ==========================================
            // FLASHBACK HERO SECTION (Flat High-Contrast)
            // ==========================================
            egui::Frame::none()
                .fill(Color32::from_rgb(26, 26, 30))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(39, 39, 45)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::same(16.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("RETROACTIVE FLASHBACK").size(12.0).color(Color32::from_rgb(161, 161, 170)).strong());
                            ui.label(RichText::new("Continuously buffer gameplay & facecam feeds. Export synchronized clips retroactively.").size(13.0).color(Color32::from_rgb(212, 212, 216)));

                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Buffer Window:").color(Color32::from_rgb(161, 161, 170)));
                                for &secs in &[60, 180, 300, 600, 900] {
                                    let label = match secs {
                                        60 => "1m",
                                        180 => "3m",
                                        300 => "5m",
                                        600 => "10m",
                                        900 => "15m",
                                        _ => "custom",
                                    };
                                    let is_selected = self.selected_flashback_secs == secs;
                                    let btn_color = if is_selected {
                                        Color32::from_rgb(99, 102, 241) // Indigo 500
                                    } else {
                                        Color32::from_rgb(45, 45, 52)
                                    };

                                    if ui.add(egui::Button::new(RichText::new(label).strong().color(Color32::WHITE)).fill(btn_color).min_size(Vec2::new(45.0, 26.0))).clicked() {
                                        self.selected_flashback_secs = secs;
                                        self.config.buffer_duration_secs = secs;
                                        let _ = self.config.save_to_file(&self.config_path);
                                    }
                                }
                            });
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (flashback_btn_text, btn_fill) = if self.is_recording {
                                (
                                    format!("⚡ SAVE LAST {} MINS\n[Space / F]", self.selected_flashback_secs / 60),
                                    Color32::from_rgb(225, 29, 72), // Rose 600
                                )
                            } else {
                                (
                                    "⚡ START ENGINE FIRST\n[Idle]".to_string(),
                                    Color32::from_rgb(55, 65, 81),
                                )
                            };

                            let btn = egui::Button::new(
                                RichText::new(flashback_btn_text)
                                    .font(FontId::new(14.0, FontFamily::Proportional))
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(btn_fill)
                            .min_size(Vec2::new(220.0, 52.0));

                            if ui.add(btn).clicked() && self.is_recording {
                                self.trigger_flashback(self.selected_flashback_secs);
                            }
                        });
                    });
                });

            ui.add_space(14.0);

            // ==========================================
            // STATUS & NOTIFICATION BANNER
            // ==========================================
            if let Some((msg, time, is_error)) = &self.status_message {
                if time.elapsed() < Duration::from_secs(8) {
                    let (bg, border, text_color) = if *is_error {
                        (Color32::from_rgb(69, 10, 10), Color32::from_rgb(185, 28, 28), Color32::from_rgb(254, 202, 202))
                    } else {
                        (Color32::from_rgb(6, 78, 59), Color32::from_rgb(5, 150, 105), Color32::from_rgb(209, 250, 229))
                    };

                    egui::Frame::none()
                        .fill(bg)
                        .stroke(Stroke::new(1.0_f32, border))
                        .rounding(Rounding::same(4.0))
                        .inner_margin(Margin::symmetric(12.0, 8.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(msg).color(text_color).strong());
                        });
                    ui.add_space(8.0);
                }
            }

            // Pre-clone device options to avoid multiple borrows inside column closures
            let video_options: Vec<(usize, String)> = self.video_devices().into_iter().map(|d| (d.index, d.name.clone())).collect();
            let audio_options: Vec<(usize, String)> = self.audio_devices().into_iter().map(|d| (d.index, d.name.clone())).collect();

            // ==========================================
            // DUAL-SOURCE CONFIGURATION CARDS
            // ==========================================
            ui.columns(2, |columns| {
                // Left Column: Stream 1 (Gameplay)
                columns[0].vertical(|ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgb(28, 28, 32))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(42, 42, 48)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::same(14.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("🎮 SOURCE 1: GAMEPLAY").strong().color(Color32::from_rgb(244, 244, 245)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new("4K60 / VT").size(11.0).color(Color32::from_rgb(113, 113, 122)));
                                });
                            });
                            ui.add_space(6.0);

                            ui.label(RichText::new("Video Input (Capture Card / Screen)").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                            let current_v_label = video_options
                                .iter()
                                .find(|(idx, _)| *idx == self.game_video_idx)
                                .map(|(idx, name)| format!("[{}] {}", idx, name))
                                .unwrap_or_else(|| format!("Device {}", self.game_video_idx));

                            egui::ComboBox::from_id_source("game_v_combo")
                                .width(ui.available_width())
                                .selected_text(current_v_label)
                                .show_ui(ui, |ui| {
                                    for (idx, name) in &video_options {
                                        ui.selectable_value(&mut self.game_video_idx, *idx, format!("[{}] {}", idx, name));
                                    }
                                });

                            ui.add_space(8.0);
                            ui.label(RichText::new("Audio Input (HDMI Game Audio)").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                            let current_a_label = audio_options
                                .iter()
                                .find(|(idx, _)| *idx == self.game_audio_idx)
                                .map(|(idx, name)| format!("[{}] {}", idx, name))
                                .unwrap_or_else(|| format!("Device {}", self.game_audio_idx));

                            egui::ComboBox::from_id_source("game_a_combo")
                                .width(ui.available_width())
                                .selected_text(current_a_label)
                                .show_ui(ui, |ui| {
                                    for (idx, name) in &audio_options {
                                        ui.selectable_value(&mut self.game_audio_idx, *idx, format!("[{}] {}", idx, name));
                                    }
                                });

                            ui.add_space(8.0);
                            if let Some(mgr) = self.managers.first() {
                                if let Ok(stats) = mgr.get_stats() {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Buffer:").size(11.0).color(Color32::from_rgb(113, 113, 122)));
                                        ui.label(RichText::new(format!("{} chunks ({:.1} MB)", stats.segment_count, stats.total_size_mb)).size(11.0).color(Color32::from_rgb(16, 185, 129)).strong());
                                    });
                                }
                            }
                        });
                });

                // Right Column: Stream 2 (Facecam)
                columns[1].vertical(|ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgb(28, 28, 32))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(42, 42, 48)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::same(14.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("📷 SOURCE 2: FACECAM").strong().color(Color32::from_rgb(244, 244, 245)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new("1080p60 / VT").size(11.0).color(Color32::from_rgb(113, 113, 122)));
                                });
                            });
                            ui.add_space(6.0);

                            ui.label(RichText::new("Video Input (Webcam / DSLR)").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                            let current_cam_v_label = video_options
                                .iter()
                                .find(|(idx, _)| *idx == self.cam_video_idx)
                                .map(|(idx, name)| format!("[{}] {}", idx, name))
                                .unwrap_or_else(|| format!("Device {}", self.cam_video_idx));

                            egui::ComboBox::from_id_source("cam_v_combo")
                                .width(ui.available_width())
                                .selected_text(current_cam_v_label)
                                .show_ui(ui, |ui| {
                                    for (idx, name) in &video_options {
                                        ui.selectable_value(&mut self.cam_video_idx, *idx, format!("[{}] {}", idx, name));
                                    }
                                });

                            ui.add_space(8.0);
                            ui.label(RichText::new("Audio Input (Microphone)").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                            let current_cam_a_label = audio_options
                                .iter()
                                .find(|(idx, _)| *idx == self.cam_audio_idx)
                                .map(|(idx, name)| format!("[{}] {}", idx, name))
                                .unwrap_or_else(|| format!("Device {}", self.cam_audio_idx));

                            egui::ComboBox::from_id_source("cam_a_combo")
                                .width(ui.available_width())
                                .selected_text(current_cam_a_label)
                                .show_ui(ui, |ui| {
                                    for (idx, name) in &audio_options {
                                        ui.selectable_value(&mut self.cam_audio_idx, *idx, format!("[{}] {}", idx, name));
                                    }
                                });

                            ui.add_space(8.0);
                            if self.managers.len() > 1 {
                                if let Ok(stats) = self.managers[1].get_stats() {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Buffer:").size(11.0).color(Color32::from_rgb(113, 113, 122)));
                                        ui.label(RichText::new(format!("{} chunks ({:.1} MB)", stats.segment_count, stats.total_size_mb)).size(11.0).color(Color32::from_rgb(16, 185, 129)).strong());
                                    });
                                }
                            }
                        });
                });
            });

            ui.add_space(14.0);

            // ==========================================
            // RECENT EXPORTS & NLE TIME-SYNC READY
            // ==========================================
            ui.horizontal(|ui| {
                ui.label(RichText::new("SAVED FLASHBACK RECORDINGS").size(12.0).color(Color32::from_rgb(161, 161, 170)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new("📂 Open Folder").size(11.0).color(Color32::from_rgb(161, 161, 170)))).clicked() {
                        let _ = Command::new("open").arg(&self.config.output_dir).spawn();
                    }
                });
            });
            ui.add_space(4.0);

            egui::Frame::none()
                .fill(Color32::from_rgb(22, 22, 25))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(36, 36, 40)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::same(12.0))
                .show(ui, |ui| {
                    if self.recent_exports.is_empty() {
                        ui.label(RichText::new("No flashbacks saved yet in this session. Hit 'Space' or click the button above while recording.").color(Color32::from_rgb(113, 113, 122)));
                    } else {
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            for exp in &self.recent_exports {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("⚡ {}", exp.timestamp)).strong().color(Color32::from_rgb(244, 244, 245)));
                                    ui.label(RichText::new(format!("({}s / {:.1} MB)", exp.duration_secs, exp.size_mb)).size(11.0).color(Color32::from_rgb(113, 113, 122)));

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.add(egui::Button::new(RichText::new("Reveal").size(11.0))).clicked() {
                                            let _ = Command::new("open").args(["-R", &exp.path.to_string_lossy()]).spawn();
                                        }
                                    });
                                });
                                ui.separator();
                            }
                        });
                    }
                });

            ui.add_space(10.0);

            // ==========================================
            // LIVE STREAM DIAGNOSTICS & DEBUG DRAWER
            // ==========================================
            ui.collapsing("🔍 Live Stream Diagnostics & Logs", |ui| {
                if self.recorders.is_empty() {
                    ui.label(RichText::new("No active streams. Start the engine to see live FFmpeg diagnostics.").color(Color32::from_rgb(113, 113, 122)));
                } else {
                    for recorder in &self.recorders {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("● Stream [{}] Output:", recorder.source_name())).strong().color(Color32::from_rgb(244, 244, 245)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("Open Log in TextEdit").size(10.0))).clicked() {
                                    let _ = Command::new("open").arg("-a").arg("TextEdit").arg(recorder.log_file_path()).spawn();
                                }
                            });
                        });

                        let logs = recorder.get_recent_logs();
                        egui::Frame::none()
                            .fill(Color32::from_rgb(15, 15, 17))
                            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(32, 32, 36)))
                            .rounding(Rounding::same(4.0))
                            .inner_margin(Margin::same(8.0))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(80.0).stick_to_bottom(true).show(ui, |ui| {
                                    if logs.is_empty() {
                                        ui.label(RichText::new("Awaiting encoder output...").size(10.0).color(Color32::from_rgb(113, 113, 122)));
                                    } else {
                                        for line in &logs {
                                            ui.label(RichText::new(line).monospace().size(10.0).color(Color32::from_rgb(209, 213, 219)));
                                        }
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    }
                }
            });
        });
    }
}
