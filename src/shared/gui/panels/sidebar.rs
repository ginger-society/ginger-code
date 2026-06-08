use eframe::egui;

use crate::shared;
use crate::shared::gui::colors::COLOR_YELLOW;

use super::super::colors::{
    COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_MAGENTA, COLOR_MUTED, COLOR_SELECTED_BG,
    COLOR_SIDEBAR_BG, COLOR_TAB_ACTIVE,
};
use super::super::types::{AppState, RightPane};

/// Result of a click in the sidebar.
pub enum SidebarAction {
    SelectService(usize),
    SelectPackage(usize),
    SelectDbSchema(usize),
}

/// Draws the scrollable sidebar: services → packages → DB schemas.
/// Returns the action (if any) taken by the user.
pub fn draw_service_list(state: &AppState, ui: &mut egui::Ui) -> Option<SidebarAction> {
    let mut action = None;

    egui::ScrollArea::vertical()
        .id_source("service_scroll")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            // ── Services ──────────────────────────────────────────────────────
            draw_section_header(ui, "Services");
            for i in 0..state.services.len() {
                if let Some(a) = draw_service_row(state, ui, i) {
                    action = Some(a);
                }
            }

            // ── Packages & Executables ────────────────────────────────────────
            if !state.packages.is_empty() {
                draw_section_header(ui, "Packages & Executables");
                for (i, pkg) in state.packages.iter().enumerate() {
                    let selected = matches!(
                        state.right_pane,
                        RightPane::PackageDetail(idx) if idx == i
                    );
                    if let Some(a) = draw_package_row(ui, pkg, i, selected) {
                        action = Some(a);
                    }
                }
            }

            // ── DB Schemas ────────────────────────────────────────────────────
            if !state.db_schemas.is_empty() {
                draw_section_header(ui, "DB Schemas");
                for (i, schema) in state.db_schemas.iter().enumerate() {
                    let selected = matches!(
                        state.right_pane,
                        RightPane::DbSchemaDetail(idx) if idx == i
                    );
                    if let Some(a) = draw_db_schema_row(ui, schema, i, selected) {
                        action = Some(a);
                    }
                }
            }
        });

    action
}

// ── Service row ───────────────────────────────────────────────────────────────

fn draw_service_row(state: &AppState, ui: &mut egui::Ui, i: usize) -> Option<SidebarAction> {
    let svc        = &state.services[i];
    let dot_char   = svc.status_dot();
    let dot_color  = svc.status_color();
    let short_name = svc.meta_name.split('/').last().unwrap_or(&svc.meta_name).to_owned();
    let ejected    = svc.ejected;
    let sub        = format!("status: {}", svc.status);
    let selected   = i == state.selected_idx
        && !matches!(state.right_pane, RightPane::PackageDetail(_))
        && !matches!(state.right_pane, RightPane::DbSchemaDetail(_));

    let (row_rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 42.0),
        egui::Sense::click(),
    );

    let bg = if selected        { COLOR_SELECTED_BG }
             else if row_resp.hovered() { egui::Color32::from_rgb(40, 40, 40) }
             else               { COLOR_SIDEBAR_BG };

    let painter = ui.painter();
    painter.rect_filled(row_rect, 0.0, bg);

    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(row_rect.min, egui::vec2(3.0, row_rect.height())),
            0.0, COLOR_TAB_ACTIVE,
        );
    }

    let name_color = if selected { egui::Color32::WHITE } else { COLOR_MUTED };
    painter.text(
        egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0),
        egui::Align2::CENTER_CENTER, dot_char,
        egui::FontId::new(11.0, egui::FontFamily::Monospace), dot_color,
    );
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

// ── Package row ───────────────────────────────────────────────────────────────

