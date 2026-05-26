use eframe::egui;
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};
use std::collections::HashMap;
use tokio::time::sleep;
use std::time::Duration;

use MetadataService::{
    apis::{
        configuration::Configuration as MetadataConfiguration,
        default_api::{metadata_get_services_and_envs, MetadataGetServicesAndEnvsParams},
    },
    get_configuration as get_metadata_configuration,
};
use ginger_shared_rs::utils::get_token_from_file_storage;

use super::colors::{COLOR_BG, COLOR_BORDER, COLOR_CYAN, COLOR_SIDEBAR_BG, COLOR_TAB_ACTIVE};
use super::panels::{
    draw_info_strip, draw_logs_pane, draw_service_list, draw_statusbar, draw_tab_bar,
    draw_terminal_pane, draw_titlebar, TabBarAction,
};
use super::terminal::{spawn_ssh, TermPerformer};
use super::types::{AppState, K8sService, RightPane, TermState};
use crate::shared::ui::kubernetes::{
    get_k8s_deployments, get_pod_logs, is_ejected, meta_to_deployment_name,
};

// ── Background channel messages ───────────────────────────────────────────────

enum BgMsg {
    /// Initial metadata fetch completed — full service list.
    Services(Vec<K8sService>),
    /// Periodic k8s status poll — map of deployment_name → (status, ready).
    K8sStatuses(HashMap<String, (String, String)>),
    /// Ejected flag update for one service by index.
    EjectedFlag { idx: usize, ejected: bool },
    /// Fresh log lines for the currently-selected service.
    Logs(Vec<String>),
    /// Metadata fetch failed.
    Error(String),
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    state:           AppState,
    rx:              mpsc::Receiver<BgMsg>,
    /// Sender kept so we can hand clones to new background tasks.
    tx:              mpsc::Sender<BgMsg>,
    loading:         bool,
    /// egui context — needed to spawn per-selection log tasks.
    ctx:             egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // ── Fonts ─────────────────────────────────────────────────────────────
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "mono".to_owned(),
            egui::FontData::from_static(include_bytes!(
                "../../../assets/JetBrainsMono-Regular.ttf"
            )),
        );
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "mono".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        let (tx, rx) = mpsc::channel::<BgMsg>();
        let ctx      = cc.egui_ctx.clone();

        // ── 1. Metadata fetch (one-shot) ──────────────────────────────────────
        {
            let tx  = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio rt");

                rt.block_on(async move {
                    let token           = get_token_from_file_storage();
                    let metadata_config = get_metadata_configuration(Some(token));

                    match metadata_get_services_and_envs(
                        &metadata_config,
                        MetadataGetServicesAndEnvsParams {
                            page_number: Some("1".to_string()),
                            page_size:   Some("100".to_string()),
                            org_id:      "ginger-society".to_string(),
                        },
                    )
                    .await
                    {
                        Err(e) => { let _ = tx.send(BgMsg::Error(format!("{e:?}"))); }
                        Ok(raw) => {
                            let services = raw.iter().map(|s| {
                                let meta_name       = s.identifier.to_string();
                                let deployment_name = meta_to_deployment_name(&meta_name);
                                let lang = s.lang
                                    .as_ref()
                                    .and_then(|l| l.as_ref())
                                    .cloned();
                                let ssh_host = Some(format!(
                                    "dev@{}-local",
                                    deployment_name.to_lowercase().replace('_', "-"),
                                ));
                                K8sService {
                                    meta_name,
                                    organization_id: s.organization_id.clone(),
                                    deployment_name: Some(deployment_name),
                                    status:  "Unknown".into(),
                                    ready:   "–".into(),
                                    lang,
                                    ejected: false,
                                    ssh_host,
                                }
                            }).collect();
                            let _ = tx.send(BgMsg::Services(services));
                        }
                    }
                    ctx.request_repaint();
                });
            });
        }

        // ── 2. k8s status poller (every 5 s) ─────────────────────────────────
        {
            let tx  = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio rt");

                rt.block_on(async move {
                    loop {
                        let deployments = get_k8s_deployments().await;
                        let _ = tx.send(BgMsg::K8sStatuses(deployments));
                        ctx.request_repaint();
                        sleep(Duration::from_secs(5)).await;
                    }
                });
            });
        }

        App {
            state:   AppState::new(13.0, vec![]),
            rx,
            tx,
            loading: true,
            ctx,
        }
    }

    // ── Spawn a one-shot log fetch + ejected-flag check for service `idx` ─────

    fn spawn_service_refresh(&self, idx: usize, deployment_name: String) {
        let tx  = self.tx.clone();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                let ejected = is_ejected(&deployment_name).await;
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });

                // Only fetch logs if not ejected.
                if !ejected {
                    let logs = get_pod_logs(&deployment_name).await;
                    let _ = tx.send(BgMsg::Logs(logs));
                }

                ctx.request_repaint();
            });
        });
    }

    // ── Ejected-only check (no log fetch) ─────────────────────────────────────

    fn spawn_ejected_check(&self, idx: usize, deployment_name: String) {
        let tx  = self.tx.clone();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                let ejected = is_ejected(&deployment_name).await;
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
                ctx.request_repaint();
            });
        });
    }

    // ── Recurring log poller — only called for non-ejected services ───────────

    fn spawn_log_poller(&self, deployment_name: String) {
        let tx  = self.tx.clone();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                loop {
                    let logs = get_pod_logs(&deployment_name).await;
                    // If the receiver is gone (app closed / service changed) stop.
                    if tx.send(BgMsg::Logs(logs)).is_err() { break; }
                    ctx.request_repaint();
                    sleep(Duration::from_secs(2)).await;
                }
            });
        });
    }

    // ── Open a new terminal tab and connect it ────────────────────────────────

    fn open_and_connect_term(&mut self, ctx: &egui::Context) {
        let rows = 24;
        let cols = 80;

        let tab_idx = match self.state.open_term_tab(rows, cols) {
            Some(i) => i,
            None    => return,
        };

        self.state.right_pane  = RightPane::TerminalTab(tab_idx);
        self.state.active_term = tab_idx;
        self.connect_tab(tab_idx, ctx);
    }

    // ── Connect one specific tab ──────────────────────────────────────────────

    fn connect_tab(&mut self, tab_idx: usize, ctx: &egui::Context) {
        let tab = match self.state.term_tabs.get(tab_idx) {
            Some(t) => t,
            None    => return,
        };
        let svc_idx = tab.service_idx;
        let rows    = tab.term_rows as u16;
        let cols    = tab.term_cols as u16;
        let host    = match self.state.services.get(svc_idx).and_then(|s| s.ssh_host.as_ref()) {
            Some(h) => h.clone(),
            None => {
                if let Some(t) = self.state.term_tabs.get_mut(tab_idx) {
                    t.state = TermState::Error("No SSH host configured for this service".into());
                }
                return;
            }
        };

        let sink: super::terminal::ScrollbackSink = Arc::new(Mutex::new(Vec::new()));
        let performer = Arc::new(Mutex::new(
            TermPerformer::new(rows as usize, cols as usize)
                .with_sink(Arc::clone(&sink)),
        ));

        let tab            = &mut self.state.term_tabs[tab_idx];
        tab.performer      = Arc::clone(&performer);
        tab.scrollback_arc = Some(Arc::clone(&sink));

        match spawn_ssh(&host, rows, cols, performer, ctx.clone()) {
            Ok(session) => tab.state = TermState::Connected(session),
            Err(e)      => tab.state = TermState::Error(e.to_string()),
        }
    }

    // ── Service selection ─────────────────────────────────────────────────────

    fn select_service(&mut self, idx: usize) {
        let svc             = &self.state.services[idx];
        let meta_name       = svc.meta_name.clone();
        let deployment_name = svc.deployment_name.clone();
        let ejected         = svc.ejected;

        self.state.selected_idx = idx;
        self.state.right_pane   = RightPane::Logs;

        if ejected {
            self.state.logs = vec![
                format!("⚡ {} is ejected — running in dev mode.", meta_name),
                "No application logs available.".into(),
                "Use a Terminal tab to interact with the container.".into(),
            ];
            // Still check the ejected flag in case it changed, but skip logs.
            if let Some(dep) = deployment_name {
                self.spawn_ejected_check(idx, dep);
            }
        } else {
            self.state.logs = vec![format!("Fetching logs for {}…", meta_name)];
            if let Some(dep) = deployment_name {
                self.spawn_service_refresh(idx, dep.clone());
                self.spawn_log_poller(dep);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Drain the background channel (process ALL pending messages) ───────
        loop {
            match self.rx.try_recv() {
                Ok(BgMsg::Services(svcs)) => {
                    self.state.services     = svcs;
                    self.state.selected_idx = 0;
                    self.loading            = false;

                    // Kick off refresh for service 0, respecting ejected state.
                    if let Some(svc) = self.state.services.first() {
                        let dep     = svc.deployment_name.clone();
                        let ejected = svc.ejected;
                        if let Some(dep) = dep {
                            self.spawn_service_refresh(0, dep.clone());
                            if !ejected {
                                self.spawn_log_poller(dep);
                            }
                        }
                    }
                }
                Ok(BgMsg::K8sStatuses(deployments)) => {
                    for svc in &mut self.state.services {
                        if let Some(ref dep) = svc.deployment_name {
                            if let Some((status, ready)) = deployments.get(dep) {
                                svc.status = status.clone();
                                svc.ready  = ready.clone();
                            } else {
                                svc.status = "Not deployed".into();
                                svc.ready  = "–".into();
                            }
                        }
                    }
                }
                Ok(BgMsg::EjectedFlag { idx, ejected }) => {
                    if let Some(svc) = self.state.services.get_mut(idx) {
                        svc.ejected = ejected;
                    }
                    // If this is the selected service and it's ejected, replace
                    // any stale "Fetching…" or real log lines with the dev-mode notice.
                    if idx == self.state.selected_idx && ejected
                        && self.state.right_pane == RightPane::Logs
                    {
                        let name = self.state.services
                            .get(idx)
                            .map(|s| s.meta_name.as_str())
                            .unwrap_or("this service");
                        self.state.logs = vec![
                            format!("⚡ {} is ejected — running in dev mode.", name),
                            "No application logs available.".into(),
                            "Use a Terminal tab to interact with the container.".into(),
                        ];
                    }
                }
                Ok(BgMsg::Logs(lines)) => {
                    // Only apply if we're still showing the Logs pane —
                    // avoids clobbering logs with stale results from a
                    // previous selection's poller.
                    if self.state.right_pane == RightPane::Logs {
                        self.state.logs = lines;
                    }
                }
                Ok(BgMsg::Error(e)) => {
                    self.loading    = false;
                    self.state.logs = vec![format!("Failed to load services: {e}")];
                }
                Err(_) => break, // channel empty (or closed)
            }
        }

        // ── Raise window on first frame (tray launch) ─────────────────────────
        if !self.state.raised_on_open {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            self.state.raised_on_open = true;
            ctx.request_repaint();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::Normal,
            ));
        }

        // ── Cursor blink ──────────────────────────────────────────────────────
        let t = ctx.input(|i| i.time);
        if t - self.state.blink_timer > 0.5 {
            self.state.blink       = !self.state.blink;
            self.state.blink_timer = t;
        }
        if matches!(self.state.right_pane, RightPane::TerminalTab(_)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // ── Sync terminal scrollback ──────────────────────────────────────────
        for tab in &mut self.state.term_tabs {
            if let Some(ref sink) = tab.scrollback_arc {
                let mut s = sink.lock();
                tab.scrollback.append(&mut *s);
            }
        }

        // ── Title bar ─────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("titlebar")
            .exact_height(28.0)
            .frame(egui::Frame::none())
            .show(ctx, |ui| draw_titlebar(&self.state, ui, ctx));

        // ── Status bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(20.0)
            .frame(egui::Frame::none())
            .show(ctx, |ui| draw_statusbar(&self.state, ui));

        // ── Sidebar ───────────────────────────────────────────────────────────
        egui::SidePanel::left("sidebar")
            .exact_width(220.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(COLOR_SIDEBAR_BG))
            .show(ctx, |ui| {
                let (hdr, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 22.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(hdr, 0.0, COLOR_SIDEBAR_BG);
                ui.painter().text(
                    egui::pos2(hdr.min.x + 8.0, hdr.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Services",
                    egui::FontId::new(11.0, egui::FontFamily::Monospace),
                    COLOR_CYAN,
                );
                ui.painter().line_segment(
                    [hdr.left_bottom(), hdr.right_bottom()],
                    egui::Stroke::new(0.5, COLOR_BORDER),
                );

                if self.loading {
                    ui.add_space(8.0);
                    ui.colored_label(COLOR_CYAN, "Loading services…");
                } else if self.state.services.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(COLOR_CYAN, "No services found.");
                } else if let Some(new_idx) = draw_service_list(&self.state, ui) {
                    self.select_service(new_idx);
                }
            });

        // ── Main panel ────────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(COLOR_BG))
            .show(ctx, |ui| {
                if self.loading || self.state.services.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(COLOR_CYAN, "Loading services…");
                    });
                    return;
                }

                ui.vertical(|ui| {
                    let strip_action = draw_info_strip(&self.state, ui);
                    if strip_action.eject_clicked {
                        if let Some(svc) = self.state.services.get(self.state.selected_idx) {
                            println!("[eject] would eject: {}", svc.meta_name);
                            // TODO: wire up real eject logic
                        }
                    }
                    if strip_action.open_editor_clicked {
                        if let Some(svc) = self.state.services.get(self.state.selected_idx) {
                            println!("[editor] would open editor for: {}", svc.meta_name);
                            // TODO: spawn `code <path>`
                        }
                    }

                    let action = draw_tab_bar(&self.state, ui);

                    match action {
                        Some(TabBarAction::SwitchToLogs) => {
                            self.state.right_pane = RightPane::Logs;
                        }
                        Some(TabBarAction::SwitchToTerm(i)) => {
                            self.state.right_pane  = RightPane::TerminalTab(i);
                            self.state.active_term = i;
                        }
                        Some(TabBarAction::NewTerm) => {
                            self.open_and_connect_term(ctx);
                        }
                        Some(TabBarAction::CloseTerm(i)) => {
                            self.state.close_term_tab(i);
                        }
                        None => {}
                    }

                    match self.state.right_pane {
                        RightPane::Logs           => draw_logs_pane(&self.state, ui),
                        RightPane::TerminalTab(i) => draw_terminal_pane(&mut self.state, ui, i),
                    }
                });
            });
    }
}