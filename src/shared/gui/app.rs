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
use super::terminal::{spawn_kubectl, TermPerformer};
use super::types::{AppState, K8sService, RightPane, TermState};
use crate::shared::ui::kubernetes::{
    get_k8s_deployments, get_pod_logs, is_ejected, meta_to_deployment_name,
};
use crate::shared::ui::eject::{eject, uneject};

// ── Background channel messages ───────────────────────────────────────────────

enum BgMsg {
    /// Initial metadata fetch completed — full service list.
    Services(Vec<K8sService>),
    /// Periodic k8s status poll — map of deployment_name → (status, ready).
    K8sStatuses(HashMap<String, (String, String)>),
    /// Ejected flag update for one service by index.
    EjectedFlag { idx: usize, ejected: bool },
    /// Fresh log lines for the currently-selected service.
    /// `generation` must match `AppState::log_generation` to be applied;
    /// stale results from old pollers are silently dropped.
    Logs { lines: Vec<String>, generation: u64 },
    /// Metadata fetch failed.
    Error(String),
    /// Result of an eject or uneject operation.
    EjectResult { success: bool, message: String, idx: usize },
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    state:    AppState,
    rx:       mpsc::Receiver<BgMsg>,
    /// Sender kept so we can hand clones to new background tasks.
    tx:       mpsc::Sender<BgMsg>,
    loading:  bool,
    /// Set while an eject/uneject operation is in flight.
    /// Contains a short human-readable description shown in the info strip.
    ejecting: Option<String>,
    /// egui context — needed to spawn per-selection log tasks.
    ctx:      egui::Context,
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
                                let pod_name = Some(deployment_name.to_lowercase().replace('_', "-"));

