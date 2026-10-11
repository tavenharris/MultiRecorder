#![allow(deprecated)]

use crate::config::{AudioTrackConfig, Config, SourceConfig};
use crate::devices::{Device, DeviceManager, DeviceType};
use crate::recorder::{AudioTrackMonitor, PreviewStream, StreamRecorder};
use crate::remuxer::Remuxer;
use crate::ring_buffer::RingBufferManager;
use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Rounding, Stroke, Vec2};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ExportRecord {
    pub timestamp: String,
    pub path: PathBuf,
    pub duration_secs: u64,
    pub size_mb: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    Flashback,
    Continuous,
}

pub struct MultiRecordingApp {
    config: Config,
    config_path: PathBuf,
    discovered_devices: Vec<Device>,
    has_loopback: bool,

    // Engine runtime state
    is_recording: bool,
    recording_mode: Option<RecordingMode>,
    recorders: Vec<StreamRecorder>,
    managers: Vec<RingBufferManager>,
    last_eviction: Instant,

    // Live Video Previews
    preview_stream_1: Option<PreviewStream>,
    preview_stream_2: Option<PreviewStream>,
    preview_texture_1: Option<egui::TextureHandle>,
    preview_texture_2: Option<egui::TextureHandle>,
    #[allow(dead_code)]
    last_preview_seq_1: u64,
    #[allow(dead_code)]
    last_preview_seq_2: u64,

    // Real Hardware Audio Monitors
    audio_monitors: [Option<AudioTrackMonitor>; 5],

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
    #[allow(dead_code)]
    copied_brew_cmd_until: Option<Instant>,
    custom_clip_sec_input: String,
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
        let initial_status = if devices.is_empty() {
            Some((
                "⚠️ No capture devices found. Please ensure FFmpeg is installed and accessible in PATH.".to_string(),
                Instant::now(),
                true,
            ))
        } else {
            None
        };

        let mut app = Self {
            config: cfg,
            config_path,
            discovered_devices: devices,
            has_loopback,
            is_recording: false,
            recording_mode: None,
            recorders: Vec::new(),
            managers: Vec::new(),
            last_eviction: Instant::now(),
            preview_stream_1: None,
            preview_stream_2: None,
            preview_texture_1: None,
            preview_texture_2: None,
            last_preview_seq_1: 0,
            last_preview_seq_2: 0,
            status_message: initial_status,
            recent_exports: Vec::new(),
            selected_flashback_secs: selected_secs,
            show_settings: false,
            recording_started_at: None,
            audio_levels: [0.0; 5],
            audio_monitors: [None, None, None, None, None],
            game_video_idx: 0,
            cam_video_idx: 0,
            copied_brew_cmd_until: None,
            custom_clip_sec_input: String::new(),
        };

