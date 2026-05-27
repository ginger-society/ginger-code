use crate::shared::{ICON_AMBER, ICON_GREEN, ICON_RED};
use crate::{Config, DeploymentEntry, ForwardStatus, StateMap, shutdown_all_threads};
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuItem, MenuEvent, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
};
use std::sync::Mutex;

#[derive(PartialEq, Clone, Copy)]
enum TrayState { AllConnected, Partial, Offline }

static GUI_CHILD: Mutex<Option<Child>> = Mutex::new(None);
static CLI_CHILD: Mutex<Option<Child>> = Mutex::new(None);

fn quit_app(state_map: &StateMap) {
    // Signal + join all forward threads before exiting so no kubectl
    // processes are left running in the background.
    shutdown_all_threads(state_map);

    if let Some(mut child) = GUI_CHILD.lock().unwrap().take() {
        let _ = child.kill();
    }
    if let Some(mut child) = CLI_CHILD.lock().unwrap().take() {
        let _ = child.kill();
    }
    std::process::exit(0);
}

fn make_icon(svg: &str) -> Icon {
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opts).expect("valid svg");
    let mut pixmap = tiny_skia::Pixmap::new(22, 22).unwrap();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    Icon::from_rgba(pixmap.take(), 22, 22).expect("icon")
}

// ── Dashboard launcher ────────────────────────────────────────────────────────

fn open_dashboard() {
    {
        let mut guard = GUI_CHILD.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                println!("[ginger-code] dashboard already running, skipping spawn");
                return;
            }
            *guard = None;
        }
    }

    #[cfg(target_os = "macos")]
    let result = std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg("--gui").spawn());

    #[cfg(target_os = "linux")]
    let result = {
        let exe = std::env::current_exe().unwrap_or_default();
        let terminals: &[(&str, &[&str])] = &[
            ("gnome-terminal", &["--", exe.to_str().unwrap_or("ginger-code"), "--gui"]),
            ("xterm",          &["-e", exe.to_str().unwrap_or("ginger-code"), "--gui"]),
            ("konsole",        &["-e", exe.to_str().unwrap_or("ginger-code"), "--gui"]),
            ("xfce4-terminal", &["-e", exe.to_str().unwrap_or("ginger-code"), "--gui"]),
        ];
        let mut res = Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal found"));
        for (term, args) in terminals {
            match std::process::Command::new(term).args(*args).spawn() {
                Ok(child) => { res = Ok(child); break; }
                Err(_)    => continue,
            }
        }
        res
    };

    #[cfg(target_os = "windows")]
    let result = std::env::current_exe()
        .and_then(|exe| {
            std::process::Command::new("cmd")
                .args(["/c", "start", "cmd", "/k", exe.to_str().unwrap_or("ginger-code"), "--gui"])
                .spawn()
        });

    match result {
        Ok(child) => {
            *GUI_CHILD.lock().unwrap() = Some(child);
            println!("[ginger-code] launched dashboard");
        }
        Err(e) => eprintln!("[ginger-code] failed to launch dashboard: {e}"),
    }
}

// ── CLI launcher ──────────────────────────────────────────────────────────────

fn open_cli() {
    {
        let mut guard = CLI_CHILD.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                println!("[ginger-code] CLI already running, skipping spawn");
                return;
            }
            *guard = None;
        }
    }

    #[cfg(target_os = "macos")]
    let result = std::env::current_exe()
        .map(|exe| exe.parent().unwrap_or(std::path::Path::new(".")).join("ginger-code-cli"))
        .and_then(|cli_bin| {
            std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        r#"tell application "Terminal" to do script "{}" & activate"#,
                        cli_bin.display()
                    ),
                ])
                .spawn()
        });

    #[cfg(target_os = "linux")]
    let result = {
        let cli_bin = std::env::current_exe()
            .map(|exe| exe.parent().unwrap_or(std::path::Path::new(".")).join("ginger-code-cli"))
            .unwrap_or_else(|_| std::path::PathBuf::from("ginger-code-cli"));
        let terminals: &[(&str, Vec<String>)] = &[
            ("gnome-terminal", vec!["--".into(), cli_bin.display().to_string()]),
            ("xterm",          vec!["-e".into(), cli_bin.display().to_string()]),
            ("konsole",        vec!["-e".into(), cli_bin.display().to_string()]),
            ("xfce4-terminal", vec!["-e".into(), cli_bin.display().to_string()]),
        ];
        let mut res = Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal found"));
        for (term, args) in terminals {
            match std::process::Command::new(term).args(args).spawn() {
                Ok(child) => { res = Ok(child); break; }
                Err(_)    => continue,
            }
        }
        res
    };

    #[cfg(target_os = "windows")]
    let result = {
        let cli_bin = std::env::current_exe()
            .map(|exe| exe.parent().unwrap_or(std::path::Path::new(".")).join("ginger-code-cli.exe"))
            .unwrap_or_else(|_| std::path::PathBuf::from("ginger-code-cli.exe"));
        std::process::Command::new("cmd")
            .args(["/c", "start", "cmd", "/k", &cli_bin.display().to_string()])
            .spawn()
    };

    match result {
        Ok(_)  => println!("[ginger-code] launched CLI"),
        Err(e) => eprintln!("[ginger-code] failed to launch CLI: {e}"),
    }
}