                                K8sService {
                                    meta_name,
                                    organization_id: s.organization_id.clone(),
                                    deployment_name: Some(deployment_name),
                                    status:  "Unknown".into(),
                                    ready:   "–".into(),
                                    lang,
                                    ejected: false,
                                    ssh_host: pod_name,
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
            state:    AppState::new(13.0, vec![]),
            rx,
            tx,
            loading:  true,
            ejecting: None,
            ctx,
        }
    }

    // ── Spawn ejected check, then conditionally start log poller ─────────────
    //
    // This is the ONLY place a log poller is ever started.  By serialising the
    // ejected check before the first poll we guarantee that:
    //   • an ejected service never gets pod-log lines written to the log view, and
    //   • the poller is never racing the ejected flag message.
    //
    // `generation` is captured at spawn time so that if the user switches to a
    // different service before results arrive, every message from this task is
    // silently discarded by the receiver.

    fn spawn_service_refresh(&self, idx: usize, deployment_name: String, generation: u64) {
        let tx  = self.tx.clone();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                // ── Step 1: ejected check (always first, no racing) ───────────
                let ejected = is_ejected(&deployment_name).await;
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
                ctx.request_repaint();

                if ejected {
                    // Ejected — do not fetch logs at all.  The EjectedFlag
                    // message above will update the log view with the dev-mode
                    // notice via the existing handler in update().
                    return;
                }

                // ── Step 2: first log fetch ───────────────────────────────────
                let lines = get_pod_logs(&deployment_name).await;
                let _ = tx.send(BgMsg::Logs { lines, generation });
                ctx.request_repaint();

                // ── Step 3: recurring poll (only for non-ejected services) ────
                loop {
                    sleep(Duration::from_secs(2)).await;
                    let lines = get_pod_logs(&deployment_name).await;
                    if tx.send(BgMsg::Logs { lines, generation }).is_err() { break; }
                    ctx.request_repaint();
                }
            });
        });
    }


    // ── Bulk ejected check for all services (sidebar badges) ─────────────────
    //
    // Spawns ONE thread that checks every service sequentially and sends an
    // EjectedFlag for each.  Called once after the initial metadata fetch so
    // the sidebar [EJECTED] badges appear without requiring a click.
    // Service 0 is skipped because spawn_service_refresh already covers it.

    fn spawn_bulk_ejected_check(&self, services: Vec<(usize, String)>) {
        let tx  = self.tx.clone();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                for (idx, deployment_name) in services {
                    let ejected = is_ejected(&deployment_name).await;
                    let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
                    ctx.request_repaint();
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


        let dep_name = match self.state.services.get(svc_idx)
            .and_then(|s| s.deployment_name.as_ref())
        {
            Some(d) => d.clone(),
            None => {
                if let Some(t) = self.state.term_tabs.get_mut(tab_idx) {
                    t.state = TermState::Error("No deployment name for this service".into());
                }
                return;
            }
        };

        match spawn_kubectl(&dep_name, rows, cols, performer, ctx.clone()) {
            Ok(session) => tab.state = TermState::Connected(session),
            Err(e)      => tab.state = TermState::Error(e.to_string()),
        }
    }

    // ── Service selection ─────────────────────────────────────────────────────
    //
    // Uses `AppState::switch_service` to atomically swap tab lists and bump
    // the log generation, then kicks off the appropriate background tasks.

    fn select_service(&mut self, new_idx: usize) {
        // switch_service saves/restores per-service tabs, bumps log_generation,
        // and returns the new generation number.
        let generation = self.state.switch_service(new_idx);

        let svc             = &self.state.services[new_idx];
        let meta_name       = svc.meta_name.clone();
        let deployment_name = svc.deployment_name.clone();
        let ejected         = svc.ejected;

        if ejected {
            // Show the dev-mode notice immediately (we know it's ejected from
            // a previous check).  spawn_service_refresh will re-confirm and
            // skip starting any log poller.
            self.state.logs = vec![
                format!("⚡ {} is ejected — running in dev mode.", meta_name),
                "No application logs available.".into(),
            ];
        } else {
            self.state.logs = vec![format!("Fetching logs for {}…", meta_name)];
        }

        // Always run refresh — it checks ejected first, then (only if not
        // ejected) fetches logs and starts the recurring poller.
        // This is the single entry-point; no separate spawn_log_poller call.
        if let Some(dep) = deployment_name {
            self.spawn_service_refresh(new_idx, dep, generation);
        }
    }

    // ── Eject the currently-selected service ──────────────────────────────────
    //
    // Guards:
    //   • service must have a deployment_name (i.e. is known to k8s)
    //   • service must have a lang (required by the eject script)
    //   • service must NOT already be ejected
    //
    // Sets self.ejecting so the info strip shows a progress indicator while
    // the operation runs in the background.

    fn run_eject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };

        if svc.ejected { return; }

        let Some(dep)  = svc.deployment_name.clone() else { return };
        let Some(lang) = svc.lang.clone()            else { return };

        let meta_name = format!("{}-{}" , svc.organization_id, svc.meta_name.clone());
        let tx        = self.tx.clone();
        let ctx       = ctx.clone();
        let idx       = self.state.selected_idx;
        let org_id = svc.organization_id.clone();

        self.ejecting = Some(format!("Ejecting {}…", meta_name));

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                let result = eject(&dep, &lang, &meta_name, &org_id ).await;
                let (success, message) = match result {
                    Ok(())  => (true,  format!("✓ Ejected {}", meta_name)),
                    Err(e)  => (false, format!("✗ Eject failed for {}: {}", meta_name, e)),
                };
                let _ = tx.send(BgMsg::EjectResult { success, message, idx });
                ctx.request_repaint();
            });
        });
    }

    // ── Un-eject the currently-selected service ───────────────────────────────
    //
    // Guards:
    //   • service must have a deployment_name
    //   • service must currently be ejected
    //
    // Sets self.ejecting so the info strip shows a progress indicator while
    // the operation runs in the background.

    fn run_uneject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };

        if !svc.ejected { return; }

        let Some(dep) = svc.deployment_name.clone() else { return };

        let meta_name = svc.meta_name.clone();
        let tx        = self.tx.clone();
        let ctx       = ctx.clone();
        let idx       = self.state.selected_idx;

        self.ejecting = Some(format!("Un-ejecting {}…", meta_name));

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt");

            rt.block_on(async move {
                let result = uneject(&dep).await;
                let (success, message) = match result {
                    Ok(())  => (true,  format!("✓ Un-ejected {}", meta_name)),
                    Err(e)  => (false, format!("✗ Un-eject failed for {}: {}", meta_name, e)),
                };
                let _ = tx.send(BgMsg::EjectResult { success, message, idx });
                ctx.request_repaint();
            });
        });
    }

    // ── Open VS Code remote for the currently-selected service ────────────────

    fn open_editor(&self) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };

        if !svc.ejected { return; }

        let Some(dep) = svc.deployment_name.as_ref() else { return };

        let host_alias = format!("{}-local", dep);
        let remote_uri = format!(
            "vscode-remote://ssh-remote+{}/workspace/{}-{}",
            host_alias,
            svc.organization_id,
            dep,
        );

        println!("Opening VS Code: {}", remote_uri);

        std::thread::spawn(move || {
            match std::process::Command::new("code")
                .arg("--folder-uri")
                .arg(&remote_uri)
                .status()
            {
                Ok(s) if s.success() => println!("✓ VS Code launched"),
                Ok(s)                => eprintln!("VS Code exited with status: {}", s),
                Err(e)               => eprintln!("Failed to launch VS Code (`code` in PATH?): {e}"),
            }
        });
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

                    // ── Service 0: full refresh (ejected check + logs + poller)
                    if let Some(svc) = self.state.services.first() {
                        let generation = self.state.log_generation;
                        if let Some(dep) = svc.deployment_name.clone() {
                            self.spawn_service_refresh(0, dep, generation);
                        }
                    }

                    // ── Services 1..N: ejected-only check for sidebar badges
                    let rest: Vec<(usize, String)> = self.state.services
                        .iter()
                        .enumerate()
                        .skip(1)
                        .filter_map(|(i, s)| s.deployment_name.clone().map(|d| (i, d)))
                        .collect();
                    if !rest.is_empty() {
                        self.spawn_bulk_ejected_check(rest);
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
                    // If this is the selected service, sync the log pane
                    // regardless of which pane is currently visible so that
                    // switching back to Logs always shows the correct content.
                    if idx == self.state.selected_idx {
                        if ejected {
                            let name = self.state.services
                                .get(idx)
                                .map(|s| s.meta_name.as_str())
                                .unwrap_or("this service");
                            self.state.logs = vec![
                                format!("⚡ {} is ejected — running in dev mode.", name),
                                "No application logs available.".into(),
                            ];
                        }
                        // If not ejected: leave the existing log content — the
                        // log poller will replace it with real lines shortly.
                    }
                }

                Ok(BgMsg::Logs { lines, generation }) => {
                    // Only apply if this result belongs to the currently selected
                    // service (via generation).  Stale results from old pollers
                    // (including any that were running before an eject) are dropped.
                    if generation == self.state.log_generation {
                        self.state.logs = lines;
                    }
                }

                Ok(BgMsg::Error(e)) => {
                    self.loading    = false;
                    self.state.logs = vec![format!("Failed to load services: {e}")];
                }

                // ── Eject / uneject result ────────────────────────────────────
                //
                // Clear the in-progress indicator unconditionally.
                //
                // On success:
                //   • Bump log_generation for the affected service so any
                //     in-flight log poller is invalidated — this prevents stale
                //     pod-log lines from overwriting the ejected/un-ejected notice.
                //   • Re-run spawn_service_refresh so the ejected badge and log
                //     pane update without requiring the user to click elsewhere.
                //
                // On failure: the error message is visible in the log pane.
                Ok(BgMsg::EjectResult { success, message, idx }) => {
                    self.ejecting = None;
                    self.state.logs.push(message);

                    if success {
                        // Invalidate any running log poller for this service.
                        if idx == self.state.selected_idx {
                            self.state.log_generation += 1;
                        }

                        if let Some(dep) = self.state.services
                            .get(idx)
                            .and_then(|s| s.deployment_name.clone())
                        {
                            let generation = if idx == self.state.selected_idx {
                                self.state.log_generation
                            } else {
                                u64::MAX
                            };
                            self.spawn_service_refresh(idx, dep, generation);
                        }
                    }
                }

                Err(_) => break,
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
                    let strip_action = draw_info_strip(&self.state, self.ejecting.as_deref(), ui);

                    if strip_action.eject_clicked {
                        self.run_eject(ctx);
                    }

                    if strip_action.uneject_clicked {
                        self.run_uneject(ctx);
                    }

                    if strip_action.open_editor_clicked {
                        self.open_editor();
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