        app.sync_device_selections();
        app.ensure_preview_streams();
        app.sync_audio_monitors();
        app
    }

    pub fn refresh_devices(&mut self) {
        match DeviceManager::list_devices() {
            Ok(devices) => {
                let count = devices.len();
                self.discovered_devices = devices;
                self.has_loopback = DeviceManager::has_loopback_device(&self.discovered_devices);
                self.sync_device_selections();
                self.sync_audio_monitors();
                self.stop_preview_streams();
                self.ensure_preview_streams();
                if count == 0 {
                    self.status_message = Some((
                        "⚠️ 0 devices discovered. Please ensure FFmpeg is installed and accessible in PATH.".to_string(),
                        Instant::now(),
                        true,
                    ));
                } else {
                    self.status_message = Some((
                        format!(
                            "Devices refreshed: {} sources found (Loopback: {})",
                            count,
                            if self.has_loopback { "Active" } else { "Not Installed" }
                        ),
                        Instant::now(),
                        false,
                    ));
                }
            }
            Err(e) => {
                self.status_message = Some((
                    format!("⚠️ Device discovery error: {e}"),
                    Instant::now(),
                    true,
                ));
            }
        }
    }

    pub fn sync_audio_monitors(&mut self) {
        for i in 0..5 {
            let track = self.config.audio_tracks.get(i);
            if let Some(track) = track {
                if track.enabled {
                    let found_dev = self.discovered_devices.iter().find(|d| d.id == track.device_id)
                        .or_else(|| {
                            let clean = track.device_id
                                .strip_prefix("out:")
                                .or_else(|| track.device_id.strip_prefix("in:"))
                                .or_else(|| track.device_id.strip_prefix("audio="))
                                .unwrap_or(&track.device_id);
                            self.discovered_devices.iter().find(|d| d.id == clean || d.name.contains(clean))
                        })
                        .or_else(|| {
                            if track.device_id == "wasapi" || track.device_id == "default" || track.name.to_lowercase().contains("desktop") {
                                self.discovered_devices.iter().find(|d| d.device_type == DeviceType::AudioOutput)
                            } else {
                                None
                            }
                        });

                    if let Some(dev) = found_dev {
                        let monitor_id = if let Some(av_idx) = dev.av_index {
                            av_idx.to_string()
                        } else {
                            dev.id.clone()
                        };
                        let need_spawn = match &self.audio_monitors[i] {
                            Some(m) => m.device_identifier() != monitor_id,
                            None => true,
                        };
                        if need_spawn {
                            if let Some(mut old) = self.audio_monitors[i].take() {
                                old.stop();
                            }
                            self.audio_monitors[i] = Some(AudioTrackMonitor::new(monitor_id));
                        }
                        continue;
                    }
                }
            }
            if let Some(mut old) = self.audio_monitors[i].take() {
                old.stop();
            }
            self.audio_levels[i] = 0.0;
        }
    }

    pub fn ensure_preview_streams(&mut self) {
        if self.is_recording {
            self.stop_preview_streams();
            return;
        }

        // Preview stream 1
        let dev1 = self.video_devices().into_iter().find(|d| d.index == self.game_video_idx).map(|d| d.id.clone()).unwrap_or_else(|| self.game_video_idx.to_string());
        if self.preview_stream_1.is_none() {
            let is_screen = self.video_devices().iter().find(|d| d.index == self.game_video_idx).map(|d| d.name.to_lowercase().contains("screen")).unwrap_or(false);
            let mut ps1 = PreviewStream::new(dev1, is_screen);
            let _ = ps1.start();
            self.preview_stream_1 = Some(ps1);
        }

        // Preview stream 2 (only if distinct video device)
        let dev2 = self.video_devices().into_iter().find(|d| d.index == self.cam_video_idx).map(|d| d.id.clone()).unwrap_or_else(|| self.cam_video_idx.to_string());
        if self.cam_video_idx != self.game_video_idx && self.preview_stream_2.is_none() {
            let is_screen = self.video_devices().iter().find(|d| d.index == self.cam_video_idx).map(|d| d.name.to_lowercase().contains("screen")).unwrap_or(false);
            let mut ps2 = PreviewStream::new(dev2, is_screen);
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
        self.preview_texture_1 = None;
        self.preview_texture_2 = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        std::thread::sleep(Duration::from_millis(250));
    }

    fn apply_flat_theme(ctx: &egui::Context) {
        ctx.set_theme(egui::ThemePreference::Dark);

        let mut visuals = egui::Visuals::dark();

        visuals.dark_mode = true;
        visuals.override_text_color = Some(Color32::from_rgb(243, 244, 246));
        visuals.window_fill = Color32::from_rgb(18, 18, 22);
        visuals.panel_fill = Color32::from_rgb(14, 14, 18);
        visuals.faint_bg_color = Color32::from_rgb(24, 24, 28);
        visuals.extreme_bg_color = Color32::from_rgb(12, 12, 16);

        // Flat surfaces and minimalist edges
        visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52));
        visuals.window_rounding = Rounding::same(6.0);
        visuals.menu_rounding = Rounding::same(6.0);

        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(22, 22, 26);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 46));
        visuals.widgets.noninteractive.rounding = Rounding::same(4.0);

        visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 28, 34);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 56));
        visuals.widgets.inactive.rounding = Rounding::same(4.0);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 38, 46);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 82));
        visuals.widgets.hovered.rounding = Rounding::same(4.0);

        visuals.widgets.active.bg_fill = Color32::from_rgb(46, 46, 56);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(100, 100, 120));
        visuals.widgets.active.rounding = Rounding::same(4.0);

        visuals.widgets.open.bg_fill = Color32::from_rgb(28, 28, 34);
        visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 82));
        visuals.widgets.open.rounding = Rounding::same(4.0);

        ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
        ctx.set_visuals_of(egui::Theme::Light, visuals.clone());
        ctx.set_visuals(visuals);
    }

    fn sync_device_selections(&mut self) {
        let v_devs: Vec<(usize, String, String)> = self.video_devices()
            .into_iter()
            .map(|d| (d.index, d.id.clone(), d.name.clone()))
            .collect();
        let valid_v_indices: Vec<usize> = v_devs.iter().map(|(idx, _, _)| *idx).collect();

        let find_v_idx = |device_str: &str, fallback_pos: usize| -> usize {
            if let Some((idx, _, _)) = v_devs.iter().find(|(_, id, _)| id == device_str) {
                return *idx;
            }
            let clean = device_str.strip_prefix("video:").unwrap_or(device_str).trim();
            if let Some((idx, _, _)) = v_devs.iter().find(|(_, id, name)| name == device_str || name == clean || id == clean) {
                return *idx;
            }
            if let Ok(num) = clean.parse::<usize>() {
                if valid_v_indices.contains(&num) {
                    return num;
                }
            }
            valid_v_indices.get(fallback_pos).or_else(|| valid_v_indices.first()).copied().unwrap_or(0)
        };

        if let Some(src) = self.config.sources.first() {
            self.game_video_idx = find_v_idx(&src.video_device, 0);
        }
        if self.config.sources.len() > 1 {
            let src2 = &self.config.sources[1];
            self.cam_video_idx = find_v_idx(&src2.video_device, 1);
        }

        // Validate encoders against current platform support
        let valid_encoders: Vec<&str> = crate::platform::CurrentPlatform
            .available_encoders()
            .iter()
            .map(|(enc, _)| *enc)
            .collect();
        let default_enc = crate::platform::CurrentPlatform.default_video_encoder();

        for src in &mut self.config.sources {
            if !valid_encoders.contains(&src.encoder.as_str()) {
                src.encoder = default_enc.to_string();
            }
        }

        // Ensure at least 2 sources exist
        while self.config.sources.len() < 2 {
            let idx = self.config.sources.len();
            let name = if idx == 0 { "gameplay" } else { "facecam" };
            self.config.sources.push(SourceConfig {
                name: name.to_string(),
                video_device: "0".to_string(),
                resolution: Some("1920x1080".to_string()),
                fps: Some(60),
                video_bitrate: if idx == 0 { "40M".to_string() } else { "15M".to_string() },
                encoder: default_enc.to_string(),
            });
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

    pub fn get_input_format(&self, stream_idx: usize) -> String {
        // 1. Try active recorder if recording
        if self.is_recording {
            if let Some(rec) = self.recorders.get(stream_idx) {
                if let Some(fmt) = rec.get_detected_format() {
                    return fmt;
                }
            }
        } else {
            // 2. Try active preview stream
            let prev_fmt = if stream_idx == 0 {
                self.preview_stream_1.as_ref().and_then(|p| p.get_detected_format())
            } else {
                self.preview_stream_2.as_ref().and_then(|p| p.get_detected_format())
            };
            if let Some(fmt) = prev_fmt {
                return fmt;
            }
        }

        // 3. Query metadata from discovered devices
        let dev_idx = if stream_idx == 0 {
            self.game_video_idx
        } else {
            self.cam_video_idx
        };

        if let Some(dev) = self.video_devices().into_iter().find(|d| d.index == dev_idx) {
            if let (Some(res), Some(fps)) = (&dev.resolution, dev.fps) {
                return format!("{}  {}fps", res, fps);
            }
            if let Some(res) = &dev.resolution {
                return format!("{}  60fps", res);
            }
        }

        "1920x1080  60fps".to_string()
    }

    pub fn parse_input_res_fps(&self, stream_idx: usize) -> (String, u32) {
        let raw = self.get_input_format(stream_idx);
        let mut res = "1920x1080".to_string();
        let mut fps = 60u32;

        for part in raw.split_whitespace() {
            if part.contains('x') {
                res = part.to_string();
            } else if let Some(stripped) = part.strip_suffix("fps") {
                if let Ok(f) = stripped.parse::<u32>() {
                    fps = f;
                }
            }
        }
        (res, fps)
    }

    pub fn get_output_resolution_label(&self, stream_idx: usize, in_res: &str) -> String {
        if let Some(src) = self.config.sources.get(stream_idx) {
            match &src.resolution {
                Some(res) if res == "1920x1080" => "1080p (1920x1080)".to_string(),
                Some(res) if res == "2560x1440" => "1440p (2560x1440)".to_string(),
                Some(res) if res == "3840x2160" => "4K (3840x2160)".to_string(),
                Some(res) if res == "1280x720" => "720p (1280x720)".to_string(),
                Some(res) => res.clone(),
                None => format!("Native ({})", in_res),
            }
        } else {
            format!("Native ({})", in_res)
        }
    }

    pub fn set_output_resolution(&mut self, stream_idx: usize, res: Option<&str>) {
        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            src.resolution = res.map(|s| s.to_string());
            let _ = self.config.save_to_file(&self.config_path);
        }
    }

    pub fn set_output_fps(&mut self, stream_idx: usize, fps: u32) {
        let (_, in_fps) = self.parse_input_res_fps(stream_idx);
        let clamped = fps.min(in_fps);
        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            src.fps = Some(clamped);
            let _ = self.config.save_to_file(&self.config_path);
        }
    }

    pub fn render_stream_format_controls(&mut self, ui: &mut egui::Ui, stream_idx: usize) {
        let (in_res, in_fps) = self.parse_input_res_fps(stream_idx);

        // Auto-clamp output fps to input fps if higher than what hardware supplies
        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            if let Some(cur_fps) = src.fps {
                if cur_fps > in_fps {
                    src.fps = Some(in_fps);
                    let _ = self.config.save_to_file(&self.config_path);
                }
            }
        }

        let cur_fps = self.config.sources.get(stream_idx).and_then(|s| s.fps).unwrap_or(in_fps).min(in_fps);

        ui.horizontal(|ui| {
            // 1. Input Format Badge
            ui.vertical(|ui| {
                ui.label(RichText::new("input").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                let in_label = format!("{}  {}fps", in_res, in_fps);
                egui::Frame::none()
                    .fill(Color32::from_rgb(26, 26, 31))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                    .rounding(Rounding::same(3.0))
                    .inner_margin(Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new(in_label).size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)));
                    });
            });

            ui.add_space(6.0);

            // 2. Output Resolution Dropdown
            ui.vertical(|ui| {
                ui.label(RichText::new("output res").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                let cur_res_label = self.get_output_resolution_label(stream_idx, &in_res);
                egui::ComboBox::from_id_source(format!("stream_{}_res_combo", stream_idx))
                    .width(135.0)
                    .selected_text(RichText::new(cur_res_label).size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)))
                    .show_ui(ui, |ui| {
                        let native_label = format!("Native ({})", in_res);
                        let is_native = self.config.sources.get(stream_idx).map(|s| s.resolution.is_none()).unwrap_or(false);
                        if ui.selectable_label(is_native, native_label).clicked() {
                            self.set_output_resolution(stream_idx, None);
                        }

                        ui.separator();

                        for &(label, res_opt) in &[
                            ("1080p (1920x1080)", "1920x1080"),
                            ("1440p (2560x1440)", "2560x1440"),
                            ("4K (3840x2160)",    "3840x2160"),
                            ("720p (1280x720)",   "1280x720"),
                        ] {
                            let is_cur = self.config.sources.get(stream_idx).and_then(|s| s.resolution.as_deref()) == Some(res_opt);
                            if ui.selectable_label(is_cur, label).clicked() {
                                self.set_output_resolution(stream_idx, Some(res_opt));
                            }
                        }
                    });
            });

            ui.add_space(6.0);

            // 3. Output Frame Rate Dropdown (Strictly constrained by input frame rate)
            ui.vertical(|ui| {
                ui.label(RichText::new("fps").size(10.0).color(Color32::from_rgb(156, 163, 175)));
                let cur_fps_label = format!("{} fps", cur_fps);
                egui::ComboBox::from_id_source(format!("stream_{}_fps_combo", stream_idx))
                    .width(72.0)
                    .selected_text(RichText::new(cur_fps_label).size(11.0).monospace().color(Color32::from_rgb(228, 228, 231)))
                    .show_ui(ui, |ui| {
                        let mut allowed_fps = Vec::new();
                        if in_fps >= 120 {
                            allowed_fps.push(120);
                        }
                        if in_fps >= 60 {
                            allowed_fps.push(60);
                        }
                        if in_fps >= 50 && in_fps < 60 {
                            allowed_fps.push(50);
                        }
                        if in_fps >= 30 {
                            allowed_fps.push(30);
                        }
                        if in_fps == 25 {
                            allowed_fps.push(25);
                        }
                        if in_fps == 24 || (in_fps > 24 && in_fps < 30) {
                            allowed_fps.push(24);
                        }
                        if allowed_fps.is_empty() {
                            allowed_fps.push(in_fps);
                        }

                        for &f in &allowed_fps {
                            let is_cur = cur_fps == f;
                            let opt_label = format!("{} fps", f);
                            if ui.selectable_label(is_cur, opt_label).clicked() {
                                self.set_output_fps(stream_idx, f);
                            }
                        }

                        if in_fps <= 30 {
                            ui.separator();
                            ui.label(RichText::new(format!("Capped at input ({}fps)", in_fps)).size(10.0).color(Color32::from_rgb(156, 163, 175)));
                        }
                    });
            });
        });
    }

    #[allow(dead_code)]
    pub fn get_output_format_label(&self, stream_idx: usize) -> String {
        if let Some(src) = self.config.sources.get(stream_idx) {
            match (&src.resolution, src.fps) {
                (Some(res), Some(fps)) => format!("{}  {}fps", res, fps),
                (Some(res), None) => format!("{}  60fps", res),
                (None, Some(fps)) => format!("Native  {}fps", fps),
                (None, None) => "Native Source".to_string(),
            }
        } else {
            "1920x1080  60fps".to_string()
        }
    }

    #[allow(dead_code)]
    pub fn is_output_format(&self, stream_idx: usize, res: Option<&str>, fps: Option<u32>) -> bool {
        if let Some(src) = self.config.sources.get(stream_idx) {
            src.resolution.as_deref() == res && src.fps == fps
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn set_output_format(&mut self, stream_idx: usize, res: Option<&str>, fps: Option<u32>) {
        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            src.resolution = res.map(|s| s.to_string());
            src.fps = fps;
            let _ = self.config.save_to_file(&self.config_path);
        }
    }

    pub fn format_interval_label(secs: u64) -> String {
        if secs < 60 {
            format!("{}s", secs)
        } else if secs % 3600 == 0 {
            format!("{}h", secs / 3600)
        } else if secs % 60 == 0 {
            format!("{}m", secs / 60)
        } else if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        }
    }

    pub fn start_engine(&mut self, mode: RecordingMode) {
        if self.is_recording {
            return;
        }

        // Pre-validate audio tracks via HAL
        if let Err(warning) = crate::platform::CurrentPlatform.validate_audio_tracks(
            &self.config.audio_tracks,
            &self.discovered_devices,
        ) {
            self.status_message = Some((warning, Instant::now(), true));
            return;
        }

        self.stop_preview_streams();

        let _ = fs::create_dir_all(&self.config.buffer_dir);
        let _ = fs::create_dir_all(&self.config.output_dir);

        // Wipe any previous un-saved cached segments in buffer_dir when starting a fresh session
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);

        // Sync dropdown selections into config
        let dev_id1 = self.video_devices().into_iter().find(|d| d.index == self.game_video_idx).map(|d| d.id.clone()).unwrap_or_else(|| self.game_video_idx.to_string());
        let dev_id2 = self.video_devices().into_iter().find(|d| d.index == self.cam_video_idx).map(|d| d.id.clone()).unwrap_or_else(|| self.cam_video_idx.to_string());

        if let Some(src1) = self.config.sources.first_mut() {
            src1.video_device = dev_id1;
        }
        if self.config.sources.len() > 1 {
            self.config.sources[1].video_device = dev_id2;
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

        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
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
            // Primary video stream attaches all discrete audio tracks (1..5);
            // Secondary video stream attaches Track 1 if sync_audio_to_secondary is enabled for NLE waveform sync.
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
                src.clone(),
                tracks,
                self.config.segment_duration_secs,
                self.config.audio_bitrate.clone(),
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
            self.recording_mode = Some(mode);
            self.recording_started_at = Some(Instant::now());
            let status_text = match mode {
                RecordingMode::Continuous => {
                    "🎬 Continuous recording LIVE! Forward recording until you click Stop.".to_string()
                }
                RecordingMode::Flashback => {
                    if sources_to_record.len() == 1 && self.config.sources.len() > 1 {
                        "Flashback active! Stream 1 live (Stream 2 standby: select a 2nd video device).".to_string()
                    } else {
                        "Flashback active! Live multi-source buffer running...".to_string()
                    }
                }
            };
            self.status_message = Some((status_text, Instant::now(), false));
        } else {
            self.stop_engine();
        }
    }

    pub fn stop_engine(&mut self) {
        if self.recording_mode == Some(RecordingMode::Continuous) {
            self.stop_continuous_recording();
            return;
        }

        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }
        self.recorders.clear();

        // Wipe all cached segments from the buffer on stop (only saved clips are kept)
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        self.ensure_preview_streams();
        self.status_message = Some(("Flashback stopped. Buffer reset & cleared.".to_string(), Instant::now(), false));
    }

    /// Stops continuous forward recording and losslessly concatenates all segments into the final video file(s)
    pub fn stop_continuous_recording(&mut self) {
        if !self.is_recording {
            return;
        }

        // 1. Stop all recorders to ensure all segments are flushed to disk
        for recorder in &mut self.recorders {
            let _ = recorder.stop();
        }
        self.recorders.clear();

        let elapsed = self.recording_started_at.map(|t| t.elapsed()).unwrap_or(Duration::from_secs(0));
        let timestamp = Remuxer::generate_session_timestamp();
        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

        let out_ext = if self.config.output_format.trim().is_empty() {
            "mp4"
        } else {
            &self.config.output_format
        };

        // Extract shared start timecode
        let shared_timecode = if self.config.embed_timecode {
            let mut found_tc = None;
            for mgr in &self.managers {
                if let Ok(segs) = mgr.list_segments() {
                    if !segs.is_empty() {
                        let paths: Vec<PathBuf> = segs.iter().map(|s| s.path.clone()).collect();
                        found_tc = Some(Remuxer::extract_start_timecode_from_segments(&paths));
                        break;
                    }
                }
            }
            found_tc
        } else {
            None
        };

        // 2. Stitch all segments for each manager
        for mgr in &self.managers {
            let segments: Vec<PathBuf> = match mgr.list_segments() {
                Ok(segs) => segs.into_iter().map(|s| s.path).collect(),
                Err(e) => {
                    self.status_message = Some((format!("Error listing segments: {}", e), Instant::now(), true));
                    continue;
                }
            };

            if segments.is_empty() {
                continue;
            }

            let out_file = self.config.output_dir.join(format!(
                "Recording_{}_{}.{}",
                timestamp,
                mgr.source_name(),
                out_ext
            ));

            if let Err(e) = Remuxer::stitch_segments(&segments, &out_file, shared_timecode.as_deref()) {
                self.status_message = Some((format!("Remuxing recording failed: {}", e), Instant::now(), true));
                continue;
            }

            if let Ok(meta) = fs::metadata(&out_file) {
                total_size += meta.len() as f64 / (1024.0 * 1024.0);
            }
            exported_files.push(out_file);
        }

        // 3. Clear temporary buffer directory now that continuous file has been created
        let _ = RingBufferManager::wipe_buffer_dir(&self.config.buffer_dir);
        for mgr in &self.managers {
            let _ = mgr.wipe_all();
        }
        self.managers.clear();

        self.is_recording = false;
        self.recording_mode = None;
        self.recording_started_at = None;
        self.last_preview_seq_1 = 0;
        self.last_preview_seq_2 = 0;
        self.ensure_preview_streams();

        if let Some(first) = exported_files.first() {
            self.recent_exports.insert(
                0,
                ExportRecord {
                    timestamp: timestamp.clone(),
                    path: first.clone(),
                    duration_secs: elapsed.as_secs(),
                    size_mb: total_size,
                },
            );
        }

        let dur_str = if elapsed.as_secs() >= 3600 {
            format!("{}:{:02}:{:02}", elapsed.as_secs() / 3600, (elapsed.as_secs() % 3600) / 60, elapsed.as_secs() % 60)
        } else {
            format!("{}:{:02}", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
        };

        if !exported_files.is_empty() {
            self.status_message = Some((
                format!("🎬 Recording saved! ({}, {:.1} MB, {} file{})", dur_str, total_size, exported_files.len(), if exported_files.len() > 1 { "s" } else { "" }),
                Instant::now(),
                false,
            ));
        } else {
            self.status_message = Some(("Recording stopped (no segments captured).".to_string(), Instant::now(), false));
        }
    }

    pub fn trigger_flashback(&mut self, duration_secs: u64) {
        let start = Instant::now();
        let timestamp = Remuxer::generate_session_timestamp();
        let target_duration = Duration::from_secs(duration_secs);

        let mut exported_files = Vec::new();
        let mut total_size = 0.0;

        // Compute synchronized session start timecode across both streams
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

            let out_ext = if self.config.output_format.trim().is_empty() {
                "mp4"
            } else {
                &self.config.output_format
            };
            let out_file = self.config.output_dir.join(format!(
                "Flashback_{}_{}.{}",
                timestamp,
                mgr.source_name(),
                out_ext
            ));

            if let Err(e) = Remuxer::stitch_segments(&segments, &out_file, shared_timecode.as_deref()) {
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
        // Enforce permanent dark mode regardless of macOS system theme
        if !ctx.style().visuals.dark_mode || ctx.options(|opt| opt.theme_preference) != egui::ThemePreference::Dark {
            Self::apply_flat_theme(ctx);
        }

        ctx.request_repaint_after(Duration::from_millis(60));

        // Background Ring Buffer Eviction (Only active during Flashback mode!)
        if self.is_recording && self.recording_mode == Some(RecordingMode::Flashback) && self.last_eviction.elapsed() >= Duration::from_secs(5) {
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

        // 2. Sample Audio VU Levels from Real Hardware Monitors
        for i in 0..5 {
            let track_enabled = self.config.audio_tracks.get(i).map(|t| t.enabled).unwrap_or(false);
            if track_enabled {
                let peak = self.audio_monitors[i].as_ref().map(|m| m.get_peak()).unwrap_or(0.0);
                if peak > self.audio_levels[i] {
                    // Fast attack for punchy responsiveness
                    self.audio_levels[i] = self.audio_levels[i] * 0.2 + peak * 0.8;
                } else {
                    // Smooth broadcast decay
                    self.audio_levels[i] = self.audio_levels[i] * 0.88;
                }
            } else {
                self.audio_levels[i] = 0.0;
            }
        }

        // Global hotkeys ('R' for continuous record, Space/F/1..9 for flashback)
        if !ctx.wants_keyboard_input() {
            // 'R' key toggles continuous forward recording
            if ctx.input(|i| i.key_pressed(egui::Key::R)) {
                if self.is_recording {
                    if self.recording_mode == Some(RecordingMode::Continuous) {
                        self.stop_continuous_recording();
                    }
                } else {
                    self.start_engine(RecordingMode::Continuous);
                }
            }

            // Flashback clip hotkeys
            if self.is_recording && self.recording_mode == Some(RecordingMode::Flashback) {
                let triggered_secs = ctx.input(|i| {
                    if i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::F) {
                        return Some(self.selected_flashback_secs);
                    }
                    let number_keys = [
                        egui::Key::Num1,
                        egui::Key::Num2,
                        egui::Key::Num3,
                        egui::Key::Num4,
                        egui::Key::Num5,
                        egui::Key::Num6,
                        egui::Key::Num7,
                        egui::Key::Num8,
                        egui::Key::Num9,
                    ];
                    let intervals = if self.config.clip_intervals.is_empty() {
                        crate::config::default_clip_intervals()
                    } else {
                        self.config.clip_intervals.clone()
                    };
                    for (idx, key) in number_keys.iter().enumerate() {
                        if idx < intervals.len() && i.key_pressed(*key) {
                            return Some(intervals[idx]);
                        }
                    }
                    None
                });

                if let Some(secs) = triggered_secs {
                    self.selected_flashback_secs = secs;
                    self.trigger_flashback(secs);
                }
            }
        }

        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(14, 14, 18))
            .inner_margin(Margin::same(12.0));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            ui.add_space(4.0);

            // ==========================================
            // TOP BAR (Matching User Sketch Exactly)
            // [Start Engine]  [⚡ 30s] [⚡ 1m] [⚡ 5m] [⚡ 20m]    13 Mb/s (20 GB)    3:58s    [⚙ settings]
            // ==========================================
            egui::Frame::none()
                .fill(Color32::from_rgb(22, 22, 26))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(40, 40, 48)))
                .rounding(Rounding::same(4.0))
                .inner_margin(Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Left: Mode Controls (Continuous Record or Flashback Buffer)
                        match self.recording_mode {
                            Some(RecordingMode::Continuous) => {
                                // In Continuous mode: Stop Recording button
                                if ui.add(
                                    egui::Button::new(RichText::new("■ Stop Recording [R]").color(Color32::WHITE).strong())
                                        .fill(Color32::from_rgb(225, 29, 72))
                                        .min_size(Vec2::new(140.0, 32.0))
                                ).on_hover_text("Stop and finalize continuous recording to output folder (Hotkey: R)").clicked() {
                                    self.stop_continuous_recording();
                                }
                            }
                            Some(RecordingMode::Flashback) => {
                                // In Flashback mode: Stop Flashback button
                                if ui.add(
                                    egui::Button::new(RichText::new("■ Stop Flashback").color(Color32::from_rgb(244, 63, 94)).strong())
                                        .min_size(Vec2::new(125.0, 32.0))
                                ).on_hover_text("Stop flashback buffer and clear temporary cache").clicked() {
                                    self.stop_engine();
                                }
                            }
                            None => {
                                // Idle state: Both '● Record' and '▶ Start Flashback' are available
                                if ui.add(
                                    egui::Button::new(RichText::new("● Record [R]").color(Color32::from_rgb(244, 63, 94)).strong())
                                        .min_size(Vec2::new(95.0, 32.0))
                                ).on_hover_text("Start continuous forward recording (Hotkey: R)").clicked() {
                                    self.start_engine(RecordingMode::Continuous);
                                }

                                ui.add_space(4.0);
                                if ui.add(
                                    egui::Button::new(RichText::new("▶ Start Flashback").color(Color32::from_rgb(16, 185, 129)).strong())
                                        .min_size(Vec2::new(125.0, 32.0))
                                ).on_hover_text("Start rolling ring buffer for flashback clip exports").clicked() {
                                    self.start_engine(RecordingMode::Flashback);
                                }
                            }
                        }

                        // Quick Save Interval Buttons (Active in Flashback mode, dimmed when idle/continuous)
                        let is_flashback = self.recording_mode == Some(RecordingMode::Flashback);
                        let intervals: Vec<u64> = if self.config.clip_intervals.is_empty() {
                            crate::config::default_clip_intervals()
                        } else {
                            self.config.clip_intervals.clone()
                        };

                        ui.add_space(8.0);
                        for (idx, &secs) in intervals.iter().enumerate() {
                            let label = Self::format_interval_label(secs);
                            let shortcut_str = if idx < 9 && is_flashback {
                                format!(" [{}]", idx + 1)
                            } else {
                                String::new()
                            };

                            let btn_color = if is_flashback {
                                if self.selected_flashback_secs == secs {
                                    Color32::from_rgb(225, 29, 72)
                                } else {
                                    Color32::from_rgb(180, 24, 58)
                                }
                            } else {
                                Color32::from_rgb(32, 32, 38)
                            };

                            let text_color = if is_flashback {
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(113, 113, 122)
                            };

                            let btn_text = format!("⚡ {}{}", label, shortcut_str);
                            let btn = egui::Button::new(
                                RichText::new(btn_text)
                                    .color(text_color)
                                    .size(11.0)
                                    .strong(),
                            )
                            .fill(btn_color)
                            .min_size(Vec2::new(56.0, 32.0));

                            let mut resp = ui.add_enabled(is_flashback, btn);
                            if is_flashback {
                                let hotkey_desc = if idx < 9 {
                                    format!(" (Hotkey: {})", idx + 1)
                                } else {
                                    String::new()
                                };
                                resp = resp.on_hover_text(format!("⚡ Save last {} clip{}", label, hotkey_desc));
                                if resp.clicked() {
                                    self.selected_flashback_secs = secs;
                                    self.trigger_flashback(secs);
                                }
                            } else if self.recording_mode == Some(RecordingMode::Continuous) {
                                resp = resp.on_disabled_hover_text("Continuous recording is saving everything forward");
                            } else {
                                resp = resp.on_disabled_hover_text("▶ Start Flashback to begin buffering clips");
                            }
                            ui.add_space(2.0);
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

                            // Live Timer (counts up during recording; 0:00s when idle)
                            let format_dur = |secs: u64| {
                                if secs >= 3600 {
                                    format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
                                } else {
                                    format!("{}:{:02}s", secs / 60, secs % 60)
                                }
                            };
                            let timer_text = if self.is_recording {
                                let elapsed = self.recording_started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                                format_dur(elapsed)
                            } else {
                                "0:00s".to_string()
                            };
                            ui.label(RichText::new(timer_text).font(FontId::new(16.0, FontFamily::Monospace)).strong().color(Color32::from_rgb(244, 244, 245)));

                            ui.add_space(18.0);

                            // Metrics: 0 Mb/s (0 MB) when idle, active write rate and disk buffer size when recording
                            let total_size_mb: f64 = if self.is_recording {
                                self.managers.iter().filter_map(|m| m.get_stats().ok()).map(|s| s.total_size_mb).sum()
                            } else {
                                0.0
                            };
                            let size_str = if total_size_mb >= 1024.0 {
                                format!("({:.1} GB)", total_size_mb / 1024.0)
                            } else {
                                format!("({:.0} MB)", total_size_mb.max(0.0))
                            };
                            let bitrate_str = if self.is_recording {
                                let total_mbps: u64 = self.config.sources.iter().filter_map(|s| {
                                    s.video_bitrate.trim_end_matches(['M', 'm']).parse::<u64>().ok()
                                }).sum();
                                if total_mbps > 0 {
                                    format!("{} Mb/s", total_mbps)
                                } else {
                                    "40 Mb/s".to_string()
                                }
                            } else {
                                "0 Mb/s".to_string()
                            };
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

            ui.add_space(4.0);

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

                            // Live Viewport Canvas (Locked 16:9 Aspect Ratio)
                            let vp_width = ui.available_width();
                            let vp_height = (vp_width * 9.0 / 16.0).round();
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                            ui.painter().rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(12, 12, 16));

                            let target_aspect = 16.0 / 9.0;
                            let (draw_w, draw_h) = if rect.width() / rect.height() > target_aspect {
                                (rect.height() * target_aspect, rect.height())
                            } else {
                                (rect.width(), rect.width() / target_aspect)
                            };
                            let draw_rect = egui::Rect::from_center_size(rect.center(), Vec2::new(draw_w, draw_h));

                            if let Some(tex) = &self.preview_texture_1 {
                                ui.painter().image(
                                    tex.id(),
                                    draw_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "📷 Video preview 1\n(Ready / Standby)",
                                    FontId::new(13.0, FontFamily::Proportional),
                                    Color32::from_rgb(113, 113, 122),
                                );
                            }
                            ui.painter().rect_stroke(rect, Rounding::same(4.0), Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 40)));

                            ui.add_space(8.0);

                            // Input & Output Resolution / FPS dropdowns
                            self.render_stream_format_controls(ui, 0);
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

                            // Live Viewport Canvas (Locked 16:9 Aspect Ratio)
                            let vp_width = ui.available_width();
                            let vp_height = (vp_width * 9.0 / 16.0).round();
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                            ui.painter().rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(12, 12, 16));

                            let target_aspect = 16.0 / 9.0;
                            let (draw_w, draw_h) = if rect.width() / rect.height() > target_aspect {
                                (rect.height() * target_aspect, rect.height())
                            } else {
                                (rect.width(), rect.width() / target_aspect)
                            };
                            let draw_rect = egui::Rect::from_center_size(rect.center(), Vec2::new(draw_w, draw_h));

                            if let Some(tex) = &self.preview_texture_2 {
                                ui.painter().image(
                                    tex.id(),
                                    draw_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
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

                            // Input & Output Resolution / FPS dropdowns
                            self.render_stream_format_controls(ui, 1);
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

            let mut audio_changed = false;

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
                                        audio_changed = true;
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
                                            audio_changed = true;
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
                                            audio_changed = true;
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

            if audio_changed {
                self.sync_audio_monitors();
                let _ = self.config.save_to_file(&self.config_path);
            }
        });

        // Separate Preferences & Settings popup window
        self.render_settings_window(ctx);
    }
}