// ── VS Code launcher ──────────────────────────────────────────────────────────

fn open_vscode(deployment_name: &str, forwarding_port: u16) {
    println!(
        "[ginger-code] open_vscode called for '{}' on port {}",
        deployment_name, forwarding_port
    );
}

// ── Menu snapshot ─────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
struct MenuSnapshot {
    entries: Vec<(String, bool)>,
}

impl MenuSnapshot {
    fn from(
        cfg: &[DeploymentEntry],
        map: &std::collections::HashMap<String, crate::ForwardState>,
    ) -> Self {
        let entries = cfg.iter().map(|e| {
            let connected = map
                .get(&e.deployment_name)
                .map_or(false, |fw| fw.status == ForwardStatus::Connected);
            (e.deployment_name.clone(), connected)
        }).collect();
        Self { entries }
    }
}

// ── Menu builder ──────────────────────────────────────────────────────────────

struct BuiltMenu {
    menu:           Menu,
    dashboard_id:   tray_icon::menu::MenuId,
    cli_id:         tray_icon::menu::MenuId,
    deployment_ids: Vec<(tray_icon::menu::MenuId, String, u16)>,
    quit_id:        tray_icon::menu::MenuId,
}

fn build_menu(
    cfg: &[DeploymentEntry],
    map: &std::collections::HashMap<String, crate::ForwardState>,
) -> BuiltMenu {
    let menu = Menu::new();

    let dashboard_item = MenuItem::new("  Open Dashboard  ", true, None);
    let dashboard_id   = dashboard_item.id().clone();
    menu.append(&dashboard_item).unwrap();

    let cli_item = MenuItem::new("  Open CLI  ", true, None);
    let cli_id   = cli_item.id().clone();
    menu.append(&cli_item).unwrap();

    menu.append(&PredefinedMenuItem::separator()).unwrap();

    let mut deployment_ids = Vec::new();
    for entry in cfg {
        let connected = map
            .get(&entry.deployment_name)
            .map_or(false, |fw| fw.status == ForwardStatus::Connected);
        let bullet = if connected { "●" } else { "○" };
        let label  = format!("  {}  {}  ", bullet, entry.deployment_name);
        let item   = MenuItem::new(&label, connected, None);
        let id     = item.id().clone();
        menu.append(&item).unwrap();
        deployment_ids.push((id, entry.deployment_name.clone(), entry.forwarding_port));
    }

    menu.append(&PredefinedMenuItem::separator()).unwrap();

    let quit_item = MenuItem::new("Quit ginger-code", true, None);
    let quit_id   = quit_item.id().clone();
    menu.append(&quit_item).unwrap();

    BuiltMenu { menu, dashboard_id, cli_id, deployment_ids, quit_id }
}

// ── Tray state helpers ────────────────────────────────────────────────────────

fn compute_tray_state(
    map:             &std::collections::HashMap<String, crate::ForwardState>,
    cfg_deployments: &[DeploymentEntry],
) -> (TrayState, usize, usize) {
    let total = cfg_deployments.len();
    if total == 0 {
        return (TrayState::AllConnected, 0, 0);
    }

    let connected = cfg_deployments
        .iter()
        .filter(|e| {
            map.get(&e.deployment_name)
                .map_or(false, |fw| fw.status == ForwardStatus::Connected)
        })
        .count();

    let offline_all = cfg_deployments.iter().all(|e| {
        map.get(&e.deployment_name)
            .map_or(false, |fw| fw.status == ForwardStatus::Offline)
    });

    if offline_all {
        (TrayState::Offline, 0, total)
    } else if connected == total {
        (TrayState::AllConnected, connected, total)
    } else {
        (TrayState::Partial, connected, total)
    }
}

fn tooltip(connected: usize, total: usize) -> String {
    if total == 0 {
        return "ginger-code — no deployments".to_string();
    }
    format!("ginger-code — {}/{} connected", connected, total)
}

// ── TrayApp ───────────────────────────────────────────────────────────────────

