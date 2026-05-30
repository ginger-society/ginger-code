use eframe::egui;
use std::io::Write;

use super::super::colors::{COLOR_BG, COLOR_CURSOR, COLOR_YELLOW};
use super::super::terminal::{key_to_char, Cell};
use super::super::types::{AppState, TermState};

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
                .color(super::super::colors::COLOR_RED));
            return;
        }
        TermState::Idle => {
            ui.label(egui::RichText::new("Connecting…")
                .font(egui::FontId::new(font_size, egui::FontFamily::Monospace))
                .color(super::super::colors::COLOR_MUTED));
            return;
        }
        TermState::Connected(_) => {}
    }

    // ── Allocate painter — derive grid dimensions from actual rect ─────────────
    let (response, painter) =
        ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    let origin = response.rect.min;

    let new_cols = (response.rect.width()  / cell_w).floor() as usize;
    let new_rows = (response.rect.height() / cell_h).floor() as usize;
    if new_cols != tab.term_cols || new_rows != tab.term_rows {
        tab.term_cols = new_cols.max(1);
        tab.term_rows = new_rows.max(1);
        tab.performer.lock().resize(tab.term_rows, tab.term_cols);
        if let TermState::Connected(ref session) = tab.state {
            session.resize(tab.term_rows as u16, tab.term_cols as u16);
        }
    }

    let term_cols = tab.term_cols;
    let term_rows = tab.term_rows;

    painter.rect_filled(response.rect, 0.0, COLOR_BG);

    // ── Snapshot grid + scrollback ────────────────────────────────────────────
    let (live_grid, cursor_row, cursor_col, scrollback_len) = {
        let p = tab.performer.lock();
        (p.grid.clone(), p.cursor_row, p.cursor_col, tab.scrollback.len())
    };
    let scrollback_snap = tab.scrollback.clone();

    let total_rows   = scrollback_len + term_rows;
    let max_offset   = scrollback_len;
    if tab.scroll_offset > max_offset { tab.scroll_offset = max_offset; }
    let at_bottom    = tab.scroll_offset == 0;
    let window_start = total_rows.saturating_sub(term_rows + tab.scroll_offset);

    // ── Mouse-wheel scroll ────────────────────────────────────────────────────
    let mut scroll_delta = 0.0_f32;
    if response.hovered() {
        ui.input(|i| { scroll_delta = i.raw_scroll_delta.y; });
    }
    if scroll_delta > 0.0 {
        tab.scroll_offset = (tab.scroll_offset + (scroll_delta / cell_h) as usize + 1).min(max_offset);
    } else if scroll_delta < 0.0 {
        let steps = (-scroll_delta / cell_h) as usize + 1;
        tab.scroll_offset = tab.scroll_offset.saturating_sub(steps);
    }
    let scroll_offset = tab.scroll_offset;

    // ── Render cells ──────────────────────────────────────────────────────────
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
            let cell_rect = egui::Rect::from_min_size(
                egui::pos2(x, y), egui::vec2(cell_w, cell_h),
            );

            if cell.bg != COLOR_BG {
                painter.rect_filled(cell_rect, 0.0, cell.bg);
            }

            let is_cursor = at_bottom
                && abs_row == scrollback_len + cursor_row
                && c == cursor_col
                && blink;

            if is_cursor {
                painter.rect_filled(cell_rect, 0.0, COLOR_CURSOR);
                if cell.ch != ' ' {
                    painter.text(
                        egui::pos2(x, y), egui::Align2::LEFT_TOP, cell.ch,
                        egui::FontId::new(font_size, egui::FontFamily::Monospace), COLOR_BG,
                    );
                }
                continue;
            }
            if cell.ch != ' ' {
                painter.text(
                    egui::pos2(x, y), egui::Align2::LEFT_TOP, cell.ch,
                    egui::FontId::new(font_size, egui::FontFamily::Monospace), cell.fg,
                );
            }
        }
    }

    // ── Scrollback indicator overlay ──────────────────────────────────────────
    if scroll_offset > 0 {
        let label = format!("↑ {} rows — PgDn / Shift+End to return", scroll_offset);
        painter.rect_filled(
            egui::Rect::from_min_size(
                origin,
                egui::vec2(label.len() as f32 * cell_w * 0.65 + 8.0, cell_h + 2.0),
            ),
            2.0,
            egui::Color32::from_rgba_premultiplied(40, 40, 40, 200),
        );
        painter.text(
            egui::pos2(origin.x + 4.0, origin.y + 1.0),
            egui::Align2::LEFT_TOP,
            &label,
            egui::FontId::new(font_size * 0.85, egui::FontFamily::Monospace),
            COLOR_YELLOW,
        );
    }

    // ── Scrollbar ─────────────────────────────────────────────────────────────
    if max_offset > 0 {
        let sb_w       = 6.0;
        let sb_x       = response.rect.max.x - sb_w - 2.0;
        let sb_top     = response.rect.min.y;
        let sb_h       = response.rect.height();
        let sb_painter = ui.painter_at(response.rect);

        sb_painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(sb_x, sb_top), egui::vec2(sb_w, sb_h)),
            3.0,
            egui::Color32::from_rgb(30, 45, 30),
        );

        let virtual_depth = (max_offset as f32).max(200.0);
        let thumb_h = (sb_h * (term_rows as f32 / (virtual_depth + term_rows as f32)))
            .max(20.0)
            .min(sb_h * 0.3);
        let frac    = scroll_offset as f32 / max_offset as f32;
        let thumb_y = sb_top + (1.0 - frac) * (sb_h - thumb_h);

        sb_painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(sb_x, thumb_y), egui::vec2(sb_w, thumb_h)),
            3.0,
            egui::Color32::from_rgb(0, 180, 50),
        );
    }

    drop(painter);

    // ── Keyboard input ────────────────────────────────────────────────────────
    let mut to_send: Vec<Vec<u8>> = Vec::new();

    if response.hovered() {
        ui.ctx().input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Text(text) => {
                        tab.scroll_offset = 0;
                        tab.sel_start     = None;
                        tab.sel_end       = None;
                        to_send.push(text.as_bytes().to_vec());
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        match key {
                            egui::Key::PageUp => {
                                tab.scroll_offset = (tab.scroll_offset + term_rows).min(max_offset);
                                return;
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

                        tab.sel_start     = None;
                        tab.sel_end       = None;
                        tab.scroll_offset = 0;

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

    // ── Mouse selection ───────────────────────────────────────────────────────
    let pos_to_cell = |pos: egui::Pos2| -> (usize, usize) {
        let col = ((pos.x - origin.x) / cell_w).floor() as isize;
        let row = ((pos.y - origin.y) / cell_h).floor() as isize;
        let col = col.clamp(0, term_cols as isize - 1) as usize;
        let row = row.clamp(0, term_rows as isize - 1) as usize;
        (window_start + row, col)
    };

    let pointer = ui.input(|i| i.pointer.clone());

    if response.hovered() && pointer.button_pressed(egui::PointerButton::Primary) {
        if let Some(pos) = pointer.interact_pos() {
            let cell      = pos_to_cell(pos);
            tab.sel_start = Some(cell);
            tab.sel_end   = Some(cell);
            tab.dragging  = true;
        }
    }

    if tab.dragging {
        if pointer.button_down(egui::PointerButton::Primary) {
            if let Some(pos) = pointer.interact_pos() {
                tab.sel_end = Some(pos_to_cell(pos));
            }
        } else {
            tab.dragging = false;
            if let (Some(start), Some(end)) = (tab.sel_start, tab.sel_end) {
                let text = extract_selection(
                    &scrollback_snap, &live_grid, scrollback_len,
                    term_cols, start, end,
                );
                if !text.is_empty() {
                    ui.output_mut(|o| o.copied_text = text);
                }
            }
        }
    }

    // ── Selection highlight ───────────────────────────────────────────────────
    if let (Some(start), Some(end)) = (tab.sel_start, tab.sel_end) {
        let (mut r1, mut c1) = start;
        let (mut r2, mut c2) = end;
        if (r1, c1) > (r2, c2) {
            std::mem::swap(&mut r1, &mut r2);
            std::mem::swap(&mut c1, &mut c2);
        }

        let sel_color   = egui::Color32::from_rgba_premultiplied(80, 120, 200, 80);
        let sel_painter = ui.painter_at(response.rect);

        for abs_row in r1..=r2 {
            if abs_row < window_start || abs_row >= window_start + term_rows { continue; }
            let screen_row = abs_row - window_start;
            let col_start  = if abs_row == r1 { c1 } else { 0 };
            let col_end    = if abs_row == r2 { c2 } else { term_cols.saturating_sub(1) };

            let x1 = origin.x + col_start as f32 * cell_w;
            let x2 = origin.x + (col_end + 1) as f32 * cell_w;
            let y1 = origin.y + screen_row as f32 * cell_h;

            sel_painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x1, y1), egui::pos2(x2, y1 + cell_h)),
                0.0,
                sel_color,
            );
        }
    }
}

