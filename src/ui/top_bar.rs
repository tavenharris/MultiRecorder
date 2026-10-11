use super::{MultiRecordingApp, RecordingMode};
use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Rounding, Stroke, Vec2};
use std::time::Duration;

impl MultiRecordingApp {
    pub(crate) fn render_top_bar(&mut self, ui: &mut egui::Ui) {
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
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("■ Stop Recording [R]")
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(225, 29, 72))
                                    .min_size(Vec2::new(140.0, 32.0)),
                                )
                                .on_hover_text(
                                    "Stop and finalize continuous recording to output folder (Hotkey: R)",
                                )
                                .clicked()
                            {
                                self.stop_continuous_recording();
                            }
                        }
                        Some(RecordingMode::Flashback) => {
                            // In Flashback mode: Stop Flashback button
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("■ Stop Flashback")
                                            .color(Color32::from_rgb(244, 63, 94))
                                            .strong(),
                                    )
                                    .min_size(Vec2::new(125.0, 32.0)),
                                )
                                .on_hover_text("Stop flashback buffer and clear temporary cache")
                                .clicked()
                            {
                                self.stop_engine();
                            }
                        }
                        None => {
                            // Idle state: Both '● Record' and '▶ Start Flashback' are available
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("● Record [R]")
                                            .color(Color32::from_rgb(244, 63, 94))
                                            .strong(),
                                    )
                                    .min_size(Vec2::new(95.0, 32.0)),
                                )
                                .on_hover_text("Start continuous forward recording (Hotkey: R)")
                                .clicked()
                            {
                                self.start_engine(RecordingMode::Continuous);
                            }

                            ui.add_space(4.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("▶ Start Flashback")
                                            .color(Color32::from_rgb(16, 185, 129))
                                            .strong(),
                                    )
                                    .min_size(Vec2::new(125.0, 32.0)),
                                )
                                .on_hover_text("Start rolling ring buffer for flashback clip exports")
                                .clicked()
                            {
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
                            resp = resp.on_hover_text(format!(
                                "⚡ Save last {} clip{}",
                                label, hotkey_desc
                            ));
                            if resp.clicked() {
                                self.selected_flashback_secs = secs;
                                self.trigger_flashback(secs);
                            }
                        } else if self.recording_mode == Some(RecordingMode::Continuous) {
                            resp = resp.on_disabled_hover_text(
                                "Continuous recording is saving everything forward",
                            );
                        } else {
                            resp = resp.on_disabled_hover_text(
                                "▶ Start Flashback to begin buffering clips",
                            );
                        }
                        ui.add_space(2.0);
                    }

                    // Right: Metrics, Live Timer & Settings Button
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Settings Button
                        let settings_color = if self.show_settings {
                            Color32::from_rgb(99, 102, 241)
                        } else {
                            Color32::from_rgb(161, 161, 170)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("⚙ settings")
                                        .color(settings_color)
                                        .size(12.0)
                                        .strong(),
                                )
                                .min_size(Vec2::new(80.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.show_settings = !self.show_settings;
                        }

                        ui.add_space(16.0);

                        // Live Timer (counts up during recording; 0:00s when idle)
                        let format_dur = |secs: u64| {
                            if secs >= 3600 {
                                format!(
                                    "{}:{:02}:{:02}",
                                    secs / 3600,
                                    (secs % 3600) / 60,
                                    secs % 60
                                )
                            } else {
                                format!("{}:{:02}s", secs / 60, secs % 60)
                            }
                        };
                        let timer_text = if self.is_recording {
                            let elapsed = self
                                .recording_started_at
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            format_dur(elapsed)
                        } else {
                            "0:00s".to_string()
                        };
                        ui.label(
                            RichText::new(timer_text)
                                .font(FontId::new(16.0, FontFamily::Monospace))
                                .strong()
                                .color(Color32::from_rgb(244, 244, 245)),
                        );

                        ui.add_space(18.0);

                        // Metrics: 0 Mb/s (0 MB) when idle, active write rate and disk buffer size when recording
                        let total_size_mb: f64 = if self.is_recording {
                            self.managers
                                .iter()
                                .filter_map(|m| m.get_stats().ok())
                                .map(|s| s.total_size_mb)
                                .sum()
                        } else {
                            0.0
                        };
                        let size_str = if total_size_mb >= 1024.0 {
                            format!("({:.1} GB)", total_size_mb / 1024.0)
                        } else {
                            format!("({:.0} MB)", total_size_mb.max(0.0))
                        };
                        let bitrate_str = if self.is_recording {
                            let total_mbps: u64 = self
                                .config
                                .sources
                                .iter()
                                .filter_map(|s| {
                                    s.video_bitrate
                                        .trim_end_matches(['M', 'm'])
                                        .parse::<u64>()
                                        .ok()
                                })
                                .sum();
                            if total_mbps > 0 {
                                format!("{} Mb/s", total_mbps)
                            } else {
                                "40 Mb/s".to_string()
                            }
                        } else {
                            "0 Mb/s".to_string()
                        };
                        ui.label(
                            RichText::new(format!("{}  {}", bitrate_str, size_str))
                                .font(FontId::new(15.0, FontFamily::Proportional))
                                .color(Color32::from_rgb(212, 212, 216)),
                        );
                    });
                });
            });
    }

    pub(crate) fn render_status_banner(&mut self, ui: &mut egui::Ui) {
        if let Some((msg, time, is_error)) = &self.status_message {
            if time.elapsed() < Duration::from_secs(8) {
                ui.add_space(4.0);
                let (bg, border, text_color) = if *is_error {
                    (
                        Color32::from_rgb(69, 10, 10),
                        Color32::from_rgb(185, 28, 28),
                        Color32::from_rgb(254, 202, 202),
                    )
                } else {
                    (
                        Color32::from_rgb(6, 78, 59),
                        Color32::from_rgb(5, 150, 105),
                        Color32::from_rgb(209, 250, 229),
                    )
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
    }
}
