use eframe::egui;

use super::super::types::AppState;
use super::log_highlight::{highlight_line, LogPalette};

pub fn draw_logs_pane(state: &AppState, ui: &mut egui::Ui) {
    let Some(_svc) = state.services.get(state.selected_idx) else { return; };

    let font_size = state.font_size;
    let palette   = LogPalette::from_monokai();

    egui::ScrollArea::vertical()
        .id_source("logs_scroll")
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for line in &state.logs {
                let spans = highlight_line(line, &palette);
                let mut job = egui::text::LayoutJob::default();
                for span in spans {
                    job.append(
                        &span.text,
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::new(font_size, egui::FontFamily::Monospace),
                            color:   span.color,
                            ..Default::default()
                        },
                    );
                }
                ui.label(job);
            }
        });
}