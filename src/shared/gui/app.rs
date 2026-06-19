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
    spawn_mount_iac,
    spawn_service_refresh,
    spawn_unmount,
    spawn_unmount_iac,
    spawn_service_logs,
    spawn_container_fetch,
    spawn_db_container_fetch,
};
use super::colors::{COLOR_BG, COLOR_CYAN, COLOR_SIDEBAR_BG};
use super::panels::{
    draw_db_schema_detail, draw_iac_detail, draw_info_strip, draw_logs_pane,
    draw_package_detail, draw_service_list, draw_tab_bar, draw_terminal_pane,
    draw_titlebar, IacDetailAction, TabBarAction,
};
use super::terminal::TermPerformer;
use super::types::{AppState, RightPane, TermState};
use crate::shared::core::eject::{eject, uneject};
use crate::shared::core::k8s_exec::attach_to_pod;

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    state:       AppState,
    rx:          mpsc::Receiver<BgMsg>,
    tx:          mpsc::Sender<BgMsg>,
    loading:     bool,
    ejecting:    Option<String>,
    mounting:    Option<(usize, String)>,
    /// In-flight IAC mount/unmount message (separate from per-package mounting).
    iac_mounting: Option<String>,
    ctx:         egui::Context,
    pub rt:      tokio::runtime::Runtime,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build Tokio runtime");

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
            state:        AppState::new(13.0, vec![]),
            rx,
            tx,
            loading:      true,
            ejecting:     None,
            mounting:     None,
            iac_mounting: None,
            ctx,
            rt,
        }
    }

    // ── Service selection ─────────────────────────────────────────────────────

    fn select_service(&mut self, new_idx: usize) {
        let generation = self.state.switch_service(new_idx);

        let svc             = &self.state.services[new_idx];
        let meta_name       = svc.meta_name.clone();
        let deployment_name = svc.deployment_name.clone();
        let ejected         = svc.ejected;

        if ejected {
            self.state.logs = vec![
                format!("⚡ {} is ejected — running in dev mode.", meta_name),
                "No application logs available.".into(),
            ];
        } else {
            self.state.logs = vec![format!("Fetching logs for {}…", meta_name)];
        }

        if let Some(dep) = deployment_name {
            spawn_service_refresh(self.tx.clone(), self.ctx.clone(), new_idx, dep, generation);
        }
    }

    // ── Package selection ─────────────────────────────────────────────────────

    fn select_package(&mut self, pkg_idx: usize) {
        self.state.new_log_cancel();
        self.state.right_pane = RightPane::PackageDetail(pkg_idx);
    }

    // ── DB schema selection ───────────────────────────────────────────────────

    fn select_db_schema(&mut self, schema_idx: usize) {
        let cancel = self.state.new_db_log_cancel();

        self.state.right_pane            = RightPane::DbSchemaDetail(schema_idx);
        self.state.db_logs               = vec![];
        self.state.db_containers         = vec![];
        self.state.db_selected_container = None;
        self.state.db_log_generation    += 1;

        let slug = self.state.db_schemas
            .get(schema_idx)
            .and_then(|s| s.k8s_name.clone())
            .unwrap_or_default();

        let gen = self.state.db_log_generation;

        spawn_db_schema_logs(
            self.tx.clone(), self.ctx.clone(),
            schema_idx, slug.clone(), None, gen, cancel,
        );

        if !slug.is_empty() {
            spawn_db_container_fetch(
                self.tx.clone(), self.ctx.clone(),
                schema_idx, slug,
            );
        }
    }

    fn select_db_container(&mut self, schema_idx: usize, container: String) {
        let cancel = self.state.new_db_log_cancel();

        self.state.db_selected_container = Some(container.clone());
        self.state.db_logs               = vec![];
        self.state.db_log_generation    += 1;

        let gen  = self.state.db_log_generation;
        let slug = self.state.db_schemas
            .get(schema_idx)
            .and_then(|s| s.k8s_name.clone())
            .unwrap_or_default();

        spawn_db_schema_logs(
            self.tx.clone(), self.ctx.clone(),
            schema_idx, slug, Some(container), gen, cancel,
        );
    }

    // ── IAC selection ─────────────────────────────────────────────────────────

    fn select_iac(&mut self) {
        // Cancel any running log stream — IAC has no logs panel.
        self.state.new_log_cancel();
        self.state.right_pane = RightPane::IacDetail;
    }

    // ── Mount / unmount (packages) ────────────────────────────────────────────

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

    // ── Mount / unmount (IAC) ─────────────────────────────────────────────────

    fn run_mount_iac(&mut self) {
        let Some(ref iac) = self.state.iac else { return };
        if iac.mounted { return; }

        self.iac_mounting = Some("Mounting IAC dev container…".to_string());
        spawn_mount_iac(self.tx.clone(), self.ctx.clone(), iac.organization_id.clone());
    }

    fn run_unmount_iac(&mut self) {
        let Some(ref iac) = self.state.iac else { return };
        if !iac.mounted { return; }

        self.iac_mounting = Some("Unmounting IAC dev container…".to_string());
        spawn_unmount_iac(self.tx.clone(), self.ctx.clone(), iac.organization_id.clone());
    }

    // ── Open VS Code (packages) ───────────────────────────────────────────────

    fn open_package_editor(&self, pkg_idx: usize) {
        let Some(pkg) = self.state.packages.get(pkg_idx) else { return };
        if !pkg.mounted { return; }

        let alias      = format!("{}-local", pkg.identifier);
        let remote_uri = format!(
            "vscode-remote://ssh-remote+{}/workspace/{}-{}",
            alias, pkg.organization_id, pkg.identifier,
        );

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

    // ── Open VS Code (IAC) ────────────────────────────────────────────────────

    fn open_iac_editor(&self) {
        let Some(ref iac) = self.state.iac else { return };
        if !iac.mounted { return; }

        // slug = "iac", alias = "iac-local", workspace dir = "{org_id}-iac"
        let remote_uri = format!(
            "vscode-remote://ssh-remote+iac-local/workspace/{}-iac",
            iac.organization_id,
        );

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

    // ── Open VS Code (ejected service) ────────────────────────────────────────

    fn open_editor(&self) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if !svc.ejected { return; }
        let Some(dep) = svc.deployment_name.as_ref() else { return };

        let remote_uri = format!(
            "vscode-remote://ssh-remote+{}-local/workspace/{}-{}",
            dep, svc.organization_id, dep,
        );

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

    // ── Eject / uneject ───────────────────────────────────────────────────────

    fn run_eject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if svc.ejected { return; }

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

        let dep_name = match self.state.services
            .get(svc_idx)
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

        let sink      = Arc::new(Mutex::new(Vec::new()));
        let performer = Arc::new(Mutex::new(
            TermPerformer::new(rows as usize, cols as usize)
                .with_sink(Arc::clone(&sink)),
        ));

        self.state.term_tabs[tab_idx].performer      = Arc::clone(&performer);
        self.state.term_tabs[tab_idx].scrollback_arc = Some(Arc::clone(&sink));
        self.state.term_tabs[tab_idx].state = TermState::Connecting;

        let tx  = self.tx.clone();
        let ctx = ctx.clone();

        self.rt.spawn(async move {
            match attach_to_pod(&dep_name, rows, cols, performer, ctx.clone(), container).await {
                Ok(session) => {
                    let _ = tx.send(BgMsg::TermConnected { tab_idx, session });
                }
                Err(e) => {
                    let _ = tx.send(BgMsg::TermError {
                        tab_idx,
                        message: e.to_string(),
                    });
                }
            }
            ctx.request_repaint();
        });
    }

    // ── Container selection ───────────────────────────────────────────────────

    fn select_container(&mut self, container: Option<String>) {
        let idx = self.state.selected_idx;
        if let Some(svc) = self.state.services.get_mut(idx) {
            svc.selected_container = container.clone();
        }

        let (selected_is_ejected, meta_name, deployment_name) =
            if let Some(svc) = self.state.services.get(idx) {
                let ejected = svc.ejected
                    && container.as_deref() == svc.ejected_container.as_deref();
                (ejected, svc.meta_name.clone(), svc.deployment_name.clone())
            } else {
                return;
            };

        if selected_is_ejected {
            self.state.new_log_cancel();
            self.state.log_generation += 1;
            self.state.logs = vec![
                format!("⚡ {} is ejected — running in dev mode.", meta_name),
                "No application logs available.".into(),
            ];
            return;
        }

        if let Some(dep) = deployment_name {
            let cancel = self.state.new_log_cancel();
            self.state.log_generation += 1;
            let gen = self.state.log_generation;
            spawn_service_logs(
                self.tx.clone(), self.ctx.clone(),
                dep, container, gen, cancel,
            );
        }
    }

    // ── Drain background channel ──────────────────────────────────────────────

    fn drain_bg_channel(&mut self) -> bool {
        let mut did_work = false;

        loop {
            match self.rx.try_recv() {
                Ok(BgMsg::TermConnected { tab_idx, session }) => {
                    if let Some(tab) = self.state.term_tabs.get_mut(tab_idx) {
                        tab.state = TermState::Connected(session);
                    }
                    did_work = true;
                }

                Ok(BgMsg::TermError { tab_idx, message }) => {
                    if let Some(tab) = self.state.term_tabs.get_mut(tab_idx) {
                        tab.state = TermState::Error(message);
                    }
                    did_work = true;
                }

                Ok(BgMsg::DbContainers { schema_idx, containers }) => {
                    if matches!(self.state.right_pane, RightPane::DbSchemaDetail(i) if i == schema_idx) {
                        self.state.db_selected_container = containers.first().cloned();
                        self.state.db_containers         = containers;

                        if let Some(container) = self.state.db_selected_container.clone() {
                            let cancel = self.state.new_db_log_cancel();
                            self.state.db_log_generation += 1;
                            let gen  = self.state.db_log_generation;
                            let slug = self.state.db_schemas
                                .get(schema_idx)
                                .and_then(|s| s.k8s_name.clone())
                                .unwrap_or_default();

                            spawn_db_schema_logs(
                                self.tx.clone(), self.ctx.clone(),
                                schema_idx, slug, Some(container), gen, cancel,
                            );
                        }
                    }
                    did_work = true;
                }

                Ok(BgMsg::TransitioningSet(set)) => {
                    for svc in &mut self.state.services {
                        if let Some(ref dep) = svc.deployment_name {
                            svc.transitioning = set.contains(dep);
                        }
                    }
                    did_work = true;
                }

                Ok(BgMsg::Containers { svc_idx, containers }) => {
                    if let Some(svc) = self.state.services.get_mut(svc_idx) {
                        let default = if svc.ejected && svc.ejected_container.is_none() {
                            None
                        } else {
                            containers.iter()
                                .find(|c| Some(c.as_str()) != svc.ejected_container.as_deref())
                                .or_else(|| containers.first())
                                .cloned()
                        };
                        svc.selected_container = default;
                        svc.containers = containers;
                    }

                    if svc_idx == self.state.selected_idx {
                        let container = self.state.services
                            .get(svc_idx)
                            .and_then(|s| s.selected_container.clone());
                        if container.is_some() {
                            self.select_container(container);
                        }
                    }
                    did_work = true;
                }

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
                    did_work = true;
                }

                Ok(BgMsg::Packages(pkgs)) => {
                    self.state.packages = pkgs;
                    did_work = true;
                }

                Ok(BgMsg::DbSchemas(schemas)) => {
                    self.state.db_schemas = schemas;
                    did_work = true;
                }

                // ── IAC arrived from metadata fetch ───────────────────────────
                Ok(BgMsg::Iac(iac)) => {
                    self.state.iac = Some(iac);
                    did_work = true;
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
                    did_work = true;
                }

                Ok(BgMsg::EjectedFlag { idx, ejected, ejected_container }) => {
                    if let Some(svc) = self.state.services.get_mut(idx) {
                        svc.ejected           = ejected;
                        svc.ejected_container = ejected_container;
                    }
                    if idx == self.state.selected_idx && ejected {
                        let name = self.state.services.get(idx)
                            .map(|s| s.meta_name.as_str())
                            .unwrap_or("this service");
                        self.state.logs = vec![
                            format!("⚡ {} is ejected — running in dev mode.", name),
                            "No application logs available.".into(),
                        ];
                    }

                    let containers_already_loaded = self.state.services
                        .get(idx)
                        .map(|s| !s.containers.is_empty())
                        .unwrap_or(false);

                    if containers_already_loaded && idx == self.state.selected_idx {
                        let container = self.state.services.get(idx).and_then(|svc| {
                            svc.containers.iter()
                                .find(|c| Some(c.as_str()) != svc.ejected_container.as_deref())
                                .or_else(|| svc.containers.first())
                                .cloned()
                        });
                        if let Some(svc) = self.state.services.get_mut(idx) {
                            svc.selected_container = container.clone();
                        }
                        self.select_container(container);
                    } else {
                        if let Some(dep) = self.state.services.get(idx)
                            .and_then(|s| s.deployment_name.clone())
                        {
                            spawn_container_fetch(self.tx.clone(), self.ctx.clone(), idx, dep);
                        }
                    }
                    did_work = true;
                }

                Ok(BgMsg::Logs { lines, generation }) => {
                    if generation == self.state.log_generation {
                        self.state.logs = lines;
                        did_work = true;
                    }
                }

                Ok(BgMsg::DbSchemaLogs { lines, schema_idx, generation }) => {
                    if generation == self.state.db_log_generation {
                        self.state.db_logs = lines;
                        did_work = true;
                    }
                }

                Ok(BgMsg::Error(e)) => {
                    self.loading    = false;
                    self.state.logs = vec![format!("Failed to load services: {e}")];
                    did_work = true;
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
                    did_work = true;
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
                    did_work = true;
                }

                // ── IAC mount result ──────────────────────────────────────────
                Ok(BgMsg::IacMountResult { success, message, mounted }) => {
                    self.iac_mounting = None;
                    if success {
                        if let Some(ref mut iac) = self.state.iac {
                            iac.mounted = mounted;
                        }
                    }
                    self.state.logs.push(message);
                    did_work = true;
                }

                Err(_) => break,
            }
        }

        did_work
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let _had_messages = self.drain_bg_channel();

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

        let terminal_visible = matches!(self.state.right_pane, RightPane::TerminalTab(_));
        if terminal_visible {
            let t = ctx.input(|i| i.time);
            if t - self.state.blink_timer > 0.5 {
                self.state.blink       = !self.state.blink;
                self.state.blink_timer = t;
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        if self.mounting.is_some() || self.iac_mounting.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }

        if self.ejecting.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }

        if let RightPane::TerminalTab(i) = self.state.right_pane {
            if let Some(tab) = self.state.term_tabs.get_mut(i) {
                if let Some(ref sink) = tab.scrollback_arc {
                    let mut s = sink.lock();
                    if !s.is_empty() {
                        tab.scrollback.append(&mut *s);
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                    }
                }
            }
        }

        // ── UI ────────────────────────────────────────────────────────────────

        egui::TopBottomPanel::top("titlebar")
            .exact_height(28.0)
            .frame(egui::Frame::none())
            .show(ctx, |ui| draw_titlebar(&self.state, ui, ctx));

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
                            SidebarAction::SelectIac           => self.select_iac(),
                        }
                    }
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(COLOR_BG))
            .show(ctx, |ui| {
                if self.loading {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(COLOR_CYAN, "Loading services…");
                    });
                    return;
                }

                // ── IAC detail panel ──────────────────────────────────────────
                if self.state.right_pane == RightPane::IacDetail {
                    if let Some(ref iac) = self.state.iac.clone() {
                        let mounting_msg = self.iac_mounting.as_deref();
                        let action = draw_iac_detail(iac, mounting_msg, ui);
                        if action.mount_clicked       { self.run_mount_iac(); }
                        if action.unmount_clicked     { self.run_unmount_iac(); }
                        if action.open_editor_clicked { self.open_iac_editor(); }
                    }
                    return;
                }

                // ── DB schema detail panel ────────────────────────────────────
                if let RightPane::DbSchemaDetail(idx) = self.state.right_pane {
                    if let Some(schema) = self.state.db_schemas.get(idx).cloned() {
                        let logs_opt: Option<&[String]> =
                            if self.state.db_log_generation > 0 && !self.state.db_logs.is_empty() {
                                Some(&self.state.db_logs)
                            } else {
                                None
                            };

                        let containers         = self.state.db_containers.clone();
                        let selected_container = self.state.db_selected_container.clone();

                        if let Some(container) = draw_db_schema_detail(
                            &schema, logs_opt, &containers, selected_container.as_deref(), ui,
                        ) {
                            self.select_db_container(idx, container);
                        }
                    }
                    return;
                }

                // ── Package detail panel ──────────────────────────────────────
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
                        RightPane::PackageDetail(_)
                        | RightPane::DbSchemaDetail(_)
                        | RightPane::IacDetail => {}
                    }
                });
            });
    }
}