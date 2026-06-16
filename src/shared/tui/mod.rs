//! TUI entry point. Owns the event loop; all logic lives in sub-modules.

pub mod background;
pub mod input;
pub mod panels;
pub mod popup;
pub mod state;
pub mod types;
pub mod kubernetes;

use std::io::{self, Write};
use std::process::exit;
use std::time::Duration;

use crossterm::{
    cursor::MoveTo,
    event::{self, Event},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
               disable_raw_mode, enable_raw_mode},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::time::sleep;

use MetadataService::apis::configuration::Configuration as MetadataConfiguration;

use crate::shared::core::{
    eject::{eject, uneject},
    k8_info::is_ejected,
    mount::{mount, unmount},
    types::{DbSchema, K8sService, Package},
};
use crate::shared::core::data_source::{self, fetch_current_workspace};
use crate::shared::tui::kubernetes::shell_into_pod;

use background::{TuiMsg, spawn_deployment_watcher, spawn_service_log_stream, spawn_container_fetch};
use input::{Action, handle_key, maybe_start_db_stream, switch_service_logs};
use state::TuiState;
use types::SidebarItem;

pub async fn fetch_metadata_and_process(
    metadata_config: &MetadataConfiguration,
    session_user:    &str,
) {
    let org_id = match fetch_current_workspace(metadata_config).await {
        Ok(id) => id,
        Err(e) => { eprintln!("Workspace fetch error: {e:?}"); exit(1); }
    };

    let packages = match data_source::fetch_packages(metadata_config, &org_id, "stage").await {
        Ok(mut pkgs) => {
            for pkg in &mut pkgs {
                let slug = crate::shared::core::image::pkg_to_slug(&pkg.identifier);
                pkg.mounted = crate::shared::core::k8_info::is_mounted(&slug).await;
            }
            pkgs
        }
        Err(e) => { eprintln!("Warning: package fetch failed: {e:?}"); vec![] }
    };

    let initial_services = match data_source::fetch_services(metadata_config, &org_id, 50).await {
        Ok(svcs) => svcs,
        Err(e)   => { eprintln!("{e:?}\nUnable to get metadata"); exit(1); }
    };

    let initial_db_schemas = match data_source::fetch_dbs_enriched(metadata_config, &org_id).await {
        Ok(schemas) => schemas,
        Err(e)      => { eprintln!("Warning: DB schema fetch failed: {e:?}"); vec![] }
    };

    if let Err(e) = run_tui(initial_services, packages, initial_db_schemas, session_user).await {
        eprintln!("TUI error: {}", e);
        exit(1);
    }
}

