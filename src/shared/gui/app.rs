use eframe::egui;
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};

use super::bg::{
    BgMsg,
    spawn_bulk_ejected_check,
    spawn_db_schema_logs,
    spawn_k8s_poller,
    spawn_metadata_fetch,
    spawn_mount,
    spawn_service_refresh,
    spawn_unmount,
    spawn_logs_for_container,
    spawn_container_fetch,
    spawn_db_container_fetch
};
use super::colors::{COLOR_BG, COLOR_CYAN, COLOR_SIDEBAR_BG};
use super::panels::{
    draw_db_schema_detail, draw_info_strip, draw_logs_pane, draw_package_detail,
    draw_service_list, draw_statusbar, draw_tab_bar, draw_terminal_pane, draw_titlebar,
    TabBarAction,
};
use super::terminal::{spawn_kubectl, TermPerformer};
use super::types::{AppState, RightPane, TermState};
use crate::shared::core::eject::{eject, uneject};
use crate::shared::core::image::pkg_to_slug;

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    state:    AppState,
    rx:       mpsc::Receiver<BgMsg>,
    tx:       mpsc::Sender<BgMsg>,
    loading:  bool,
    /// In-flight eject/uneject description (shown in the service info strip).
    ejecting: Option<String>,
    /// In-flight mount/unmount description + package index.
    mounting: Option<(usize, String)>,
    /// Index of the DB schema whose logs are currently being polled.
    db_log_schema_idx: Option<usize>,
    ctx:      egui::Context,
    db_log_pending_idx: Option<usize>,
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

        spawn_metadata_fetch(tx.clone(), ctx.clone());
        spawn_k8s_poller(tx.clone(), ctx.clone());

        App {
            state:              AppState::new(13.0, vec![]),
            rx,
            tx,
            loading:            true,
            ejecting:           None,
            mounting:           None,
            db_log_schema_idx:  None,
            ctx,
            db_log_pending_idx: None,
        }
    }

    // ── Service selection ─────────────────────────────────────────────────────

    fn select_service(&mut self, new_idx: usize) {
        let generation = self.state.switch_service(new_idx);

        let svc               = &self.state.services[new_idx];
        let meta_name         = svc.meta_name.clone();
        let deployment_name   = svc.deployment_name.clone();
        let ejected           = svc.ejected;
        let ejected_container = svc.ejected_container.clone();
        let containers        = svc.containers.clone();

        let has_other_containers = ejected_container.as_ref()
            .map(|ej| containers.iter().any(|c| c != ej))
            .unwrap_or(false);

        if ejected && !has_other_containers {
            // Single-container ejected: show dev mode message immediately
            self.state.logs = vec![
                format!("⚡ {} is ejected — running in dev mode.", meta_name),
                "No application logs available.".into(),
            ];
        } else {
            self.state.logs = vec![format!("Fetching logs for {}…", meta_name)];
        }

        if let Some(dep) = deployment_name.clone() {
            spawn_service_refresh(self.tx.clone(), self.ctx.clone(), new_idx, dep.clone(), generation);
            spawn_container_fetch(self.tx.clone(), self.ctx.clone(), new_idx, dep);
        }
    }

    // ── Package selection ─────────────────────────────────────────────────────

    fn select_package(&mut self, pkg_idx: usize) {
        self.state.right_pane = RightPane::PackageDetail(pkg_idx);
    }



    // ── DB schema selection ───────────────────────────────────────────────────

    fn select_db_schema(&mut self, schema_idx: usize) {
        self.state.right_pane  = RightPane::DbSchemaDetail(schema_idx);
        self.state.db_logs     = vec![];
        self.state.db_containers        = vec![];     // ← reset
        self.state.db_selected_container = None;      // ← reset

        if self.db_log_schema_idx == Some(schema_idx) {
            return;
        }
        self.db_log_schema_idx  = None;
        self.db_log_pending_idx = Some(schema_idx);

        let slug = self.state.db_schemas
            .get(schema_idx)
            .and_then(|s| s.k8s_name.clone())
            .unwrap_or_default();

        spawn_db_schema_logs(
            self.tx.clone(), self.ctx.clone(),
            schema_idx, slug.clone(),
            None,    // ← no container override initially
        );

        // Fetch containers for the tab bar
        if !slug.is_empty() {
            spawn_db_container_fetch(
                self.tx.clone(), self.ctx.clone(),
                schema_idx, slug,
            );
        }
    }

    fn select_db_container(&mut self, schema_idx: usize, container: String) {
        self.state.db_selected_container = Some(container.clone());
        self.state.db_logs = vec![];

        self.db_log_schema_idx  = None;
        self.db_log_pending_idx = Some(schema_idx);

        let slug = self.state.db_schemas
            .get(schema_idx)
            .and_then(|s| s.k8s_name.clone())
            .unwrap_or_default();

        spawn_db_schema_logs(
            self.tx.clone(), self.ctx.clone(),
            schema_idx, slug,
            Some(container),
        );
    }

    // ── Mount / unmount ───────────────────────────────────────────────────────

    fn run_mount(&mut self, pkg_idx: usize) {
        let Some(pkg) = self.state.packages.get(pkg_idx) else { return };
        if pkg.mounted { return; }

        let label = pkg.identifier.clone();
        self.mounting = Some((pkg_idx, format!("Mounting {}…", label)));

        spawn_mount(
            self.tx.clone(),
            self.ctx.clone(),
            pkg_idx,
            pkg.organization_id.clone(),
            pkg.identifier.clone(),
            pkg.lang.clone(),
        );
    }

    fn run_unmount(&mut self, pkg_idx: usize) {
        let Some(pkg) = self.state.packages.get(pkg_idx) else { return };
        if !pkg.mounted { return; }

        let label = pkg.identifier.clone();
        self.mounting = Some((pkg_idx, format!("Unmounting {}…", label)));

        spawn_unmount(
            self.tx.clone(),
            self.ctx.clone(),
            pkg_idx,
            pkg.organization_id.clone(),
            pkg.identifier.clone(),
        );
    }

    // ── Open VS Code for a mounted package ────────────────────────────────────

    fn open_package_editor(&self, pkg_idx: usize) {
        let Some(pkg) = self.state.packages.get(pkg_idx) else { return };
        if !pkg.mounted { return; }

        let alias      = format!("{}-local", pkg.identifier);
        let remote_uri = format!(
            "vscode-remote://ssh-remote+{}/workspace/{}-{}",
            alias, pkg.organization_id, pkg.identifier,
        );
        println!("Opening VS Code for package: {}", remote_uri);

        std::thread::spawn(move || {
            match std::process::Command::new("code")
                .arg("--folder-uri").arg(&remote_uri).status()
            {
                Ok(s) if s.success() => println!("✓ VS Code launched"),
                Ok(s)                => eprintln!("VS Code exited: {}", s),
                Err(e)               => eprintln!("Failed to launch VS Code: {e}"),
            }
        });
    }

    // ── Service eject / uneject ───────────────────────────────────────────────

    fn run_eject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if svc.ejected { return; }

        // Close all terminal tabs — the pod is about to restart and every
        // existing session will be forcibly disconnected anyway.
        self.state.term_tabs.clear();
        self.state.right_pane  = RightPane::Logs;
        self.state.active_term = 0;

        let Some(dep)  = svc.deployment_name.clone() else { return };
        let Some(lang) = svc.lang.clone()            else { return };

        let meta_name = svc.meta_name.clone();
        let org_id    = svc.organization_id.clone();
        let tx        = self.tx.clone();
        let ctx       = ctx.clone();
        let idx       = self.state.selected_idx;

        self.ejecting = Some(format!("Ejecting {}…", meta_name));

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().expect("tokio rt");
            rt.block_on(async move {
                let result = eject(&dep, &lang, &meta_name, &org_id).await;
                let (success, message) = match result {
                    Ok(())  => (true,  format!("✓ Ejected {}", meta_name)),
                    Err(e)  => (false, format!("✗ Eject failed for {}: {}", meta_name, e)),
                };
                let _ = tx.send(BgMsg::EjectResult { success, message, idx });
                ctx.request_repaint();
            });
        });
    }

    fn run_uneject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if !svc.ejected { return; }


        // Close all terminal tabs — the pod is about to restart and every
        // existing session will be forcibly disconnected anyway.
        self.state.term_tabs.clear();
        self.state.right_pane  = RightPane::Logs;
        self.state.active_term = 0;

        let Some(dep) = svc.deployment_name.clone() else { return };
        let meta_name = svc.meta_name.clone();
        let tx        = self.tx.clone();
        let ctx       = ctx.clone();
        let idx       = self.state.selected_idx;

        self.ejecting = Some(format!("Un-ejecting {}…", meta_name));

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().expect("tokio rt");
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

    // ── Open VS Code for ejected service ─────────────────────────────────────

    fn open_editor(&self) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if !svc.ejected { return; }
        let Some(dep) = svc.deployment_name.as_ref() else { return };

        let remote_uri = format!(
            "vscode-remote://ssh-remote+{}-local/workspace/{}-{}",
            dep, svc.organization_id, dep,
        );
        println!("Opening VS Code: {}", remote_uri);

        std::thread::spawn(move || {
            match std::process::Command::new("code")
                .arg("--folder-uri").arg(&remote_uri).status()
            {
                Ok(s) if s.success() => println!("✓ VS Code launched"),
                Ok(s)                => eprintln!("VS Code exited: {}", s),
                Err(e)               => eprintln!("Failed to launch VS Code: {e}"),
            }
        });
    }

    // ── Terminal helpers ──────────────────────────────────────────────────────

    fn open_and_connect_term(&mut self, ctx: &egui::Context) {
        let label = self.state.services
            .get(self.state.selected_idx)
            .map(|svc| {
                svc.selected_container
                    .clone()
                    .or_else(|| svc.deployment_name.clone())
                    .unwrap_or_else(|| "terminal".to_string())
            })
            .unwrap_or_else(|| "terminal".to_string());

        let tab_idx = match self.state.open_term_tab_with_label(24, 80, label) {
            Some(i) => i,
            None    => return,
        };
        self.state.right_pane  = RightPane::TerminalTab(tab_idx);
        self.state.active_term = tab_idx;
        self.connect_tab(tab_idx, ctx);
    }

    fn connect_tab(&mut self, tab_idx: usize, ctx: &egui::Context) {
        let tab = match self.state.term_tabs.get(tab_idx) {
            Some(t) => t,
            None    => return,
        };
        let svc_idx = tab.service_idx;
        let rows    = tab.term_rows as u16;
        let cols    = tab.term_cols as u16;

        let _host = match self.state.services.get(svc_idx).and_then(|s| s.ssh_host.as_ref()) {
            Some(h) => h.clone(),
            None => {
                if let Some(t) = self.state.term_tabs.get_mut(tab_idx) {
                    t.state = TermState::Error("No SSH host configured for this service".into());
                }
                return;
            }
        };

        let sink      = Arc::new(Mutex::new(Vec::new()));
        let performer = Arc::new(Mutex::new(
            TermPerformer::new(rows as usize, cols as usize)
                .with_sink(Arc::clone(&sink)),
        ));

        self.state.term_tabs[tab_idx].performer      = Arc::clone(&performer);
        self.state.term_tabs[tab_idx].scrollback_arc = Some(Arc::clone(&sink));

        let dep_name = match self.state.services.get(svc_idx)
            .and_then(|s| s.deployment_name.as_ref())
        {
            Some(d) => d.clone(),
            None => {
                self.state.term_tabs[tab_idx].state =
                    TermState::Error("No deployment name for this service".into());
                return;
            }
        };

        let container = self.state.services
            .get(svc_idx)
            .and_then(|s| s.selected_container.clone());

        match spawn_kubectl(&dep_name, rows, cols, performer, ctx.clone(), container) {
            Ok(session) => self.state.term_tabs[tab_idx].state = TermState::Connected(session),
            Err(e)      => self.state.term_tabs[tab_idx].state = TermState::Error(e.to_string()),
        }
    }

    // ── Container selection ───────────────────────────────────────────────────

    fn select_container(&mut self, container: Option<String>) {
        let idx = self.state.selected_idx;
        if let Some(svc) = self.state.services.get_mut(idx) {
            svc.selected_container = container.clone();
        }

        // Don't poll logs for the ejected container — it's a dev container
        // running sleep/entrypoint, not the application.
        if let Some(svc) = self.state.services.get(idx) {
            let selected_is_ejected = svc.ejected
                && svc.ejected_container.is_some()
                && svc.selected_container == svc.ejected_container;

            if selected_is_ejected {
                self.state.logs = vec![
                    format!(
                        "⚡ {} is ejected — running in dev mode.",
                        svc.meta_name
                    ),
                    "No application logs available.".into(),
                ];
                return;
            }
        }

        let dep = match self.state.services.get(idx)
            .and_then(|s| s.deployment_name.clone())
        {
            Some(d) => d,
            None    => return,
        };
        self.state.log_generation += 1;
        let gen = self.state.log_generation;
        spawn_logs_for_container(
            self.tx.clone(), self.ctx.clone(),
            dep, container, gen,
        );
    }

    // ── Drain background channel ──────────────────────────────────────────────

    fn drain_bg_channel(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(BgMsg::DbContainers { schema_idx, containers }) => {
                    // Only apply if this is still the active schema
                    if matches!(self.state.right_pane, RightPane::DbSchemaDetail(i) if i == schema_idx) {
                        self.state.db_selected_container = containers.first().cloned();
                        self.state.db_containers = containers;

                        // Restart log poller with the now-known first container
                        if let Some(container) = self.state.db_selected_container.clone() {
                            self.db_log_schema_idx  = None;
                            self.db_log_pending_idx = Some(schema_idx);

                            let slug = self.state.db_schemas
                                .get(schema_idx)
                                .and_then(|s| s.k8s_name.clone())
                                .unwrap_or_default();

                            spawn_db_schema_logs(
                                self.tx.clone(), self.ctx.clone(),
                                schema_idx, slug,
                                Some(container),
                            );
                        }
                    }
                }
                Ok(BgMsg::TransitioningSet(set)) => {
                    for svc in &mut self.state.services {
                        if let Some(ref dep) = svc.deployment_name {
                            svc.transitioning = set.contains(dep);
                        }
                    }
                }
                // ── Containers arrived ────────────────────────────────────────
                Ok(BgMsg::Containers { svc_idx, containers }) => {
                    if let Some(svc) = self.state.services.get_mut(svc_idx) {
                        svc.selected_container = containers.first().cloned();
                        svc.containers         = containers;
                    }

                    if svc_idx == self.state.selected_idx {
                        if let Some(svc) = self.state.services.get(svc_idx) {
                            // Skip log polling if the selected container is the ejected one —
                            // the dev container has no application logs to tail.
                            let selected_is_ejected = svc.ejected
                                && svc.ejected_container.is_some()
                                && svc.selected_container == svc.ejected_container;

                            if !selected_is_ejected {
                                if let (Some(dep), Some(container)) = (
                                    svc.deployment_name.clone(),
                                    svc.selected_container.clone(),
                                ) {
                                    self.state.log_generation += 1;
                                    let gen = self.state.log_generation;
                                    spawn_logs_for_container(
                                        self.tx.clone(), self.ctx.clone(),
                                        dep, Some(container), gen,
                                    );
                                }
                            }
                        }
                    }
                }

                // ── Services list arrived ─────────────────────────────────────
                Ok(BgMsg::Services(svcs)) => {
                    self.state.services     = svcs;
                    self.state.selected_idx = 0;
                    self.loading            = false;

                    if let Some(svc) = self.state.services.first() {
                        let gen = self.state.log_generation;
                        if let Some(dep) = svc.deployment_name.clone() {
                            spawn_service_refresh(self.tx.clone(), self.ctx.clone(), 0, dep, gen);
                        }
                    }

                    let rest: Vec<(usize, String)> = self.state.services
                        .iter().enumerate().skip(1)
                        .filter_map(|(i, s)| s.deployment_name.clone().map(|d| (i, d)))
                        .collect();
                    if !rest.is_empty() {
                        spawn_bulk_ejected_check(self.tx.clone(), self.ctx.clone(), rest);
                    }
                }

                Ok(BgMsg::Packages(pkgs)) => {
                    self.state.packages = pkgs;
                }

                Ok(BgMsg::DbSchemas(schemas)) => {
                    self.state.db_schemas = schemas;
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

                // ── Ejected flag arrived ──────────────────────────────────────
                Ok(BgMsg::EjectedFlag { idx, ejected, ejected_container }) => {
                    if let Some(svc) = self.state.services.get_mut(idx) {
                        svc.ejected           = ejected;
                        svc.ejected_container = ejected_container.clone();

                        // If ejected and multi-container, steer selected_container
                        // away from the ejected container to the first available one.
                        if ejected {
                            if let Some(ref ej) = ejected_container {
                                if let Some(alt) = svc.containers.iter().find(|c| *c != ej).cloned() {
                                    svc.selected_container = Some(alt);
                                }
                            }
                        }
                    }

                    // Update logs for the active service
                    if idx == self.state.selected_idx {
                        let svc                  = self.state.services.get(idx);
                        let is_ejected           = svc.map(|s| s.ejected).unwrap_or(false);
                        let containers           = svc.map(|s| s.containers.clone()).unwrap_or_default();
                        let ej_container         = svc.and_then(|s| s.ejected_container.clone());
                        let selected_container   = svc.and_then(|s| s.selected_container.clone());
                        let dep                  = svc.and_then(|s| s.deployment_name.clone());
                        let name                 = svc.map(|s| s.meta_name.clone()).unwrap_or_default();

                        let has_other_containers = ej_container.as_ref()
                            .map(|ej| containers.iter().any(|c| c != ej))
                            .unwrap_or(false);

                        if is_ejected && !has_other_containers {
                            // Single-container ejected: show dev mode message
                            self.state.logs = vec![
                                format!("⚡ {} is ejected — running in dev mode.", name),
                                "No application logs available.".into(),
                            ];
                        } else if is_ejected && has_other_containers {
                            // Multi-container ejected: stream logs for the
                            // selected non-ejected container
                            if let (Some(d), Some(container)) = (dep, selected_container) {
                                self.state.log_generation += 1;
                                let gen = self.state.log_generation;
                                spawn_logs_for_container(
                                    self.tx.clone(), self.ctx.clone(),
                                    d, Some(container), gen,
                                );
                            }
                        }
                        // If not ejected, spawn_service_refresh already handles logs
                    }
                }

                Ok(BgMsg::Logs { lines, generation }) => {
                    if generation == self.state.log_generation {
                        self.state.logs = lines;
                    }
                }

                Ok(BgMsg::DbSchemaLogs { lines, schema_idx }) => {
                    if self.db_log_pending_idx == Some(schema_idx) {
                        self.db_log_schema_idx  = Some(schema_idx);
                        self.db_log_pending_idx = None;
                    }
                    if self.db_log_schema_idx == Some(schema_idx) {
                        self.state.db_logs = lines;
                    }
                }

                Ok(BgMsg::Error(e)) => {
                    self.loading    = false;
                    self.state.logs = vec![format!("Failed to load services: {e}")];
                }

                Ok(BgMsg::EjectResult { success, message, idx }) => {
                    self.ejecting = None;
                    self.state.logs.push(message);

                    if success {
                        if idx == self.state.selected_idx {
                            self.state.log_generation += 1;
                        }
                        if let Some(dep) = self.state.services.get(idx)
                            .and_then(|s| s.deployment_name.clone())
                        {
                            let gen = if idx == self.state.selected_idx {
                                self.state.log_generation
                            } else {
                                u64::MAX
                            };
                            spawn_service_refresh(self.tx.clone(), self.ctx.clone(), idx, dep, gen);
                        }
                    }
                }

                Ok(BgMsg::MountResult { success, message, pkg_idx, mounted }) => {
                    if matches!(self.mounting, Some((i, _)) if i == pkg_idx) {
                        self.mounting = None;
                    }
                    if success {
                        if let Some(pkg) = self.state.packages.get_mut(pkg_idx) {
                            pkg.mounted = mounted;
                        }
                    }
                    self.state.logs.push(message);
                }

                Err(_) => break,
            }
        }
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_secs(2));

        self.drain_bg_channel();

        // ── Raise window on first frame ───────────────────────────────────────
        if !self.state.raised_on_open {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            self.state.raised_on_open = true;
            ctx.request_repaint();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
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
        if self.mounting.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
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
                if self.loading {
                    ui.add_space(8.0);
                    ui.colored_label(COLOR_CYAN, "Loading services…");
                } else {
                    use super::panels::sidebar::SidebarAction;
                    if let Some(action) = draw_service_list(&self.state, ui) {
                        match action {
                            SidebarAction::SelectService(idx)  => self.select_service(idx),
                            SidebarAction::SelectPackage(idx)  => self.select_package(idx),
                            SidebarAction::SelectDbSchema(idx) => self.select_db_schema(idx),
                        }
                    }
                }
            });

        // ── Main panel ────────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(COLOR_BG))
            .show(ctx, |ui| {
                if self.loading {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(COLOR_CYAN, "Loading services…");
                    });
                    return;
                }

                // ── DB schema detail ──────────────────────────────────────────
                if let RightPane::DbSchemaDetail(idx) = self.state.right_pane {
                    if let Some(schema) = self.state.db_schemas.get(idx).cloned() {
                        let logs_opt: Option<&[String]> = if self.db_log_schema_idx == Some(idx) {
                            Some(&self.state.db_logs)
                        } else {
                            None
                        };

                        // Pass container state into the detail panel
                        let containers        = self.state.db_containers.clone();
                        let selected_container = self.state.db_selected_container.clone();

                        if let Some(container) = draw_db_schema_detail(
                            &schema, logs_opt, &containers, selected_container.as_deref(), ui,
                        ) {
                            self.select_db_container(idx, container);
                        }
                    }
                    return;
                }

                // ── Package detail view ───────────────────────────────────────
                if let RightPane::PackageDetail(pkg_idx) = self.state.right_pane {
                    if let Some(pkg) = self.state.packages.get(pkg_idx).cloned() {
                        let mounting_msg = self.mounting.as_ref()
                            .filter(|(i, _)| *i == pkg_idx)
                            .map(|(_, m)| m.as_str());

                        let action = draw_package_detail(&pkg, mounting_msg, ui);

                        if action.mount_clicked       { self.run_mount(pkg_idx); }
                        if action.unmount_clicked     { self.run_unmount(pkg_idx); }
                        if action.open_editor_clicked { self.open_package_editor(pkg_idx); }
                    }
                    return;
                }

                // ── Service view (logs / terminal) ────────────────────────────
                if self.state.services.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(COLOR_CYAN, "No services found.");
                    });
                    return;
                }

                ui.vertical(|ui| {
                    let strip_action = draw_info_strip(&self.state, self.ejecting.as_deref(), ui);
                    if strip_action.eject_clicked       { self.run_eject(ctx); }
                    if strip_action.uneject_clicked     { self.run_uneject(ctx); }
                    if strip_action.open_editor_clicked { self.open_editor(); }

                    match draw_tab_bar(&self.state, ui) {
                        Some(TabBarAction::SelectContainer(name)) => {
                            let container = if name.is_empty() { None } else { Some(name) };
                            self.select_container(container);
                            self.state.right_pane = RightPane::Logs;
                        }
                        Some(TabBarAction::OpenTermForContainer(name)) => {
                            // Set the container first so open_and_connect_term picks it up
                            if let Some(svc) = self.state.services.get_mut(self.state.selected_idx) {
                                svc.selected_container = Some(name);
                            }
                            self.open_and_connect_term(ctx);
                        }
                        Some(TabBarAction::SwitchToTerm(i)) => {
                            self.state.right_pane  = RightPane::TerminalTab(i);
                            self.state.active_term = i;
                        }
                        Some(TabBarAction::CloseTerm(i)) => self.state.close_term_tab(i),
                        None => {}
                    }

                    match self.state.right_pane {
                        RightPane::Logs           => draw_logs_pane(&self.state, ui),
                        RightPane::TerminalTab(i) => draw_terminal_pane(&mut self.state, ui, i),
                        RightPane::PackageDetail(_) | RightPane::DbSchemaDetail(_) => {}
                    }
                });
            });
    }
}