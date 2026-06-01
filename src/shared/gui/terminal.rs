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
    scroll_top:      usize,
    scroll_bottom:   usize,
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
            scroll_top:      0,
            scroll_bottom:   rows - 1,
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
        self.cursor_row    = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col    = self.cursor_col.min(cols.saturating_sub(1));
        self.scroll_top    = 0;
        self.scroll_bottom = rows - 1;
    }

    fn scroll_up(&mut self) {
        if self.scroll_top == 0 && self.scroll_bottom == self.rows - 1 {
            let evicted = self.grid.remove(0);
            if let Some(ref sink) = self.scrollback_sink {
                sink.lock().push(evicted);
            }
            self.grid.push(vec![Cell::default(); self.cols]);
        } else {
            self.grid.remove(self.scroll_top);
            self.grid.insert(self.scroll_bottom, vec![Cell::default(); self.cols]);
            self.grid.truncate(self.rows);
        }
    }

    fn scroll_down(&mut self) {
        if self.scroll_bottom < self.grid.len() {
            self.grid.remove(self.scroll_bottom);
        }
        self.grid.insert(self.scroll_top, vec![Cell::default(); self.cols]);
        self.grid.truncate(self.rows);
    }

    fn write_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col  = 0;
            self.cursor_row += 1;
        }
        if self.cursor_row > self.scroll_bottom {
            self.scroll_up();
            self.cursor_row = self.scroll_bottom;
        } else if self.cursor_row >= self.rows {
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
                    if let (Some(&r), Some(&g), Some(&b)) =
                        (params.get(i+2), params.get(i+3), params.get(i+4))
                    {
                        self.current_fg = egui::Color32::from_rgb(r as u8, g as u8, b as u8);
                        i += 4;
                    }
                }
                39 => self.current_fg = COLOR_FG,
                40..=47  => self.current_bg = ANSI_COLORS[(params[i] - 40) as usize],
                48 if params.get(i+1) == Some(&5) => {
                    if let Some(&idx) = params.get(i+2) { self.current_bg = ansi256(idx as u8); i += 2; }
                }
                48 if params.get(i+1) == Some(&2) => {
                    if let (Some(&r), Some(&g), Some(&b)) =
                        (params.get(i+2), params.get(i+3), params.get(i+4))
                    {
                        self.current_bg = egui::Color32::from_rgb(r as u8, g as u8, b as u8);
                        i += 4;
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
                if self.cursor_row == self.scroll_bottom {
                    self.scroll_up();
                } else {
                    self.cursor_row = (self.cursor_row + 1).min(self.rows - 1);
                }
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
            'A' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            'C' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
            }
            'D' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                let row = (p.first().copied().unwrap_or(1).max(1) - 1) as usize;
                let col = (p.get(1).copied().unwrap_or(1).max(1) - 1) as usize;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            'G' => {
                let col = (p.first().copied().unwrap_or(1).max(1) - 1) as usize;
                self.cursor_col = col.min(self.cols - 1);
            }
            'd' => {
                let row = (p.first().copied().unwrap_or(1).max(1) - 1) as usize;
                self.cursor_row = row.min(self.rows - 1);
            }
            'J' => match p.first().copied().unwrap_or(0) {
                0 => {
                    for col in self.cursor_col..self.cols {
                        self.grid[self.cursor_row][col] = Cell::default();
                    }
                    for row in (self.cursor_row + 1)..self.rows {
                        self.grid[row] = vec![Cell::default(); self.cols];
                    }
                }
                1 => {
                    for col in 0..=self.cursor_col {
                        self.grid[self.cursor_row][col] = Cell::default();
                    }
                    for row in 0..self.cursor_row {
                        self.grid[row] = vec![Cell::default(); self.cols];
                    }
                }
                2 | 3 => {
                    for row in &mut self.grid {
                        *row = vec![Cell::default(); self.cols];
                    }
                }
                _ => {}
            },
            'K' => match p.first().copied().unwrap_or(0) {
                0 => {
                    for col in self.cursor_col..self.cols {
                        self.grid[self.cursor_row][col] = Cell::default();
                    }
                }
                1 => {
                    for col in 0..=self.cursor_col {
                        self.grid[self.cursor_row][col] = Cell::default();
                    }
                }
                2 => {
                    self.grid[self.cursor_row] = vec![Cell::default(); self.cols];
                }
                _ => {}
            },
            'X' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let row = &mut self.grid[self.cursor_row];
                for col in self.cursor_col..(self.cursor_col + n).min(self.cols) {
                    row[col] = Cell::default();
                }
            }
            'm' => self.apply_sgr(&p),
            's' => { self.saved_row = self.cursor_row; self.saved_col = self.cursor_col; }
            'u' => { self.cursor_row = self.saved_row; self.cursor_col = self.saved_col; }
            'r' => {
                let top    = (p.first().copied().unwrap_or(1).max(1) - 1) as usize;
                let bottom = (p.get(1).copied().unwrap_or(self.rows as i64).max(1) - 1) as usize;
                self.scroll_top    = top.min(self.rows - 1);
                self.scroll_bottom = bottom.min(self.rows - 1);
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            'S' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n { self.scroll_up(); }
            }
            'T' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n { self.scroll_down(); }
            }
            'L' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.grid.insert(self.cursor_row, vec![Cell::default(); self.cols]);
                    self.grid.truncate(self.rows);
                }
            }
            'M' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    if self.cursor_row < self.grid.len() {
                        self.grid.remove(self.cursor_row);
                    }
                    self.grid.push(vec![Cell::default(); self.cols]);
                }
            }
            '@' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let row = &mut self.grid[self.cursor_row];
                for _ in 0..n {
                    if row.len() > self.cursor_col {
                        row.insert(self.cursor_col, Cell::default());
                        row.truncate(self.cols);
                    }
                }
            }
            'P' => {
                let n = p.first().copied().unwrap_or(1).max(1) as usize;
                let row = &mut self.grid[self.cursor_row];
                for _ in 0..n {
                    if self.cursor_col < row.len() {
                        row.remove(self.cursor_col);
                        row.push(Cell::default());
                    }
                }
            }
            'b' => {}
            'h' | 'l' => {}
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
            b'M' => {
                if self.cursor_row == self.scroll_top {
                    self.scroll_down();
                } else {
                    self.cursor_row = self.cursor_row.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

// ── SSH / kubectl session ─────────────────────────────────────────────────────

pub struct SshSession {
    pub writer:   Arc<Mutex<Box<dyn Write + Send>>>,
    _pty_pair:    portable_pty::PtyPair,
}

impl SshSession {
    /// Notify the local PTY master of a size change (sends SIGWINCH to the
    /// local kubectl process). Also returns the new dimensions so the caller
    /// can send an in-band resize notification to the shell inside the pod.
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self._pty_pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width:  0,
            pixel_height: 0,
        });
    }
}

