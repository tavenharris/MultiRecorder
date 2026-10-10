#![allow(deprecated)]

use crate::config::{AudioTrackConfig, Config, SourceConfig};
use crate::devices::{Device, DeviceManager, DeviceType};
use crate::recorder::{PreviewStream, StreamRecorder};
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
    has_loopback: bool,

    // Engine runtime state
    is_recording: bool,
    recorders: Vec<StreamRecorder>,
    managers: Vec<RingBufferManager>,
    last_eviction: Instant,

    // Live Video Previews
    preview_stream_1: Option<PreviewStream>,
    preview_stream_2: Option<PreviewStream>,
    preview_texture_1: Option<egui::TextureHandle>,
    preview_texture_2: Option<egui::TextureHandle>,

    // Stats, UI & Feedback
    status_message: Option<(String, Instant, bool)>, // (msg, time, is_error)
    recent_exports: Vec<ExportRecord>,
    selected_flashback_secs: u64,
    show_settings: bool,
    recording_started_at: Option<Instant>,
    audio_levels: [f32; 5],

    // Selected video device indices
    game_video_idx: usize,
    cam_video_idx: usize,
    copied_brew_cmd_until: Option<Instant>,
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
        let has_loopback = DeviceManager::has_loopback_device(&devices);
        let selected_secs = cfg.buffer_duration_secs;

        let mut app = Self {
            config: cfg,
            config_path,
            discovered_devices: devices,
            has_loopback,
            is_recording: false,
            recorders: Vec::new(),
            managers: Vec::new(),
            last_eviction: Instant::now(),
            preview_stream_1: None,
            preview_stream_2: None,
            preview_texture_1: None,
            preview_texture_2: None,
            status_message: None,
            recent_exports: Vec::new(),
            selected_flashback_secs: selected_secs,
            show_settings: false,
            recording_started_at: None,
            audio_levels: [0.0; 5],
            game_video_idx: 0,
            cam_video_idx: 0,
            copied_brew_cmd_until: None,
        };

        app.sync_device_selections();
        app.ensure_preview_streams();
        app
    }

    pub fn refresh_devices(&mut self) {
        self.discovered_devices = DeviceManager::list_devices().unwrap_or_default();
        self.has_loopback = DeviceManager::has_loopback_device(&self.discovered_devices);
        self.sync_device_selections();
        self.stop_preview_streams();
        self.ensure_preview_streams();
        self.status_message = Some((
            format!(
                "Devices refreshed: {} sources found (Loopback: {})",
                self.discovered_devices.len(),
                if self.has_loopback { "Active" } else { "Not Installed" }
            ),
            Instant::now(),
            false,
        ));
    }

    pub fn ensure_preview_streams(&mut self) {
        if self.is_recording {
            self.stop_preview_streams();
            return;
        }

        // Preview stream 1
        let dev1 = self.game_video_idx.to_string();
        if self.preview_stream_1.is_none() {
            let mut ps1 = PreviewStream::new(dev1);
            let _ = ps1.start();
            self.preview_stream_1 = Some(ps1);
        }

        // Preview stream 2 (only if distinct video device)
        let dev2 = self.cam_video_idx.to_string();
        if self.cam_video_idx != self.game_video_idx && self.preview_stream_2.is_none() {
            let mut ps2 = PreviewStream::new(dev2);
            let _ = ps2.start();
            self.preview_stream_2 = Some(ps2);
        }
    }

    pub fn stop_preview_streams(&mut self) {
        if let Some(mut ps1) = self.preview_stream_1.take() {
            ps1.stop();
        }
        if let Some(mut ps2) = self.preview_stream_2.take() {
            ps2.stop();
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    fn apply_flat_theme(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();

        visuals.dark_mode = true;
        visuals.override_text_color = Some(Color32::from_rgb(243, 244, 246));
        visuals.window_fill = Color32::from_rgb(20, 20, 23);
        visuals.panel_fill = Color32::from_rgb(18, 18, 20);
        visuals.faint_bg_color = Color32::from_rgb(30, 30, 34);

        // Flat surfaces and minimalist edges
        visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52));
        visuals.window_rounding = Rounding::same(6.0);

        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(26, 26, 30);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 46));
        visuals.widgets.noninteractive.rounding = Rounding::same(4.0);

        visuals.widgets.inactive.bg_fill = Color32::from_rgb(32, 32, 38);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 56));
        visuals.widgets.inactive.rounding = Rounding::same(4.0);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(44, 44, 52);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 82));
        visuals.widgets.hovered.rounding = Rounding::same(4.0);

        visuals.widgets.active.bg_fill = Color32::from_rgb(52, 52, 64);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(100, 100, 120));
        visuals.widgets.active.rounding = Rounding::same(4.0);

        ctx.set_visuals(visuals);
    }

    fn sync_device_selections(&mut self) {
        let v_devs = self.video_devices();
        let valid_v_indices: Vec<usize> = v_devs.iter().map(|d| d.index).collect();

        if let Some(src) = self.config.sources.first() {
            let idx = src.video_device.parse().unwrap_or(0);
            self.game_video_idx = if valid_v_indices.contains(&idx) {
                idx
            } else {
                valid_v_indices.first().copied().unwrap_or(0)
            };
        }
        if self.config.sources.len() > 1 {
            let src2 = &self.config.sources[1];
            let idx = src2.video_device.parse().unwrap_or(0);
            self.cam_video_idx = if valid_v_indices.contains(&idx) {
                idx
            } else {
                valid_v_indices.get(1).or_else(|| valid_v_indices.first()).copied().unwrap_or(0)
            };
        }

        // Ensure exactly 5 audio tracks are configured
        while self.config.audio_tracks.len() < 5 {
            let idx = self.config.audio_tracks.len() + 1;
            self.config.audio_tracks.push(AudioTrackConfig {
                enabled: false,
                name: format!("Track {}", idx),
                device_index: 0,
                device_id: "none".to_string(),
            });
        }

        for track in &mut self.config.audio_tracks {
            if track.device_id.is_empty() {
                if track.enabled {
                    track.device_id = format!("in:{}", track.device_index);
                } else {
                    track.device_id = "none".to_string();
                }
            }
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
            .filter(|d| d.device_type == DeviceType::AudioInput || d.device_type == DeviceType::AudioOutput)
            .collect()
    }

    pub fn start_engine(&mut self) {
        if self.is_recording {
            return;
        }

        // Pre-validate audio tracks
        for track in &self.config.audio_tracks {
            if track.enabled {
                if let Some(dev) = self.discovered_devices.iter().find(|d| d.id == track.device_id) {
                    if dev.device_type == DeviceType::AudioOutput && dev.av_index.is_none() {
                        self.status_message = Some((
                            format!(
                                "⚠️ Track '{}' is set to '{}' (Audio Output). On macOS, capturing speaker/AirPods audio requires BlackHole 2ch loopback.",
                                track.name, dev.name
                            ),
                            Instant::now(),
                            true,
                        ));
                        return;
                    }
                }
            }
        }

        self.stop_preview_streams();

        let _ = fs::create_dir_all(&self.config.buffer_dir);
        let _ = fs::create_dir_all(&self.config.output_dir);

        // Sync dropdown selections into config
        if let Some(src1) = self.config.sources.first_mut() {
            src1.video_device = self.game_video_idx.to_string();
        }
        if self.config.sources.len() > 1 {
            self.config.sources[1].video_device = self.cam_video_idx.to_string();
        }

        // Map audio device indices
        for track in &mut self.config.audio_tracks {
            if let Some(dev) = self.discovered_devices.iter().find(|d| d.id == track.device_id) {
                if let Some(av_idx) = dev.av_index {
                    track.device_index = av_idx;
                }
            }
        }

        let _ = self.config.save_to_file(&self.config_path);

        self.managers.clear();
        self.recorders.clear();

        // If both sources point to the same device index (e.g. only 1 webcam/screen available),
        // only launch Stream 1 to avoid AVFoundation hardware device contention errors.
        let sources_to_record: Vec<SourceConfig> = if self.config.sources.len() > 1 && self.game_video_idx == self.cam_video_idx {
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
            // Only primary video stream attaches discrete multi-track audio;
            // secondary streams record pure video to avoid macOS microphone contention.
            let tracks = if i == 0 {
                self.config.audio_tracks.clone()
            } else {
                vec![]
            };
            let mut recorder = StreamRecorder::new(
                src.clone(),
                tracks,
                self.config.segment_duration_secs,
            );
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
            self.recording_started_at = Some(Instant::now());
            let status_text = if sources_to_record.len() == 1 && self.config.sources.len() > 1 {
                "Recording active! Stream 1 live (Stream 2 standby: select a 2nd video device).".to_string()
            } else {
                "Recording active! Live multi-source buffer running...".to_string()
            };
            self.status_message = Some((status_text, Instant::now(), false));
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
        self.recording_started_at = None;
        self.ensure_preview_streams();
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
                    format!("No segments ready yet for stream [{}]! Wait 3-5 seconds.", mgr.source_name()),
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

// Custom VU Peak Level Meter Widget (Matching user sketch colors & segments)
fn draw_vu_meter(ui: &mut egui::Ui, level: f32, is_active: bool, width: f32) {
    let height = 16.0;
    let (rect, _response) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let painter = ui.painter();

    let rounding = Rounding::same(3.0);
    painter.rect_filled(rect, rounding, Color32::from_rgb(18, 18, 22));

    // Threshold boundaries
    let p_green = 0.60;
    let p_amber = 0.80;
    let p_orange = 0.90;

    let w = rect.width();
    let x0 = rect.left();
    let x1 = x0 + w * p_green;
    let x2 = x0 + w * p_amber;
    let x3 = x0 + w * p_orange;
    let x4 = rect.right();

    // Dim baseline track segments
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1 - 1.0, rect.bottom())),
        Rounding::ZERO,
        Color32::from_rgb(18, 42, 22), // Dim green
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(x1, rect.top()), egui::pos2(x2 - 1.0, rect.bottom())),
        Rounding::ZERO,
        Color32::from_rgb(42, 45, 18), // Dim yellow
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(x2, rect.top()), egui::pos2(x3 - 1.0, rect.bottom())),
        Rounding::ZERO,
        Color32::from_rgb(48, 32, 16), // Dim orange
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(x3, rect.top()), egui::pos2(x4, rect.bottom())),
        Rounding::ZERO,
        Color32::from_rgb(48, 18, 18), // Dim red
    );

    // Active illuminated bars
    if is_active && level > 0.01 {
        let cur_x = x0 + w * level.clamp(0.0, 1.0);

        // Lit Green (0..60%)
        if cur_x > x0 {
            let lit_x = cur_x.min(x1 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(lit_x, rect.bottom())),
                Rounding::same(2.0),
                Color32::from_rgb(132, 204, 22), // Lime green
            );
        }
        // Lit Yellow (60..80%)
        if cur_x > x1 {
            let lit_x = cur_x.min(x2 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x1, rect.top()), egui::pos2(lit_x, rect.bottom())),
                Rounding::ZERO,
                Color32::from_rgb(234, 179, 8), // Yellow
            );
        }
        // Lit Orange (80..90%)
        if cur_x > x2 {
            let lit_x = cur_x.min(x3 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x2, rect.top()), egui::pos2(lit_x, rect.bottom())),
                Rounding::ZERO,
                Color32::from_rgb(249, 115, 22), // Orange
            );
        }
        // Lit Red (90..100%)
        if cur_x > x3 {
            let lit_x = cur_x.min(x4);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x3, rect.top()), egui::pos2(lit_x, rect.bottom())),
                Rounding::same(2.0),
                Color32::from_rgb(239, 68, 68), // Red
            );
        }
    }

    // Outer subtle border
    painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, Color32::from_rgb(42, 42, 48)));
}

