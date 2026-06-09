use eframe::egui;

use super::super::colors::{COLOR_BORDER, COLOR_MUTED, COLOR_YELLOW};
use super::super::types::AppState;
use super::log_highlight::{highlight_line, LogPalette};

/// Returns Some(Some(name)) to switch to a container, Some(None) to reset to default.
/// Returns None if no chip was clicked.
pub fn draw_logs_pane(state: &AppState, ui: &mut egui::Ui) -> Option<Option<String>> {
    let Some(svc) = state.services.get(state.selected_idx) else { return None; };

    let mut action = None;

    // ── Container selector (only when multiple containers exist) ──────────────
    if svc.containers.len() > 1 {
        action = draw_container_chips(&svc.containers, svc.selected_container.as_deref(), ui);
    }

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

    action
}

// ── Container chip bar ────────────────────────────────────────────────────────

fn draw_container_chips(
    containers:         &[String],
    selected_container: Option<&str>,
    ui:                 &mut egui::Ui,
) -> Option<Option<String>> {
    let mut clicked = None;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 26.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        bar_rect,
        0.0,
        egui::Color32::from_rgb(22, 22, 22),
    );
    ui.painter().line_segment(
        [bar_rect.left_bottom(), bar_rect.right_bottom()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("container:")
                .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                .color(COLOR_MUTED),
        );
        ui.add_space(6.0);

        // "default" chip — clears the override
        let default_active = selected_container.is_none();
        if chip(ui, "default", default_active).clicked() && !default_active {
            clicked = Some(None);
        }

        for name in containers {
            let is_active = selected_container == Some(name.as_str());
            if chip(ui, name, is_active).clicked() && !is_active {
                clicked = Some(Some(name.clone()));
            }
        }
    });

    clicked
}

fn chip(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let text = egui::RichText::new(label)
        .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
        .color(if active { egui::Color32::BLACK } else { COLOR_MUTED });
    ui.add(
        egui::Button::new(text)
            .fill(if active { COLOR_YELLOW } else { egui::Color32::TRANSPARENT })
            .stroke(egui::Stroke::new(
                0.5,
                if active { COLOR_YELLOW } else { COLOR_BORDER },
            ))
            .rounding(3.0),
    )
}