impl MultiRecordingApp {
    fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let mut is_open = self.show_settings;
        let window_frame = egui::Frame::window(&ctx.style())
            .fill(Color32::from_rgb(22, 22, 27))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 58)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(14.0));

        egui::Window::new(RichText::new("⚙ Preferences & Recording Settings").strong().color(Color32::from_rgb(244, 244, 245)))
            .open(&mut is_open)
            .resizable(true)
            .collapsible(false)
            .default_width(700.0)
            .default_height(480.0)
            .min_width(580.0)
            .min_height(350.0)
            .frame(window_frame)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // --- Row 1: Output Format & Output Folder ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("📁 Output Format:").strong().color(Color32::from_rgb(244, 244, 245)));
                        for &(fmt, label) in &[("mp4", "MP4 (.mp4)"), ("mov", "MOV (.mov)"), ("mkv", "MKV (.mkv)")] {
                            let is_cur = self.config.output_format.eq_ignore_ascii_case(fmt);
                            let btn_color = if is_cur { Color32::from_rgb(99, 102, 241) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE).size(11.0)).fill(btn_color)).clicked() {
                                self.config.output_format = fmt.to_string();
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }

                        ui.add_space(16.0);
                        ui.label(RichText::new("Folder:").strong().color(Color32::from_rgb(244, 244, 245)));
                        let mut dir_str = self.config.output_dir.to_string_lossy().to_string();
                        if ui.add(egui::TextEdit::singleline(&mut dir_str).desired_width(180.0)).changed() {
                            self.config.output_dir = PathBuf::from(dir_str);
                            let _ = self.config.save_to_file(&self.config_path);
                        }

                        if ui.add(egui::Button::new(RichText::new("📂 Browse...").size(11.0))).clicked() {
                            if let Some(p) = crate::platform::pick_folder() {
                                self.config.output_dir = p;
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }

                        if ui.add(egui::Button::new(RichText::new("Reveal").size(11.0))).clicked() {
                            let _ = fs::create_dir_all(&self.config.output_dir);
                            crate::platform::reveal_path(&self.config.output_dir);
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // --- Row 2: Video Encoder & Bitrates ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🎬 Video Encoder:").strong().color(Color32::from_rgb(244, 244, 245)));
                        let current_enc = self.config.sources.first().map(|s| s.encoder.clone()).unwrap_or_else(|| crate::platform::CurrentPlatform.default_video_encoder().to_string());
                        for &(enc, label) in crate::platform::CurrentPlatform.available_encoders() {
                            let is_cur = current_enc == enc;
                            let btn_color = if is_cur { Color32::from_rgb(99, 102, 241) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE).size(11.0)).fill(btn_color)).clicked() {
                                for src in &mut self.config.sources {
                                    src.encoder = enc.to_string();
                                }
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }

                        ui.add_space(16.0);
                        ui.label(RichText::new("Segment:").strong().color(Color32::from_rgb(244, 244, 245)));
                        for &(dur, label) in &[(2, "2s (Fast)"), (3, "3s"), (5, "5s")] {
                            let is_cur = self.config.segment_duration_secs == dur;
                            let btn_color = if is_cur { Color32::from_rgb(99, 102, 241) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE).size(11.0)).fill(btn_color)).clicked() {
                                self.config.segment_duration_secs = dur;
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }
                    });

                    ui.add_space(6.0);

                    // --- Row 3: Bitrates (Stream 1, Stream 2, Audio) ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚡ Bitrates:").strong().color(Color32::from_rgb(244, 244, 245)));

                        ui.label(RichText::new("Stream 1:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                        let cur_b1 = self.config.sources.first().map(|s| s.video_bitrate.clone()).unwrap_or_else(|| "40M".to_string());
                        for &b in &["20M", "30M", "40M", "60M", "80M"] {
                            let is_cur = cur_b1 == b;
                            let btn_color = if is_cur { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(b).color(Color32::WHITE).size(10.0)).fill(btn_color)).clicked() {
                                if let Some(src) = self.config.sources.first_mut() {
                                    src.video_bitrate = b.to_string();
                                    let _ = self.config.save_to_file(&self.config_path);
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.label(RichText::new("Stream 2:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                        let cur_b2 = self.config.sources.get(1).map(|s| s.video_bitrate.clone()).unwrap_or_else(|| "15M".to_string());
                        for &b in &["8M", "12M", "15M", "20M"] {
                            let is_cur = cur_b2 == b;
                            let btn_color = if is_cur { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(b).color(Color32::WHITE).size(10.0)).fill(btn_color)).clicked() {
                                if self.config.sources.len() > 1 {
                                    self.config.sources[1].video_bitrate = b.to_string();
                                    let _ = self.config.save_to_file(&self.config_path);
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.label(RichText::new("Audio:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                        for &ab in &["192k", "256k", "320k"] {
                            let is_cur = self.config.audio_bitrate == ab;
                            let btn_color = if is_cur { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(45, 45, 52) };
                            if ui.add(egui::Button::new(RichText::new(ab).color(Color32::WHITE).size(10.0)).fill(btn_color)).clicked() {
                                self.config.audio_bitrate = ab.to_string();
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // --- Row 4: Flashback Buffer Window ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⏱ Flashback Buffer Window:").strong().color(Color32::from_rgb(244, 244, 245)));
                        for &(secs, label) in &[
                            (60, "1m"),
                            (300, "5m"),
                            (900, "15m"),
                            (1800, "30m"),
                            (3600, "1h"),
                            (7200, "2h"),
                        ] {
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

                        ui.add_space(8.0);
                        let mut mins = (self.selected_flashback_secs / 60).clamp(1, 120);
                        if ui.add(egui::Slider::new(&mut mins, 1..=120).suffix(" min")).changed() {
                            self.selected_flashback_secs = mins * 60;
                            self.config.buffer_duration_secs = mins * 60;
                            let _ = self.config.save_to_file(&self.config_path);
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("🧹 Clean Buffer Cache").fill(Color32::from_rgb(45, 45, 52))).clicked() {
                                if let Ok(entries) = fs::read_dir(&self.config.buffer_dir) {
                                    for entry in entries.flatten() {
                                        let p = entry.path();
                                        if p.extension().map(|e| e == "mp4").unwrap_or(false) {
                                            let _ = fs::remove_file(p);
                                        }
                                    }
                                    self.status_message = Some(("Buffer cache cleaned.".to_string(), Instant::now(), false));
                                }
                            }
                        });
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // --- Row 4b: Quick Clip Intervals Configuration ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚡ Quick Clip Intervals:").strong().color(Color32::from_rgb(244, 244, 245)));
                        ui.label(RichText::new("(Toggle presets or add custom)").size(11.0).color(Color32::from_rgb(156, 163, 175)));

                        let presets = [
                            (15, "15s"),
                            (30, "30s"),
                            (45, "45s"),
                            (60, "1m"),
                            (120, "2m"),
                            (300, "5m"),
                            (600, "10m"),
                            (900, "15m"),
                            (1200, "20m"),
                            (1800, "30m"),
                        ];

                        let mut changed = false;
                        for &(secs, label) in &presets {
                            let is_active = self.config.clip_intervals.contains(&secs);
                            let btn_color = if is_active {
                                Color32::from_rgb(225, 29, 72)
                            } else {
                                Color32::from_rgb(45, 45, 52)
                            };
                            let text_color = if is_active {
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(161, 161, 170)
                            };

                            if ui.add(
                                egui::Button::new(RichText::new(label).color(text_color).size(10.0))
                                    .fill(btn_color)
                            ).clicked() {
                                if is_active {
                                    self.config.clip_intervals.retain(|&x| x != secs);
                                } else {
                                    self.config.clip_intervals.push(secs);
                                    self.config.clip_intervals.sort_unstable();
                                }
                                changed = true;
                            }
                        }

                        ui.add_space(6.0);
                        ui.label(RichText::new("Sec:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                        let resp = ui.add(egui::TextEdit::singleline(&mut self.custom_clip_sec_input).desired_width(40.0));
                        if ui.add(egui::Button::new(RichText::new("+ Add").size(10.0))).clicked()
                            || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            if let Ok(secs) = self.custom_clip_sec_input.trim().parse::<u64>() {
                                if secs > 0 && !self.config.clip_intervals.contains(&secs) {
                                    self.config.clip_intervals.push(secs);
                                    self.config.clip_intervals.sort_unstable();
                                    self.custom_clip_sec_input.clear();
                                    changed = true;
                                }
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(RichText::new("Reset Defaults").size(10.0)).fill(Color32::from_rgb(45, 45, 52))).clicked() {
                                self.config.clip_intervals = crate::config::default_clip_intervals();
                                changed = true;
                            }
                        });

                        if changed {
                            if self.config.clip_intervals.is_empty() {
                                self.config.clip_intervals = vec![60];
                            }
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // --- Row 5: NLE Multicam & Post Sync (Premiere Pro / DaVinci Resolve) ---
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🔗 NLE Post Sync:").strong().color(Color32::from_rgb(244, 244, 245)));

                        if ui.checkbox(&mut self.config.sync_audio_to_secondary, "Mirror Track 1 Audio to Video 2 (Waveform Sync)").changed() {
                            let _ = self.config.save_to_file(&self.config_path);
                        }

                        ui.add_space(16.0);

                        if ui.checkbox(&mut self.config.embed_timecode, "Embed SMPTE Timecode (Timecode Sync)").changed() {
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // --- Row 6: Hardware & System Audio Driver ---
                    ui.horizontal(|ui| {
                        #[cfg(target_os = "macos")]
                        {
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
                                    crate::platform::open_system_audio_control();
                                }
                            }
                        }

                        #[cfg(target_os = "windows")]
                        {
                            ui.label(RichText::new("● Windows DirectShow & CoreAudio Capture: Active").color(Color32::from_rgb(16, 185, 129)).size(11.0));
                            if ui.add(egui::Button::new(RichText::new("🎛 Sound Control Panel").size(10.0))).clicked() {
                                crate::platform::open_system_audio_control();
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("🔄 Refresh Hardware Devices")).clicked() {
                                self.refresh_devices();
                            }
                        });
                    });

                    if !self.recent_exports.is_empty() {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.label(RichText::new("Recent Saved Flashbacks:").size(11.0).color(Color32::from_rgb(161, 161, 170)));
                        for exp in &self.recent_exports {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("⚡ {}", exp.timestamp)).size(11.0).strong().color(Color32::from_rgb(244, 244, 245)));
                                ui.label(RichText::new(format!("({}s / {:.1} MB)", exp.duration_secs, exp.size_mb)).size(10.0).color(Color32::from_rgb(113, 113, 122)));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(egui::Button::new(RichText::new("Reveal").size(10.0))).clicked() {
                                        crate::platform::reveal_file(&exp.path);
                                    }
                                });
                            });
                        }
                    }

                    if !self.recorders.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            for rec in &self.recorders {
                                if ui.add(egui::Button::new(RichText::new(format!("Open [{}] Log", rec.source_name())).size(10.0))).clicked() {
                                    crate::platform::open_text_file(rec.log_file_path());
                                }
                            }
                        });
                    }
                });
            });

        self.show_settings = is_open;
    }
}
