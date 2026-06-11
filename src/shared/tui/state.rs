//! All mutable state for the TUI loop lives here.
//! `mod.rs` creates a `TuiState`, then passes `&mut TuiState` to
//! `input::handle_key` and `panels::draw`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::shared::core::types::{DbSchema, K8sService, Package};
use super::types::{Focus, Popup, SidebarItem};

pub struct TuiState {
    // ── Data (shared with background tasks) ──────────────────────────────────
    pub services:   Arc<Mutex<Vec<K8sService>>>,
    pub packages:   Arc<Mutex<Vec<Package>>>,
    pub db_schemas: Arc<Mutex<Vec<DbSchema>>>,

    // ── Log buffers ───────────────────────────────────────────────────────────
    pub logs:    Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub db_logs: Arc<Mutex<Option<Vec<String>>>>,

    // ── Log-stream generation counters ───────────────────────────────────────
    /// Incremented each time we switch to a new service/container stream.
    /// Background messages from an older generation are silently ignored.
    pub svc_log_generation: u64,
    pub db_log_generation:  u64,
    pub db_log_schema:      Option<usize>,

    // ── Container state ───────────────────────────────────────────────────────
    /// Map from service-index → chosen container index.
    pub container_selection: HashMap<usize, usize>,
    pub db_containers:           Vec<String>,
    pub db_selected_container:   Option<String>,

    // ── Navigation / focus ────────────────────────────────────────────────────
    pub focus:        Focus,
    pub sidebar_item: SidebarItem,

    // ── Scroll ────────────────────────────────────────────────────────────────
    pub auto_scroll:        bool,
    pub scroll_offset:      usize,
    pub db_last_max_scroll: usize,
    pub sidebar_scroll:     usize,

    // ── Popup ─────────────────────────────────────────────────────────────────
    pub popup: Option<Popup>,
}

impl TuiState {
    pub fn new(
        services:   Vec<K8sService>,
        packages:   Vec<Package>,
        db_schemas: Vec<DbSchema>,
    ) -> Self {
        Self {
            services:   Arc::new(Mutex::new(services)),
            packages:   Arc::new(Mutex::new(packages)),
            db_schemas: Arc::new(Mutex::new(db_schemas)),
            logs:    Arc::new(Mutex::new(HashMap::new())),
            db_logs: Arc::new(Mutex::new(None)),
            svc_log_generation: 0,
            db_log_generation:  0,
            db_log_schema:      None,
            container_selection: HashMap::new(),
            db_containers:           Vec::new(),
            db_selected_container:   None,
            focus:        Focus::Sidebar,
            sidebar_item: SidebarItem::Service(0),
            auto_scroll:        true,
            scroll_offset:      0,
            db_last_max_scroll: 0,
            sidebar_scroll:     0,
            popup: None,
        }
    }
}