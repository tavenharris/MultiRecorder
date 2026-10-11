use super::MultiRecordingApp;
use crate::devices::{Device, DeviceType};
use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Rounding, Stroke, Vec2};

impl MultiRecordingApp {
    pub(crate) fn render_audio_section(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Audio")
                .font(FontId::new(15.0, FontFamily::Proportional))
                .strong()
                .color(Color32::from_rgb(244, 244, 245)),
        );
        ui.add_space(4.0);

        let audio_device_items: Vec<Device> = self.audio_devices().into_iter().cloned().collect();
        let has_loopback = self.has_loopback;

        let mut audio_changed = false;
        let mut volume_changed = false;

        egui::Frame::none()
            .fill(Color32::from_rgb(22, 22, 26))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(38, 38, 44)))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                let row_height = 24.0;
                let idx_col_w = 18.0;
                let dev_col_w = 220.0;
                let gain_col_w = 170.0;

                for i in 0..5 {
                    let track = &mut self.config.audio_tracks[i];

                    ui.horizontal(|ui| {
                        // 1. Track Number (Fixed width so all columns align)
                        ui.allocate_ui_with_layout(
                            Vec2::new(idx_col_w, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(idx_col_w);
                                ui.set_max_width(idx_col_w);
                                ui.label(
                                    RichText::new(format!("{}", i + 1))
                                        .font(FontId::new(14.0, FontFamily::Proportional))
                                        .strong()
                                        .color(Color32::from_rgb(212, 212, 216)),
                                );
                            },
                        );

                        // 2. Fixed-Width Audio Device Dropdown (Fixed char length + fixed pixel box)
                        let full_selected_text = if !track.enabled || track.device_id == "none" {
                            "None".to_string()
                        } else {
                            audio_device_items
                                .iter()
                                .find(|d| d.id == track.device_id)
                                .map(|d| d.display_label(has_loopback))
                                .unwrap_or_else(|| track.name.clone())
                        };
                        let fixed_selected_text =
                            Self::format_fixed_audio_label(&full_selected_text, 24);

                        ui.allocate_ui_with_layout(
                            Vec2::new(dev_col_w, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(dev_col_w);
                                ui.set_max_width(dev_col_w);

                                let combo_resp =
                                    egui::ComboBox::from_id_source(format!("audio_track_box_{}", i))
                                        .width(dev_col_w - 4.0)
                                        .truncate()
                                        .selected_text(
                                            RichText::new(fixed_selected_text)
                                                .font(FontId::new(11.5, FontFamily::Monospace)),
                                        )
                                        .show_ui(ui, |ui| {
                                            // Option: None
                                            if ui.selectable_label(!track.enabled, "None").clicked()
                                            {
                                                track.enabled = false;
                                                track.device_id = "none".to_string();
                                                audio_changed = true;
                                            }

                                            ui.separator();
                                            ui.label(
                                                RichText::new("── HARDWARE INPUTS ──")
                                                    .size(10.0)
                                                    .color(Color32::from_rgb(156, 163, 175)),
                                            );
                                            for dev in audio_device_items
                                                .iter()
                                                .filter(|d| d.device_type == DeviceType::AudioInput)
                                            {
                                                let is_sel =
                                                    track.enabled && track.device_id == dev.id;
                                                if ui
                                                    .selectable_label(
                                                        is_sel,
                                                        dev.display_label(has_loopback),
                                                    )
                                                    .clicked()
                                                {
                                                    track.enabled = true;
                                                    track.device_id = dev.id.clone();
                                                    track.device_index = dev.av_index.unwrap_or(0);
                                                    track.name = dev.name.clone();
                                                    audio_changed = true;
                                                }
                                            }

                                            ui.separator();
                                            ui.label(
                                                RichText::new("── AUDIO OUTPUTS ──")
                                                    .size(10.0)
                                                    .color(Color32::from_rgb(156, 163, 175)),
                                            );
                                            for dev in audio_device_items.iter().filter(|d| {
                                                d.device_type == DeviceType::AudioOutput
                                            }) {
                                                let is_sel =
                                                    track.enabled && track.device_id == dev.id;
                                                if ui
                                                    .selectable_label(
                                                        is_sel,
                                                        dev.display_label(has_loopback),
                                                    )
                                                    .clicked()
                                                {
                                                    track.enabled = true;
                                                    track.device_id = dev.id.clone();
                                                    track.device_index = dev.av_index.unwrap_or(0);
                                                    track.name = dev.name.clone();
                                                    audio_changed = true;
                                                }
                                            }
                                        });
                                combo_resp.response.on_hover_text(&full_selected_text);
                            },
                        );

                        ui.add_space(6.0);

                        // 3. Gain / Volume Slider (Fixed width: Mute toggle + 0..200% slider + % readout)
                        ui.allocate_ui_with_layout(
                            Vec2::new(gain_col_w, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(gain_col_w);
                                ui.set_max_width(gain_col_w);

                                let mut pct =
                                    (track.volume * 100.0).round().clamp(0.0, 200.0) as i32;
                                let icon = if !track.enabled || pct == 0 {
                                    "🔇"
                                } else if pct < 80 {
                                    "🔉"
                                } else {
                                    "🔊"
                                };

                                if ui
                                    .add_enabled(
                                        track.enabled,
                                        egui::Button::new(RichText::new(icon).size(11.5))
                                            .frame(false)
                                            .min_size(Vec2::new(18.0, 18.0)),
                                    )
                                    .on_hover_text("Click to toggle Mute (0% / 100%)")
                                    .clicked()
                                {
                                    track.volume = if track.volume > 0.01 { 0.0 } else { 1.0 };
                                    pct = (track.volume * 100.0).round() as i32;
                                    volume_changed = true;
                                }

                                ui.spacing_mut().slider_width = 96.0;
                                let db_str = if track.volume <= 0.001 {
                                    "-∞ dB".to_string()
                                } else {
                                    format!("{:+.1} dB", 20.0 * track.volume.log10())
                                };
                                let slider_resp = ui
                                    .add_enabled(
                                        track.enabled,
                                        egui::Slider::new(&mut pct, 0..=200).show_value(false),
                                    )
                                    .on_hover_text(format!(
                                        "Track {} Gain / Volume: {}% ({})\nDouble-click to reset to 100% (0.0 dB)",
                                        i + 1,
                                        pct,
                                        db_str
                                    ));

                                if slider_resp.double_clicked() {
                                    pct = 100;
                                    track.volume = 1.0;
                                    volume_changed = true;
                                } else if slider_resp.changed() {
                                    track.volume = (pct as f32) / 100.0;
                                    volume_changed = true;
                                }

                                ui.label(
                                    RichText::new(format!("{:>3}%", pct))
                                        .font(FontId::new(11.0, FontFamily::Monospace))
                                        .color(if track.enabled {
                                            Color32::from_rgb(212, 212, 216)
                                        } else {
                                            Color32::from_rgb(113, 113, 122)
                                        }),
                                );
                            },
                        );

                        ui.add_space(6.0);

                        // 4. Dual Stereo (Left / Right) Multi-Color Segmented VU Meter (Identical Remaining Width)
                        let meter_width = ui.available_width().max(80.0);
                        draw_stereo_vu_meter(ui, self.audio_levels[i], track.enabled, meter_width);
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
        } else if volume_changed {
            let _ = self.config.save_to_file(&self.config_path);
        }
    }
}