pub fn spawn_kubectl(
    deployment_name: &str,
    rows:      u16,
    cols:      u16,
    performer: Arc<Mutex<TermPerformer>>,
    ctx:       egui::Context,
) -> Result<SshSession, Box<dyn std::error::Error>> {

    // ── Find the first running pod matching the deployment prefix ─────────────
    let pod_output = std::process::Command::new("kubectl")
        .args([
            "get", "pods",
            "--field-selector=status.phase=Running",
            "-o", "jsonpath={range .items[*]}{.metadata.name}{'\\n'}{end}",
        ])
        .output()?;

    let stdout = String::from_utf8(pod_output.stdout)?;
    let prefix = deployment_name.to_lowercase().replace('_', "-");

    let pod_name = stdout
        .lines()
        .find(|line| line.starts_with(&prefix))
        .ok_or_else(|| format!("No running pod found with prefix '{}'", prefix))?
        .to_string();

    // ── Open a local PTY ──────────────────────────────────────────────────────
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width:  0,
        pixel_height: 0,
    })?;

    // ── Build the kubectl exec command ────────────────────────────────────────
    //
    // Core problem: `kubectl exec -i` connects stdin/stdout as plain pipes.
    // Inside the pod there is no real TTY, so:
    //   • stty fails silently (ENOTTY — can't ioctl a pipe)
    //   • the kernel keeps stdin in canonical/line-buffered mode
    //   • nano/htop/vim receive keypresses only after Enter
    //
    // Fix: use `script -q -c '...' /dev/null` as a PTY wrapper.
    // `script` allocates a real pseudo-TTY inside the pod and execs the
    // shell inside it.  Everything that follows now has a real tty:
    //   • stty / TIOCSWINSZ work correctly
    //   • the kernel switches to raw mode for interactive apps
    //   • SIGWINCH is delivered on resize
    //
    // We inline the size + env setup into the -c command so the very first
    // prompt already knows the correct dimensions — no deferred init needed.
    let shell_cmd = format!(
        "export COLUMNS={cols} LINES={rows} TERM=xterm-256color PYTHONDONTWRITEBYTECODE=1; \
         stty rows {rows} cols {cols}; \
         exec /bin/sh -i",
        cols = cols,
        rows = rows,
    );

    let mut cmd = CommandBuilder::new("kubectl");
    cmd.arg("exec");
    cmd.arg("-i");
    cmd.arg(&pod_name);
    cmd.arg("-c");
    cmd.arg(&prefix);
    cmd.arg("--");
    cmd.arg("script");
    cmd.arg("-q");
    cmd.arg("-c");
    cmd.arg(&shell_cmd);
    cmd.arg("/dev/null");

    // ── Spawn inside the PTY slave ────────────────────────────────────────────
    let _child     = pair.slave.spawn_command(cmd)?;
    let writer     = pair.master.take_writer()?;
    let mut reader = pair.master.try_clone_reader()?;

    // No deferred init thread needed — size and shell mode are configured
    // inside shell_cmd before `sh -i` starts.
    let writer_arc = Arc::new(Mutex::new(writer));

    // ── Background reader thread → VTE parser ────────────────────────────────
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

    Ok(SshSession { writer: writer_arc, _pty_pair: pair })
}

// ── Key → char helper ─────────────────────────────────────────────────────────

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