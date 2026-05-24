use eframe::egui;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use super::colors::{ansi256, ANSI_COLORS, COLOR_BG, COLOR_FG};

// ── Terminal cell ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Cell {
    pub ch:   char,
    pub fg:   egui::Color32,
    pub bg:   egui::Color32,
    pub bold: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', fg: COLOR_FG, bg: COLOR_BG, bold: false }
    }
}

// ── Scrollback sink ───────────────────────────────────────────────────────────

pub type ScrollbackSink = Arc<Mutex<Vec<Vec<Cell>>>>;

// ── VTE performer ─────────────────────────────────────────────────────────────

pub struct TermPerformer {
    pub grid:        Vec<Vec<Cell>>,
    pub cursor_row:  usize,
    pub cursor_col:  usize,
    pub rows:        usize,
    pub cols:        usize,
    current_fg:      egui::Color32,
    current_bg:      egui::Color32,
    bold:            bool,
    saved_row:       usize,
    saved_col:       usize,
    scrollback_sink: Option<ScrollbackSink>,
}

impl TermPerformer {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            grid:            vec![vec![Cell::default(); cols]; rows],
            cursor_row:      0,
            cursor_col:      0,
            rows,
            cols,
            current_fg:      COLOR_FG,
            current_bg:      COLOR_BG,
            bold:            false,
            saved_row:       0,
            saved_col:       0,
            scrollback_sink: None,
        }
    }

    pub fn with_sink(mut self, sink: ScrollbackSink) -> Self {
        self.scrollback_sink = Some(sink);
        self
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.rows = rows;
        self.cols = cols;
        self.grid.resize(rows, vec![Cell::default(); cols]);
        for row in &mut self.grid { row.resize(cols, Cell::default()); }
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
    }

    fn scroll_up(&mut self) {
        let evicted = self.grid.remove(0);
        if let Some(ref sink) = self.scrollback_sink {
            sink.lock().push(evicted);
        }
        self.grid.push(vec![Cell::default(); self.cols]);
    }

    fn write_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col  = 0;
            self.cursor_row += 1;
        }
        if self.cursor_row >= self.rows {
            self.scroll_up();
            self.cursor_row = self.rows - 1;
        }
        self.grid[self.cursor_row][self.cursor_col] = Cell {
            ch, fg: self.current_fg, bg: self.current_bg, bold: self.bold,
        };
        self.cursor_col += 1;
    }

    fn apply_sgr(&mut self, params: &[i64]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0  => { self.current_fg = COLOR_FG; self.current_bg = COLOR_BG; self.bold = false; }
                1  => self.bold = true,
                22 => self.bold = false,
                30..=37 => self.current_fg = ANSI_COLORS[(params[i] - 30) as usize],
                38 if params.get(i+1) == Some(&5) => {
                    if let Some(&idx) = params.get(i+2) { self.current_fg = ansi256(idx as u8); i += 2; }
                }
                38 if params.get(i+1) == Some(&2) => {
                    if let (Some(&r), Some(&g), Some(&b)) = (params.get(i+2), params.get(i+3), params.get(i+4)) {
                        self.current_fg = egui::Color32::from_rgb(r as u8, g as u8, b as u8); i += 4;
                    }
                }
                39 => self.current_fg = COLOR_FG,
                40..=47  => self.current_bg = ANSI_COLORS[(params[i] - 40) as usize],
                48 if params.get(i+1) == Some(&5) => {
                    if let Some(&idx) = params.get(i+2) { self.current_bg = ansi256(idx as u8); i += 2; }
                }
                48 if params.get(i+1) == Some(&2) => {
                    if let (Some(&r), Some(&g), Some(&b)) = (params.get(i+2), params.get(i+3), params.get(i+4)) {
                        self.current_bg = egui::Color32::from_rgb(r as u8, g as u8, b as u8); i += 4;
                    }
                }
                49        => self.current_bg = COLOR_BG,
                90..=97   => self.current_fg = ANSI_COLORS[(params[i] - 90 + 8) as usize],
                100..=107 => self.current_bg = ANSI_COLORS[(params[i] - 100 + 8) as usize],
                _ => {}
            }
            i += 1;
        }
    }
}

