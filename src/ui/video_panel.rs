use super::MultiRecordingApp;
use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Rounding, Stroke, Vec2};

impl MultiRecordingApp {
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
                self.preview_stream_1
                    .as_ref()
                    .and_then(|p| p.get_detected_format())
            } else {
                self.preview_stream_2
                    .as_ref()
                    .and_then(|p| p.get_detected_format())
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

        // Auto-clamp output fps to input fps if higher than what hardware supplies (and heal invalid <24fps)
        if let Some(src) = self.config.sources.get_mut(stream_idx) {
            if let Some(cur_fps) = src.fps {
                if cur_fps > in_fps || cur_fps < 24 {
                    src.fps = Some(in_fps);
                    let _ = self.config.save_to_file(&self.config_path);
                }
            }
        }

        let cur_fps = self
            .config
            .sources
            .get(stream_idx)
            .and_then(|s| s.fps)
            .unwrap_or(in_fps)
            .min(in_fps);

        ui.horizontal(|ui| {
            // 1. Input Format Badge
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("input")
                        .size(10.0)
                        .color(Color32::from_rgb(156, 163, 175)),
                );
                let in_label = format!("{}  {}fps", in_res, in_fps);
                egui::Frame::none()
                    .fill(Color32::from_rgb(26, 26, 31))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 52)))
                    .rounding(Rounding::same(3.0))
                    .inner_margin(Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(in_label)
                                .size(11.0)
                                .monospace()
                                .color(Color32::from_rgb(228, 228, 231)),
                        );
                    });
            });

            ui.add_space(6.0);

            // 2. Output Resolution Dropdown
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("output res")
                        .size(10.0)
                        .color(Color32::from_rgb(156, 163, 175)),
                );
                let cur_res_label = self.get_output_resolution_label(stream_idx, &in_res);
                egui::ComboBox::from_id_source(format!("stream_{}_res_combo", stream_idx))
                    .width(135.0)
                    .selected_text(
                        RichText::new(cur_res_label)
                            .size(11.0)
                            .monospace()
                            .color(Color32::from_rgb(228, 228, 231)),
                    )
                    .show_ui(ui, |ui| {
                        let native_label = format!("Native ({})", in_res);
                        let is_native = self
                            .config
                            .sources
                            .get(stream_idx)
                            .map(|s| s.resolution.is_none())
                            .unwrap_or(false);
                        if ui.selectable_label(is_native, native_label).clicked() {
                            self.set_output_resolution(stream_idx, None);
                        }

                        ui.separator();

                        for &(label, res_opt) in &[
                            ("1080p (1920x1080)", "1920x1080"),
                            ("1440p (2560x1440)", "2560x1440"),
                            ("4K (3840x2160)", "3840x2160"),
                            ("720p (1280x720)", "1280x720"),
                        ] {
                            let is_cur = self
                                .config
                                .sources
                                .get(stream_idx)
                                .and_then(|s| s.resolution.as_deref())
                                == Some(res_opt);
                            if ui.selectable_label(is_cur, label).clicked() {
                                self.set_output_resolution(stream_idx, Some(res_opt));
                            }
                        }
                    });
            });

            ui.add_space(6.0);

            // 3. Output Frame Rate Dropdown (Strictly constrained by input frame rate)
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("fps")
                        .size(10.0)
                        .color(Color32::from_rgb(156, 163, 175)),
                );
                let cur_fps_label = format!("{} fps", cur_fps);
                egui::ComboBox::from_id_source(format!("stream_{}_fps_combo", stream_idx))
                    .width(72.0)
                    .selected_text(
                        RichText::new(cur_fps_label)
                            .size(11.0)
                            .monospace()
                            .color(Color32::from_rgb(228, 228, 231)),
                    )
                    .show_ui(ui, |ui| {
                        let mut allowed_fps = Vec::new();
                        if in_fps >= 120 {
                            allowed_fps.push(120);
                        }
                        if in_fps >= 60 {
                            allowed_fps.push(60);
                        }
                        if (50..60).contains(&in_fps) {
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
                            ui.label(
                                RichText::new(format!("Capped at input ({}fps)", in_fps))
                                    .size(10.0)
                                    .color(Color32::from_rgb(156, 163, 175)),
                            );
                        }
                    });
            });
        });

        // 4. Real-time encoder health & bottleneck diagnostic (shown during recording)
        if self.is_recording {
            if let Some(rec) = self.recorders.get(stream_idx) {
                let health = rec.get_health();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if health.frames_processed > 0 {
                        if health.is_lagging {
                            ui.label(
                                RichText::new("⚠️ ENCODER BOTTLENECK:")
                                    .size(10.5)
                                    .color(Color32::from_rgb(244, 63, 94))
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{:.1} fps ({:.2}x speed) - Dropping frames!",
                                    health.current_fps, health.speed_ratio
                                ))
                                .size(10.5)
                                .color(Color32::from_rgb(251, 113, 133)),
                            );
                        } else {
                            ui.label(
                                RichText::new("⚡ Real-time:")
                                    .size(10.5)
                                    .color(Color32::from_rgb(16, 185, 129))
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{:.1} fps ({:.2}x speed)",
                                    health.current_fps, health.speed_ratio
                                ))
                                .size(10.5)
                                .color(Color32::from_rgb(212, 212, 216)),
                            );
                        }
                    } else {
                        ui.label(
                            RichText::new("⚡ Encoder:")
                                .size(10.5)
                                .color(Color32::from_rgb(161, 161, 170)),
                        );
                        ui.label(
                            RichText::new("Initializing stream...")
                                .size(10.5)
                                .color(Color32::from_rgb(113, 113, 122)),
                        );
                    }
                });
            } else if self.recorders.len() == 1 && stream_idx == 1 {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("ℹ️ Standby (Single video source active)")
                        .size(10.5)
                        .color(Color32::from_rgb(113, 113, 122)),
                );
            }
        }
    }

    pub(crate) fn render_video_section(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Video")
                .font(FontId::new(15.0, FontFamily::Proportional))
                .strong()
                .color(Color32::from_rgb(244, 244, 245)),
        );
        ui.add_space(4.0);

        let video_options: Vec<(usize, String)> = self
            .video_devices()
            .into_iter()
            .map(|d| (d.index, d.name.clone()))
            .collect();

        // Reserve vertical space for the Video card controls + the 5-track Audio section below
        let reserved_for_controls_and_audio = if self.is_recording { 370.0 } else { 350.0 };
        let max_vp_height = (ui.available_height() - reserved_for_controls_and_audio)
            .max(80.0)
            .round();

        ui.columns(2, |columns| {
            // Column 0: Video Preview 1 (Gameplay)
            columns[0].vertical(|ui| {
                egui::Frame::none()
                    .fill(Color32::from_rgb(22, 22, 26))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        // Header + Device Selector
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Gameplay")
                                    .strong()
                                    .color(Color32::from_rgb(244, 244, 245)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let cur_v_label = video_options
                                        .iter()
                                        .find(|(idx, _)| *idx == self.game_video_idx)
                                        .map(|(idx, name)| format!("[{}] {}", idx, name))
                                        .unwrap_or_else(|| {
                                            format!("Device {}", self.game_video_idx)
                                        });

                                    egui::ComboBox::from_id_source("game_v_combo")
                                        .width(170.0)
                                        .selected_text(cur_v_label)
                                        .show_ui(ui, |ui| {
                                            for (idx, name) in &video_options {
                                                if ui
                                                    .selectable_value(
                                                        &mut self.game_video_idx,
                                                        *idx,
                                                        format!("[{}] {}", idx, name),
                                                    )
                                                    .clicked()
                                                {
                                                    self.stop_preview_streams();
                                                    self.ensure_preview_streams();
                                                }
                                            }
                                        });
                                },
                            );
                        });

                        ui.add_space(6.0);

                        // Live Viewport Canvas (Locked 16:9 Aspect Ratio, responsive to available height)
                        let vp_width = ui.available_width();
                        let vp_height = (vp_width * 9.0 / 16.0).round().min(max_vp_height);
                        let (rect, _) = ui
                            .allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                        ui.painter()
                            .rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(12, 12, 16));

                        let target_aspect = 16.0 / 9.0;
                        let (draw_w, draw_h) = if rect.width() / rect.height() > target_aspect {
                            (rect.height() * target_aspect, rect.height())
                        } else {
                            (rect.width(), rect.width() / target_aspect)
                        };
                        let draw_rect =
                            egui::Rect::from_center_size(rect.center(), Vec2::new(draw_w, draw_h));

                        if let Some(tex) = &self.preview_texture_1 {
                            ui.painter().image(
                                tex.id(),
                                draw_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else {
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "📷 Gameplay\n(Ready / Standby)",
                                FontId::new(13.0, FontFamily::Proportional),
                                Color32::from_rgb(113, 113, 122),
                            );
                        }
                        ui.painter().rect_stroke(
                            rect,
                            Rounding::same(4.0),
                            Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 40)),
                        );

                        ui.add_space(8.0);

                        // Input & Output Resolution / FPS dropdowns
                        self.render_stream_format_controls(ui, 0);
                    });
            });

            // Column 1: Video Preview 2 (Facecam)
            columns[1].vertical(|ui| {
                egui::Frame::none()
                    .fill(Color32::from_rgb(22, 22, 26))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        // Header + Device Selector
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Facecam")
                                    .strong()
                                    .color(Color32::from_rgb(244, 244, 245)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let cur_cam_v_label = video_options
                                        .iter()
                                        .find(|(idx, _)| *idx == self.cam_video_idx)
                                        .map(|(idx, name)| format!("[{}] {}", idx, name))
                                        .unwrap_or_else(|| {
                                            format!("Device {}", self.cam_video_idx)
                                        });

                                    egui::ComboBox::from_id_source("cam_v_combo")
                                        .width(170.0)
                                        .selected_text(cur_cam_v_label)
                                        .show_ui(ui, |ui| {
                                            for (idx, name) in &video_options {
                                                if ui
                                                    .selectable_value(
                                                        &mut self.cam_video_idx,
                                                        *idx,
                                                        format!("[{}] {}", idx, name),
                                                    )
                                                    .clicked()
                                                {
                                                    self.stop_preview_streams();
                                                    self.ensure_preview_streams();
                                                }
                                            }
                                        });
                                },
                            );
                        });

                        ui.add_space(6.0);

                        // Live Viewport Canvas (Locked 16:9 Aspect Ratio, responsive to available height)
                        let vp_width = ui.available_width();
                        let vp_height = (vp_width * 9.0 / 16.0).round().min(max_vp_height);
                        let (rect, _) = ui
                            .allocate_exact_size(Vec2::new(vp_width, vp_height), egui::Sense::hover());

                        ui.painter()
                            .rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(12, 12, 16));

                        let target_aspect = 16.0 / 9.0;
                        let (draw_w, draw_h) = if rect.width() / rect.height() > target_aspect {
                            (rect.height() * target_aspect, rect.height())
                        } else {
                            (rect.width(), rect.width() / target_aspect)
                        };
                        let draw_rect =
                            egui::Rect::from_center_size(rect.center(), Vec2::new(draw_w, draw_h));

                        if let Some(tex) = &self.preview_texture_2 {
                            ui.painter().image(
                                tex.id(),
                                draw_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else {
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "📷 Facecam\n(Standby)",
                                FontId::new(13.0, FontFamily::Proportional),
                                Color32::from_rgb(113, 113, 122),
                            );
                        }
                        ui.painter().rect_stroke(
                            rect,
                            Rounding::same(4.0),
                            Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 40)),
                        );

                        ui.add_space(8.0);

                        // Input & Output Resolution / FPS dropdowns
                        self.render_stream_format_controls(ui, 1);
                    });
            });
        });
    }
}
