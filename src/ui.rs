#![allow(deprecated)]

mod audio_panel;
mod engine_control;
mod settings_window;
mod theme;
mod top_bar;
mod video_panel;

use crate::config::Config;
use crate::devices::{Device, DeviceManager};
use crate::recorder::{AudioTrackMonitor, PreviewStream, StreamRecorder};
use crate::ring_buffer::RingBufferManager;
use eframe::egui::{self, Color32, Margin};
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
    pub(crate) config: Config,
    pub(crate) config_path: PathBuf,
    pub(crate) discovered_devices: Vec<Device>,
    pub(crate) has_loopback: bool,

    // Engine runtime state
    pub(crate) is_recording: bool,
    pub(crate) recording_mode: Option<RecordingMode>,
    pub(crate) recorders: Vec<StreamRecorder>,
    pub(crate) managers: Vec<RingBufferManager>,
    pub(crate) last_eviction: Instant,

    // Live Video Previews
    pub(crate) preview_stream_1: Option<PreviewStream>,
    pub(crate) preview_stream_2: Option<PreviewStream>,
    pub(crate) preview_texture_1: Option<egui::TextureHandle>,
    pub(crate) preview_texture_2: Option<egui::TextureHandle>,
    pub(crate) last_preview_seq_1: u64,
    pub(crate) last_preview_seq_2: u64,

    // Real Hardware Audio Monitors
    pub(crate) audio_monitors: [Option<AudioTrackMonitor>; 5],

    // Stats, UI & Feedback
    pub(crate) status_message: Option<(String, Instant, bool)>, // (msg, time, is_error)
    pub(crate) recent_exports: Vec<ExportRecord>,
    pub(crate) selected_flashback_secs: u64,
    pub(crate) show_settings: bool,
    pub(crate) recording_started_at: Option<Instant>,
    pub(crate) audio_levels: [(f32, f32); 5],

    // Selected video device indices
    pub(crate) game_video_idx: usize,
    pub(crate) cam_video_idx: usize,
    #[allow(dead_code)]
    pub(crate) copied_brew_cmd_until: Option<Instant>,
    pub(crate) custom_clip_sec_input: String,
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
                "⚠️ No capture devices found. Please ensure FFmpeg is installed and accessible in PATH."
                    .to_string(),
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
            audio_levels: [(0.0, 0.0); 5],
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

    fn poll_background_tasks(&mut self, ctx: &egui::Context) {
        // Background Ring Buffer Eviction (Only active during Flashback mode!)
        if self.is_recording
            && self.recording_mode == Some(RecordingMode::Flashback)
            && self.last_eviction.elapsed() >= Duration::from_secs(5)
        {
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
                self.stop_engine();
                self.status_message = Some((err, Instant::now(), true));
            }
        }

        // 1. Process Video Preview Frames into Textures (only when a new frame arrives)
        let cur_seq_1 = if self.is_recording {
            self.recorders.first().map(|r| r.frame_seq()).unwrap_or(0)
        } else {
            self.preview_stream_1
                .as_ref()
                .map(|p| p.frame_seq())
                .unwrap_or(0)
        };

        if cur_seq_1 != self.last_preview_seq_1 || self.preview_texture_1.is_none() {
            let frame_1_bytes = if self.is_recording {
                self.recorders.first().and_then(|r| r.get_latest_frame())
            } else {
                self.preview_stream_1
                    .as_ref()
                    .and_then(|p| p.get_latest_frame())
            };

            if let Some(bytes) = frame_1_bytes {
                if bytes.len() == crate::recorder::PREVIEW_FRAME_SIZE {
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [
                            crate::recorder::PREVIEW_WIDTH,
                            crate::recorder::PREVIEW_HEIGHT,
                        ],
                        &bytes,
                    );
                    if let Some(tex) = &mut self.preview_texture_1 {
                        tex.set(color_img, egui::TextureOptions::LINEAR);
                    } else {
                        self.preview_texture_1 =
                            Some(ctx.load_texture("preview_1", color_img, egui::TextureOptions::LINEAR));
                    }
                    self.last_preview_seq_1 = cur_seq_1;
                }
            }
        }

        let cur_seq_2 = if self.is_recording {
            if self.recorders.len() > 1 {
                self.recorders[1].frame_seq()
            } else {
                0
            }
        } else {
            self.preview_stream_2
                .as_ref()
                .map(|p| p.frame_seq())
                .unwrap_or(0)
        };

        if cur_seq_2 != self.last_preview_seq_2 || self.preview_texture_2.is_none() {
            let frame_2_bytes = if self.is_recording {
                if self.recorders.len() > 1 {
                    self.recorders[1].get_latest_frame()
                } else {
                    None
                }
            } else {
                self.preview_stream_2
                    .as_ref()
                    .and_then(|p| p.get_latest_frame())
            };

            if let Some(bytes) = frame_2_bytes {
                if bytes.len() == crate::recorder::PREVIEW_FRAME_SIZE {
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [
                            crate::recorder::PREVIEW_WIDTH,
                            crate::recorder::PREVIEW_HEIGHT,
                        ],
                        &bytes,
                    );
                    if let Some(tex) = &mut self.preview_texture_2 {
                        tex.set(color_img, egui::TextureOptions::LINEAR);
                    } else {
                        self.preview_texture_2 =
                            Some(ctx.load_texture("preview_2", color_img, egui::TextureOptions::LINEAR));
                    }
                    self.last_preview_seq_2 = cur_seq_2;
                }
            }
        }

        // 2. Sample Stereo (L/R) Audio VU Levels from Real Hardware Monitors & apply per-track Gain/Volume
        for i in 0..5 {
            let (track_enabled, gain) = self
                .config
                .audio_tracks
                .get(i)
                .map(|t| (t.enabled, t.volume.max(0.0)))
                .unwrap_or((false, 1.0));
            if track_enabled {
                let (raw_l, raw_r) = self.audio_monitors[i]
                    .as_ref()
                    .map(|m| m.get_peaks())
                    .unwrap_or((0.0, 0.0));
                let peak_l = (raw_l * gain).clamp(0.0, 1.0);
                let peak_r = (raw_r * gain).clamp(0.0, 1.0);

                let (mut cur_l, mut cur_r) = self.audio_levels[i];
                if peak_l > cur_l {
                    cur_l = cur_l * 0.2 + peak_l * 0.8;
                } else {
                    cur_l *= 0.88;
                }
                if peak_r > cur_r {
                    cur_r = cur_r * 0.2 + peak_r * 0.8;
                } else {
                    cur_r *= 0.88;
                }
                self.audio_levels[i] = (cur_l, cur_r);
            } else {
                self.audio_levels[i] = (0.0, 0.0);
            }
        }

        // 3. Global hotkeys ('R' for continuous record, Space/F/1..9 for flashback)
        if !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::R)) {
                if self.is_recording {
                    if self.recording_mode == Some(RecordingMode::Continuous) {
                        self.stop_continuous_recording();
                    }
                } else {
                    self.start_engine(RecordingMode::Continuous);
                }
            }

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
    }
}

impl eframe::App for MultiRecordingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Enforce permanent dark mode regardless of macOS/Windows system theme
        if !ctx.style().visuals.dark_mode
            || ctx.options(|opt| opt.theme_preference) != egui::ThemePreference::Dark
        {
            Self::apply_flat_theme(ctx);
        }

        ctx.request_repaint_after(Duration::from_millis(16));

        self.poll_background_tasks(ctx);

        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(14, 14, 18))
            .inner_margin(Margin::same(12.0));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            ui.add_space(4.0);

            self.render_top_bar(ui);
            self.render_status_banner(ui);

            ui.add_space(4.0);

            self.render_video_section(ui);

            ui.add_space(10.0);

            self.render_audio_section(ui);
        });

        self.render_settings_window(ctx);
    }
}
