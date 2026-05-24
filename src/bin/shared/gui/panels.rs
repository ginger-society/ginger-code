//! Pure-UI drawing functions.  Each fn returns data for decisions rather than
//! mutating state directly, keeping business logic in app.rs.

use eframe::egui;
use std::io::Write;

use super::colors::*;
use super::terminal::{key_to_char, Cell};
use super::types::{AppState, RightPane, TermState, MAX_TERM_TABS};

// ── Title bar ─────────────────────────────────────────────────────────────────

pub fn draw_titlebar(state: &AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 28.0),
        egui::Sense::click_and_drag(),
    );
    if response.dragged() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    let dot_y      = rect.center().y;
    let dot_colors = [
        egui::Color32::from_rgb(255, 95,  86),
        egui::Color32::from_rgb(255, 189, 46),
        egui::Color32::from_rgb(39,  201, 63),
    ];

    let dot_data: Vec<(egui::Pos2, bool)> = dot_colors.iter().enumerate().map(|(i, _)| {
        let center   = egui::pos2(rect.min.x + 14.0 + i as f32 * 20.0, dot_y);
        let dot_rect = egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0));
        let clicked  = ui.allocate_rect(dot_rect, egui::Sense::click()).clicked();
        (center, clicked)
    }).collect();

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 30, 30));
    for ((center, _), color) in dot_data.iter().zip(dot_colors.iter()) {
        painter.circle_filled(*center, 6.0, *color);
    }
    let svc   = &state.services[state.selected_idx];
    let title = format!("GingerKube  —  {}", svc.meta_name);
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, title,
        egui::FontId::proportional(12.0), egui::Color32::from_rgb(160, 160, 160));
    drop(painter);

    for (i, (_, clicked)) in dot_data.iter().enumerate() {
        if *clicked {
            match i {
                0 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                1 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                2 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true)),
                _ => {}
            }
        }
    }
}

// ── Sidebar service list ──────────────────────────────────────────────────────

/// Returns `Some(idx)` when a different service row is clicked.
pub fn draw_service_list(state: &AppState, ui: &mut egui::Ui) -> Option<usize> {
    let mut clicked_idx = None;
    egui::ScrollArea::vertical()
        .id_source("service_scroll")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for i in 0..state.services.len() {
                let svc        = &state.services[i];
                let dot_char   = svc.status_dot();
                let dot_color  = svc.status_color();
                let short_name = svc.meta_name.split('/').last().unwrap_or(&svc.meta_name).to_owned();
                let ejected    = svc.ejected;
                let sub        = format!("ready: {}  status: {}", svc.ready, svc.status);
                let selected   = i == state.selected_idx;
                let bg         = if selected { COLOR_SELECTED_BG } else { COLOR_SIDEBAR_BG };

                let (row_rect, row_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 42.0), egui::Sense::click(),
                );
                if row_resp.clicked() && !selected { clicked_idx = Some(i); }

                let painter = ui.painter();
                painter.rect_filled(row_rect, 0.0, bg);
                if selected {
                    painter.rect_filled(
                        egui::Rect::from_min_size(row_rect.min, egui::vec2(3.0, row_rect.height())),
                        0.0, COLOR_TAB_ACTIVE,
                    );
                }
                let name_color = if selected { egui::Color32::WHITE } else { COLOR_MUTED };
                let dot_pos    = egui::pos2(row_rect.min.x + 14.0, row_rect.min.y + 13.0);
                painter.text(dot_pos, egui::Align2::CENTER_CENTER, dot_char,
                    egui::FontId::new(11.0, egui::FontFamily::Monospace), dot_color);
                painter.text(egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 8.0),
                    egui::Align2::LEFT_TOP, &short_name,
                    egui::FontId::new(12.0, egui::FontFamily::Monospace), name_color);
                if ejected {
                    let ej_x = row_rect.min.x + 24.0 + short_name.len() as f32 * 7.2 + 6.0;
                    painter.text(egui::pos2(ej_x, row_rect.min.y + 8.0),
                        egui::Align2::LEFT_TOP, "[EJECTED]",
                        egui::FontId::new(10.0, egui::FontFamily::Monospace), COLOR_MAGENTA);
                }
                painter.text(egui::pos2(row_rect.min.x + 24.0, row_rect.min.y + 24.0),
                    egui::Align2::LEFT_TOP, &sub,
                    egui::FontId::new(10.0, egui::FontFamily::Monospace), COLOR_DIM);
                painter.line_segment(
                    [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
                    egui::Stroke::new(0.5, COLOR_BORDER),
                );
            }
        });
    clicked_idx
}

