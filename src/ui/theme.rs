use super::MultiRecordingApp;
use eframe::egui::{self, Color32, Rounding, Stroke};

impl MultiRecordingApp {
    pub(crate) fn apply_flat_theme(ctx: &egui::Context) {
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
}