impl vte::Perform for TermPerformer {
    fn print(&mut self, c: char) { self.write_char(c); }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.cursor_col = 0,
            b'\n' => {
                self.cursor_row += 1;
                if self.cursor_row >= self.rows { self.scroll_up(); self.cursor_row = self.rows - 1; }
            }
            8  => { if self.cursor_col > 0 { self.cursor_col -= 1; } }
            7  => {}
            _  => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, _: &[u8], _: bool, action: char) {
        let p: Vec<i64> = params.iter()
            .map(|sub| sub.first().copied().unwrap_or(0) as i64)
            .collect();
        match action {
            'A' => { let n = p.first().copied().unwrap_or(1).max(1) as usize; self.cursor_row = self.cursor_row.saturating_sub(n); }
            'B' => { let n = p.first().copied().unwrap_or(1).max(1) as usize; self.cursor_row = (self.cursor_row + n).min(self.rows - 1); }
            'C' => { let n = p.first().copied().unwrap_or(1).max(1) as usize; self.cursor_col = (self.cursor_col + n).min(self.cols - 1); }
            'D' => { let n = p.first().copied().unwrap_or(1).max(1) as usize; self.cursor_col = self.cursor_col.saturating_sub(n); }
            'H' | 'f' => {
                let row = (p.first().copied().unwrap_or(1).max(1) - 1) as usize;
                let col = (p.get(1).copied().unwrap_or(1).max(1) - 1) as usize;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            'G' => { let col = (p.first().copied().unwrap_or(1).max(1) - 1) as usize; self.cursor_col = col.min(self.cols - 1); }
            'd' => { let row = (p.first().copied().unwrap_or(1).max(1) - 1) as usize; self.cursor_row = row.min(self.rows - 1); }
            'J' => match p.first().copied().unwrap_or(0) {
                0 => {
                    for col in self.cursor_col..self.cols { self.grid[self.cursor_row][col] = Cell::default(); }
                    for row in (self.cursor_row+1)..self.rows { self.grid[row] = vec![Cell::default(); self.cols]; }
                }
                1 => {
                    for col in 0..=self.cursor_col { self.grid[self.cursor_row][col] = Cell::default(); }
                    for row in 0..self.cursor_row { self.grid[row] = vec![Cell::default(); self.cols]; }
                }
                2 | 3 => { for row in &mut self.grid { *row = vec![Cell::default(); self.cols]; } }
                _ => {}
            },
            'K' => match p.first().copied().unwrap_or(0) {
                0 => { for col in self.cursor_col..self.cols { self.grid[self.cursor_row][col] = Cell::default(); } }
                1 => { for col in 0..=self.cursor_col { self.grid[self.cursor_row][col] = Cell::default(); } }
                2 => { self.grid[self.cursor_row] = vec![Cell::default(); self.cols]; }
                _ => {}
            },
            'm' => self.apply_sgr(&p),
            's' => { self.saved_row = self.cursor_row; self.saved_col = self.cursor_col; }
            'u' => { self.cursor_row = self.saved_row; self.cursor_col = self.saved_col; }
            'S' => { let n = p.first().copied().unwrap_or(1).max(1) as usize; for _ in 0..n { self.scroll_up(); } }
            'T' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.grid.insert(0, vec![Cell::default(); self.cols]);
                    self.grid.truncate(self.rows);
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
    fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn esc_dispatch(&mut self, _: &[u8], _: bool, byte: u8) {
        match byte {
            b'7' => { self.saved_row = self.cursor_row; self.saved_col = self.cursor_col; }
            b'8' => { self.cursor_row = self.saved_row; self.cursor_col = self.saved_col; }
            _ => {}
        }
    }
}

// ── SSH session ───────────────────────────────────────────────────────────────

pub struct SshSession {
    pub writer:   Box<dyn Write + Send>,
    _pty_pair:    portable_pty::PtyPair,
}

impl SshSession {
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self._pty_pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width:  0,
            pixel_height: 0,
        });
    }
}

pub fn spawn_ssh(
    host:      &str,
    rows:      u16,
    cols:      u16,
    performer: Arc<Mutex<TermPerformer>>,
    ctx:       egui::Context,
) -> Result<SshSession, Box<dyn std::error::Error>> {
    let pty_system = native_pty_system();
    let pair       = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

    let mut cmd = CommandBuilder::new("ssh");
    cmd.arg("-o");
    cmd.arg("StrictHostKeyChecking=accept-new");
    cmd.arg(host);

    let _child     = pair.slave.spawn_command(cmd)?;
    let writer     = pair.master.take_writer()?;
    let mut reader = pair.master.try_clone_reader()?;

    thread::spawn(move || {
        let mut parser = vte::Parser::new();
        let mut buf    = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut p = performer.lock();
                    for &b in &buf[..n] { parser.advance(&mut *p, b); }
                    drop(p);
                    ctx.request_repaint();
                }
            }
        }
    });

    Ok(SshSession { writer, _pty_pair: pair })
}

// ── Key → char helper ────────────────────────────────────────────────────────

pub fn key_to_char(key: egui::Key) -> Option<char> {
    use egui::Key::*;
    match key {
        A=>'a', B=>'b', C=>'c', D=>'d', E=>'e', F=>'f', G=>'g', H=>'h',
        I=>'i', J=>'j', K=>'k', L=>'l', M=>'m', N=>'n', O=>'o', P=>'p',
        Q=>'q', R=>'r', S=>'s', T=>'t', U=>'u', V=>'v', W=>'w', X=>'x',
        Y=>'y', Z=>'z',
        _ => return None,
    }.into()
}