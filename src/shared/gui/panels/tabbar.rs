use eframe::egui;

use super::super::colors::{
    COLOR_BORDER, COLOR_MAGENTA, COLOR_MUTED, COLOR_TAB_ACTIVE,
    COLOR_TAB_BAR, COLOR_TAB_INACTIVE,
};
use super::super::types::{AppState, RightPane, MAX_TERM_TABS};

pub enum TabBarAction {
    SelectContainer(String),
    OpenTermForContainer(String),
    SwitchToTerm(usize),
    CloseTerm(usize),
}

pub fn draw_tab_bar(state: &AppState, ui: &mut egui::Ui) -> Option<TabBarAction> {
    const TAB_H:   f32 = 28.0;
    const PLUS_W:  f32 = 26.0;
    const CLOSE_W: f32 = 28.0;
    const PAD:     f32 = 10.0;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TAB_H),
        egui::Sense::hover(),
    );

    let mut action: Option<TabBarAction> = None;
    let mut x = bar_rect.min.x;

    // ── Intermediate results collected before painting ────────────────────────
    // egui's painter holds an immutable borrow of `ui`, so all `allocate_rect`
    // (mutable) calls must complete before we call `ui.painter()`.

    struct ContainerTabResult {
        tab_rect:      egui::Rect,
        plus_zone:     egui::Rect,
        plus_hovered:  bool,
        can_open_term: bool,
        active:        bool,
        is_ejected:    bool,
        label:         String,
    }

    struct TermTabResult {
        tab_rect:      egui::Rect,
        close_zone:    egui::Rect,
        label_zone:    egui::Rect,
        close_hovered: bool,
        active:        bool,
        label:         String,
    }

    let svc = state.services.get(state.selected_idx);

    // ── Container tabs ────────────────────────────────────────────────────────
    let mut container_tabs: Vec<ContainerTabResult> = Vec::new();
    let mut show_fallback = false;

    if let Some(svc) = svc {
        if svc.containers.is_empty() {
            show_fallback = true;
            let tab_w    = 80.0_f32;
            let tab_rect = egui::Rect::from_min_size(
                egui::pos2(x, bar_rect.min.y),
                egui::vec2(tab_w, TAB_H),
            );
            let resp   = ui.allocate_rect(tab_rect, egui::Sense::click());
            let active = matches!(state.right_pane, RightPane::Logs);
            if resp.clicked() && !active {
                action = Some(TabBarAction::SelectContainer(String::new()));
            }
            container_tabs.push(ContainerTabResult {
                tab_rect,
                plus_zone:     egui::Rect::NOTHING,
                plus_hovered:  false,
                can_open_term: false,
                active,
                is_ejected:    false,
                label:         "Logs".to_string(),
            });
            x += tab_w;
        } else {
            for container_name in &svc.containers {
                let is_selected_log = matches!(state.right_pane, RightPane::Logs)
                    && svc.selected_container.as_deref() == Some(container_name.as_str());

                let is_ejected = svc.ejected
                    && svc.ejected_container.as_deref() == Some(container_name.as_str());

                let can_open_term = svc.ssh_host.is_some()
                    && !is_ejected
                    && state.term_tabs.len() < MAX_TERM_TABS;

                // Badge width: " ejected " pill + gap
                let ejected_w = if is_ejected {
                    " ejected ".len() as f32 * 6.2 + 4.0 + 5.0
                } else {
                    0.0
                };
                let label_w   = container_name.len() as f32 * 7.0 + PAD * 2.0;
                let plus_slot = if can_open_term { PLUS_W } else { 0.0 };
                let tab_w     = (ejected_w + label_w + plus_slot).max(80.0);

                let tab_rect = egui::Rect::from_min_size(
                    egui::pos2(x, bar_rect.min.y),
                    egui::vec2(tab_w, TAB_H),
                );

                // [+] zone and label zone — only split when + is shown
                let plus_zone = if can_open_term {
                    egui::Rect::from_min_max(
                        egui::pos2(tab_rect.max.x - PLUS_W, tab_rect.min.y),
                        tab_rect.max,
                    )
                } else {
                    egui::Rect::NOTHING
                };
                let label_zone = if can_open_term {
                    egui::Rect::from_min_max(
                        tab_rect.min,
                        egui::pos2(tab_rect.max.x - PLUS_W, tab_rect.max.y),
                    )
                } else {
                    tab_rect
                };

                // Allocate [+] first so it wins hit-test priority over label
                let plus_resp = if can_open_term {
                    Some(ui.allocate_rect(plus_zone, egui::Sense::click()))
                } else {
                    None
                };
                let label_resp = ui.allocate_rect(label_zone, egui::Sense::click());

                if plus_resp.as_ref().map_or(false, |r| r.clicked()) {
                    action = Some(TabBarAction::OpenTermForContainer(container_name.clone()));
                } else if label_resp.clicked() && !is_selected_log {
                    action = Some(TabBarAction::SelectContainer(container_name.clone()));
                }

                container_tabs.push(ContainerTabResult {
                    tab_rect,
                    plus_zone,
                    plus_hovered: plus_resp.as_ref().map_or(false, |r| r.hovered()),
                    can_open_term,
                    active: is_selected_log,
                    is_ejected,
                    label: container_name.clone(),
                });

                x += tab_w;
            }
        }
    }

    // ── Terminal tabs ─────────────────────────────────────────────────────────
    let mut term_tabs: Vec<TermTabResult> = Vec::new();

    for (i, tab) in state.term_tabs.iter().enumerate() {
        let tab_w    = 150.0_f32;
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(x, bar_rect.min.y),
            egui::vec2(tab_w, TAB_H),
        );
        let active = state.right_pane == RightPane::TerminalTab(i);

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

        term_tabs.push(TermTabResult {
            tab_rect,
            close_zone,
            label_zone,
            close_hovered: close_resp.hovered(),
            active,
            label: tab.label.clone(),
        });

        x += tab_w;
    }

    // ── All allocations done — paint everything ───────────────────────────────
    let painter = ui.painter();

    painter.rect_filled(bar_rect, 0.0, COLOR_TAB_BAR);

    // Container tabs
    for ct in &container_tabs {
        draw_tab_bg_painter(&painter, ct.tab_rect, ct.active);

        if show_fallback {
            painter.text(
                egui::pos2(ct.tab_rect.min.x + PAD, ct.tab_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &ct.label,
                egui::FontId::new(12.0, egui::FontFamily::Monospace),
                if ct.active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
            );
        } else {
            // [+] divider and button — only when present
            if ct.can_open_term {
                painter.line_segment(
                    [ct.plus_zone.left_top(), ct.plus_zone.left_bottom()],
                    egui::Stroke::new(0.5, COLOR_BORDER),
                );
                painter.text(
                    ct.plus_zone.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::new(15.0, egui::FontFamily::Monospace),
                    if ct.plus_hovered { egui::Color32::WHITE } else { COLOR_MUTED },
                );
            }

            // Ejected pill badge
            let mut text_x = ct.tab_rect.min.x + PAD;
            if ct.is_ejected {
                let badge      = " ejected ";
                let badge_w    = badge.len() as f32 * 6.2 + 4.0;
                let badge_h    = 14.0;
                let badge_rect = egui::Rect::from_min_size(
                    egui::pos2(text_x, ct.tab_rect.center().y - badge_h / 2.0),
                    egui::vec2(badge_w, badge_h),
                );
                painter.rect_filled(badge_rect, 3.0, COLOR_MAGENTA);
                painter.text(
                    badge_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    badge,
                    egui::FontId::new(9.0, egui::FontFamily::Monospace),
                    egui::Color32::WHITE,
                );
                text_x += badge_w + 5.0;
            }

            // Container name label
            painter.text(
                egui::pos2(text_x, ct.tab_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &ct.label,
                egui::FontId::new(11.0, egui::FontFamily::Monospace),
                if ct.active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
            );
        }
    }

    // Terminal tabs
    for tt in &term_tabs {
        draw_tab_bg_painter(&painter, tt.tab_rect, tt.active);

        painter.line_segment(
            [tt.close_zone.left_top(), tt.close_zone.left_bottom()],
            egui::Stroke::new(0.5, COLOR_BORDER),
        );

        painter
            .with_clip_rect(tt.label_zone.shrink2(egui::vec2(0.0, 2.0)))
            .text(
                egui::pos2(tt.tab_rect.min.x + PAD, tt.tab_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &tt.label,
                egui::FontId::new(11.0, egui::FontFamily::Monospace),
                if tt.active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
            );

        painter.text(
            tt.close_zone.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::new(15.0, egui::FontFamily::Monospace),
            if tt.close_hovered { egui::Color32::WHITE } else { COLOR_MUTED },
        );
    }

    // Fill remainder
    let remaining = egui::Rect::from_min_max(egui::pos2(x, bar_rect.min.y), bar_rect.max);
    if remaining.width() > 0.0 {
        painter.rect_filled(remaining, 0.0, COLOR_TAB_BAR);
    }

    // Bottom border across the full bar
    painter.line_segment(
        [bar_rect.left_bottom(), bar_rect.right_bottom()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    action
}

// ── Shared helper ─────────────────────────────────────────────────────────────

fn draw_tab_bg_painter(painter: &egui::Painter, rect: egui::Rect, active: bool) {
    painter.rect_filled(
        rect,
        0.0,
        if active {
            egui::Color32::from_rgb(28, 28, 28)
        } else {
            COLOR_TAB_BAR
        },
    );
    if active {
        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(2.0, COLOR_TAB_ACTIVE),
        );
    }
}