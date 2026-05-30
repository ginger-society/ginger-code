use eframe::egui;

use super::super::colors::{COLOR_DIM, COLOR_FG, COLOR_RED, COLOR_YELLOW};
use super::super::types::AppState;

pub fn draw_logs_pane(state: &AppState, ui: &mut egui::Ui) {
    let Some(_svc) = state.services.get(state.selected_idx) else { return; };

    let font_size = state.font_size;
    egui::ScrollArea::both()
        .id_source("logs_scroll")
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for line in &state.logs {
                let color = if line.starts_with("ERROR")
                    || line.contains("error")
                    || line.contains("panic")
                {
                    COLOR_RED
                } else if line.contains("WARN") || line.contains("warn") {
                    COLOR_YELLOW
                } else if line.contains("Fetching") {
                    COLOR_DIM
                } else {
                    COLOR_FG
                };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                            .color(color),
                    )
                    .wrap(false),
                );
            }
        });
}