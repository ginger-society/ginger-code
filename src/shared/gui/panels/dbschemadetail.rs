//! Detail panel for a DB schema: info strip at the top, log pane below.

use eframe::egui;

use crate::shared::core::types::DbSchema;

use super::super::colors::{
    COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_FG, COLOR_MUTED, COLOR_RED, COLOR_YELLOW,
};

// ── Main entry point ──────────────────────────────────────────────────────────

/// Draw the DB schema detail view.
///
/// * `schema`  — the selected schema metadata
/// * `logs`    — `None`  → still loading
///               `Some([])`  → no deployment found in k8s
///               `Some([…])` → live log lines
pub fn draw_db_schema_detail(
    schema: &DbSchema,
    logs:   Option<&[String]>,
    ui:     &mut egui::Ui,
) {
    ui.vertical(|ui| {
        draw_info_strip(schema, ui);
        ui.add_space(0.0); // separator handled inside strip
        draw_logs_pane(logs, ui);
    });
}

// ── Info strip ────────────────────────────────────────────────────────────────

fn draw_info_strip(schema: &DbSchema, ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 72.0),
        egui::Sense::hover(),
    );

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
    painter.line_segment(
        [
            egui::pos2(rect.min.x, rect.max.y - 0.5),
            egui::pos2(rect.max.x, rect.max.y - 0.5),
        ],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    let pad  = 12.0;
    let mut y = rect.min.y + 10.0;

    // ── Row 1: name + db_type badge ───────────────────────────────────────────
    let name = &schema.name;
    painter.text(
        egui::pos2(rect.min.x + pad, y),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::new(14.0, egui::FontFamily::Monospace),
        egui::Color32::WHITE,
    );

    if let Some(ref db_type) = schema.db_type {
        let name_w = name.len() as f32 * 8.5;
        painter.text(
            egui::pos2(rect.min.x + pad + name_w + 8.0, y + 1.0),
            egui::Align2::LEFT_TOP,
            &format!("[{}]", db_type),
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            COLOR_CYAN,
        );
    }

    y += 20.0;

    // ── Row 2: identifier · org · tables count ────────────────────────────────
    let id_part  = schema.identifier.as_deref().unwrap_or("—");
    let meta_row = format!(
        "id: {}   org: {}   tables: {}",
        id_part,
        schema.organization_id,
        schema.tables.len(),
    );
    painter.text(
        egui::pos2(rect.min.x + pad, y),
        egui::Align2::LEFT_TOP,
        &meta_row,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        COLOR_MUTED,
    );

    y += 18.0;

    // ── Row 3: description (truncated to one line) ────────────────────────────
    if let Some(ref desc) = schema.description {
        if !desc.is_empty() {
            // Truncate to available width
            let max_chars = ((rect.width() - pad * 2.0) / 6.5) as usize;
            let display   = if desc.len() > max_chars {
                format!("{}…", &desc[..max_chars.saturating_sub(1)])
            } else {
                desc.clone()
            };
            painter.text(
                egui::pos2(rect.min.x + pad, y),
                egui::Align2::LEFT_TOP,
                &display,
                egui::FontId::new(10.5, egui::FontFamily::Monospace),
                COLOR_DIM,
            );
        }
    }
}

// ── Logs pane ─────────────────────────────────────────────────────────────────

fn draw_logs_pane(logs: Option<&[String]>, ui: &mut egui::Ui) {
    match logs {
        // Still loading — spinner message
        None => {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let t    = ui.input(|i| i.time);
                let dots = match ((t * 2.0) as usize) % 4 { 0=>"", 1=>".", 2=>"..", _=>"..." };
                ui.label(
                    egui::RichText::new(format!("Looking for deployment{}", dots))
                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                        .color(COLOR_CYAN),
                );
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            });
        }

        // No matching deployment found
        Some([]) => {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("○  No deployment found in the default namespace for this schema.")
                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                        .color(COLOR_DIM),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(
                        "Expected a deployment whose name matches the schema identifier.",
                    )
                    .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                    .color(COLOR_DIM),
                );
            });
        }

        // Live logs
        Some(lines) => {
            egui::ScrollArea::both()
                .id_source("db_logs_scroll")
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for line in lines {
                        let color = if line.contains("ERROR") || line.contains("error") || line.contains("panic") {
                            COLOR_RED
                        } else if line.contains("WARN") || line.contains("warn") {
                            COLOR_YELLOW
                        } else {
                            COLOR_FG
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line)
                                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                    .color(color),
                            )
                            .wrap(false),
                        );
                    }
                });
        }
    }
}