// ── Deployment info strip ─────────────────────────────────────────────────────

pub fn draw_info_strip(state: &AppState, ui: &mut egui::Ui) {
    let svc = &state.services[state.selected_idx];
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let painter   = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(22, 22, 22));
    painter.line_segment([egui::pos2(rect.min.x, rect.max.y), rect.max], egui::Stroke::new(0.5, COLOR_BORDER));
    let deploy = svc.deployment_name.as_deref().unwrap_or("—");
    let lang   = svc.lang.as_deref().unwrap_or("—");
    let text   = format!(
        "  deploy: {}   lang: {}   ready: {}   status: {}{}",
        deploy, lang, svc.ready, svc.status,
        if svc.ejected { "   [EJECTED]" } else { "" },
    );
    painter.text(egui::pos2(rect.min.x, rect.center().y), egui::Align2::LEFT_CENTER,
        &text, egui::FontId::new(10.5, egui::FontFamily::Monospace), COLOR_MUTED);
}

// ── Tab bar ───────────────────────────────────────────────────────────────────

/// What the user did this frame. Returned to app.rs for processing.
pub enum TabBarAction {
    SwitchToLogs,
    SwitchToTerm(usize),
    /// "+" button: open new terminal for current service.
    NewTerm,
    /// "×" on a terminal tab.
    CloseTerm(usize),
}

/// Draw the Logs tab + all terminal tabs + optional "+" button.
/// Returns an action if the user clicked something.
pub fn draw_tab_bar(state: &AppState, ui: &mut egui::Ui) -> Option<TabBarAction> {
    const TAB_H:    f32 = 28.0;
    const LOGS_W:   f32 = 70.0;
    const TERM_W:   f32 = 120.0;  // wider to fit label + ×
    const PLUS_W:   f32 = 28.0;

    let (bar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TAB_H), egui::Sense::hover(),
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
        painter.rect_filled(tab_rect, 0.0, if active { egui::Color32::from_rgb(28,28,28) } else { COLOR_TAB_BAR });
        if active {
            painter.line_segment([tab_rect.left_bottom(), tab_rect.right_bottom()], egui::Stroke::new(2.0, COLOR_TAB_ACTIVE));
        }
        painter.text(tab_rect.center(), egui::Align2::CENTER_CENTER, "Logs",
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
            if active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE });
        x += LOGS_W;
    }

    // ── Terminal tabs ─────────────────────────────────────────────────────────
    for (i, tab) in state.term_tabs.iter().enumerate() {
        let tab_rect  = egui::Rect::from_min_size(egui::pos2(x, bar_rect.min.y), egui::vec2(TERM_W, TAB_H));
        let active    = state.right_pane == RightPane::TerminalTab(i);

        // Close button "×" — allocate first so it takes priority over the tab click.
        let close_size = 14.0_f32;
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(tab_rect.max.x - 10.0, tab_rect.center().y),
            egui::vec2(close_size, close_size),
        );
        let close_clicked = ui.allocate_rect(close_rect, egui::Sense::click()).clicked();
        let tab_clicked   = ui.allocate_rect(tab_rect,   egui::Sense::click()).clicked();

        if close_clicked { action = Some(TabBarAction::CloseTerm(i)); }
        else if tab_clicked && !active { action = Some(TabBarAction::SwitchToTerm(i)); }

        let painter = ui.painter();
        painter.rect_filled(tab_rect, 0.0, if active { egui::Color32::from_rgb(28,28,28) } else { COLOR_TAB_BAR });
        if active {
            painter.line_segment([tab_rect.left_bottom(), tab_rect.right_bottom()], egui::Stroke::new(2.0, COLOR_TAB_ACTIVE));
        }
        // Label — truncate if needed
        let label_rect = egui::Rect::from_min_size(
            tab_rect.min, egui::vec2(TERM_W - close_size - 8.0, TAB_H),
        );
        painter.text(
            egui::pos2(label_rect.min.x + 8.0, tab_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &tab.label,
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            if active { egui::Color32::WHITE } else { COLOR_TAB_INACTIVE },
        );
        // Close ×
        painter.text(close_rect.center(), egui::Align2::CENTER_CENTER, "×",
            egui::FontId::new(14.0, egui::FontFamily::Monospace), COLOR_DIM);

        x += TERM_W;
    }

    // ── "+" button (only if under cap) ───────────────────────────────────────
    let svc_has_host = state.services[state.selected_idx].ssh_host.is_some();
    if state.term_tabs.len() < MAX_TERM_TABS && svc_has_host {
        let plus_rect = egui::Rect::from_min_size(egui::pos2(x, bar_rect.min.y), egui::vec2(PLUS_W, TAB_H));
        let clicked   = ui.allocate_rect(plus_rect, egui::Sense::click()).clicked();
        if clicked { action = Some(TabBarAction::NewTerm); }

        let painter = ui.painter();
        painter.rect_filled(plus_rect, 0.0, COLOR_TAB_BAR);
        painter.text(plus_rect.center(), egui::Align2::CENTER_CENTER, "+",
            egui::FontId::new(16.0, egui::FontFamily::Monospace), COLOR_MUTED);
    }

    // ── Background fill + bottom border for bar ───────────────────────────────
    {
        let painter = ui.painter();
        // Fill gap to the right of all tabs
        let remaining = egui::Rect::from_min_max(egui::pos2(x + PLUS_W, bar_rect.min.y), bar_rect.max);
        if remaining.width() > 0.0 {
            painter.rect_filled(remaining, 0.0, COLOR_TAB_BAR);
        }
        painter.line_segment([bar_rect.left_bottom(), bar_rect.right_bottom()],
            egui::Stroke::new(0.5, COLOR_BORDER));
    }

    action
}