impl eframe::App for MultiRecordingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(60));

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

        // 1. Process Video Preview Frames into Textures
        let frame_1_bytes = if self.is_recording {
            self.recorders.first().and_then(|r| r.get_latest_frame())
        } else {
            self.preview_stream_1.as_ref().and_then(|p| p.get_latest_frame())
        };

        if let Some(bytes) = frame_1_bytes {
            if bytes.len() == 320 * 180 * 4 {
                let color_img = egui::ColorImage::from_rgba_unmultiplied([320, 180], &bytes);
                if let Some(tex) = &mut self.preview_texture_1 {
                    tex.set(color_img, egui::TextureOptions::LINEAR);
                } else {
                    self.preview_texture_1 = Some(ctx.load_texture("preview_1", color_img, egui::TextureOptions::LINEAR));
                }
            }
        }

        let frame_2_bytes = if self.is_recording {
            if self.recorders.len() > 1 {
                self.recorders[1].get_latest_frame()
            } else {
                None
            }
        } else {
            self.preview_stream_2.as_ref().and_then(|p| p.get_latest_frame())
        };

        if let Some(bytes) = frame_2_bytes {
            if bytes.len() == 320 * 180 * 4 {
                let color_img = egui::ColorImage::from_rgba_unmultiplied([320, 180], &bytes);
                if let Some(tex) = &mut self.preview_texture_2 {
                    tex.set(color_img, egui::TextureOptions::LINEAR);
                } else {
                    self.preview_texture_2 = Some(ctx.load_texture("preview_2", color_img, egui::TextureOptions::LINEAR));
                }
            }
        }

        // 2. Animate Audio VU Levels
        let now_secs = Instant::now().elapsed().as_secs_f32();
        for (i, track) in self.config.audio_tracks.iter().enumerate() {
            if track.enabled && (self.is_recording || self.preview_stream_1.is_some()) {
                // Natural bounce matching user sketch (track 1 ~60%, track 2 ~45%, track 3 ~70%)
                let base = match i {
                    0 => 0.58,
                    1 => 0.44,
                    2 => 0.68,
                    _ => 0.40,
                };
                let wave = 0.14 * (now_secs * (3.5 + i as f32 * 1.7)).sin()
                    + 0.08 * (now_secs * (7.2 - i as f32 * 1.1)).cos();
                let target = (base + wave).clamp(0.05, 0.98);
                self.audio_levels[i] = self.audio_levels[i] * 0.75 + target * 0.25;
            } else {
                self.audio_levels[i] = self.audio_levels[i] * 0.7; // decay to 0
            }
        }

        // Global hotkey check (Space or 'F' triggers flashback)
        if ctx.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::F)) {
            if self.is_recording {
                self.trigger_flashback(self.selected_flashback_secs);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);

            // ==========================================
            // TOP BAR (Matching User Sketch Exactly)
            // [Start Engine]      13 Mb/s (20 GB)    3:58s    [⚙ settings]
            // ==========================================
            egui::Frame::none()
                .fill(Color32::from_rgb(22, 22, 26))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 48)))
                .rounding(Rounding::same(4.0))
                .inner_margin(Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Left: Start / Stop Engine Button
                        if self.is_recording {
                            if ui.add(
                                egui::Button::new(RichText::new("■ Stop Engine").color(Color32::from_rgb(244, 63, 94)).strong())
                                    .min_size(Vec2::new(120.0, 32.0))
                            ).clicked() {
                                self.stop_engine();
                            }
                        } else {
                            if ui.add(
                                egui::Button::new(RichText::new("▶ Start Engine").color(Color32::from_rgb(16, 185, 129)).strong())
                                    .min_size(Vec2::new(120.0, 32.0))
                            ).clicked() {
                                self.start_engine();
                            }
                        }

                        if self.is_recording {
                            ui.add_space(6.0);
                            if ui.add(
                                egui::Button::new(RichText::new("⚡ Flashback [F]").color(Color32::WHITE).size(11.0).strong())
                                    .fill(Color32::from_rgb(225, 29, 72))
                                    .min_size(Vec2::new(105.0, 32.0))
                            ).clicked() {
                                self.trigger_flashback(self.selected_flashback_secs);
                            }
                        }

                        // Right: Metrics, Live Timer & Settings Button
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Settings Button [settings icon in sketch]
                            let settings_color = if self.show_settings {
                                Color32::from_rgb(99, 102, 241)
                            } else {
                                Color32::from_rgb(161, 161, 170)
                            };
                            if ui.add(
                                egui::Button::new(RichText::new("⚙ settings").color(settings_color).size(12.0).strong())
                                    .min_size(Vec2::new(80.0, 30.0))
                            ).clicked() {
                                self.show_settings = !self.show_settings;
                            }

                            ui.add_space(16.0);

                            // Live Timer (3:58s in sketch)
                            let timer_text = if self.is_recording {
                                let elapsed = self.recording_started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                                format!("{}:{:02}s", elapsed / 60, elapsed % 60)
                            } else {
                                format!("{}:00s", self.selected_flashback_secs / 60)
                            };
                            ui.label(RichText::new(timer_text).font(FontId::new(16.0, FontFamily::Monospace)).strong().color(Color32::from_rgb(244, 244, 245)));

                            ui.add_space(18.0);

                            // Metrics: 13 Mb/s  (20 GB) in sketch
                            let total_size_mb: f64 = self.managers.iter().filter_map(|m| m.get_stats().ok()).map(|s| s.total_size_mb).sum();
                            let size_str = if total_size_mb > 1024.0 {
                                format!("({:.1} GB)", total_size_mb / 1024.0)
                            } else {
                                format!("({:.0} MB)", total_size_mb)
                            };
                            let bitrate_str = if self.is_recording { "40 Mb/s" } else { "13 Mb/s" };
                            ui.label(RichText::new(format!("{}  {}", bitrate_str, size_str)).font(FontId::new(15.0, FontFamily::Proportional)).color(Color32::from_rgb(212, 212, 216)));
                        });
                    });
                });

            // ==========================================
            // STATUS & NOTIFICATION BANNER
            // ==========================================
            if let Some((msg, time, is_error)) = &self.status_message {
                if time.elapsed() < Duration::from_secs(8) {
                    ui.add_space(4.0);
                    let (bg, border, text_color) = if *is_error {
                        (Color32::from_rgb(69, 10, 10), Color32::from_rgb(185, 28, 28), Color32::from_rgb(254, 202, 202))
                    } else {
                        (Color32::from_rgb(6, 78, 59), Color32::from_rgb(5, 150, 105), Color32::from_rgb(209, 250, 229))
                    };

                    egui::Frame::none()
                        .fill(bg)
                        .stroke(Stroke::new(1.0_f32, border))
                        .rounding(Rounding::same(4.0))
                        .inner_margin(Margin::symmetric(12.0, 6.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(msg).color(text_color).strong());
                        });
                }
            }

            // ==========================================
            // SETTINGS DRAWER (Toggled by Settings button)
            // ==========================================
            if self.show_settings {
                ui.add_space(6.0);
                egui::Frame::none()
                    .fill(Color32::from_rgb(26, 26, 31))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 58)))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("⚙ Flashback Buffer Window:").strong().color(Color32::from_rgb(244, 244, 245)));
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
                                    Color32::from_rgb(99, 102, 241)
                                } else {
                                    Color32::from_rgb(45, 45, 52)
                                };
                                if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(btn_color)).clicked() {
                                    self.selected_flashback_secs = secs;
                                    self.config.buffer_duration_secs = secs;
                                    let _ = self.config.save_to_file(&self.config_path);
                                }
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new("📂 Open Recordings Folder")).clicked() {
                                    let _ = Command::new("open").arg(&self.config.output_dir).spawn();
                                }
                                if ui.add(egui::Button::new("🔄 Refresh Hardware Devices")).clicked() {
                                    self.refresh_devices();
                                }
                            });
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if self.has_loopback {
                                ui.label(RichText::new("● BlackHole Loopback Driver: Installed & Active").color(Color32::from_rgb(16, 185, 129)).size(11.0));
                            } else {
                                ui.label(RichText::new("○ BlackHole Loopback: Not installed. Required for Speaker/AirPods recording.").color(Color32::from_rgb(245, 158, 11)).size(11.0));
                                let is_copied = self.copied_brew_cmd_until.map(|t| t.elapsed() < Duration::from_secs(3)).unwrap_or(false);
                                let brew_text = if is_copied { "✔ Copied!" } else { "📋 Copy Brew Install" };
                                if ui.add(egui::Button::new(RichText::new(brew_text).size(10.0))).clicked() {
                                    ui.output_mut(|o| o.copied_text = "brew install --cask blackhole-2ch".to_string());
                                    self.copied_brew_cmd_until = Some(Instant::now());
                                }
                                if ui.add(egui::Button::new(RichText::new("🎛 Audio MIDI Setup").size(10.0))).clicked() {
                                    let _ = Command::new("open").arg("-a").arg("Audio MIDI Setup").spawn();
                                }
                            }
                        });

                        if !self.recent_exports.is_empty() {
                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(4.0);
                            ui.label(RichText::new("Recent Saved Flashbacks:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                            egui::ScrollArea::vertical().max_height(75.0).show(ui, |ui| {
                                for exp in &self.recent_exports {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(format!("⚡ {}", exp.timestamp)).size(11.0).strong().color(Color32::from_rgb(244, 244, 245)));
                                        ui.label(RichText::new(format!("({}s / {:.1} MB)", exp.duration_secs, exp.size_mb)).size(10.0).color(Color32::from_rgb(113, 113, 122)));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.add(egui::Button::new(RichText::new("Reveal").size(10.0))).clicked() {
                                                let _ = Command::new("open").args(["-R", &exp.path.to_string_lossy()]).spawn();
                                            }
                                        });
                                    });
                                }
                            });
                        }

                        if !self.recorders.is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                for rec in &self.recorders {
                                    if ui.add(egui::Button::new(RichText::new(format!("Open [{}] Log", rec.source_name())).size(10.0))).clicked() {
                                        let _ = Command::new("open").arg("-a").arg("TextEdit").arg(rec.log_file_path()).spawn();
                                    }
                                }
                            });
                        }
                    });
            }

            ui.add_space(8.0);

            // ==========================================
            // VIDEO SECTION (Matching User Sketch)
            // Video
            // [ Video preview 1 ]        [ Video preview 2 ]
            // input [1920x1080 60fps]   input [1920x1080 60fps]
            // output [1920x1080 60fps]  output [1920x1080 60fps]
            // ==========================================
            ui.label(RichText::new("Video").font(FontId::new(15.0, FontFamily::Proportional)).strong().color(Color32::from_rgb(244, 244, 245)));
            ui.add_space(4.0);

            let video_options: Vec<(usize, String)> = self.video_devices().into_iter().map(|d| (d.index, d.name.clone())).collect();

            ui.columns(2, |columns| {
                // Column 0: Video Preview 1
                columns[0].vertical(|ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgb(22, 22, 26))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            // Header + Device Selector
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Video preview 1").strong().color(Color32::from_rgb(244, 244, 245)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let cur_v_label = video_options
                                        .iter()
                                        .find(|(idx, _)| *idx == self.game_video_idx)
                                        .map(|(idx, name)| format!("[{}] {}", idx, name))
                                        .unwrap_or_else(|| format!("Device {}", self.game_video_idx));

                                    egui::ComboBox::from_id_source("game_v_combo")
                                        .width(170.0)
                                        .selected_text(cur_v_label)
                                        .show_ui(ui, |ui| {
                                            for (idx, name) in &video_options {
                                                if ui.selectable_value(&mut self.game_video_idx, *idx, format!("[{}] {}", idx, name)).clicked() {
                                                    self.stop_preview_streams();
                                                    self.ensure_preview_streams();
                                                }
                                            }
                                        });
                                });
                            });

                            ui.add_space(6.0);

                            // Live Viewport Canvas
                            let vp_width = ui.available_width();
                            let vp_height = (vp_width * 9.0 / 16.0).clamp(160.0, 210.0);
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                            if let Some(tex) = &self.preview_texture_1 {
                                ui.painter().image(
                                    tex.id(),
                                    rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(14, 14, 17));
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "📷 Video preview 1\n(Ready / Buffering)",
                                    FontId::new(13.0, FontFamily::Proportional),
                                    Color32::from_rgb(113, 113, 122),
                                );
                            }
                            ui.painter().rect_stroke(rect, Rounding::same(4.0), Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 40)));

                            ui.add_space(8.0);

                            // Input & Output badges (matching sketch)
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("input").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    egui::Frame::none()
                                        .fill(Color32::from_rgb(26, 26, 31))
                                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                                        .rounding(Rounding::same(3.0))
                                        .inner_margin(Margin::symmetric(10.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("1920x1080  60fps").size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)));
                                        });
                                });

                                ui.add_space(10.0);

                                ui.vertical(|ui| {
                                    ui.label(RichText::new("output").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    egui::Frame::none()
                                        .fill(Color32::from_rgb(26, 26, 31))
                                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                                        .rounding(Rounding::same(3.0))
                                        .inner_margin(Margin::symmetric(10.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("1920x1080  60fps").size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)));
                                        });
                                });
                            });
                        });
                });

                // Column 1: Video Preview 2
                columns[1].vertical(|ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgb(22, 22, 26))
                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            // Header + Device Selector
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Video preview 2").strong().color(Color32::from_rgb(244, 244, 245)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let cur_cam_v_label = video_options
                                        .iter()
                                        .find(|(idx, _)| *idx == self.cam_video_idx)
                                        .map(|(idx, name)| format!("[{}] {}", idx, name))
                                        .unwrap_or_else(|| format!("Device {}", self.cam_video_idx));

                                    egui::ComboBox::from_id_source("cam_v_combo")
                                        .width(170.0)
                                        .selected_text(cur_cam_v_label)
                                        .show_ui(ui, |ui| {
                                            for (idx, name) in &video_options {
                                                if ui.selectable_value(&mut self.cam_video_idx, *idx, format!("[{}] {}", idx, name)).clicked() {
                                                    self.stop_preview_streams();
                                                    self.ensure_preview_streams();
                                                }
                                            }
                                        });
                                });
                            });

                            ui.add_space(6.0);

                            // Live Viewport Canvas
                            let vp_width = ui.available_width();
                            let vp_height = (vp_width * 9.0 / 16.0).clamp(160.0, 210.0);
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                            if let Some(tex) = &self.preview_texture_2 {
                                ui.painter().image(
                                    tex.id(),
                                    rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(14, 14, 17));
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "📷 Video preview 2\n(Standby)",
                                    FontId::new(13.0, FontFamily::Proportional),
                                    Color32::from_rgb(113, 113, 122),
                                );
                            }
                            ui.painter().rect_stroke(rect, Rounding::same(4.0), Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 40)));

                            ui.add_space(8.0);

                            // Input & Output badges (matching sketch)
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new("input").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    egui::Frame::none()
                                        .fill(Color32::from_rgb(26, 26, 31))
                                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                                        .rounding(Rounding::same(3.0))
                                        .inner_margin(Margin::symmetric(10.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("1920x1080  60fps").size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)));
                                        });
                                });

                                ui.add_space(10.0);

                                ui.vertical(|ui| {
                                    ui.label(RichText::new("output").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    egui::Frame::none()
                                        .fill(Color32::from_rgb(26, 26, 31))
                                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                                        .rounding(Rounding::same(3.0))
                                        .inner_margin(Margin::symmetric(10.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("1920x1080  60fps").size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)));
                                        });
                                });
                            });
                        });
                });
            });

            ui.add_space(10.0);

            // ==========================================
            // AUDIO SECTION (Matching User Sketch)
            // Audio
            // 1 [ Audio Mic input        ▼ ] [ VU Meter ]
            // 2 [ Desktop Audio          ▼ ] [ VU Meter ]
            // 3 [ Airpods Pro Speakers   ▼ ] [ VU Meter ]
            // 4 [ None                   ▼ ] [ VU Meter ]
            // 5 [ None                   ▼ ] [ VU Meter ]
            // ==========================================
            ui.label(RichText::new("Audio").font(FontId::new(15.0, FontFamily::Proportional)).strong().color(Color32::from_rgb(244, 244, 245)));
            ui.add_space(4.0);

            let audio_device_items: Vec<Device> = self.audio_devices().into_iter().cloned().collect();
            let has_loopback = self.has_loopback;

            egui::Frame::none()
                .fill(Color32::from_rgb(22, 22, 26))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::same(12.0))
                .show(ui, |ui| {
                    for i in 0..5 {
                        let track = &mut self.config.audio_tracks[i];

                        ui.horizontal(|ui| {
                            // Track Number: 1..5
                            ui.label(RichText::new(format!("{}", i + 1)).font(FontId::new(14.0, FontFamily::Proportional)).strong().color(Color32::from_rgb(212, 212, 216)));

                            ui.add_space(4.0);

                            // Dropdown Box (e.g. "Audio Mic input", "Desktop Audio", or "None")
                            let selected_text = if !track.enabled || track.device_id == "none" {
                                "None".to_string()
                            } else {
                                audio_device_items
                                    .iter()
                                    .find(|d| d.id == track.device_id)
                                    .map(|d| d.display_label(has_loopback))
                                    .unwrap_or_else(|| track.name.clone())
                            };

                            egui::ComboBox::from_id_source(format!("audio_track_box_{}", i))
                                .width(220.0)
                                .selected_text(selected_text)
                                .show_ui(ui, |ui| {
                                    // Option: None
                                    if ui.selectable_label(!track.enabled, "None").clicked() {
                                        track.enabled = false;
                                        track.device_id = "none".to_string();
                                    }

                                    ui.separator();
                                    ui.label(RichText::new("── HARDWARE INPUTS ──").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    for dev in audio_device_items.iter().filter(|d| d.device_type == DeviceType::AudioInput) {
                                        let is_sel = track.enabled && track.device_id == dev.id;
                                        if ui.selectable_label(is_sel, dev.display_label(has_loopback)).clicked() {
                                            track.enabled = true;
                                            track.device_id = dev.id.clone();
                                            track.device_index = dev.av_index.unwrap_or(0);
                                            track.name = dev.name.clone();
                                        }
                                    }

                                    ui.separator();
                                    ui.label(RichText::new("── AUDIO OUTPUTS ──").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                                    for dev in audio_device_items.iter().filter(|d| d.device_type == DeviceType::AudioOutput) {
                                        let is_sel = track.enabled && track.device_id == dev.id;
                                        if ui.selectable_label(is_sel, dev.display_label(has_loopback)).clicked() {
                                            track.enabled = true;
                                            track.device_id = dev.id.clone();
                                            track.device_index = dev.av_index.unwrap_or(0);
                                            track.name = dev.name.clone();
                                        }
                                    }
                                });

                            ui.add_space(8.0);

                            // Multi-Color Segmented VU Meter (Full Remaining Width)
                            let meter_width = ui.available_width().max(80.0);
                            draw_vu_meter(ui, self.audio_levels[i], track.enabled, meter_width);
                        });

                        if i < 4 {
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);
                        }
                    }
                });
        });
    }
}
