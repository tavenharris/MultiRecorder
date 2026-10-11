use super::MultiRecordingApp;
use eframe::egui::{self, Color32, Margin, RichText, Rounding, Stroke};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

impl MultiRecordingApp {
    pub(crate) fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let mut is_open = self.show_settings;
        let window_frame = egui::Frame::window(&ctx.style())
            .fill(Color32::from_rgb(22, 22, 27))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(48, 48, 58)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(14.0));

        egui::Window::new(
            RichText::new("⚙ Preferences & Recording Settings")
                .strong()
                .color(Color32::from_rgb(244, 244, 245)),
        )
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
                    ui.label(
                        RichText::new("📁 Output Format:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
                    for &(fmt, label) in &[
                        ("mp4", "MP4 (.mp4)"),
                        ("mov", "MOV (.mov)"),
                        ("mkv", "MKV (.mkv)"),
                    ] {
                        let is_cur = self.config.output_format.eq_ignore_ascii_case(fmt);
                        let btn_color = if is_cur {
                            Color32::from_rgb(99, 102, 241)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label).color(Color32::WHITE).size(11.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                        {
                            self.config.output_format = fmt.to_string();
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }

                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("Folder:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
                    let mut dir_str = self.config.output_dir.to_string_lossy().to_string();
                    if ui
                        .add(egui::TextEdit::singleline(&mut dir_str).desired_width(180.0))
                        .changed()
                    {
                        self.config.output_dir = PathBuf::from(dir_str);
                        let _ = self.config.save_to_file(&self.config_path);
                    }

                    if ui
                        .add(egui::Button::new(RichText::new("📂 Browse...").size(11.0)))
                        .clicked()
                    {
                        if let Some(p) = crate::platform::pick_folder() {
                            self.config.output_dir = p;
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }

                    if ui
                        .add(egui::Button::new(RichText::new("Reveal").size(11.0)))
                        .clicked()
                    {
                        let _ = fs::create_dir_all(&self.config.output_dir);
                        crate::platform::reveal_path(&self.config.output_dir);
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // --- Row 2: Video Encoder & Bitrates ---
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("🎬 Video Encoder:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
                    let current_enc = self
                        .config
                        .sources
                        .first()
                        .map(|s| s.encoder.clone())
                        .unwrap_or_else(|| {
                            crate::platform::CurrentPlatform
                                .default_video_encoder()
                                .to_string()
                        });
                    for &(enc, label) in crate::platform::CurrentPlatform.available_encoders() {
                        let is_cur = current_enc == enc;
                        let btn_color = if is_cur {
                            Color32::from_rgb(99, 102, 241)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label).color(Color32::WHITE).size(11.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                        {
                            for src in &mut self.config.sources {
                                src.encoder = enc.to_string();
                            }
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }

                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("Segment:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
                    for &(dur, label) in &[(2, "2s (Fast)"), (3, "3s"), (5, "5s")] {
                        let is_cur = self.config.segment_duration_secs == dur;
                        let btn_color = if is_cur {
                            Color32::from_rgb(99, 102, 241)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label).color(Color32::WHITE).size(11.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                        {
                            self.config.segment_duration_secs = dur;
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }
                });

                ui.add_space(6.0);

                // --- Row 3: Bitrates (Stream 1, Stream 2, Audio) ---
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⚡ Bitrates:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );

                    ui.label(
                        RichText::new("Stream 1:")
                            .size(11.0)
                            .color(Color32::from_rgb(161, 161, 170)),
                    );
                    let cur_b1 = self
                        .config
                        .sources
                        .first()
                        .map(|s| s.video_bitrate.clone())
                        .unwrap_or_else(|| "40M".to_string());
                    for &b in &["20M", "30M", "40M", "60M", "80M"] {
                        let is_cur = cur_b1 == b;
                        let btn_color = if is_cur {
                            Color32::from_rgb(16, 185, 129)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(b).color(Color32::WHITE).size(10.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                        {
                            if let Some(src) = self.config.sources.first_mut() {
                                src.video_bitrate = b.to_string();
                                let _ = self.config.save_to_file(&self.config_path);
                            }
                        }
                    }

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Stream 2:")
                            .size(11.0)
                            .color(Color32::from_rgb(161, 161, 170)),
                    );
                    let cur_b2 = self
                        .config
                        .sources
                        .get(1)
                        .map(|s| s.video_bitrate.clone())
                        .unwrap_or_else(|| "15M".to_string());
                    for &b in &["8M", "12M", "15M", "20M"] {
                        let is_cur = cur_b2 == b;
                        let btn_color = if is_cur {
                            Color32::from_rgb(16, 185, 129)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(b).color(Color32::WHITE).size(10.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                            && self.config.sources.len() > 1
                        {
                            self.config.sources[1].video_bitrate = b.to_string();
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Audio:")
                            .size(11.0)
                            .color(Color32::from_rgb(161, 161, 170)),
                    );
                    for &ab in &["192k", "256k", "320k"] {
                        let is_cur = self.config.audio_bitrate == ab;
                        let btn_color = if is_cur {
                            Color32::from_rgb(16, 185, 129)
                        } else {
                            Color32::from_rgb(45, 45, 52)
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(ab).color(Color32::WHITE).size(10.0),
                                )
                                .fill(btn_color),
                            )
                            .clicked()
                        {
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
                    ui.label(
                        RichText::new("⏱ Flashback Buffer Window:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
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
                        if ui
                            .add(
                                egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                    .fill(btn_color),
                            )
                            .clicked()
                        {
                            self.selected_flashback_secs = secs;
                            self.config.buffer_duration_secs = secs;
                            let _ = self.config.save_to_file(&self.config_path);
                        }
                    }

                    ui.add_space(8.0);
                    let mut mins = (self.selected_flashback_secs / 60).clamp(1, 120);
                    if ui
                        .add(egui::Slider::new(&mut mins, 1..=120).suffix(" min"))
                        .changed()
                    {
                        self.selected_flashback_secs = mins * 60;
                        self.config.buffer_duration_secs = mins * 60;
                        let _ = self.config.save_to_file(&self.config_path);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new("🧹 Clean Buffer Cache")
                                    .fill(Color32::from_rgb(45, 45, 52)),
                            )
                            .clicked()
                        {
                            if let Ok(entries) = fs::read_dir(&self.config.buffer_dir) {
                                for entry in entries.flatten() {
                                    let p = entry.path();
                                    if p.extension().map(|e| e == "mp4").unwrap_or(false) {
                                        let _ = fs::remove_file(p);
                                    }
                                }
                                self.status_message = Some((
                                    "Buffer cache cleaned.".to_string(),
                                    Instant::now(),
                                    false,
                                ));
                            }
                        }
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // --- Row 4b: Quick Clip Intervals Configuration ---
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⚡ Quick Clip Intervals:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );
                    ui.label(
                        RichText::new("(Toggle presets or add custom)")
                            .size(11.0)
                            .color(Color32::from_rgb(156, 163, 175)),
                    );

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

                        if ui
                            .add(
                                egui::Button::new(RichText::new(label).color(text_color).size(10.0))
                                    .fill(btn_color),
                            )
                            .clicked()
                        {
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
                    ui.label(
                        RichText::new("Sec:")
                            .size(11.0)
                            .color(Color32::from_rgb(161, 161, 170)),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.custom_clip_sec_input)
                            .desired_width(40.0),
                    );
                    if ui
                        .add(egui::Button::new(RichText::new("+ Add").size(10.0)))
                        .clicked()
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
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Reset Defaults").size(10.0))
                                    .fill(Color32::from_rgb(45, 45, 52)),
                            )
                            .clicked()
                        {
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
                    ui.label(
                        RichText::new("🔗 NLE Post Sync:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );

                    if ui
                        .checkbox(
                            &mut self.config.sync_audio_to_secondary,
                            "Mirror Track 1 Audio to Video 2 (Waveform Sync)",
                        )
                        .changed()
                    {
                        let _ = self.config.save_to_file(&self.config_path);
                    }

                    ui.add_space(16.0);

                    if ui
                        .checkbox(
                            &mut self.config.embed_timecode,
                            "Embed SMPTE Timecode (Timecode Sync)",
                        )
                        .changed()
                    {
                        let _ = self.config.save_to_file(&self.config_path);
                    }
                });

                // --- Row 6: Recording Engine Performance & Overhead ---
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("🚀 Performance:")
                            .strong()
                            .color(Color32::from_rgb(244, 244, 245)),
                    );

                    if ui
                        .checkbox(
                            &mut self.config.enable_recording_preview,
                            "Live Video Preview in GUI during Recording (uncheck for Ultra Performance)",
                        )
                        .changed()
                    {
                        let _ = self.config.save_to_file(&self.config_path);
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // --- Row 7: Hardware & System Audio Driver ---
                ui.horizontal(|ui| {
                    #[cfg(target_os = "macos")]
                    {
                        if self.has_loopback {
                            ui.label(
                                RichText::new("● BlackHole Loopback Driver: Installed & Active")
                                    .color(Color32::from_rgb(16, 185, 129))
                                    .size(11.0),
                            );
                        } else {
                            ui.label(
                                RichText::new(
                                    "○ BlackHole Loopback: Not installed. Required for Speaker/AirPods recording.",
                                )
                                .color(Color32::from_rgb(245, 158, 11))
                                .size(11.0),
                            );
                            let is_copied = self
                                .copied_brew_cmd_until
                                .map(|t| t.elapsed() < Duration::from_secs(3))
                                .unwrap_or(false);
                            let brew_text = if is_copied {
                                "✔ Copied!"
                            } else {
                                "📋 Copy Brew Install"
                            };
                            if ui
                                .add(egui::Button::new(RichText::new(brew_text).size(10.0)))
                                .clicked()
                            {
                                ui.output_mut(|o| {
                                    o.copied_text = "brew install --cask blackhole-2ch".to_string()
                                });
                                self.copied_brew_cmd_until = Some(Instant::now());
                            }
                            if ui
                                .add(egui::Button::new(
                                    RichText::new("🎛 Audio MIDI Setup").size(10.0),
                                ))
                                .clicked()
                            {
                                crate::platform::open_system_audio_control();
                            }
                        }
                    }

                    #[cfg(target_os = "windows")]
                    {
                        ui.label(
                            RichText::new("● Windows DirectShow & CoreAudio Capture: Active")
                                .color(Color32::from_rgb(16, 185, 129))
                                .size(11.0),
                        );
                        if ui
                            .add(egui::Button::new(
                                RichText::new("🎛 Sound Control Panel").size(10.0),
                            ))
                            .clicked()
                        {
                            crate::platform::open_system_audio_control();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("🔄 Refresh Hardware Devices"))
                            .clicked()
                        {
                            self.refresh_devices();
                        }
                    });
                });

                if !self.recent_exports.is_empty() {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Recent Saved Flashbacks:")
                            .size(11.0)
                            .color(Color32::from_rgb(161, 161, 170)),
                    );
                    for exp in &self.recent_exports {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("⚡ {}", exp.timestamp))
                                    .size(11.0)
                                    .strong()
                                    .color(Color32::from_rgb(244, 244, 245)),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "({}s / {:.1} MB)",
                                    exp.duration_secs, exp.size_mb
                                ))
                                .size(10.0)
                                .color(Color32::from_rgb(113, 113, 122)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(egui::Button::new(RichText::new("Reveal").size(10.0)))
                                        .clicked()
                                    {
                                        crate::platform::reveal_file(&exp.path);
                                    }
                                },
                            );
                        });
                    }
                }

                if !self.recorders.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for rec in &self.recorders {
                            if ui
                                .add(egui::Button::new(
                                    RichText::new(format!("Open [{}] Log", rec.source_name()))
                                        .size(10.0),
                                ))
                                .clicked()
                            {
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