// ── Selection text extraction ─────────────────────────────────────────────────

fn extract_selection(
    scrollback:     &[Vec<Cell>],
    live_grid:      &[Vec<Cell>],
    scrollback_len: usize,
    term_cols:      usize,
    start:          (usize, usize),
    end:            (usize, usize),
) -> String {
    let (mut r1, mut c1) = start;
    let (mut r2, mut c2) = end;
    if (r1, c1) > (r2, c2) {
        std::mem::swap(&mut r1, &mut r2);
        std::mem::swap(&mut c1, &mut c2);
    }

    let get_row = |abs_row: usize| -> Option<&[Cell]> {
        if abs_row < scrollback_len {
            scrollback.get(abs_row).map(|r| r.as_slice())
        } else {
            live_grid.get(abs_row - scrollback_len).map(|r| r.as_slice())
        }
    };

    let mut out = String::new();
    for abs_row in r1..=r2 {
        let col_start = if abs_row == r1 { c1 } else { 0 };
        let col_end   = if abs_row == r2 { c2 } else { term_cols.saturating_sub(1) };
        if let Some(row) = get_row(abs_row) {
            let line: String = row.iter()
                .skip(col_start)
                .take(col_end - col_start + 1)
                .map(|c| c.ch)
                .collect();
            out.push_str(line.trim_end());
        }
        if abs_row < r2 { out.push('\n'); }
    }
    out
}