fn draw_single_vu_bar(painter: &egui::Painter, rect: egui::Rect, level: f32, is_active: bool) {
    let rounding = Rounding::same(2.0);
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
        egui::Rect::from_min_max(
            egui::pos2(x0, rect.top()),
            egui::pos2(x1 - 1.0, rect.bottom()),
        ),
        Rounding::ZERO,
        Color32::from_rgb(18, 42, 22), // Dim green
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(x1, rect.top()),
            egui::pos2(x2 - 1.0, rect.bottom()),
        ),
        Rounding::ZERO,
        Color32::from_rgb(42, 45, 18), // Dim yellow
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(x2, rect.top()),
            egui::pos2(x3 - 1.0, rect.bottom()),
        ),
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
                egui::Rect::from_min_max(
                    egui::pos2(x0, rect.top()),
                    egui::pos2(lit_x, rect.bottom()),
                ),
                Rounding::same(1.5),
                Color32::from_rgb(132, 204, 22), // Lime green
            );
        }
        // Lit Yellow (60..80%)
        if cur_x > x1 {
            let lit_x = cur_x.min(x2 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x1, rect.top()),
                    egui::pos2(lit_x, rect.bottom()),
                ),
                Rounding::ZERO,
                Color32::from_rgb(234, 179, 8), // Yellow
            );
        }
        // Lit Orange (80..90%)
        if cur_x > x2 {
            let lit_x = cur_x.min(x3 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x2, rect.top()),
                    egui::pos2(lit_x, rect.bottom()),
                ),
                Rounding::ZERO,
                Color32::from_rgb(249, 115, 22), // Orange
            );
        }
        // Lit Red (90..100%)
        if cur_x > x3 {
            let lit_x = cur_x.min(x4);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x3, rect.top()),
                    egui::pos2(lit_x, rect.bottom()),
                ),
                Rounding::same(1.5),
                Color32::from_rgb(239, 68, 68), // Red
            );
        }
    }

    // Outer subtle border
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0_f32, Color32::from_rgb(42, 42, 48)),
    );
}

// Custom Stereo (Left / Right) VU Peak Level Meter Widget
fn draw_stereo_vu_meter(ui: &mut egui::Ui, levels: (f32, f32), is_active: bool, width: f32) {
    let total_height = 20.0;
    let bar_height = 8.5;
    let label_col_w = 13.0;
    let (rect, _response) =
        ui.allocate_exact_size(Vec2::new(width, total_height), egui::Sense::hover());
    let painter = ui.painter();

    let bar_left_x = (rect.left() + label_col_w).min(rect.right());
    let top_bar_rect = egui::Rect::from_min_max(
        egui::pos2(bar_left_x, rect.top()),
        egui::pos2(rect.right(), rect.top() + bar_height),
    );
    let bottom_bar_rect = egui::Rect::from_min_max(
        egui::pos2(bar_left_x, rect.bottom() - bar_height),
        egui::pos2(rect.right(), rect.bottom()),
    );

    let ch_color = if is_active {
        Color32::from_rgb(161, 161, 170)
    } else {
        Color32::from_rgb(90, 90, 100)
    };
    painter.text(
        egui::pos2(rect.left() + 4.5, top_bar_rect.center().y),
        egui::Align2::CENTER_CENTER,
        "L",
        FontId::new(8.5, FontFamily::Monospace),
        ch_color,
    );
    painter.text(
        egui::pos2(rect.left() + 4.5, bottom_bar_rect.center().y),
        egui::Align2::CENTER_CENTER,
        "R",
        FontId::new(8.5, FontFamily::Monospace),
        ch_color,
    );

    draw_single_vu_bar(painter, top_bar_rect, levels.0, is_active);
    draw_single_vu_bar(painter, bottom_bar_rect, levels.1, is_active);
}
