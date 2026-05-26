use eframe::egui;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use super::colors::{COLOR_DIM, COLOR_RED, COLOR_YELLOW};
use super::terminal::{Cell, ScrollbackSink, SshSession, TermPerformer};

pub const MAX_TERM_TABS: usize = 5;

// ── K8s service ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct K8sService {
    pub meta_name:       String,
    pub organization_id: String,
    pub deployment_name: Option<String>,
    pub status:          String,
    pub ready:           String,
    pub lang:            Option<String>,
    pub ejected:         bool,
    pub ssh_host:        Option<String>,
}

impl K8sService {
    pub fn status_color(&self) -> egui::Color32 {
        match self.status.as_str() {
            "Running"      => egui::Color32::from_rgb(39, 201, 63),
            "Degraded"     => COLOR_YELLOW,
            "Pending"      => COLOR_YELLOW,
            "Not deployed" => COLOR_DIM,
            _              => COLOR_RED,
        }
    }

    pub fn status_dot(&self) -> &'static str {
        match self.status.as_str() {
            "Running"      => "●",
            "Degraded"     => "◐",
            "Pending"      => "○",
            "Not deployed" => "·",
            _              => "✗",
        }
    }
}

// ── Right-pane tab ────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum RightPane {
    Logs,
    /// Index into `AppState::term_tabs`.
    TerminalTab(usize),
}

// ── Per-terminal-tab state ────────────────────────────────────────────────────

pub enum TermState {
    /// Not yet connected; connect lazily on first render.
    Idle,
    Connected(SshSession),
    Error(String),
}

/// One independent terminal session.
pub struct TermTab {
    /// Label shown on the tab chip, e.g. `"iam-admin-fe"` or `"iam-admin-fe #2"`.
    pub label:          String,
    /// Index of the service this terminal is connected to.
    pub service_idx:    usize,
    /// The live VTE grid + cursor.
    pub performer:      Arc<Mutex<TermPerformer>>,
    /// SSH connection state.
    pub state:          TermState,
    /// Rows scrolled off the top of the live grid, accumulated here each frame
    /// from `scrollback_arc`.
    pub scrollback:     Vec<Vec<Cell>>,
    /// Arc to the same `Vec` that `TermPerformer::scroll_up` pushes into.
    /// `app.rs` drains it into `scrollback` every frame.
    pub scrollback_arc: Option<ScrollbackSink>,
    /// 0 = bottom (live); higher = further into scrollback history.
    pub scroll_offset:  usize,
    pub term_rows:      usize,
    pub term_cols:      usize,
    pub sel_start:      Option<(usize, usize)>,
    pub sel_end:        Option<(usize, usize)>,
    pub dragging:       bool,
}

impl TermTab {
    pub fn new(label: String, service_idx: usize, rows: usize, cols: usize) -> Self {
        TermTab {
            label,
            service_idx,
            performer:      Arc::new(Mutex::new(TermPerformer::new(rows, cols))),
            state:          TermState::Idle,
            scrollback:     Vec::new(),
            scrollback_arc: None,
            scroll_offset:  0,
            term_rows:      rows,
            term_cols:      cols,
            sel_start:      None,
            sel_end:        None,
            dragging:       false,
        }
    }
}

// ── App-wide state ────────────────────────────────────────────────────────────

pub struct AppState {
    pub services:        Vec<K8sService>,
    pub selected_idx:    usize,
    pub right_pane:      RightPane,
    /// Log lines for the selected service.
    pub logs:            Vec<String>,
    /// Terminal tabs for the *currently selected* service.
    /// Swapped in/out of `tabs_by_service` on every service switch.
    pub term_tabs:       Vec<TermTab>,
    /// Saved terminal tabs for every service index that has ever had tabs opened.
    pub tabs_by_service: HashMap<usize, Vec<TermTab>>,
    /// Index of the currently active terminal tab.
    pub active_term:     usize,
    /// Incremented on every service switch so stale log pollers can be ignored.
    pub log_generation:  u64,
    pub font_size:       f32,
    pub cell_w:          f32,
    pub cell_h:          f32,
    pub blink:           bool,
    pub blink_timer:     f64,
    pub raised_on_open:  bool,
}

impl AppState {
    pub fn new(font_size: f32, services: Vec<K8sService>) -> Self {
        AppState {
            services,
            selected_idx:    0,
            right_pane:      RightPane::Logs,
            logs:            vec!["Fetching logs…".into()],
            term_tabs:       Vec::new(),
            tabs_by_service: HashMap::new(),
            active_term:     0,
            log_generation:  0,
            font_size,
            cell_w:          font_size * 0.601,
            cell_h:          font_size * 1.4,
            blink:           true,
            blink_timer:     0.0,
            raised_on_open:  false,
        }
    }

    /// Allocate a new `TermTab` for the currently selected service.
    /// Returns the index of the new tab, or `None` when the cap is reached.
    pub fn open_term_tab(&mut self, rows: usize, cols: usize) -> Option<usize> {
        if self.term_tabs.len() >= MAX_TERM_TABS {
            return None;
        }
        let svc   = &self.services[self.selected_idx];
        let n     = self.term_tabs.iter()
            .filter(|t| t.service_idx == self.selected_idx)
            .count() + 1;
        let short = svc.meta_name.split('/').last().unwrap_or(&svc.meta_name);
        let label = if n == 1 { short.to_string() } else { format!("{} #{}", short, n) };

        self.term_tabs.push(TermTab::new(label, self.selected_idx, rows, cols));
        Some(self.term_tabs.len() - 1)
    }

    /// Close the tab at `idx`, fix indices, and update `right_pane`.
    pub fn close_term_tab(&mut self, idx: usize) {
        if idx >= self.term_tabs.len() { return; }
        self.term_tabs.remove(idx);

        if self.term_tabs.is_empty() {
            self.right_pane  = RightPane::Logs;
            self.active_term = 0;
        } else {
            self.active_term = self.active_term.min(self.term_tabs.len() - 1);
            if let RightPane::TerminalTab(ref mut i) = self.right_pane {
                if *i >= self.term_tabs.len() {
                    *i = self.term_tabs.len() - 1;
                } else if *i > idx {
                    *i -= 1;
                }
                self.active_term = *i;
            }
        }
    }

    /// Swap out the current service's tabs and swap in `new_idx`'s tabs.
    /// Returns the generation number that log pollers should use for `new_idx`.
    pub fn switch_service(&mut self, new_idx: usize) -> u64 {
        // Save the current service's tab list.
        let old_idx  = self.selected_idx;
        let old_tabs = std::mem::take(&mut self.term_tabs);
        if !old_tabs.is_empty() {
            self.tabs_by_service.insert(old_idx, old_tabs);
        } else {
            // Remove stale empty entry if it existed.
            self.tabs_by_service.remove(&old_idx);
        }

        // Restore the new service's tab list (or start fresh).
        self.term_tabs = self.tabs_by_service.remove(&new_idx).unwrap_or_default();

        self.selected_idx  = new_idx;
        self.log_generation += 1;

        // Decide which pane to show.
        if self.term_tabs.is_empty() {
            self.right_pane  = RightPane::Logs;
            self.active_term = 0;
        } else {
            // Restore to the last active tab (clamp in case tabs were closed).
            self.active_term = self.active_term.min(self.term_tabs.len() - 1);
            self.right_pane  = RightPane::TerminalTab(self.active_term);
        }

        self.log_generation
    }
}