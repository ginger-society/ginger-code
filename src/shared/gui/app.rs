use eframe::egui;
use parking_lot::Mutex;
use std::sync::Arc;

use super::colors::{COLOR_BG, COLOR_BORDER, COLOR_CYAN, COLOR_SIDEBAR_BG, COLOR_TAB_ACTIVE};
use super::panels::{
    draw_info_strip, draw_logs_pane, draw_service_list, draw_statusbar, draw_tab_bar,
    draw_terminal_pane, draw_titlebar, TabBarAction,
};
use super::terminal::{spawn_ssh, TermPerformer};
use super::types::{AppState, K8sService, RightPane, TermState};

pub struct App {
    state: AppState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "mono".to_owned(),
            egui::FontData::from_static(include_bytes!("../../../assets/JetBrainsMono-Regular.ttf")),
        );
        fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "mono".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        // ── Stub services — replace with real data source ─────────────────────
        let services = vec![
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-frontend-users".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-frontend-users".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/iam-admin-fe".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("iam-admin-fe".into()),
                status: "Not deployed".into(), ready: "–".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/IAMAdminService".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("IAMAdminService".into()),
                status: "Running".into(), ready: "1/1".into(),
                lang: Some("Rust".into()), ejected: true,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/dev-portal".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("dev-portal".into()),
                status: "Running".into(), ready: "1/1".into(),
                lang: Some("TS".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
            K8sService {
                meta_name: "@ginger-society/NotificationService".into(),
                organization_id: "ginger-society".into(),
                deployment_name: None,
                status: "Not deployed".into(), ready: "–".into(),
                lang: None, ejected: false, ssh_host: None,
            },
            K8sService {
                meta_name: "@ginger-society/MetadataService".into(),
                organization_id: "ginger-society".into(),
                deployment_name: Some("metadata-service".into()),
                status: "Degraded".into(), ready: "0/1".into(),
                lang: Some("Rust".into()), ejected: false,
                ssh_host: Some("dev@iamadminservice-local".into()),
            },
        ];

        App { state: AppState::new(13.0, services) }
    }

    // ── Open a new terminal tab and connect it ────────────────────────────────

    fn open_and_connect_term(&mut self, ctx: &egui::Context) {
        // Default initial grid size; will resize on first render.
        let rows = 24;
        let cols = 80;

        let tab_idx = match self.state.open_term_tab(rows, cols) {
            Some(i) => i,
            None    => return, // cap reached — button should be hidden, belt-and-suspenders
        };

        self.state.right_pane = RightPane::TerminalTab(tab_idx);
        self.state.active_term = tab_idx;
        self.connect_tab(tab_idx, ctx);
    }

    // ── Connect one specific tab ──────────────────────────────────────────────

    fn connect_tab(&mut self, tab_idx: usize, ctx: &egui::Context) {
        let tab = match self.state.term_tabs.get(tab_idx) {
            Some(t) => t,
            None    => return,
        };
        let svc_idx  = tab.service_idx;
        let rows     = tab.term_rows as u16;
        let cols     = tab.term_cols as u16;
        let host     = match self.state.services.get(svc_idx).and_then(|s| s.ssh_host.as_ref()) {
            Some(h) => h.clone(),
            None => {
                if let Some(t) = self.state.term_tabs.get_mut(tab_idx) {
                    t.state = TermState::Error("No SSH host configured for this service".into());
                }
                return;
            }
        };

        // Build a scrollback sink shared between the performer and the tab.
        let sink: super::terminal::ScrollbackSink = Arc::new(Mutex::new(Vec::new()));
        let performer = Arc::new(Mutex::new(
            TermPerformer::new(rows as usize, cols as usize)
                .with_sink(Arc::clone(&sink)),
        ));

        // Replace the tab's performer with the new one that has the sink wired up.
        let tab = &mut self.state.term_tabs[tab_idx];
        tab.performer = Arc::clone(&performer);

        // Share the same Vec as the tab's scrollback so the sink writes directly
        // into tab.scrollback.  We do this by cloning the Arc into the tab — both
        // sides point at the same Mutex<Vec<…>>.
        // (We store a separate Arc on the tab for read access from the render thread.)
        // Since TermPerformer already holds the Arc, we just need our tab to hold
        // a reference to the same data.  We achieve this by handing the tab the Arc.
        tab.scrollback_arc = Some(Arc::clone(&sink));

        match spawn_ssh(&host, rows, cols, performer, ctx.clone()) {
            Ok(session) => tab.state = TermState::Connected(session),
            Err(e)      => tab.state = TermState::Error(e.to_string()),
        }
    }

    // ── Service selection ─────────────────────────────────────────────────────

    fn select_service(&mut self, idx: usize) {
        let meta_name = self.state.services[idx].meta_name.clone();
        self.state.selected_idx = idx;
        // Don't kill existing terminal tabs — they belong to whatever service
        // opened them.  Just switch the pane view to Logs for the new service.
        self.state.right_pane = RightPane::Logs;
        self.state.logs = vec![format!("Fetching logs for {}…", meta_name)];
        // TODO: kick off async log fetch here
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Raise window on first frame (tray launch) ─────────────────────────
        if !self.state.raised_on_open {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            self.state.raised_on_open = true;
            ctx.request_repaint();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        }

        // ── Cursor blink (only when a terminal tab is visible) ────────────────
        let t = ctx.input(|i| i.time);
        if t - self.state.blink_timer > 0.5 {
            self.state.blink       = !self.state.blink;
            self.state.blink_timer = t;
        }
        if matches!(self.state.right_pane, RightPane::TerminalTab(_)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Sync scrollback from Arc sinks into tab.scrollback each frame.
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
                    egui::vec2(ui.available_width(), 22.0), egui::Sense::hover(),
                );
                ui.painter().rect_filled(hdr, 0.0, COLOR_SIDEBAR_BG);
                ui.painter().text(
                    egui::pos2(hdr.min.x + 8.0, hdr.center().y),
                    egui::Align2::LEFT_CENTER, "Services",
                    egui::FontId::new(11.0, egui::FontFamily::Monospace), COLOR_CYAN,
                );
                ui.painter().line_segment(
                    [hdr.left_bottom(), hdr.right_bottom()],
                    egui::Stroke::new(0.5, COLOR_BORDER),
                );
                if let Some(new_idx) = draw_service_list(&self.state, ui) {
                    self.select_service(new_idx);
                }
            });

        // ── Main panel ────────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(COLOR_BG))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    // 1. Deployment details strip
                    let eject_clicked = draw_info_strip(&self.state, ui);
                    if eject_clicked {
                        let svc = &self.state.services[self.state.selected_idx];
                        println!("[eject] dummy callback — would eject: {}", svc.meta_name);
                        // TODO: wire up real eject logic here
                    }

                    // 2. Tab bar (Logs + terminal tabs + "+" button)
                    let action = draw_tab_bar(&self.state, ui);

                    // Process tab bar actions after the immutable borrow ends.
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

                    // 3. Pane content
                    match self.state.right_pane {
                        RightPane::Logs => draw_logs_pane(&self.state, ui),
                        RightPane::TerminalTab(i) => draw_terminal_pane(&mut self.state, ui, i),
                    }
                });
            });
    }
}