pub mod popup;
pub mod render;
pub mod types;
pub mod kubernetes;

use std::{
    collections::HashMap,
    io::{self, Write},
    process::exit,
    sync::{Arc, Mutex},
    time::Duration,
};
use crate::shared::core::data_source;

use crossterm::{
    cursor::MoveTo,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::time::sleep;

use MetadataService::{
    apis::{
        configuration::Configuration as MetadataConfiguration,
    },
};


use crate::shared::core::{eject::{eject, uneject}, k8_info::{get_k8s_deployments, get_pod_logs, is_ejected}, mount::{mount , unmount}, types::{K8sService, Package}};

use self::{
    kubernetes::{shell_into_pod},
    render::draw,
    types::{Focus, Popup, PopupAction, SidebarItem},
};

/* ================================================================
   ENTRY POINT
   ================================================================ */
pub async fn fetch_metadata_and_process(
    metadata_config: &MetadataConfiguration,
    session_user: &str,
) {
    // ── Packages (non-fatal) ──────────────────────────────────────────────────
    let packages: Vec<Package> =
        match data_source::fetch_packages(metadata_config, "ginger-society", "stage").await {
            Ok(pkgs) => pkgs,
            Err(e) => {
                eprintln!("Warning: package fetch failed: {e:?}");
                vec![]
            }
        };

    // ── Services (fatal on error) ─────────────────────────────────────────────
    let initial_services: Vec<K8sService> =
        match data_source::fetch_services(metadata_config, "ginger-society", 50).await {
            Ok(svcs) => svcs,
            Err(e) => {
                eprintln!("{e:?}\nUnable to get metadata");
                exit(1);
            }
        };

    if let Err(e) = run_tui(initial_services, packages, session_user).await {
        eprintln!("TUI error: {}", e);
        exit(1);
    }
}

/* ================================================================
   TUI LOOP
   ================================================================ */

async fn run_tui(
    initial_services: Vec<K8sService>,
    initial_packages: Vec<Package>,
    _session_user:    &str,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend      = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let services:  Arc<Mutex<Vec<K8sService>>> = Arc::new(Mutex::new(initial_services));
    let packages:  Arc<Mutex<Vec<Package>>>    = Arc::new(Mutex::new(initial_packages));
    let logs:      Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    // The index of the currently "active" service for log streaming.
    let svc_log_idx: Arc<Mutex<usize>>         = Arc::new(Mutex::new(0));

    // ── Background: k8s status + ejected flags ────────────────────────────────
    {
        let services = services.clone();
        tokio::spawn(async move {
            loop {
                let deployments = get_k8s_deployments().await;
                {
                    let mut svcs = services.lock().unwrap();
                    for svc in svcs.iter_mut() {
                        if let Some(ref dep) = svc.deployment_name {
                            if let Some((status, ready)) = deployments.get(dep) {
                                svc.status = status.clone();
                                svc.ready  = ready.clone();
                            } else {
                                svc.status = "Not deployed".to_string();
                                svc.ready  = "-".to_string();
                            }
                        }
                    }
                }
                let deps: Vec<(usize, String)> = {
                    let svcs = services.lock().unwrap();
                    svcs.iter().enumerate()
                        .filter_map(|(i, s)| s.deployment_name.clone().map(|d| (i, d)))
                        .collect()
                };
                for (i, dep) in deps {
                    let ejected = is_ejected(&dep).await;
                    if let Some(svc) = services.lock().unwrap().get_mut(i) {
                        svc.ejected = ejected;
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
    }

    // ── Background: stream logs for the active service ────────────────────────
    {
        let services    = services.clone();
        let logs        = logs.clone();
        let svc_log_idx = svc_log_idx.clone();
        tokio::spawn(async move {
            loop {
                let (dep_name, meta_name) = {
                    let svcs = services.lock().unwrap();
                    let idx  = *svc_log_idx.lock().unwrap();
                    svcs.get(idx)
                        .map(|s| (s.deployment_name.clone(), s.meta_name.clone()))
                        .unwrap_or((None, String::new()))
                };
                if let Some(dep) = dep_name {
                    let lines = get_pod_logs(&dep).await;
                    logs.lock().unwrap().insert(meta_name, lines);
                }
                sleep(Duration::from_secs(2)).await;
            }
        });
    }

    // ── UI state ──────────────────────────────────────────────────────────────
    let mut focus:         Focus       = Focus::Sidebar;
    let mut sidebar_item:  SidebarItem = SidebarItem::Service(0);
    let mut auto_scroll:   bool        = true;
    let mut scroll_offset: usize       = 0;
    let mut popup:         Option<Popup> = None;
    let mut sidebar_scroll: usize      = 0; // last rendered scroll offset, for hit-test

    loop {
        let services_snap = services.lock().unwrap().clone();
        let packages_snap = packages.lock().unwrap().clone();
        let logs_snap     = logs.lock().unwrap().clone();

        // Derive per-frame context from sidebar_item.
        let (selected_svc, has_deployment, has_lang, is_ejected_now) =
            if let SidebarItem::Service(i) = sidebar_item {
                let svc = services_snap.get(i);
                let has_dep = svc.map(|s| s.status != "Not deployed" && s.status != "Unknown").unwrap_or(false);
                let has_lang = svc.and_then(|s| s.lang.as_ref()).is_some();
                let ejected  = svc.map(|s| s.ejected).unwrap_or(false);
                (svc, has_dep, has_lang, ejected)
            } else {
                (None, false, false, false)
            };

        // ── Draw ──────────────────────────────────────────────────────────────
        terminal.draw(|f| {
            // Sync log scroll.
            if let Some(svc) = selected_svc {
                let log_text = logs_snap.get(&svc.meta_name).map(|l| l.join("\n")).unwrap_or_default();
                let max_scroll = log_text.lines().count()
                    .saturating_sub(f.size().height.saturating_sub(10) as usize);
                if auto_scroll {
                    scroll_offset = max_scroll;
                } else {
                    scroll_offset = scroll_offset.min(max_scroll);
                    if scroll_offset >= max_scroll { auto_scroll = true; }
                }
            }

            let drawn = draw(
                f,
                &services_snap,
                &packages_snap,
                &sidebar_item,
                &logs_snap,
                &focus,
                auto_scroll,
                scroll_offset,
                has_deployment,
                has_lang,
                is_ejected_now,
                popup.as_ref(),
            );
            sidebar_scroll = drawn.sidebar_scroll;
        })?;

        /* ================================================================
           INPUT
           ================================================================ */
        if !event::poll(Duration::from_millis(100))? { continue; }

        match event::read()? {

            /* ── Mouse ──────────────────────────────────────────────────── */
            Event::Mouse(mouse) => {
                if popup.is_some() {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) { popup = None; }
                    continue;
                }
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let (col, row) = (mouse.column, mouse.row);
                        // Check sidebar click.
                        let sidebar_area = {
                            // Recompute sidebar rect — same split as in draw().
                            let term = terminal.size()?;
                            let root_h = term.height.saturating_sub(2);
                            let sidebar_w = term.width * 35 / 100;
                            ratatui::layout::Rect { x: 0, y: 0, width: sidebar_w, height: root_h }
                        };
                        if let Some(item) = render::click_sidebar_item(
                            col, row, sidebar_area, sidebar_scroll,
                            services_snap.len(), packages_snap.len(),
                        ) {
                            focus = Focus::Sidebar;
                            if let SidebarItem::Service(i) = item {
                                *svc_log_idx.lock().unwrap() = i;
                                auto_scroll = true;
                            }
                            sidebar_item = item;
                        } else {
                            // Clicked right pane → focus logs (for service view).
                            if matches!(sidebar_item, SidebarItem::Service(_)) {
                                focus = Focus::Logs;
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if focus == Focus::Logs { auto_scroll = false; scroll_offset = scroll_offset.saturating_sub(3); }
                    }
                    MouseEventKind::ScrollDown => {
                        if focus == Focus::Logs { scroll_offset += 3; }
                    }
                    _ => {}
                }
            }

            /* ── Keyboard ───────────────────────────────────────────────── */
            Event::Key(key) => {
                /* ── Popup active ───────────────────────────────────────── */
                if let Some(ref mut p) = popup {
                    if p.action == PopupAction::ShellBlocked { popup = None; continue; }
                    match key.code {
                        KeyCode::Left  | KeyCode::Char('h') => p.selected = 0,
                        KeyCode::Right | KeyCode::Char('l') => p.selected = 1,
                        KeyCode::Tab => p.selected = (p.selected + 1) % 2,
                        KeyCode::Esc => { popup = None; }
                        KeyCode::Enter => {
                            if p.selected == 0 {
                                match p.action {
                                    PopupAction::Quit => { popup = None; break; }
                                    PopupAction::ShellBlocked => unreachable!(),

                                    PopupAction::Eject | PopupAction::Uneject => {
                                        if let SidebarItem::Service(svc_i) = sidebar_item {
                                            let svcs = services.lock().unwrap();
                                            if let Some(svc) = svcs.get(svc_i) {
                                                let dep      = svc.deployment_name.clone();
                                                let lang     = svc.lang.clone();
                                                let ejected  = svc.ejected;
                                                let meta     = svc.meta_name.clone();
                                                let org      = svc.organization_id.clone();
                                                drop(svcs);
                                                if let Some(dep_name) = dep {
                                                    popup = None;
                                                    leave_tui(&mut terminal)?;
                                                    let r = if ejected {
                                                        uneject(&dep_name).await
                                                    } else {
                                                        eject(&dep_name, lang.as_deref().unwrap_or(""), &meta, &org).await
                                                    };
                                                    if let Err(e) = r { eprintln!("Error: {e}"); }
                                                    sleep(Duration::from_secs(2)).await;
                                                    enter_tui(&mut terminal)?;
                                                    continue;
                                                }
                                            }
                                        }
                                        popup = None;
                                    }

                                    PopupAction::Mount => {
                                        if let SidebarItem::Package(pkg_i) = sidebar_item {
                                            let (org, id, lang) = {
                                                let pkgs = packages.lock().unwrap();
                                                pkgs.get(pkg_i).map(|p| (
                                                    p.organization_id.clone(),
                                                    p.identifier.clone(),
                                                    p.lang.clone(),
                                                )).unwrap_or_default()
                                            };
                                            popup = None;
                                            leave_tui(&mut terminal)?;
                                            match mount(&org, &id, &lang).await {
                                                Ok(()) => {
                                                    if let Some(p) = packages.lock().unwrap().get_mut(pkg_i) {
                                                        p.mounted = true;
                                                    }
                                                    println!("✓ Mounted dev container for {id}");
                                                }
                                                Err(e) => eprintln!("✗ Mount failed: {e}"),
                                            }
                                            sleep(Duration::from_secs(1)).await;
                                            enter_tui(&mut terminal)?;
                                        } else {
                                            popup = None;
                                        }
                                        continue;
                                    }

                                    PopupAction::Unmount => {
                                        if let SidebarItem::Package(pkg_i) = sidebar_item {
                                            let (org, id) = {
                                                let pkgs = packages.lock().unwrap();
                                                pkgs.get(pkg_i).map(|p| (
                                                    p.organization_id.clone(),
                                                    p.identifier.clone(),
                                                )).unwrap_or_default()
                                            };
                                            popup = None;
                                            leave_tui(&mut terminal)?;
                                            match unmount(&org, &id).await {
                                                Ok(()) => {
                                                    if let Some(p) = packages.lock().unwrap().get_mut(pkg_i) {
                                                        p.mounted = false;
                                                    }
                                                    println!("✓ Unmounted dev container for {id}");
                                                }
                                                Err(e) => eprintln!("✗ Unmount failed: {e}"),
                                            }
                                            sleep(Duration::from_secs(1)).await;
                                            enter_tui(&mut terminal)?;
                                        } else {
                                            popup = None;
                                        }
                                        continue;
                                    }
                                }
                            } else {
                                popup = None;
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                /* ── Normal keys ────────────────────────────────────────── */
                match key.code {
                    KeyCode::Char('q') => {
                        popup = Some(Popup { service_name: String::new(), action: PopupAction::Quit, selected: 1 });
                    }

                    // ── Arrow left: move focus back to sidebar ────────────
                    KeyCode::Left => { focus = Focus::Sidebar; }

                    // ── Arrow right: move focus to logs pane (service only)
                    KeyCode::Right => {
                        if matches!(sidebar_item, SidebarItem::Service(_)) {
                            focus = Focus::Logs;
                        }
                    }

                    // ── Up / k ────────────────────────────────────────────
                    KeyCode::Up | KeyCode::Char('k') => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset = scroll_offset.saturating_sub(1);
                        } else {
                            sidebar_item = sidebar_prev(&sidebar_item, &services_snap, &packages_snap);
                            if let SidebarItem::Service(i) = sidebar_item {
                                *svc_log_idx.lock().unwrap() = i;
                                auto_scroll = true;
                            }
                        }
                    }

                    // ── Down / j ──────────────────────────────────────────
                    KeyCode::Down | KeyCode::Char('j') => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset += 1;
                        } else {
                            sidebar_item = sidebar_next(&sidebar_item, &services_snap, &packages_snap);
                            if let SidebarItem::Service(i) = sidebar_item {
                                *svc_log_idx.lock().unwrap() = i;
                                auto_scroll = true;
                            }
                        }
                    }

                    KeyCode::PageDown => { if focus == Focus::Logs { auto_scroll = true; } }
                    KeyCode::PageUp   => { if focus == Focus::Logs { auto_scroll = false; scroll_offset = 0; } }
                    KeyCode::Char('g') => { if focus == Focus::Logs { auto_scroll = false; scroll_offset = 0; } }
                    KeyCode::Char('G') => { if focus == Focus::Logs { auto_scroll = true; } }

                    // ── m: mount / unmount ────────────────────────────────
                    KeyCode::Char('m') => {
                        if let SidebarItem::Package(pkg_i) = sidebar_item {
                            if let Some(pkg) = packages_snap.get(pkg_i) {
                                popup = Some(Popup {
                                    service_name: pkg.identifier.clone(),
                                    action:       if pkg.mounted { PopupAction::Unmount } else { PopupAction::Mount },
                                    selected:     0,
                                });
                            }
                        }
                    }

                    // ── c: VS Code ────────────────────────────────────────
                    KeyCode::Char('c') => {
                        match sidebar_item {
                            SidebarItem::Package(pkg_i) => {
                                if let Some(pkg) = packages_snap.get(pkg_i) {
                                    if pkg.mounted {
                                        let alias = format!("{}-{}-local", pkg.organization_id, pkg.identifier);
                                        let uri   = format!("vscode-remote://ssh-remote+{}/workspace/{}-{}",
                                            alias, pkg.organization_id, pkg.identifier);
                                        open_vscode(&mut terminal, &uri).await?;
                                    }
                                }
                            }
                            SidebarItem::Service(svc_i) => {
                                if is_ejected_now {
                                    if let Some(svc) = services_snap.get(svc_i) {
                                        if let Some(ref dep) = svc.deployment_name {
                                            let uri = format!(
                                                "vscode-remote://ssh-remote+{}-local/workspace/{}-{}",
                                                dep, svc.organization_id, dep
                                            );
                                            open_vscode(&mut terminal, &uri).await?;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── s: shell into pod ─────────────────────────────────
                    KeyCode::Char('s') => {
                        if let SidebarItem::Service(svc_i) = sidebar_item {
                            if let Some(svc) = services_snap.get(svc_i) {
                                if svc.status != "Not deployed" && svc.status != "Unknown" {
                                    if svc.ejected {
                                        popup = Some(Popup {
                                            service_name: String::new(),
                                            action:       PopupAction::ShellBlocked,
                                            selected:     0,
                                        });
                                    } else if let Some(ref dep) = svc.deployment_name {
                                        let dep = dep.clone();
                                        leave_tui(&mut terminal)?;
                                        let _ = shell_into_pod(&dep).await;
                                        enter_tui(&mut terminal)?;
                                    }
                                }
                            }
                        }
                    }

                    // ── e: eject / uneject ────────────────────────────────
                    KeyCode::Char('e') => {
                        if has_deployment && has_lang {
                            if let SidebarItem::Service(svc_i) = sidebar_item {
                                if let Some(svc) = services_snap.get(svc_i) {
                                    popup = Some(Popup {
                                        service_name: svc.meta_name.clone(),
                                        action:       if svc.ejected { PopupAction::Uneject } else { PopupAction::Eject },
                                        selected:     0,
                                    });
                                }
                            }
                        }
                    }

                    _ => {}
                }
            }

            _ => {}
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/* ================================================================
   SIDEBAR NAVIGATION HELPERS
   ================================================================ */

/// Total items in the sidebar (services + packages).
fn sidebar_total(services: &[K8sService], packages: &[Package]) -> usize {
    services.len() + packages.len()
}

/// Convert a sidebar_item to its flat index (services first, packages after).
fn sidebar_flat(item: &SidebarItem, svc_count: usize) -> usize {
    match item {
        SidebarItem::Service(i) => *i,
        SidebarItem::Package(i) => svc_count + i,
    }
}

/// Convert a flat index back to a SidebarItem.
fn sidebar_from_flat(flat: usize, svc_count: usize) -> SidebarItem {
    if flat < svc_count {
        SidebarItem::Service(flat)
    } else {
        SidebarItem::Package(flat - svc_count)
    }
}

fn sidebar_next(
    current:  &SidebarItem,
    services: &[K8sService],
    packages: &[Package],
) -> SidebarItem {
    let total = sidebar_total(services, packages);
    if total == 0 { return current.clone(); }
    let flat  = sidebar_flat(current, services.len());
    let next  = (flat + 1).min(total - 1);
    sidebar_from_flat(next, services.len())
}

fn sidebar_prev(
    current:  &SidebarItem,
    services: &[K8sService],
    packages: &[Package],
) -> SidebarItem {
    if sidebar_total(services, packages) == 0 { return current.clone(); }
    let flat = sidebar_flat(current, services.len());
    sidebar_from_flat(flat.saturating_sub(1), services.len())
}

/* ================================================================
   TUI SUSPEND / RESUME
   ================================================================ */

fn leave_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture,
        Clear(ClearType::All), MoveTo(0, 0))?;
    terminal.show_cursor()?;
    io::stdout().flush()?;
    Ok(())
}

fn enter_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

async fn open_vscode<B: ratatui::backend::Backend + io::Write>(
    terminal:   &mut Terminal<B>,
    remote_uri: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    leave_tui(terminal)?;
    println!("Opening VS Code: {}", remote_uri);
    match tokio::process::Command::new("code")
        .arg("--folder-uri").arg(remote_uri).status().await
    {
        Ok(s) if s.success() => println!("✓ VS Code launched"),
        Ok(s)  => eprintln!("VS Code exited: {s}"),
        Err(e) => eprintln!("Failed to launch VS Code: {e}"),
    }
    sleep(Duration::from_secs(1)).await;
    enter_tui(terminal)?;
    Ok(())
}