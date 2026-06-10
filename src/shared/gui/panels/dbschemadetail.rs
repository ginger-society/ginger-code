use eframe::egui;

use crate::shared::{
    core::types::DbSchema,
    gui::panels::log_highlight::{highlight_line, LogPalette},
};

use super::super::colors::{
    COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_MUTED,
    COLOR_TAB_ACTIVE, COLOR_TAB_BAR, COLOR_TAB_INACTIVE,
};

/// Returns `Some(container_name)` if the user clicked a different container tab.
pub fn draw_db_schema_detail(
    schema:    &DbSchema,
    logs:      Option<&[String]>,
    containers: &[String],
    selected:  Option<&str>,
    ui:        &mut egui::Ui,
) -> Option<String> {
    let mut switched = None;

    ui.vertical(|ui| {
        draw_info_strip(schema, ui);
        if let Some(name) = draw_container_tab_bar(containers, selected, ui) {
            switched = Some(name);
        }
        draw_logs_pane(logs, ui);
    });

    switched
}

// ── Info strip ────────────────────────────────────────────────────────────────

fn draw_info_strip(schema: &DbSchema, ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 90.0),
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

    // Row 1: name + db_type badge
    let name = &schema.name;
    painter.text(
        egui::pos2(rect.min.x + pad, y),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::new(14.0, egui::FontFamily::Monospace),
        egui::Color32::WHITE,
    );
    if let Some(ref db_type) = schema.db_type {
        let name_w = name.len() as f32 * 7.8;
        painter.text(
            egui::pos2(rect.min.x + pad + name_w + 6.0, y + 1.0),
            egui::Align2::LEFT_TOP,
            &format!("[{}]", db_type),
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            COLOR_CYAN,
        );
    }
    y += 20.0;

    // Row 2: identifier · org · tables count
    let id_part  = schema.identifier.as_deref().unwrap_or("—");
    let meta_row = format!(
        "id: {}   org: {}   tables: {}",
        id_part, schema.organization_id, schema.tables.len(),
    );
    painter.text(
        egui::pos2(rect.min.x + pad, y),
        egui::Align2::LEFT_TOP,
        &meta_row,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        COLOR_MUTED,
    );
    y += 18.0;

    // Row 3: description
    if let Some(ref desc) = schema.description {
        if !desc.is_empty() {
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
    y += 18.0;

    // Row 4: k8s status
    let status_color = match schema.k8s_status.as_str() {
        "Running"                => egui::Color32::from_rgb(39, 201, 63),
        "Degraded" | "Pending"   => super::super::colors::COLOR_YELLOW,
        "Not deployed"           => COLOR_DIM,
        _                        => super::super::colors::COLOR_RED,
    };
    let k8s_row = format!(
        "k8s: {}   ready: {}{}",
        schema.k8s_status,
        schema.k8s_ready,
        schema.k8s_name.as_deref()
            .map(|n| format!("   ({})", n))
            .unwrap_or_default(),
    );
    painter.text(
        egui::pos2(rect.min.x + pad, y),
        egui::Align2::LEFT_TOP,
        &k8s_row,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        status_color,
    );
}

// ── Container tab bar ─────────────────────────────────────────────────────────

/// Draws a tab bar for container selection. Returns the name of a newly
/// selected container if the user clicked a different tab, else `None`.
fn draw_container_tab_bar(
    containers: &[String],
    selected:   Option<&str>,
    ui:         &mut egui::Ui,
) -> Option<String> {
    // Nothing to show until containers are known or if there's only one
    if containers.len() <= 1 {
        return None;
    }

    const TAB_H:  f32 = 26.0;
    const PAD:    f32 = 10.0;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TAB_H),
        egui::Sense::hover(),
    );

    // ── Allocate all interaction zones first ──────────────────────────────────
    struct TabResult {
        rect:    egui::Rect,
        active:  bool,
        clicked: bool,
        label:   String,
    }

    let mut tab_results: Vec<TabResult> = Vec::new();
    let mut x = bar_rect.min.x;
    let mut action: Option<String> = None;

    for name in containers {
        let active  = selected == Some(name.as_str());
        let tab_w   = (name.len() as f32 * 7.0 + PAD * 2.0).max(70.0);
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(x, bar_rect.min.y),
            egui::vec2(tab_w, TAB_H),
        );
        let resp    = ui.allocate_rect(tab_rect, egui::Sense::click());
        let clicked = resp.clicked();

        if clicked && !active {
            action = Some(name.clone());
        }

        tab_results.push(TabResult {
            rect: tab_rect,
            active,
            clicked,
            label: name.clone(),
        });
        x += tab_w;
    }

    // ── Paint ─────────────────────────────────────────────────────────────────
    let painter = ui.painter();
    painter.rect_filled(bar_rect, 0.0, COLOR_TAB_BAR);

    for tr in &tab_results {
        // Tab background
        painter.rect_filled(
            tr.rect,
            0.0,
            if tr.active {
                egui::Color32::from_rgb(28, 28, 28)
            } else {
                COLOR_TAB_BAR
            },
        );
        // Active underline
        if tr.active {
            painter.line_segment(
                [tr.rect.left_bottom(), tr.rect.right_bottom()],
                egui::Stroke::new(2.0, COLOR_TAB_ACTIVE),
            );
        }
        // Label
        painter.text(
            egui::pos2(tr.rect.min.x + PAD, tr.rect.center().y),
            egui::Align2::LEFT_CENTER,
            &tr.label,
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            if tr.active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
        );
    }

    // Fill remainder + bottom border
    let remaining = egui::Rect::from_min_max(egui::pos2(x, bar_rect.min.y), bar_rect.max);
    if remaining.width() > 0.0 {
        painter.rect_filled(remaining, 0.0, COLOR_TAB_BAR);
    }
    painter.line_segment(
        [bar_rect.left_bottom(), bar_rect.right_bottom()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    action
}

// ── Logs pane ─────────────────────────────────────────────────────────────────

fn draw_logs_pane(logs: Option<&[String]>, ui: &mut egui::Ui) {
    match logs {
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

        Some(lines) if lines.is_empty() => {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(
                        "○  No deployment found in the default namespace for this schema.",
                    )
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

        Some(lines) => {
            let palette = LogPalette::from_monokai();
            egui::ScrollArea::vertical()
                .id_source("db_logs_scroll")
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for line in lines {
                        let spans = highlight_line(line, &palette);
                        let mut job = egui::text::LayoutJob::default();
                        for span in spans {
                            job.append(
                                &span.text,
                                0.0,
                                egui::TextFormat {
                                    font_id: egui::FontId::new(
                                        12.0,
                                        egui::FontFamily::Monospace,
                                    ),
                                    color: span.color,
                                    ..Default::default()
                                },
                            );
                        }
                        ui.label(job);
                    }
                });
        }
    }
}