fn draw_package_row(
    ui:       &mut egui::Ui,
    pkg:      &shared::core::types::Package,
    idx:      usize,
    selected: bool,
) -> Option<SidebarAction> {
    let short_name = pkg.identifier.split('/').last().unwrap_or(&pkg.identifier);

    let (row_rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 42.0),
        egui::Sense::click(),
    );

    let bg = if selected              { COLOR_SELECTED_BG }
             else if row_resp.hovered() { egui::Color32::from_rgb(35, 35, 35) }
             else                     { COLOR_SIDEBAR_BG };

    let painter = ui.painter();
    painter.rect_filled(row_rect, 0.0, bg);

    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(row_rect.min, egui::vec2(3.0, row_rect.height())),
            0.0, COLOR_TAB_ACTIVE,
        );
    }

    let badge_color = match pkg.package_type.as_str() {
        "lib" | "library"    => COLOR_CYAN,
        "bin" | "executable" => COLOR_MAGENTA,
        _                    => COLOR_DIM,
    };
    let name_color = if selected { egui::Color32::WHITE } else { COLOR_MUTED };

    // Mounted indicator dot
    let dot       = if pkg.mounted { "●" } else { "○" };
    let dot_color = if pkg.mounted {
        egui::Color32::from_rgb(39, 201, 63)
    } else {
        COLOR_DIM
    };
    painter.text(
        egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0),
        egui::Align2::CENTER_CENTER, dot,
        egui::FontId::new(11.0, egui::FontFamily::Monospace), dot_color,
    );

    // Name
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 8.0),
        egui::Align2::LEFT_TOP, short_name,
        egui::FontId::new(12.0, egui::FontFamily::Monospace), name_color,
    );

    // [MOUNTED] tag
    if pkg.mounted {
        let tag_x = row_rect.min.x + 24.0 + short_name.len() as f32 * 7.2 + 6.0;
        painter.text(
            egui::pos2(tag_x, row_rect.min.y + 8.0),
            egui::Align2::LEFT_TOP, "[MOUNTED]",
            egui::FontId::new(10.0, egui::FontFamily::Monospace),
            egui::Color32::from_rgb(39, 201, 63),
        );
    }

    // Type · lang
    let sub = format!("{}  ·  {}", pkg.package_type, pkg.lang);
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 24.0),
        egui::Align2::LEFT_TOP, &sub,
        egui::FontId::new(10.0, egui::FontFamily::Monospace), badge_color,
    );

    painter.line_segment(
        [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    if row_resp.clicked() {
        Some(SidebarAction::SelectPackage(idx))
    } else {
        None
    }
}

// ── DB Schema row ─────────────────────────────────────────────────────────────

fn draw_db_schema_row(
    ui:       &mut egui::Ui,
    schema:   &shared::core::types::DbSchema,
    idx:      usize,
    selected: bool,
) -> Option<SidebarAction> {
    let (row_rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 42.0),
        egui::Sense::click(),
    );

    let bg = if selected              { COLOR_SELECTED_BG }
             else if row_resp.hovered() { egui::Color32::from_rgb(35, 35, 35) }
             else                     { COLOR_SIDEBAR_BG };

    let painter   = ui.painter();
    painter.rect_filled(row_rect, 0.0, bg);

    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(row_rect.min, egui::vec2(3.0, row_rect.height())),
            0.0, COLOR_TAB_ACTIVE,
        );
    }

    let name_color = if selected { egui::Color32::WHITE } else { COLOR_MUTED };
    let db_type    = schema.db_type.as_deref().unwrap_or("db");

    // Database icon (cylinder-ish)
    painter.text(
        egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0),
        egui::Align2::CENTER_CENTER, "⬡",
        egui::FontId::new(11.0, egui::FontFamily::Monospace), COLOR_CYAN,
    );

    // Name
    painter.text(
        egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 8.0),
        egui::Align2::LEFT_TOP, &schema.name,
        egui::FontId::new(12.0, egui::FontFamily::Monospace), name_color,
    );

    // Sub-line: db_type · N tables
    let sub = format!("{}  ·  {}", db_type, schema.k8s_status);

    let dot = match schema.k8s_status.as_str() {
        "Running"      => "●",
        "Degraded"     => "◐",
        "Pending"      => "○",
        "Not deployed" => "·",
        _              => "✗",
    };
    let dot_color = match schema.k8s_status.as_str() {
        "Running"  => egui::Color32::from_rgb(39, 201, 63),
        "Degraded" | "Pending" => COLOR_YELLOW,
        _          => COLOR_DIM,
    };

    painter.text(
        egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0),
        egui::Align2::CENTER_CENTER, dot,
        egui::FontId::new(11.0, egui::FontFamily::Monospace), dot_color,
    );

    painter.line_segment(
        [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    if row_resp.clicked() {
        Some(SidebarAction::SelectDbSchema(idx))
    } else {
        None
    }
}

// ── Section header ────────────────────────────────────────────────────────────

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
        egui::Align2::LEFT_CENTER, label,
        egui::FontId::new(10.0, egui::FontFamily::Monospace), COLOR_CYAN,
    );
}