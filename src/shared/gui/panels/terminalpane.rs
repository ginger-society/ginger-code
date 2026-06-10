use eframe::egui;
use std::io::Write;

use super::super::colors::{COLOR_BG, COLOR_CURSOR, COLOR_YELLOW};
use super::super::terminal::{key_to_char, Cell};
use super::super::types::{AppState, TermState};

// ── Rewrite a command line to fix known interactive-mode issues ───────────────
fn fixup_command(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = if let Some(r) = trimmed.strip_prefix("python3") {
        r.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit())
    } else if let Some(r) = trimmed.strip_prefix("python") {
        r
    } else {
        return None;
    };
    if rest.trim().is_empty() {
        Some(format!("{} -i", trimmed))
    } else {
        None
    }
}

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
        TermState::Idle | TermState::Connecting => {
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
        if let TermState::Connected(ref mut session) = tab.state {
            // 1. Resize the local PTY master — sends SIGWINCH to kubectl.
            session.resize(tab.term_rows as u16, tab.term_cols as u16);
            // 2. Send in-band resize to the shell inside the pod.
            //    Because we now launch via `script`, the pod has a real PTY,
            //    so stty / TIOCSWINSZ works and delivers SIGWINCH to the
            //    foreground process (nano, htop, vim).
            //    \x15 (Ctrl+U) clears any partial command the user was typing
            //    so the stty lands cleanly on an empty prompt line; \x0d
            //    submits it. The export keeps $COLUMNS/$LINES in sync for
            //    shells and scripts that read env vars instead of the tty.
            let resize_cmd = format!(
                "\x15stty rows {rows} cols {cols} 2>/dev/null; \
                 export COLUMNS={cols} LINES={rows}\x0d",
                cols = tab.term_cols,
                rows = tab.term_rows,
            );
            let _ = session.writer.lock().write_all(resize_cmd.as_bytes());
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
    //
    // IMPORTANT: We collect ALL bytes to send here, then flush them after the
    // input closure. We never `return` early inside the closure — doing so
    // would skip remaining events in the same frame.
    //
    // Ctrl-key handling strategy:
    //   egui fires Event::Key  with modifiers.ctrl = true
    //   egui may also fire Event::Text with the plain character (e.g. "x" for
    //   Ctrl+X). To avoid double-sending we:
    //     1. Check ctrl_held BEFORE the input closure (can't nest input borrows).
    //     2. Skip ANY Event::Text when Ctrl is held — those chars are handled
    //        exclusively by the Key branch below.
    //     3. Also skip Event::Text that contains raw control bytes (codepoint
    //        < 0x20 or == 0x7f) for the same reason.

    // Snapshot modifier state before entering the input closure so we can
    // reference it inside Event::Text without a nested borrow of ui.input.
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);

    let mut to_send: Vec<Vec<u8>> = Vec::new();
    let mut scroll_cmd: Option<i32> = None; // +N = scroll up N rows, -N = down

    if response.hovered() {
        ui.ctx().input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Text(text) => {
                        // Drop raw control characters — handled by Key branch.
                        if text.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
                            continue;
                        }
                        // Drop ALL text events when Ctrl is held. On most
                        // platforms egui fires both Event::Key (with
                        // modifiers.ctrl) AND Event::Text with the bare letter
                        // (e.g. Ctrl+X → Key{X, ctrl} + Text("x")). Sending
                        // the Text here would type a literal "x" in addition
                        // to the ^X control byte sent by the Key branch.
                        if ctrl_held {
                            continue;
                        }
                        tab.scroll_offset = 0;
                        tab.sel_start     = None;
                        tab.sel_end       = None;
                        to_send.push(text.as_bytes().to_vec());
                    }

                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        // ── Scrollback navigation (no bytes sent to pty) ──────
                        match key {
                            egui::Key::PageUp if !modifiers.ctrl => {
                                scroll_cmd = Some(term_rows as i32);
                                continue;
                            }
                            egui::Key::PageDown if !modifiers.ctrl => {
                                scroll_cmd = Some(-(term_rows as i32));
                                continue;
                            }
                            egui::Key::End if modifiers.shift => {
                                scroll_cmd = Some(i32::MIN); // jump to bottom
                                continue;
                            }
                            egui::Key::Home if modifiers.shift => {
                                scroll_cmd = Some(i32::MAX); // jump to top
                                continue;
                            }
                            _ => {}
                        }

                        tab.sel_start     = None;
                        tab.sel_end       = None;
                        tab.scroll_offset = 0;

                        // ── Ctrl+letter → control byte (^A..^Z) ──────────────
                        if modifiers.ctrl {
                            if let Some(ch) = key_to_char(*key) {
                                if ch >= 'a' && ch <= 'z' {
                                    to_send.push(vec![(ch as u8) & 0x1f]);
                                    continue;
                                }
                            }
                            // Ctrl+[ = Escape, Ctrl+\ = FS, Ctrl+] = GS, etc.
                            // Fall through to normal key handling for anything
                            // we don't recognise as a letter.
                        }

                        // ── Special keys → escape sequences ───────────────────
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
                            egui::Key::Delete     => Some(b"\x1b[3~"),
                            egui::Key::Insert     => Some(b"\x1b[2~"),
                            egui::Key::F1         => Some(b"\x1bOP"),
                            egui::Key::F2         => Some(b"\x1bOQ"),
                            egui::Key::F3         => Some(b"\x1bOR"),
                            egui::Key::F4         => Some(b"\x1bOS"),
                            egui::Key::F5         => Some(b"\x1b[15~"),
                            egui::Key::F6         => Some(b"\x1b[17~"),
                            egui::Key::F7         => Some(b"\x1b[18~"),
                            egui::Key::F8         => Some(b"\x1b[19~"),
                            egui::Key::F9         => Some(b"\x1b[20~"),
                            egui::Key::F10        => Some(b"\x1b[21~"),
                            egui::Key::F11        => Some(b"\x1b[23~"),
                            egui::Key::F12        => Some(b"\x1b[24~"),
                            _ => None,
                        };
                        if let Some(b) = bytes { to_send.push(b.to_vec()); }
                    }

                    _ => {}
                }
            }
        });
    }

    // ── Apply scroll commands ─────────────────────────────────────────────────
    if let Some(delta) = scroll_cmd {
        if delta == i32::MAX {
            tab.scroll_offset = max_offset;
        } else if delta == i32::MIN {
            tab.scroll_offset = 0;
        } else if delta > 0 {
            tab.scroll_offset = (tab.scroll_offset + delta as usize).min(max_offset);
        } else {
            tab.scroll_offset = tab.scroll_offset.saturating_sub((-delta) as usize);
        }
    }

    // ── Send bytes, intercepting Enter to rewrite commands if needed ──────────
    if let TermState::Connected(ref mut session) = tab.state {
        for bytes in to_send {
            if bytes == b"\r" {
                // Peek at the current input line to check for command rewrites.
                let line: String = {
                    let p = tab.performer.lock();
                    p.grid
                        .get(p.cursor_row)
                        .map(|row| row.iter().take(p.cursor_col).map(|c| c.ch).collect())
                        .unwrap_or_default()
                };
                let cmd_part = line
                    .rfind(|c| c == '$' || c == '#' || c == '%')
                    .map(|i| line[i + 1..].trim())
                    .unwrap_or(line.trim());

                if let Some(rewritten) = fixup_command(cmd_part) {
                    let erase_count = cmd_part.len();
                    let mut payload = Vec::with_capacity(erase_count + rewritten.len() + 1);
                    for _ in 0..erase_count { payload.push(0x7f); }
                    payload.extend_from_slice(rewritten.as_bytes());
                    payload.push(b'\r');
                    let _ = session.writer.lock().write_all(&payload);
                    continue;
                }
                let _ = session.writer.lock().write_all(b"\r");
            } else {
                let _ = session.writer.lock().write_all(&bytes);
            }
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