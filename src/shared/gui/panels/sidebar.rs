use eframe::egui;

use super::super::colors::{
    COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_MAGENTA, COLOR_MUTED, COLOR_SELECTED_BG,
    COLOR_SIDEBAR_BG, COLOR_TAB_ACTIVE,
};
use super::super::types::AppState;

/// Result of a click in the sidebar.
pub enum SidebarAction {
    SelectService(usize),
}

/// Draws the scrollable sidebar containing the services list and the packages
/// section below it.  Returns the action (if any) taken by the user.
pub fn draw_service_list(state: &AppState, ui: &mut egui::Ui) -> Option<SidebarAction> {
    let mut action = None;

    egui::ScrollArea::vertical()
        .id_source("service_scroll")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            // ── Services ──────────────────────────────────────────────────────
            for i in 0..state.services.len() {
                if let Some(a) = draw_service_row(state, ui, i) {
                    action = Some(a);
                }
            }

            // ── Packages & Executables section header ─────────────────────────
            if !state.packages.is_empty() {
                draw_section_header(ui, "Packages & Executables");

                for pkg in &state.packages {
                    draw_package_row(ui, pkg);
                }
            }
        });

    action
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn draw_service_row(state: &AppState, ui: &mut egui::Ui, i: usize) -> Option<SidebarAction> {
    let svc        = &state.services[i];
    let dot_char   = svc.status_dot();
    let dot_color  = svc.status_color();
    let short_name = svc.meta_name.split('/').last().unwrap_or(&svc.meta_name).to_owned();
    let ejected    = svc.ejected;
    let sub        = format!("status: {}", svc.status);
    let selected   = i == state.selected_idx;

    let (row_rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 42.0),
        egui::Sense::click(),
    );

    let bg = if selected {
        COLOR_SELECTED_BG
    } else if row_resp.hovered() {
        egui::Color32::from_rgb(40, 40, 40)
    } else {
        COLOR_SIDEBAR_BG
    };

    let painter = ui.painter();
    painter.rect_filled(row_rect, 0.0, bg);

    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(row_rect.min, egui::vec2(3.0, row_rect.height())),
            0.0,
            COLOR_TAB_ACTIVE,
        );
    }

    let name_color = if selected { egui::Color32::WHITE } else { COLOR_MUTED };
    let dot_pos    = egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0);

    painter.text(dot_pos, egui::Align2::CENTER_CENTER, dot_char,
        egui::FontId::new(11.0, egui::FontFamily::Monospace), dot_color);
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 8.0),
        egui::Align2::LEFT_TOP, &short_name,
        egui::FontId::new(12.0, egui::FontFamily::Monospace), name_color,
    );
    if ejected {
        let ej_x = row_rect.min.x + 24.0 + short_name.len() as f32 * 7.2 + 6.0;
        painter.text(
            egui::pos2(ej_x, row_rect.min.y + 8.0),
            egui::Align2::LEFT_TOP, "[EJECTED]",
            egui::FontId::new(10.0, egui::FontFamily::Monospace), COLOR_MAGENTA,
        );
    }
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 24.0),
        egui::Align2::LEFT_TOP, &sub,
        egui::FontId::new(10.0, egui::FontFamily::Monospace), COLOR_DIM,
    );
    painter.line_segment(
        [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    if row_resp.clicked() && !selected {
        Some(SidebarAction::SelectService(i))
    } else {
        None
    }
}

fn draw_section_header(ui: &mut egui::Ui, label: &str) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 26.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );
    painter.text(
        egui::pos2(rect.min.x + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(10.0, egui::FontFamily::Monospace),
        COLOR_CYAN,
    );
}

fn draw_package_row(ui: &mut egui::Ui, pkg: &super::super::types::Package) {
    let short_name = pkg.identifier.split('/').last().unwrap_or(&pkg.identifier);

    let (row_rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 42.0),
        egui::Sense::hover(),
    );

    let bg = if row_resp.hovered() {
        egui::Color32::from_rgb(35, 35, 35)
    } else {
        COLOR_SIDEBAR_BG
    };

    let painter = ui.painter();
    painter.rect_filled(row_rect, 0.0, bg);

    // Type badge colour: cyan for lib, magenta for executable, dim for others.
    let badge_color = match pkg.package_type.as_str() {
        "lib" | "library" => COLOR_CYAN,
        "bin" | "executable" => COLOR_MAGENTA,
        _ => COLOR_DIM,
    };

    // Icon dot (non-interactive — packages aren't deployed on k8s).
    painter.text(
        egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0),
        egui::Align2::CENTER_CENTER,
        "📦",
        egui::FontId::new(10.0, egui::FontFamily::Monospace),
        COLOR_MUTED,
    );

    // Name + version on top line.
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 8.0),
        egui::Align2::LEFT_TOP,
        short_name,
        egui::FontId::new(12.0, egui::FontFamily::Monospace),
        COLOR_MUTED,
    );

    // Version badge after name.
    let name_w = short_name.len() as f32 * 7.2;
    painter.text(
        egui::pos2(row_rect.min.x + 24.0 + name_w + 6.0, row_rect.min.y + 9.0),
        egui::Align2::LEFT_TOP,
        &format!("v{}", pkg.version),
        egui::FontId::new(9.0, egui::FontFamily::Monospace),
        COLOR_DIM,
    );

    // Type + lang on bottom line.
    let sub = format!("{}  ·  {}", pkg.package_type, pkg.lang);
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 24.0),
        egui::Align2::LEFT_TOP,
        &sub,
        egui::FontId::new(10.0, egui::FontFamily::Monospace),
        badge_color,
    );

    painter.line_segment(
        [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );
}