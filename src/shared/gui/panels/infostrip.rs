use eframe::egui;

use super::super::colors::{COLOR_BORDER, COLOR_CYAN, COLOR_MUTED};
use super::super::types::AppState;

pub struct InfoStripAction {
    pub eject_clicked:       bool,
    pub uneject_clicked:     bool,
    pub open_editor_clicked: bool,
}

pub fn draw_info_strip(
    state:    &AppState,
    ejecting: Option<&str>,
    ui:       &mut egui::Ui,
) -> InfoStripAction {
    let no_action = InfoStripAction {
        eject_clicked:       false,
        uneject_clicked:     false,
        open_editor_clicked: false,
    };

    let Some(svc) = state.services.get(state.selected_idx) else {
        return no_action;
    };

    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 40.0),
        egui::Sense::hover(),
    );

    let pad_y            = 8.0;
    let content_center_y = rect.min.y + pad_y + (rect.height() - pad_y * 2.0) / 2.0;
    let btn_h            = 18.0;
    let btn_y            = content_center_y - btn_h / 2.0;

    // ── In-progress spinner overlay ───────────────────────────────────────────
    if let Some(msg) = ejecting {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
        painter.line_segment(
            [egui::pos2(rect.min.x, rect.max.y - 0.5), egui::pos2(rect.max.x, rect.max.y - 0.5)],
            egui::Stroke::new(0.5, COLOR_BORDER),
        );
        let t    = ui.input(|i| i.time);
        let dots = match ((t * 2.0) as usize) % 4 { 0 => "", 1 => ".", 2 => "..", _ => "..." };
        painter.text(
            egui::pos2(rect.min.x + 8.0, content_center_y),
            egui::Align2::LEFT_CENTER,
            &format!("{}{}", msg, dots),
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
            COLOR_CYAN,
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        return no_action;
    }

    // ── Button layout (right → left) ─────────────────────────────────────────
    // Not ejected:  [ ⏏ Eject ]
    // Ejected:      [ ↩ Un-eject ]  [ [>] Open Editor ]

    let eject_btn_rect: Option<egui::Rect> = if !svc.ejected {
        let w = 58.0;
        Some(egui::Rect::from_min_size(
            egui::pos2(rect.max.x - w - 8.0, btn_y),
            egui::vec2(w, btn_h),
        ))
    } else {
        None
    };

    let editor_btn_rect: Option<egui::Rect> = if svc.ejected {
        let w = 100.0;
        Some(egui::Rect::from_min_size(
            egui::pos2(rect.max.x - w - 8.0, btn_y),
            egui::vec2(w, btn_h),
        ))
    } else {
        None
    };

    let uneject_btn_rect: Option<egui::Rect> = if svc.ejected {
        let w        = 82.0;
        let editor_x = editor_btn_rect.unwrap().min.x;
        Some(egui::Rect::from_min_size(
            egui::pos2(editor_x - w - 6.0, btn_y),
            egui::vec2(w, btn_h),
        ))
    } else {
        None
    };

    // Allocate interaction zones before painting.
    let eject_resp   = eject_btn_rect.map(|r|   ui.allocate_rect(r, egui::Sense::click()));
    let editor_resp  = editor_btn_rect.map(|r|  ui.allocate_rect(r, egui::Sense::click()));
    let uneject_resp = uneject_btn_rect.map(|r| ui.allocate_rect(r, egui::Sense::click()));

    let eject_clicked       = eject_resp.as_ref().map_or(false,   |r| r.clicked());
    let open_editor_clicked = editor_resp.as_ref().map_or(false,  |r| r.clicked());
    let uneject_clicked     = uneject_resp.as_ref().map_or(false, |r| r.clicked());

    // ── Paint background + text ───────────────────────────────────────────────
    let deploy = svc.deployment_name.as_deref().unwrap_or("—");
    let lang   = svc.lang.as_deref().unwrap_or("—");
    let detail = format!(
        "   lang: {}   status: {}{}",
        lang, svc.status,
        if svc.ejected { "   [EJECTED]" } else { "" },
    );

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
    painter.line_segment(
        [egui::pos2(rect.min.x, rect.max.y - 0.5), egui::pos2(rect.max.x, rect.max.y - 0.5)],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    let deploy_x = rect.min.x + 8.0;
    painter.text(
        egui::pos2(deploy_x, content_center_y),
        egui::Align2::LEFT_CENTER,
        deploy,
        egui::FontId::new(13.0, egui::FontFamily::Monospace),
        egui::Color32::WHITE,
    );
    let deploy_w = deploy.len() as f32 * 7.8;
    painter.text(
        egui::pos2(deploy_x + deploy_w, content_center_y),
        egui::Align2::LEFT_CENTER,
        &detail,
        egui::FontId::new(10.5, egui::FontFamily::Monospace),
        COLOR_MUTED,
    );

    // ── Eject button ──────────────────────────────────────────────────────────
    if let (Some(r), Some(resp)) = (eject_btn_rect, &eject_resp) {
        let color = if resp.hovered() {
            egui::Color32::from_rgb(220, 80, 40)
        } else {
            egui::Color32::from_rgb(160, 60, 30)
        };
        painter.rect_filled(r, 3.0, color);
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "⏏ Eject",
            egui::FontId::new(10.0, egui::FontFamily::Monospace), egui::Color32::WHITE);
    }

    // ── Un-eject button ───────────────────────────────────────────────────────
    if let (Some(r), Some(resp)) = (uneject_btn_rect, &uneject_resp) {
        let color = if resp.hovered() {
            egui::Color32::from_rgb(180, 120, 20)
        } else {
            egui::Color32::from_rgb(130, 85, 10)
        };
        painter.rect_filled(r, 3.0, color);
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "↩ Un-eject",
            egui::FontId::new(10.0, egui::FontFamily::Monospace), egui::Color32::WHITE);
    }

    // ── Open Editor button ────────────────────────────────────────────────────
    if let (Some(r), Some(resp)) = (editor_btn_rect, &editor_resp) {
        let color = if resp.hovered() {
            egui::Color32::from_rgb(30, 100, 180)
        } else {
            egui::Color32::from_rgb(20, 75, 140)
        };
        painter.rect_filled(r, 3.0, color);
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "[>] Open Editor",
            egui::FontId::new(10.0, egui::FontFamily::Monospace), egui::Color32::WHITE);
    }

    InfoStripAction { eject_clicked, uneject_clicked, open_editor_clicked }
}