// ── Logs pane ─────────────────────────────────────────────────────────────────

pub fn draw_logs_pane(state: &AppState, ui: &mut egui::Ui) {
    let svc       = &state.services[state.selected_idx];
    let font_size = state.font_size;
    egui::ScrollArea::vertical()
        .id_source("logs_scroll")
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for line in &state.logs {
                let color = if line.starts_with("ERROR") || line.contains("error") || line.contains("panic") {
                    COLOR_RED
                } else if line.contains("WARN") || line.contains("warn") {
                    COLOR_YELLOW
                } else if line.contains("Fetching") {
                    COLOR_DIM
                } else {
                    COLOR_FG
                };
                ui.add(egui::Label::new(
                    egui::RichText::new(line)
                        .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                        .color(color),
                ).wrap(false));
            }
            if svc.ejected {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(
                    "⚡ Service is in dev mode — container runs sleep infinity. No application logs.")
                    .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                    .color(COLOR_MAGENTA));
                ui.label(egui::RichText::new("Switch to the Terminal tab to interact with the container.")
                    .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                    .color(COLOR_MUTED));
            }
        });
}

// ── Terminal pane (single tab) ────────────────────────────────────────────────

/// Draw one terminal tab.  `tab_idx` is the index into `state.term_tabs`.
/// Must only be called when `state.right_pane == RightPane::TerminalTab(tab_idx)`.
pub fn draw_terminal_pane(state: &mut AppState, ui: &mut egui::Ui, tab_idx: usize) {
    let font_size = state.font_size;
    let cell_w    = state.cell_w;
    let cell_h    = state.cell_h;
    let blink     = state.blink;

    let tab = match state.term_tabs.get_mut(tab_idx) {
        Some(t) => t,
        None    => return,
    };

    // ── Guard: not yet connected / error ──────────────────────────────────────
    match &tab.state {
        TermState::Error(e) => {
            ui.label(egui::RichText::new(format!("Connection error: {}", e))
                .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                .color(COLOR_RED));
            return;
        }
        TermState::Idle => {
            ui.label(egui::RichText::new("Connecting…")
                .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                .color(COLOR_MUTED));
            return;
        }
        TermState::Connected(_) => {}
    }

    // ── Resize grid if panel changed size ─────────────────────────────────────
    let available = ui.available_size();
    let new_cols  = (available.x / cell_w).floor() as usize;
    let new_rows  = ((available.y - 20.0) / cell_h).floor() as usize; // leave room for scroll hint
    if new_cols != tab.term_cols || new_rows != tab.term_rows {
        tab.term_cols = new_cols.max(1);
        tab.term_rows = new_rows.max(1);
        tab.performer.lock().resize(tab.term_rows, tab.term_cols);
    }

    let term_cols  = tab.term_cols;
    let term_rows  = tab.term_rows;

    // ── Build the combined scrollback + live view ─────────────────────────────
    // We snapshot both while the performer is locked.
    let (live_grid, cursor_row, cursor_col, scrollback_len) = {
        let p = tab.performer.lock();
        (p.grid.clone(), p.cursor_row, p.cursor_col, tab.scrollback.len())
    };
    let scrollback_snap = tab.scrollback.clone();

    let total_rows  = scrollback_len + term_rows;
    // scroll_offset = 0 means "at bottom"; max = scrollback_len
    let max_offset  = scrollback_len;
    // Clamp current offset
    if tab.scroll_offset > max_offset { tab.scroll_offset = max_offset; }
    let scroll_offset = tab.scroll_offset;
    let at_bottom     = scroll_offset == 0;

    // The window of rows we will render (term_rows tall):
    //   row 0 of the window = total_rows - term_rows - scroll_offset
    let window_start = (total_rows).saturating_sub(term_rows + scroll_offset);

    // ── Scroll area (mouse-wheel + vertical scrollbar) ────────────────────────
    // We manage scroll_offset ourselves; egui::ScrollArea just captures wheel.
    let scroll_area_id = egui::Id::new(("term_scroll", tab_idx));
    let mut scroll_delta = 0.0_f32;
    ui.input(|i| {
        // Only capture if the pointer is over our future painter rect.
        // We check globally here; the painter check below will also guard input.
        scroll_delta = i.raw_scroll_delta.y;
    });
    if scroll_delta > 0.0 {
        // Scroll up (into scrollback)
        tab.scroll_offset = (tab.scroll_offset + (scroll_delta / cell_h) as usize + 1)
            .min(max_offset);
    } else if scroll_delta < 0.0 {
        // Scroll down (towards live)
        let steps = (-scroll_delta / cell_h) as usize + 1;
        tab.scroll_offset = tab.scroll_offset.saturating_sub(steps);
    }
    let scroll_offset = tab.scroll_offset; // re-read after update

    // ── Allocate painter ──────────────────────────────────────────────────────
    let term_size = egui::vec2(term_cols as f32 * cell_w, term_rows as f32 * cell_h);
    let (response, painter) = ui.allocate_painter(term_size, egui::Sense::click_and_drag());
    painter.rect_filled(response.rect, 0.0, COLOR_BG);

    let origin = response.rect.min;

    // ── Render rows ───────────────────────────────────────────────────────────
    for r in 0..term_rows {
        let abs_row = window_start + r;
        let row: &[Cell] = if abs_row < scrollback_len {
            &scrollback_snap[abs_row]
        } else {
            let live_r = abs_row - scrollback_len;
            if live_r < live_grid.len() { &live_grid[live_r] } else { continue; }
        };

        for (c, cell) in row.iter().enumerate().take(term_cols) {
            let x         = origin.x + c as f32 * cell_w;
            let y         = origin.y + r as f32 * cell_h;
            let cell_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell_w, cell_h));

            if cell.bg != COLOR_BG {
                painter.rect_filled(cell_rect, 0.0, cell.bg);
            }

            // Show cursor only when at bottom (live view)
            let is_cursor = at_bottom
                && abs_row == scrollback_len + cursor_row
                && c == cursor_col
                && blink;

            if is_cursor {
                painter.rect_filled(cell_rect, 0.0, COLOR_CURSOR);
                if cell.ch != ' ' {
                    painter.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, cell.ch,
                        egui::FontId::new(font_size, egui::FontFamily::Monospace), COLOR_BG);
                }
                continue;
            }
            if cell.ch != ' ' {
                painter.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, cell.ch,
                    egui::FontId::new(font_size, egui::FontFamily::Monospace), cell.fg);
            }
        }
    }

    // ── Scrollback indicator (top-right corner overlay) ───────────────────────
    if scroll_offset > 0 {
        let label = format!("↑ {}↑ scroll — PgDn / End to return", scroll_offset);
        painter.rect_filled(
            egui::Rect::from_min_size(origin, egui::vec2(label.len() as f32 * cell_w * 0.65 + 8.0, cell_h + 2.0)),
            2.0, egui::Color32::from_rgba_premultiplied(40, 40, 40, 200),
        );
        painter.text(egui::pos2(origin.x + 4.0, origin.y + 1.0), egui::Align2::LEFT_TOP,
            &label, egui::FontId::new(font_size * 0.85, egui::FontFamily::Monospace), COLOR_YELLOW);
    }

    drop(painter);

    // ── Keyboard: scroll hotkeys + PTY input ─────────────────────────────────
    let mut to_send: Vec<Vec<u8>> = Vec::new();

    if response.hovered() {
        ui.ctx().input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Text(text) => {
                        // Any text input jumps back to live
                        tab.scroll_offset = 0;
                        to_send.push(text.as_bytes().to_vec());
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        // Scroll keys (don't send to PTY)
                        match key {
                            egui::Key::PageUp => {
                                tab.scroll_offset =
                                    (tab.scroll_offset + term_rows).min(max_offset);
                                return; // don't forward
                            }
                            egui::Key::PageDown => {
                                tab.scroll_offset = tab.scroll_offset.saturating_sub(term_rows);
                                return;
                            }
                            egui::Key::End if modifiers.shift => {
                                tab.scroll_offset = 0;
                                return;
                            }
                            egui::Key::Home if modifiers.shift => {
                                tab.scroll_offset = max_offset;
                                return;
                            }
                            _ => {}
                        }

                        // PTY keys
                        tab.scroll_offset = 0; // any PTY key → snap to live
                        let bytes: Option<&[u8]> = match key {
                            egui::Key::Enter      => Some(b"\r"),
                            egui::Key::Backspace  => Some(b"\x7f"),
                            egui::Key::Tab        => Some(b"\t"),
                            egui::Key::Escape     => Some(b"\x1b"),
                            egui::Key::ArrowUp    => Some(b"\x1b[A"),
                            egui::Key::ArrowDown  => Some(b"\x1b[B"),
                            egui::Key::ArrowRight => Some(b"\x1b[C"),
                            egui::Key::ArrowLeft  => Some(b"\x1b[D"),
                            egui::Key::Home       => Some(b"\x1b[H"),
                            egui::Key::End        => Some(b"\x1b[F"),
                            egui::Key::PageUp     => Some(b"\x1b[5~"),
                            egui::Key::PageDown   => Some(b"\x1b[6~"),
                            egui::Key::Delete     => Some(b"\x1b[3~"),
                            egui::Key::F1         => Some(b"\x1bOP"),
                            egui::Key::F2         => Some(b"\x1bOQ"),
                            egui::Key::F3         => Some(b"\x1bOR"),
                            egui::Key::F4         => Some(b"\x1bOS"),
                            _ => {
                                if modifiers.ctrl {
                                    if let Some(ch) = key_to_char(*key) {
                                        if ch >= 'a' && ch <= 'z' {
                                            to_send.push(vec![(ch as u8) - b'a' + 1]);
                                        }
                                    }
                                }
                                None
                            }
                        };
                        if let Some(b) = bytes { to_send.push(b.to_vec()); }
                    }
                    _ => {}
                }
            }
        });
    }

    if let TermState::Connected(ref mut session) = tab.state {
        for bytes in to_send {
            let _ = session.writer.write_all(&bytes);
        }
    }
}

// ── Status bar ────────────────────────────────────────────────────────────────

pub fn draw_statusbar(state: &AppState, ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
    let painter   = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(18, 18, 18));
    painter.line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(0.5, COLOR_BORDER));

    let (text, color) = if let RightPane::TerminalTab(i) = state.right_pane {
        if let Some(tab) = state.term_tabs.get(i) {
            match &tab.state {
                TermState::Idle         => ("  ○ Ready".to_string(), COLOR_DIM),
                TermState::Connected(_) => (
                    format!("  ● {}  {}×{}", tab.label, tab.term_cols, tab.term_rows),
                    COLOR_TAB_ACTIVE,
                ),
                TermState::Error(e)     => (format!("  ✗ {}", e), COLOR_RED),
            }
        } else {
            ("".to_string(), COLOR_DIM)
        }
    } else {
        ("  ○ Ready".to_string(), COLOR_DIM)
    };

    painter.text(egui::pos2(rect.min.x, rect.center().y), egui::Align2::LEFT_CENTER,
        text, egui::FontId::proportional(11.0), color);
}