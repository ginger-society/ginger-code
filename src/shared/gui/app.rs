use eframe::egui;
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};
use std::collections::HashMap;

use super::bg::{
    BgMsg,
    spawn_bulk_ejected_check,
    spawn_k8s_poller,
    spawn_metadata_fetch,
    spawn_service_refresh,
};
use super::colors::{COLOR_BG, COLOR_BORDER, COLOR_CYAN, COLOR_SIDEBAR_BG, COLOR_TAB_ACTIVE};
use super::panels::{
    draw_info_strip, draw_logs_pane, draw_service_list, draw_statusbar,
    draw_tab_bar, draw_terminal_pane, draw_titlebar, TabBarAction,
};
use super::terminal::{spawn_kubectl, TermPerformer};
use super::types::{AppState, K8sService, RightPane, TermState};
use crate::shared::ui::eject::{eject, uneject};

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    state:    AppState,
    rx:       mpsc::Receiver<BgMsg>,
    tx:       mpsc::Sender<BgMsg>,
    loading:  bool,
    /// Human-readable description of the in-flight eject/uneject operation,
    /// shown as a spinner in the info strip.
    ejecting: Option<String>,
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

        // Background tasks — all spawn logic lives in bg.rs.
        spawn_metadata_fetch(tx.clone(), ctx.clone());
        spawn_k8s_poller(tx.clone(), ctx.clone());

        App {
            state:    AppState::new(13.0, vec![]),
            rx,
            tx,
            loading:  true,
            ejecting: None,
            ctx,
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

    // ── Eject ─────────────────────────────────────────────────────────────────

    fn run_eject(&mut self, ctx: &egui::Context) {
        let Some(svc) = self.state.services.get(self.state.selected_idx) else { return };
        if svc.ejected { return; }

        let Some(dep)  = svc.deployment_name.clone() else { return };
        let Some(lang) = svc.lang.clone()            else { return };

        let meta_name = format!("{}-{}", svc.organization_id, svc.meta_name.clone());
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

    // ── Un-eject ──────────────────────────────────────────────────────────────

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

    // ── Open VS Code remote ───────────────────────────────────────────────────

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

    // ── Terminal: open + connect ──────────────────────────────────────────────

    fn open_and_connect_term(&mut self, ctx: &egui::Context) {
        let tab_idx = match self.state.open_term_tab(24, 80) {
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

        let host = match self.state.services.get(svc_idx).and_then(|s| s.ssh_host.as_ref()) {
            Some(h) => h.clone(),
            None => {
                if let Some(t) = self.state.term_tabs.get_mut(tab_idx) {
                    t.state = TermState::Error("No SSH host configured for this service".into());
                }
                return;
            }
        };

        let sink = Arc::new(Mutex::new(Vec::new()));
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
            Ok(session) => self.state.term_tabs[tab_idx].state = TermState::Connected(session),
            Err(e)      => self.state.term_tabs[tab_idx].state = TermState::Error(e.to_string()),
        }
    }

    // ── Drain background channel ──────────────────────────────────────────────

    fn drain_bg_channel(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(BgMsg::Services(svcs)) => {
                    self.state.services     = svcs;
                    self.state.selected_idx = 0;
                    self.loading            = false;

                    // Service 0: full refresh.
                    if let Some(svc) = self.state.services.first() {
                        let gen = self.state.log_generation;
                        if let Some(dep) = svc.deployment_name.clone() {
                            spawn_service_refresh(self.tx.clone(), self.ctx.clone(), 0, dep, gen);
                        }
                    }

                    // Services 1..N: ejected-only check for sidebar badges.
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
                    if idx == self.state.selected_idx && ejected {
                        let name = self.state.services
                            .get(idx)
                            .map(|s| s.meta_name.as_str())
                            .unwrap_or("this service");
                        self.state.logs = vec![
                            format!("⚡ {} is ejected — running in dev mode.", name),
                            "No application logs available.".into(),
                        ];
                    }
                }

                Ok(BgMsg::Logs { lines, generation }) => {
                    if generation == self.state.log_generation {
                        self.state.logs = lines;
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
                        if let Some(dep) = self.state.services
                            .get(idx)
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
                // The "Services" header scrolls away with the content so the
                // full sidebar area is usable by the scroll area.
                if self.loading {
                    ui.add_space(8.0);
                    ui.colored_label(COLOR_CYAN, "Loading services…");
                } else if self.state.services.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(COLOR_CYAN, "No services found.");
                } else {
                    // draw_service_list now returns a SidebarAction enum.
                    use super::panels::sidebar::SidebarAction;
                    if let Some(action) = draw_service_list(&self.state, ui) {
                        match action {
                            SidebarAction::SelectService(idx) => self.select_service(idx),
                        }
                    }
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

                    if strip_action.eject_clicked       { self.run_eject(ctx); }
                    if strip_action.uneject_clicked     { self.run_uneject(ctx); }
                    if strip_action.open_editor_clicked { self.open_editor(); }

                    match draw_tab_bar(&self.state, ui) {
                        Some(TabBarAction::SwitchToLogs)    => self.state.right_pane = RightPane::Logs,
                        Some(TabBarAction::SwitchToTerm(i)) => {
                            self.state.right_pane  = RightPane::TerminalTab(i);
                            self.state.active_term = i;
                        }
                        Some(TabBarAction::NewTerm)         => self.open_and_connect_term(ctx),
                        Some(TabBarAction::CloseTerm(i))    => self.state.close_term_tab(i),
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