struct TrayApp {
    state_map:      StateMap,
    shutdown:       Arc<AtomicBool>,
    // offline passed in so quit_app can signal it before joining threads
    _offline:       Arc<AtomicBool>,
    sock_path:      PathBuf,
    cfg_path:       PathBuf,
    _tray:          tray_icon::TrayIcon,
    icon_green:     Icon,
    icon_amber:     Icon,
    icon_red:       Icon,
    last_state:     TrayState,
    last_tick:      std::time::Instant,
    dashboard_id:   tray_icon::menu::MenuId,
    cli_id:         tray_icon::menu::MenuId,
    deployment_ids: Vec<(tray_icon::menu::MenuId, String, u16)>,
    quit_id:        tray_icon::menu::MenuId,
    last_snapshot:  MenuSnapshot,
}

impl TrayApp {
    fn new(
        state_map: StateMap,
        shutdown:  Arc<AtomicBool>,
        offline:   Arc<AtomicBool>,
        sock_path: PathBuf,
        cfg_path:  PathBuf,
    ) -> Self {
        let icon_green = make_icon(ICON_GREEN);
        let icon_amber = make_icon(ICON_AMBER);
        let icon_red   = make_icon(ICON_RED);

        let cfg = Config::load(&cfg_path);
        let (init_state, connected, total) = {
            let map = state_map.lock().unwrap();
            compute_tray_state(&map, &cfg.deployments)
        };

        let snapshot = {
            let map = state_map.lock().unwrap();
            MenuSnapshot::from(&cfg.deployments, &map)
        };

        let built = {
            let map = state_map.lock().unwrap();
            build_menu(&cfg.deployments, &map)
        };

        let init_icon = match init_state {
            TrayState::AllConnected => icon_green.clone(),
            TrayState::Partial      => icon_amber.clone(),
            TrayState::Offline      => icon_red.clone(),
        };

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(built.menu))
            .with_icon(init_icon)
            .with_tooltip(tooltip(connected, total))
            .build()
            .expect("tray icon");

        Self {
            state_map,
            shutdown,
            _offline: offline,
            sock_path,
            cfg_path,
            _tray: tray,
            icon_green,
            icon_amber,
            icon_red,
            last_state: init_state,
            last_tick: std::time::Instant::now(),
            dashboard_id: built.dashboard_id,
            cli_id: built.cli_id,
            deployment_ids: built.deployment_ids,
            quit_id: built.quit_id,
            last_snapshot: snapshot,
        }
    }
}

impl ApplicationHandler for TrayApp {
    fn resumed(&mut self, _el: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _el:  &ActiveEventLoop,
        _id:  winit::window::WindowId,
        _ev:  winit::event::WindowEvent,
    ) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // ── Drain menu events ─────────────────────────────────────────────────
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == self.quit_id {
                println!("[ginger-code] quit via tray");
                self.shutdown.store(true, Ordering::Relaxed);
                let _ = std::fs::remove_file(&self.sock_path);
                quit_app(&self.state_map);
                event_loop.exit();
                return;
            }

            if ev.id == self.dashboard_id {
                open_dashboard();
                continue;
            }

            if ev.id == self.cli_id {
                open_cli();
                continue;
            }

            for (id, name, port) in &self.deployment_ids {
                if ev.id == *id {
                    open_vscode(name, *port);
                    break;
                }
            }
        }

        // ── Periodic update (icon, tooltip, menu) — once per second ──────────
        if self.last_tick.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_tick = std::time::Instant::now();

            let cfg = Config::load(&self.cfg_path);
            let (state, connected, total, snapshot) = {
                let map = self.state_map.lock().unwrap();
                let (state, connected, total) = compute_tray_state(&map, &cfg.deployments);
                let snapshot = MenuSnapshot::from(&cfg.deployments, &map);
                (state, connected, total, snapshot)
            };

            // Rebuild menu only when something changed
            if snapshot != self.last_snapshot {
                let built = {
                    let map = self.state_map.lock().unwrap();
                    build_menu(&cfg.deployments, &map)
                };
                self._tray.set_menu(Some(Box::new(built.menu)));
                self.dashboard_id   = built.dashboard_id;
                self.cli_id         = built.cli_id;
                self.deployment_ids = built.deployment_ids;
                self.quit_id        = built.quit_id;
                self.last_snapshot  = snapshot;
            }

            if state != self.last_state {
                let icon = match state {
                    TrayState::AllConnected => self.icon_green.clone(),
                    TrayState::Partial      => self.icon_amber.clone(),
                    TrayState::Offline      => self.icon_red.clone(),
                };
                self._tray.set_icon(Some(icon)).ok();
                self.last_state = state;
            }

            self._tray.set_tooltip(Some(&tooltip(connected, total))).ok();
        }

        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(50),
        ));
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run_tray(
    state_map: StateMap,
    shutdown:  Arc<AtomicBool>,
    offline:   Arc<AtomicBool>,
    sock_path: PathBuf,
    cfg_path:  PathBuf,
) {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app    = TrayApp::new(state_map, shutdown, offline, sock_path, cfg_path);
    event_loop.run_app(&mut app).expect("event loop run");
}