async fn run_tui(
    initial_services:   Vec<K8sService>,
    initial_packages:   Vec<Package>,
    initial_db_schemas: Vec<DbSchema>,
    _session_user:      &str,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend      = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new(initial_services, initial_packages, initial_db_schemas);
    let (bg_tx, bg_rx) = std::sync::mpsc::channel::<TuiMsg>();

    spawn_deployment_watcher(state.services.clone());

    {
        let svcs = state.services.lock().unwrap();
        if let Some(svc) = svcs.first() {
            if let Some(ref dep) = svc.deployment_name {
                state.svc_log_generation += 1;
                // FIX 1: pass cancel token
                spawn_service_log_stream(
                    bg_tx.clone(), dep.clone(), None,
                    state.svc_log_generation, state.svc_cancel.clone(),
                );
                spawn_container_fetch(bg_tx.clone(), dep.clone(), 0);
            }
        }
    }

    'main: loop {
        drain_background_messages(&bg_rx, &mut state, &bg_tx);

        let services_snap   = state.services.lock().unwrap().clone();
        let packages_snap   = state.packages.lock().unwrap().clone();
        let db_schemas_snap = state.db_schemas.lock().unwrap().clone();
        let logs_snap       = state.logs.lock().unwrap().clone();
        let db_logs_snap    = state.db_logs.lock().unwrap().clone();

        let (has_deployment, has_lang, is_ejected_now) =
            service_flags(&state.sidebar_item, &services_snap);

        let (active_container_idx, active_container_name) =
            active_container(&state, &services_snap);

        let viewing_ejected_container = is_viewing_ejected(
            &state.sidebar_item, &services_snap,
            active_container_name.as_deref(), &state.container_selection,
        );

        let can_shell = state.focus == types::Focus::Logs
            && matches!(state.sidebar_item, SidebarItem::Service(_))
            && has_deployment
            && !viewing_ejected_container;

        let db_logs_opt: Option<&[String]> = match state.sidebar_item {
            SidebarItem::DbSchema(i) if state.db_log_schema == Some(i) => db_logs_snap.as_deref(),
            _ => None,
        };

        let mut scroll_offset_tmp      = state.scroll_offset;
        let mut db_last_max_scroll_tmp = 0usize;
        let mut sidebar_scroll_tmp     = 0usize;
        let mut log_max_scroll_tmp     = state.log_max_scroll;

        terminal.draw(|f| {
            panels::draw(
                f,
                &services_snap, &packages_snap, &db_schemas_snap,
                &state.sidebar_item, &logs_snap, db_logs_opt,
                &state.focus,
                state.auto_scroll, state.scroll_offset,
                has_deployment, has_lang, is_ejected_now,
                state.popup.as_ref(),
                active_container_idx, active_container_name.as_deref(),
                &state.db_containers, state.db_selected_container.as_deref(),
                can_shell,
                &mut db_last_max_scroll_tmp,
                &mut scroll_offset_tmp,
                &mut sidebar_scroll_tmp,
                &mut log_max_scroll_tmp,
            );
        })?;

        state.scroll_offset  = scroll_offset_tmp;
        state.log_max_scroll = log_max_scroll_tmp;

        if !event::poll(Duration::from_millis(100))? { continue; }

        match event::read()? {
            Event::Key(key) => {
                let action = handle_key(
                    key, &mut state, &bg_tx,
                    &services_snap, &packages_snap, &db_schemas_snap,
                );
                match action {
                    Action::Continue => {}
                    Action::Quit     => break 'main,

                    Action::Shell(deployment, container) => {
                        leave_tui(&mut terminal)?;
                        let _ = shell_into_pod(&deployment, container.as_deref()).await;
                        enter_tui(&mut terminal)?;
                    }

                    Action::OpenVsCode(uri) => {
                        open_vscode(&mut terminal, &uri).await?;
                    }

                    Action::Eject { deployment, lang, meta, org } => {
                        leave_tui(&mut terminal)?;
                        if let Err(e) = eject(&deployment, &lang, &meta, &org).await {
                            eprintln!("Error: {e}");
                        }
                        if let SidebarItem::Service(i) = state.sidebar_item {
                            state.container_selection.remove(&i);
                        }
                        sleep(Duration::from_secs(2)).await;
                        enter_tui(&mut terminal)?;
                    }

                    Action::Uneject { deployment } => {
                        leave_tui(&mut terminal)?;
                        if let Err(e) = uneject(&deployment).await {
                            eprintln!("Error: {e}");
                        }
                        if let SidebarItem::Service(i) = state.sidebar_item {
                            state.container_selection.remove(&i);
                        }
                        sleep(Duration::from_secs(2)).await;
                        enter_tui(&mut terminal)?;
                    }

                    Action::Mount { org, id, lang } => {
                        leave_tui(&mut terminal)?;
                        match mount(&org, &id, &lang).await {
                            Ok(()) => {
                                if let SidebarItem::Package(i) = state.sidebar_item {
                                    if let Some(p) = state.packages.lock().unwrap().get_mut(i) {
                                        p.mounted = true;
                                    }
                                }
                                println!("✓ Mounted dev container for {id}");
                            }
                            Err(e) => eprintln!("✗ Mount failed: {e}"),
                        }
                        sleep(Duration::from_secs(1)).await;
                        enter_tui(&mut terminal)?;
                    }

                    Action::Unmount { org, id } => {
                        leave_tui(&mut terminal)?;
                        match unmount(&org, &id).await {
                            Ok(()) => {
                                if let SidebarItem::Package(i) = state.sidebar_item {
                                    if let Some(p) = state.packages.lock().unwrap().get_mut(i) {
                                        p.mounted = false;
                                    }
                                }
                                println!("✓ Unmounted dev container for {id}");
                            }
                            Err(e) => eprintln!("✗ Unmount failed: {e}"),
                        }
                        sleep(Duration::from_secs(1)).await;
                        enter_tui(&mut terminal)?;
                    }
                }
            }
            _ => {}
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn drain_background_messages(
    bg_rx: &std::sync::mpsc::Receiver<TuiMsg>,
    state: &mut TuiState,
    bg_tx: &std::sync::mpsc::Sender<TuiMsg>,
) {
    use input::select_container;

    loop {
        match bg_rx.try_recv() {
            Ok(TuiMsg::ServiceLogs { lines, generation }) => {
                if generation == state.svc_log_generation {
                    let key = if let SidebarItem::Service(i) = state.sidebar_item {
                        state.services.lock().unwrap().get(i).map(|s| s.meta_name.clone())
                    } else { None };
                    if let Some(k) = key {
                        state.logs.lock().unwrap().insert(k, lines);
                    }
                }
            }

            Ok(TuiMsg::DbLogs { lines, generation }) => {
                if generation == state.db_log_generation {
                    *state.db_logs.lock().unwrap() = Some(lines);
                }
            }

            Ok(TuiMsg::Containers { svc_idx, containers }) => {
                let ejected_container = state.services.lock().unwrap()
                    .get(svc_idx).and_then(|s| s.ejected_container.clone());
                let is_ejected_svc = state.services.lock().unwrap()
                    .get(svc_idx).map(|s| s.ejected).unwrap_or(false);

                {
                    let mut svcs = state.services.lock().unwrap();
                    if let Some(svc) = svcs.get_mut(svc_idx) {
                        svc.containers = containers.clone();
                    }
                }

                if !state.container_selection.contains_key(&svc_idx) {
                    let target_idx = if is_ejected_svc {
                        let ejected_name = ejected_container.as_deref().unwrap_or("");
                        containers.iter().position(|c| c.as_str() != ejected_name)
                    } else {
                        containers.first().map(|_| 0)
                    };

                    if let Some(idx) = target_idx {
                        let svcs_snap = state.services.lock().unwrap().clone();
                        // FIX 2: pass cancel token
                        select_container(
                            svc_idx, idx, &svcs_snap[svc_idx..=svc_idx],
                            &mut state.container_selection,
                            &mut state.svc_log_generation, bg_tx, &state.logs,
                            &mut state.svc_cancel,
                        );
                    } else if !state.container_selection.contains_key(&svc_idx) {
                        state.container_selection.insert(svc_idx, 0);
                        let svcs = state.services.lock().unwrap();
                        if let Some(svc) = svcs.get(svc_idx) {
                            state.logs.lock().unwrap().remove(&svc.meta_name);
                        }
                    }
                }
            }

            Ok(TuiMsg::DbContainers { schema_idx, containers }) => {
                if state.db_log_schema == Some(schema_idx) {
                    state.db_containers = containers.clone();
                    if let Some(first) = containers.first().cloned() {
                        state.db_selected_container = Some(first.clone());
                        // FIX 3: cancel old stream, pass new cancel token
                        state.db_cancel.cancel();
                        state.db_cancel = tokio_util::sync::CancellationToken::new();
                        state.db_log_generation += 1;
                        *state.db_logs.lock().unwrap() = None;

                        let slug = state.db_schemas.lock().unwrap()
                            .get(schema_idx)
                            .and_then(|s| s.k8s_name.clone())
                            .unwrap_or_default();

                        if !slug.is_empty() {
                            use background::spawn_db_log_stream as dls;
                            dls(bg_tx.clone(), slug, Some(first), state.db_log_generation, state.db_cancel.clone());
                        }
                    }
                }
            }

            Err(_) => break,
        }
    }
}

fn service_flags(
    sidebar_item:  &SidebarItem,
    services_snap: &[K8sService],
) -> (bool, bool, bool) {
    if let SidebarItem::Service(i) = sidebar_item {
        if let Some(svc) = services_snap.get(*i) {
            let has_dep  = svc.status != "Not deployed" && svc.status != "Unknown";
            let has_lang = svc.lang.is_some();
            return (has_dep, has_lang, svc.ejected);
        }
    }
    (false, false, false)
}

fn active_container(state: &TuiState, services_snap: &[K8sService]) -> (usize, Option<String>) {
    if let SidebarItem::Service(i) = state.sidebar_item {
        if let Some(svc) = services_snap.get(i) {
            let idx  = *state.container_selection.get(&i).unwrap_or(&0);
            let name = svc.containers.get(idx).cloned();
            return (idx, name);
        }
    }
    (0, None)
}

fn is_viewing_ejected(
    sidebar_item:        &SidebarItem,
    services_snap:       &[K8sService],
    active_container:    Option<&str>,
    container_selection: &std::collections::HashMap<usize, usize>,
) -> bool {
    let SidebarItem::Service(i) = sidebar_item else { return false };
    let Some(svc) = services_snap.get(*i) else { return false };
    if !svc.ejected { return false; }
    match (active_container, svc.ejected_container_name()) {
        (Some(ac), Some(ec)) => ac == ec,
        _                    => svc.containers.len() <= 1,
    }
}

fn leave_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,
             Clear(ClearType::All), MoveTo(0, 0))?;
    terminal.show_cursor()?;
    io::stdout().flush()?;
    Ok(())
}

fn enter_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
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
    match tokio::process::Command::new("code").arg("--folder-uri").arg(remote_uri).status().await {
        Ok(s) if s.success() => println!("✓ VS Code launched"),
        Ok(s)  => eprintln!("VS Code exited: {s}"),
        Err(e) => eprintln!("Failed to launch VS Code: {e}"),
    }
    sleep(Duration::from_secs(1)).await;
    enter_tui(terminal)?;
    Ok(())
}