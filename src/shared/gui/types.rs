use eframe::egui;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::colors::{COLOR_DIM, COLOR_RED, COLOR_YELLOW};
use super::terminal::{Cell, ScrollbackSink, SshSession, TermPerformer};
use crate::shared::core::types::{DbSchema, InfraAsCode, Package, K8sService};

pub const MAX_TERM_TABS: usize = 5;


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
    /// Detail view for a package at the given index in `AppState::packages`.
    PackageDetail(usize),
    /// Detail view for a DB schema at the given index in `AppState::db_schemas`.
    DbSchemaDetail(usize),
    /// Detail view for the single Infra-as-Code entry.
    IacDetail,
}

// ── Per-terminal-tab state ────────────────────────────────────────────────────

pub enum TermState {
    Idle,
    Connecting,
    Connected(SshSession),
    Error(String),
}

/// One independent terminal session.
pub struct TermTab {
    pub label:          String,
    pub service_idx:    usize,
    pub performer:      Arc<Mutex<TermPerformer>>,
    pub state:          TermState,
    pub scrollback:     Vec<Vec<Cell>>,
    pub scrollback_arc: Option<ScrollbackSink>,
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
    pub packages:        Vec<Package>,
    pub db_schemas:      Vec<DbSchema>,
    /// The single Infra-as-Code entry for this workspace.
    pub iac:             Option<InfraAsCode>,
    pub selected_idx:    usize,
    pub right_pane:      RightPane,
    pub logs:            Vec<String>,
    /// Live logs for the currently selected DB schema deployment (if any).
    pub db_logs:         Vec<String>,
    pub db_containers:        Vec<String>,
    pub db_selected_container: Option<String>,
    pub db_log_generation:     u64,
    pub term_tabs:       Vec<TermTab>,
    pub tabs_by_service: HashMap<usize, Vec<TermTab>>,
    pub active_term:     usize,
    pub log_generation:  u64,
    pub font_size:       f32,
    pub cell_w:          f32,
    pub cell_h:          f32,
    pub blink:           bool,
    pub blink_timer:     f64,
    pub raised_on_open:  bool,

    /// Cancels the currently running service-log stream.
    pub log_cancel:      CancellationToken,

    /// Cancels the currently running DB-schema-log stream.
    pub db_log_cancel:   CancellationToken,
}

impl AppState {
    pub fn new(font_size: f32, services: Vec<K8sService>) -> Self {
        AppState {
            services,
            packages:        Vec::new(),
            db_schemas:      Vec::new(),
            iac:             None,
            selected_idx:    0,
            right_pane:      RightPane::Logs,
            logs:            vec!["Fetching logs…".into()],
            db_logs:         vec!["Select a DB schema to view logs…".into()],
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
            db_containers:         Vec::new(),
            db_selected_container: None,
            db_log_generation:     0,
            log_cancel:            CancellationToken::new(),
            db_log_cancel:         CancellationToken::new(),
        }
    }

    // ── Cancel helpers ────────────────────────────────────────────────────────

    pub fn new_log_cancel(&mut self) -> CancellationToken {
        self.log_cancel.cancel();
        self.log_cancel = CancellationToken::new();
        self.log_cancel.clone()
    }

    pub fn new_db_log_cancel(&mut self) -> CancellationToken {
        self.db_log_cancel.cancel();
        self.db_log_cancel = CancellationToken::new();
        self.db_log_cancel.clone()
    }

    // ── Terminal tab helpers ───────────────────────────────────────────────────

    pub fn open_term_tab_with_label(
        &mut self,
        rows:  usize,
        cols:  usize,
        label: String,
    ) -> Option<usize> {
        if self.term_tabs.len() >= MAX_TERM_TABS { return None; }
        let svc_idx = self.selected_idx;
        self.term_tabs.push(TermTab {
            label,
            service_idx:  svc_idx,
            term_rows:    rows,
            term_cols:    cols,
            state:        TermState::Idle,
            performer:    Arc::new(Mutex::new(TermPerformer::new(rows, cols))),
            scrollback:   vec![],
            scrollback_arc: None,
            scroll_offset: 0,
            sel_start:    None,
            sel_end:      None,
            dragging:     false,
        });
        Some(self.term_tabs.len() - 1)
    }

    pub fn open_term_tab(&mut self, rows: usize, cols: usize) -> Option<usize> {
        let label = "terminal".to_string();
        self.open_term_tab_with_label(rows, cols, label)
    }

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

    pub fn switch_service(&mut self, new_idx: usize) -> u64 {
        let old_idx  = self.selected_idx;
        let old_tabs = std::mem::take(&mut self.term_tabs);
        if !old_tabs.is_empty() {
            self.tabs_by_service.insert(old_idx, old_tabs);
        } else {
            self.tabs_by_service.remove(&old_idx);
        }

        self.term_tabs     = self.tabs_by_service.remove(&new_idx).unwrap_or_default();
        self.selected_idx  = new_idx;
        self.log_generation += 1;

        if self.term_tabs.is_empty() {
            self.right_pane  = RightPane::Logs;
            self.active_term = 0;
        } else {
            self.active_term = self.active_term.min(self.term_tabs.len() - 1);
            self.right_pane  = RightPane::TerminalTab(self.active_term);
        }

        self.log_generation
    }
}