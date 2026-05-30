use eframe::egui;

use super::super::colors::{COLOR_BORDER, COLOR_MUTED, COLOR_TAB_ACTIVE, COLOR_TAB_BAR, COLOR_TAB_INACTIVE};
use super::super::types::{AppState, RightPane, MAX_TERM_TABS};

pub enum TabBarAction {
    SwitchToLogs,
    SwitchToTerm(usize),
    NewTerm,
    CloseTerm(usize),
}

pub fn draw_tab_bar(state: &AppState, ui: &mut egui::Ui) -> Option<TabBarAction> {
    const TAB_H:       f32 = 28.0;
    const LOGS_W:      f32 = 70.0;
    const TERM_W:      f32 = 150.0;
    const PLUS_W:      f32 = 28.0;
    const CLOSE_W:     f32 = 30.0;
    const LABEL_PAD_L: f32 = 10.0;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TAB_H),
        egui::Sense::hover(),
    );

    let mut action: Option<TabBarAction> = None;
    let mut x = bar_rect.min.x;

    // ── "Logs" tab ────────────────────────────────────────────────────────────
    {
        let tab_rect = egui::Rect::from_min_size(egui::pos2(x, bar_rect.min.y), egui::vec2(LOGS_W, TAB_H));
        let active   = state.right_pane == RightPane::Logs;
        let clicked  = ui.allocate_rect(tab_rect, egui::Sense::click()).clicked();
        if clicked && !active { action = Some(TabBarAction::SwitchToLogs); }

        let painter = ui.painter();
        painter.rect_filled(
            tab_rect, 0.0,
            if active { egui::Color32::from_rgb(28, 28, 28) } else { COLOR_TAB_BAR },
        );
        if active {
            painter.line_segment(
                [tab_rect.left_bottom(), tab_rect.right_bottom()],
                egui::Stroke::new(2.0, COLOR_TAB_ACTIVE),
            );
        }
        painter.text(tab_rect.center(), egui::Align2::CENTER_CENTER, "Logs",
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
            if active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE });
        x += LOGS_W;
    }

    // ── Terminal tabs ─────────────────────────────────────────────────────────
    for (i, tab) in state.term_tabs.iter().enumerate() {
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(x, bar_rect.min.y),
            egui::vec2(TERM_W, TAB_H),
        );
        let active = state.right_pane == RightPane::TerminalTab(i);

        // Close zone allocated first to win hit-test priority.
        let close_zone = egui::Rect::from_min_max(
            egui::pos2(tab_rect.max.x - CLOSE_W, tab_rect.min.y),
            tab_rect.max,
        );
        let label_zone = egui::Rect::from_min_max(
            tab_rect.min,
            egui::pos2(tab_rect.max.x - CLOSE_W, tab_rect.max.y),
        );

        let close_resp = ui.allocate_rect(close_zone, egui::Sense::click());
        let label_resp = ui.allocate_rect(label_zone, egui::Sense::click());

        if close_resp.clicked() {
            action = Some(TabBarAction::CloseTerm(i));
        } else if label_resp.clicked() && !active {
            action = Some(TabBarAction::SwitchToTerm(i));
        }

        let painter = ui.painter();
        painter.rect_filled(
            tab_rect, 0.0,
            if active { egui::Color32::from_rgb(28, 28, 28) } else { COLOR_TAB_BAR },
        );
        if active {
            painter.line_segment(
                [tab_rect.left_bottom(), tab_rect.right_bottom()],
                egui::Stroke::new(2.0, COLOR_TAB_ACTIVE),
            );
        }
        painter.line_segment(
            [close_zone.left_top(), close_zone.left_bottom()],
            egui::Stroke::new(0.5, COLOR_BORDER),
        );

        ui.painter().with_clip_rect(label_zone.shrink2(egui::vec2(0.0, 2.0))).text(
            egui::pos2(tab_rect.min.x + LABEL_PAD_L, tab_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &tab.label,
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            if active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
        );
        painter.text(
            close_zone.center(), egui::Align2::CENTER_CENTER, "×",
            egui::FontId::new(15.0, egui::FontFamily::Monospace),
            if close_resp.hovered() { egui::Color32::WHITE } else { super::super::colors::COLOR_DIM },
        );

        x += TERM_W;
    }

    // ── "+" button (only shown when service has an SSH host and isn't ejected) ─
    let svc_has_host = state.services
        .get(state.selected_idx)
        .map(|s| s.ssh_host.is_some() && !s.ejected)
        .unwrap_or(false);

    if state.term_tabs.len() < MAX_TERM_TABS && svc_has_host {
        let plus_rect = egui::Rect::from_min_size(
            egui::pos2(x, bar_rect.min.y),
            egui::vec2(PLUS_W, TAB_H),
        );
        if ui.allocate_rect(plus_rect, egui::Sense::click()).clicked() {
            action = Some(TabBarAction::NewTerm);
        }
        let painter = ui.painter();
        painter.rect_filled(plus_rect, 0.0, COLOR_TAB_BAR);
        painter.text(plus_rect.center(), egui::Align2::CENTER_CENTER, "+",
            egui::FontId::new(16.0, egui::FontFamily::Monospace), COLOR_MUTED);
        x += PLUS_W;
    }

    // ── Fill remaining bar + bottom border ────────────────────────────────────
    {
        let painter   = ui.painter();
        let remaining = egui::Rect::from_min_max(egui::pos2(x, bar_rect.min.y), bar_rect.max);
        if remaining.width() > 0.0 {
            painter.rect_filled(remaining, 0.0, COLOR_TAB_BAR);
        }
        painter.line_segment(
            [bar_rect.left_bottom(), bar_rect.right_bottom()],
            egui::Stroke::new(0.5, COLOR_BORDER),
        );
    }

    action
}