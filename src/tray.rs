// src/tray.rs
use crate::{Config, ForwardStatus, StateMap};
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuItem, MenuEvent},
    Icon, TrayIconBuilder,
};
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
};

#[derive(PartialEq, Clone, Copy)]
enum TrayState { AllConnected, Partial, Offline }

const ICON_GREEN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#1D9E75" stroke-width="2"/>
  <circle cx="6"  cy="11" r="1.5" fill="#1D9E75"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#1D9E75" stroke-width="2"/>
  <circle cx="16" cy="11" r="1.5" fill="#1D9E75"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#1D9E75" stroke-width="2" stroke-linecap="round"/>
  <circle cx="18" cy="4" r="4" fill="#1D9E75"/>
  <path d="M15.5 4 L17.5 6 L20.5 2" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##;

const ICON_AMBER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#EF9F27" stroke-width="2"/>
  <circle cx="6"  cy="11" r="1.5" fill="#EF9F27"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#EF9F27" stroke-width="2" opacity="0.4"/>
  <circle cx="16" cy="11" r="1.5" fill="#EF9F27" opacity="0.4"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#EF9F27" stroke-width="2" stroke-linecap="round" stroke-dasharray="2 2"/>
  <circle cx="18" cy="4" r="4" fill="#EF9F27"/>
  <text x="18" y="5" font-size="6" font-weight="bold" text-anchor="middle" dominant-baseline="central" fill="#fff">!</text>
</svg>"##;

const ICON_RED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">
  <circle cx="6"  cy="11" r="3.5" fill="none" stroke="#E24B4A" stroke-width="2" opacity="0.4"/>
  <circle cx="6"  cy="11" r="1.5" fill="#E24B4A" opacity="0.4"/>
  <circle cx="16" cy="11" r="3.5" fill="none" stroke="#E24B4A" stroke-width="2" opacity="0.4"/>
  <circle cx="16" cy="11" r="1.5" fill="#E24B4A" opacity="0.4"/>
  <line x1="9.5" y1="11" x2="12.5" y2="11" stroke="#E24B4A" stroke-width="2" stroke-linecap="round" opacity="0.25"/>
  <line x1="9.5" y1="8.5" x2="12.5" y2="13.5" stroke="#E24B4A" stroke-width="2" stroke-linecap="round"/>
  <line x1="12.5" y1="8.5" x2="9.5" y2="13.5" stroke="#E24B4A" stroke-width="2" stroke-linecap="round"/>
  <circle cx="18" cy="4" r="4" fill="#E24B4A"/>
  <line x1="16" y1="4" x2="20" y2="4" stroke="#fff" stroke-width="2" stroke-linecap="round"/>
</svg>"##;

fn make_icon(svg: &str) -> Icon {
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opts).expect("valid svg");
    let mut pixmap = tiny_skia::Pixmap::new(22, 22).unwrap();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    Icon::from_rgba(pixmap.take(), 22, 22).expect("icon")
}

// Count connected vs total using config as the source of truth for total,
// and state_map for live status. This ensures the tray tooltip and the
// List response always agree, since both are now driven by config order.
fn compute_tray_state(
    map: &std::collections::HashMap<String, crate::ForwardState>,
    cfg_deployments: &[crate::DeploymentEntry],
) -> (TrayState, usize, usize) {
    let total = cfg_deployments.len();
    if total == 0 { return (TrayState::AllConnected, 0, 0); }

    let connected = cfg_deployments.iter()
        .filter(|e| map.get(&e.deployment_name)
            .map_or(false, |fw| fw.status == ForwardStatus::Connected))
        .count();

    let offline_all = cfg_deployments.iter()
        .all(|e| map.get(&e.deployment_name)
            .map_or(false, |fw| fw.status == ForwardStatus::Offline));

    if offline_all {
        (TrayState::Offline, 0, total)
    } else if connected == total {
        (TrayState::AllConnected, connected, total)
    } else {
        (TrayState::Partial, connected, total)
    }
}

fn tooltip(connected: usize, total: usize) -> String {
    if total == 0 { return "ginger-code — no deployments".to_string(); }
    format!("ginger-code — {}/{} connected", connected, total)
}

struct TrayApp {
    state_map:  StateMap,
    shutdown:   Arc<AtomicBool>,
    sock_path:  PathBuf,
    cfg_path:   PathBuf,
    quit_id:    tray_icon::menu::MenuId,
    _tray:      tray_icon::TrayIcon,
    icon_green: Icon,
    icon_amber: Icon,
    icon_red:   Icon,
    last_state: TrayState,
    last_tick:  std::time::Instant,
}

impl TrayApp {
    fn new(
        state_map: StateMap,
        shutdown: Arc<AtomicBool>,
        sock_path: PathBuf,
        cfg_path: PathBuf,
    ) -> Self {
        let icon_green = make_icon(ICON_GREEN);
        let icon_amber = make_icon(ICON_AMBER);
        let icon_red   = make_icon(ICON_RED);

        let quit_item = MenuItem::new("Quit ginger-code", true, None);
        let quit_id   = quit_item.id().clone();
        let menu = Menu::new();
        menu.append(&quit_item).unwrap();

        // Evaluate real state before building the tray so the initial icon is correct
        let (init_state, connected, total) = {
            let map = state_map.lock().unwrap();
            let cfg = Config::load(&cfg_path);
            compute_tray_state(&map, &cfg.deployments)
        };
        let init_icon = match init_state {
            TrayState::AllConnected => icon_green.clone(),
            TrayState::Partial      => icon_amber.clone(),
            TrayState::Offline      => icon_red.clone(),
        };

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(init_icon)
            .with_tooltip(tooltip(connected, total))
            .build()
            .expect("tray icon");

        Self {
            state_map, shutdown, sock_path, cfg_path, quit_id, _tray: tray,
            icon_green, icon_amber, icon_red,
            last_state: init_state,
            last_tick: std::time::Instant::now(),
        }
    }
}

impl ApplicationHandler for TrayApp {
    fn resumed(&mut self, _el: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _el: &ActiveEventLoop,
        _id: winit::window::WindowId,
        _ev: winit::event::WindowEvent,
    ) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Drain menu events
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == self.quit_id {
                println!("[ginger-code] quit via tray");
                self.shutdown.store(true, Ordering::Relaxed);
                let _ = std::fs::remove_file(&self.sock_path);
                event_loop.exit();
                return;
            }
        }

        // Update icon/tooltip once per second
        if self.last_tick.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_tick = std::time::Instant::now();

            // Load config here so tooltip total matches List response total exactly
            let cfg = Config::load(&self.cfg_path);
            let (state, connected, total) = {
                let map = self.state_map.lock().unwrap();
                compute_tray_state(&map, &cfg.deployments)
            };

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

pub fn run_tray(
    state_map: StateMap,
    shutdown: Arc<AtomicBool>,
    sock_path: PathBuf,
    cfg_path: PathBuf,
) {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = TrayApp::new(state_map, shutdown, sock_path, cfg_path);
    event_loop.run_app(&mut app).expect("event loop run");
}