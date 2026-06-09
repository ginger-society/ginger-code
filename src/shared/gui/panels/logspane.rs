use eframe::egui;

use crate::shared::gui::colors::COLOR_MAGENTA;

use super::super::colors::{COLOR_BORDER, COLOR_MUTED, COLOR_YELLOW};
use super::super::types::AppState;
use super::log_highlight::{highlight_line, LogPalette};

/// Returns Some(Some(name)) to switch to a container, Some(None) to reset to default.
/// Returns None if no chip was clicked.
// logspane.rs — draw_logs_pane no longer handles chips, just logs
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

/// Returns Some(Some(name)) to switch container, Some(None) to reset to default, None if no click.
/// Only renders anything when the service has more than one container.
pub fn draw_container_chips(state: &AppState, ui: &mut egui::Ui) -> Option<String> {
    let svc = state.services.get(state.selected_idx)?;
    if svc.containers.len() <= 1 { return None; }

    let mut clicked: Option<String> = None;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 28.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(bar_rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
    ui.painter().line_segment(
        [bar_rect.left_bottom(), bar_rect.right_bottom()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    let mut child_ui = ui.child_ui(bar_rect, egui::Layout::left_to_right(egui::Align::Center));
    child_ui.add_space(8.0);
    child_ui.label(
        egui::RichText::new("container:")
            .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
            .color(COLOR_MUTED),
    );
    child_ui.add_space(6.0);

    // Just the real container names — no "default" chip
    for name in &svc.containers {
        let is_active  = svc.selected_container.as_deref() == Some(name.as_str());
        let is_ejected = svc.ejected
            && svc.ejected_container.as_deref() == Some(name.as_str());

        if is_ejected {
            draw_ejected_chip(&mut child_ui, name, is_active);
        } else {
            if chip(&mut child_ui, name, is_active).clicked() && !is_active {
                clicked = Some(name.clone());
            }
        }
        child_ui.add_space(4.0);
    }
    clicked
}

fn draw_ejected_chip(ui: &mut egui::Ui, name: &str, is_active: bool) {
    let badge_text  = "⚡ Ejected";
    let font        = egui::FontId::new(10.5, egui::FontFamily::Monospace);
    let badge_w     = badge_text.len() as f32 * 6.2 + 10.0;
    let name_w      = name.len()       as f32 * 6.2 + 10.0;
    let chip_h      = 18.0;
    let total_w     = badge_w + 1.0 + name_w; // 1px divider

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(total_w, chip_h),
        egui::Sense::click(),
    );

    let painter = ui.painter();

    // ── Left half: "⚡ Ejected" in magenta ───────────────────────────────────
    let left = egui::Rect::from_min_size(rect.min, egui::vec2(badge_w, chip_h));
    painter.rect_filled(left, egui::Rounding { nw: 3.0, sw: 3.0, ne: 0.0, se: 0.0 }, COLOR_MAGENTA);
    painter.text(
        left.center(),
        egui::Align2::CENTER_CENTER,
        badge_text,
        font.clone(),
        egui::Color32::WHITE,
    );

    // ── Divider ───────────────────────────────────────────────────────────────
    let divider_x = rect.min.x + badge_w;
    painter.line_segment(
        [
            egui::pos2(divider_x, rect.min.y + 2.0),
            egui::pos2(divider_x, rect.max.y - 2.0),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
    );

    // ── Right half: container name, same style as a normal active chip ────────
    let right = egui::Rect::from_min_size(
        egui::pos2(divider_x + 1.0, rect.min.y),
        egui::vec2(name_w, chip_h),
    );
    painter.rect_filled(
        right,
        egui::Rounding { nw: 0.0, sw: 0.0, ne: 3.0, se: 3.0 },
        if is_active { COLOR_YELLOW } else { egui::Color32::TRANSPARENT },
    );
    // Right half border (top, right, bottom only — left is the divider)
    painter.rect_stroke(
        right,
        egui::Rounding { nw: 0.0, sw: 0.0, ne: 3.0, se: 3.0 },
        egui::Stroke::new(0.5, if is_active { COLOR_YELLOW } else { COLOR_BORDER }),
    );
    painter.text(
        right.center(),
        egui::Align2::CENTER_CENTER,
        name,
        font,
        if is_active { egui::Color32::BLACK } else { COLOR_MUTED },
    );

    // The whole pill is clickable — selects this container
    if response.clicked() && !is_active {
        // Caller checks return — but since we're inside the loop we need
        // to signal the click. Use the same approach: set clicked in outer scope.
        // We handle this by making draw_ejected_chip return bool:
    }
}

// ── Container chip bar ────────────────────────────────────────────────────────


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