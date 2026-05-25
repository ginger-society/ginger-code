pub mod eject;
pub mod kubernetes;
pub mod popup;
pub mod render;
pub mod types;

use std::{
    collections::HashMap,
    io::{self, Write},
    path::Path,
    process::exit,
    sync::{Arc, Mutex},
    time::Duration,
};

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
        default_api::{metadata_get_services_and_envs, MetadataGetServicesAndEnvsParams},
    },
};
use ginger_shared_rs::read_service_config_file;

use self::{
    eject::{eject, uneject},
    kubernetes::{
        get_k8s_deployments, get_pod_logs, is_ejected, meta_to_deployment_name, shell_into_pod,
    },
    render::draw,
    types::{Focus, K8sService, Popup, PopupAction},
};

/* ================================================================
   ENTRY POINT
   ================================================================ */

pub async fn fetch_metadata_and_process(
    metadata_config: &MetadataConfiguration,
    session_user: &str,
) {

    let raw_services = match metadata_get_services_and_envs(
        metadata_config,
        MetadataGetServicesAndEnvsParams {
            page_number: Some("1".to_string()),
            page_size:   Some("50".to_string()),
            org_id:      "ginger-society".to_string(),  // TODO: get from session or config
        },
    )
    .await
    {
        Ok(s)  => s,
        Err(e) => {
            eprintln!("{:?}", e);
            eprintln!("Unable to get metadata");
            exit(1);
        }
    };

    let initial_services: Vec<K8sService> = raw_services
        .iter()
        .map(|s| {
            let meta_name       = format!("@{}/{}", s.organization_id, s.identifier);
            let deployment_name = meta_to_deployment_name(&meta_name);
            let lang            = s.lang
                .as_ref()
                .and_then(|l| l.as_ref())
                .map(|l| l.clone());
            K8sService {
                meta_name,
                deployment_name: Some(deployment_name),
                status: "Unknown".to_string(),
                ready:  "-".to_string(),
                organization_id: s.organization_id.clone(),
                lang,
                ejected: false,
            }
        })
        .collect();

    if let Err(e) = run_tui(initial_services, session_user).await {
        eprintln!("TUI error: {}", e);
        exit(1);
    }
}

/* ================================================================
   TUI LOOP
   ================================================================ */

async fn run_tui(
    initial_services: Vec<K8sService>,
    session_user: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend      = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let services:     Arc<Mutex<Vec<K8sService>>>              = Arc::new(Mutex::new(initial_services));
    let logs:         Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let selected_idx: Arc<Mutex<usize>>                        = Arc::new(Mutex::new(0));

    // ── Background: poll k8s deployment statuses + ejected flag ──────────────
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
                    svcs.iter()
                        .enumerate()
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

    // ── Background: stream logs for selected service ──────────────────────────
    {
        let services     = services.clone();
        let logs         = logs.clone();
        let selected_idx = selected_idx.clone();
        tokio::spawn(async move {
            loop {
                let (dep_name, meta_name) = {
                    let svcs = services.lock().unwrap();
                    let idx  = *selected_idx.lock().unwrap();
                    svcs.get(idx)
                        .map(|s| (s.deployment_name.clone(), s.meta_name.clone()))
                        .unwrap_or((None, String::new()))
                };

                if let Some(dep) = dep_name {
                    let new_logs = get_pod_logs(&dep).await;
                    logs.lock().unwrap().insert(meta_name, new_logs);
                }

                sleep(Duration::from_secs(2)).await;
            }
        });
    }

    let mut focus         = Focus::Services;
    let mut auto_scroll   = true;
    let mut scroll_offset: usize = 0;
    let mut popup: Option<Popup> = None;

    let mut services_list_area = ratatui::layout::Rect::default();
    let mut logs_area          = ratatui::layout::Rect::default();

    loop {
        let services_snap  = services.lock().unwrap().clone();
        let current_idx    = *selected_idx.lock().unwrap();
        let selected       = services_snap.get(current_idx).cloned();
        let has_deployment = selected
            .as_ref()
            .map(|s| s.status != "Not deployed" && s.status != "Unknown")
            .unwrap_or(false);
        let has_lang       = selected.as_ref().and_then(|s| s.lang.as_ref()).is_some();
        let is_ejected_now = selected.as_ref().map(|s| s.ejected).unwrap_or(false);

        // ── Draw ──────────────────────────────────────────────────────────────
        let logs_snap = logs.lock().unwrap().clone();
        terminal.draw(|f| {
            let log_text = selected.as_ref().and_then(|s| logs_snap.get(&s.meta_name))
                .map(|l| l.join("\n"))
                .unwrap_or_default();
            let num_lines  = log_text.lines().count();
            let height     = f.size().height.saturating_sub(10) as usize;
            let max_scroll = num_lines.saturating_sub(height);
            if auto_scroll {
                scroll_offset = max_scroll;
            } else {
                scroll_offset = scroll_offset.min(max_scroll);
                if scroll_offset >= max_scroll {
                    auto_scroll = true;
                }
            }

            draw(
                f,
                &services_snap,
                current_idx,
                selected.as_ref(),
                &logs_snap,
                &focus,
                auto_scroll,
                scroll_offset,
                has_deployment,
                has_lang,
                is_ejected_now,
                popup.as_ref(),
            );
        })?;

        {
            use ratatui::layout::{Constraint, Direction, Layout};
            let area = terminal.get_frame().size();
            let root = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(2)])
                .split(area);
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(root[0]);
            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(6), Constraint::Min(0)])
                .split(chunks[1]);
            services_list_area = chunks[0];
            logs_area          = right_chunks[1];
        }

        /* ================================================================
           INPUT
           ================================================================ */
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {

                /* ── Mouse ──────────────────────────────────────────────── */
                Event::Mouse(mouse) => {
                    if popup.is_some() {
                        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                            popup = None;
                        }
                        continue;
                    }
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let (col, row) = (mouse.column, mouse.row);
                            if let Some(idx) = render::click_service_index(
                                col, row, services_list_area, services_snap.len(),
                            ) {
                                focus = Focus::Services;
                                *selected_idx.lock().unwrap() = idx;
                                auto_scroll = true;
                            } else if render::point_in_rect(col, row, logs_area) {
                                focus = Focus::Logs;
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            if focus == Focus::Logs {
                                auto_scroll   = false;
                                scroll_offset = scroll_offset.saturating_sub(3);
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if focus == Focus::Logs {
                                scroll_offset += 3;
                            }
                        }
                        _ => {}
                    }
                }

                /* ── Keyboard ───────────────────────────────────────────── */
                Event::Key(key) => {
                    /* ── Popup active ───────────────────────────────────── */
                    if let Some(ref mut p) = popup {
                        if p.action == PopupAction::ShellBlocked {
                            popup = None;
                            continue;
                        }
                        match key.code {
                            KeyCode::Left  | KeyCode::Char('h') => p.selected = 0,
                            KeyCode::Right | KeyCode::Char('l') => p.selected = 1,
                            KeyCode::Tab => p.selected = (p.selected + 1) % 2,
                            KeyCode::Enter => {
                                if p.selected == 0 {
                                    match p.action {
                                        PopupAction::Quit => {
                                            popup = None;
                                            break;
                                        }
                                        PopupAction::ShellBlocked => unreachable!(),
                                        PopupAction::Eject | PopupAction::Uneject => {
                                            // ── grab dep, lang, ejected, meta_name ──
                                            let (dep, lang, ejected, meta_name) = {
                                                let svcs = services.lock().unwrap();
                                                let idx  = *selected_idx.lock().unwrap();
                                                svcs.get(idx)
                                                    .map(|s| (
                                                        s.deployment_name.clone(),
                                                        s.lang.clone(),
                                                        s.ejected,
                                                        s.meta_name.clone(),
                                                    ))
                                                    .unwrap_or((None, None, false, String::new()))
                                            };

                                            if let Some(dep_name) = dep {
                                                popup = None;
                                                disable_raw_mode()?;
                                                execute!(
                                                    terminal.backend_mut(),
                                                    LeaveAlternateScreen,
                                                    DisableMouseCapture,
                                                    Clear(ClearType::All),
                                                    MoveTo(0, 0)
                                                )?;
                                                terminal.show_cursor()?;
                                                io::stdout().flush()?;

                                                let result = if ejected {
                                                    uneject(&dep_name).await
                                                } else {
                                                    eject(
                                                        &dep_name,
                                                        lang.as_deref().unwrap_or(""),
                                                        &meta_name,
                                                    ).await
                                                };

                                                if let Err(e) = result {
                                                    eprintln!("Error: {}", e);
                                                }
                                                sleep(Duration::from_secs(2)).await;

                                                enable_raw_mode()?;
                                                execute!(
                                                    terminal.backend_mut(),
                                                    EnterAlternateScreen,
                                                    EnableMouseCapture
                                                )?;
                                                terminal.hide_cursor()?;
                                                terminal.clear()?;
                                                continue;
                                            }
                                        }
                                    }
                                }
                                popup = None;
                            }
                            KeyCode::Esc => popup = None,
                            _ => {}
                        }
                        continue;
                    }

                    /* ── Normal keys ────────────────────────────────────── */
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            popup = Some(Popup {
                                service_name: String::new(),
                                action:   PopupAction::Quit,
                                selected: 1,
                            });
                        }

                        KeyCode::Left  => focus = Focus::Services,
                        KeyCode::Right => focus = Focus::Logs,

                        KeyCode::Down | KeyCode::Char('j') => {
                            if focus == Focus::Services {
                                let len = services.lock().unwrap().len();
                                if len > 0 {
                                    let mut idx = selected_idx.lock().unwrap();
                                    *idx = (*idx + 1).min(len - 1);
                                    auto_scroll = true;
                                }
                            } else {
                                auto_scroll   = false;
                                scroll_offset += 1;
                            }
                        }

                        KeyCode::Up | KeyCode::Char('k') => {
                            if focus == Focus::Services {
                                let mut idx = selected_idx.lock().unwrap();
                                *idx = idx.saturating_sub(1);
                                auto_scroll = true;
                            } else {
                                auto_scroll   = false;
                                scroll_offset = scroll_offset.saturating_sub(1);
                            }
                        }

                        KeyCode::PageDown  => { if focus == Focus::Logs { auto_scroll = true; } }
                        KeyCode::PageUp    => { if focus == Focus::Logs { auto_scroll = false; scroll_offset = 0; } }
                        KeyCode::Char('g') => { if focus == Focus::Logs { auto_scroll = false; scroll_offset = 0; } }
                        KeyCode::Char('G') => { if focus == Focus::Logs { auto_scroll = true; } }

                        /* ── Open VS Code remote ────────────────────────── */
                        KeyCode::Char('c') => {
                            if has_deployment && is_ejected_now {
                                if let Some((dep_name, organization_id)) = selected
                                        .as_ref()
                                        .and_then(|s| s.deployment_name.as_ref().map(|d| (d, &s.organization_id)))
                                    {
                                    let host_alias = format!("{}-local", dep_name);
                                    let remote_uri = format!(
                                        "vscode-remote://ssh-remote+{}/workspace/{}-{}",
                                        host_alias,
                                        organization_id, // change this to organization id
                                        dep_name
                                    );

                                    disable_raw_mode()?;
                                    execute!(
                                        terminal.backend_mut(),
                                        LeaveAlternateScreen,
                                        DisableMouseCapture,
                                        Clear(ClearType::All),
                                        MoveTo(0, 0)
                                    )?;
                                    terminal.show_cursor()?;
                                    io::stdout().flush()?;

                                    println!("Opening VS Code: {}", remote_uri);
                                    let result = tokio::process::Command::new("code")
                                        .arg("--folder-uri")
                                        .arg(&remote_uri)
                                        .status()
                                        .await;

                                    match result {
                                        Ok(s) if s.success() => println!("✓ VS Code launched"),
                                        Ok(s)  => eprintln!("VS Code exited with status: {}", s),
                                        Err(e) => eprintln!("Failed to launch VS Code (is `code` in PATH?): {e}"),
                                    }

                                    sleep(Duration::from_secs(1)).await;

                                    enable_raw_mode()?;
                                    execute!(
                                        terminal.backend_mut(),
                                        EnterAlternateScreen,
                                        EnableMouseCapture
                                    )?;
                                    terminal.hide_cursor()?;
                                    terminal.clear()?;
                                }
                            }
                        }

                        /* ── Shell into pod ─────────────────────────────── */
                        KeyCode::Char('s') => {
                            let (dep, ejected) = {
                                let svcs = services.lock().unwrap();
                                let idx  = *selected_idx.lock().unwrap();
                                svcs.get(idx)
                                    .filter(|s| {
                                        s.status != "Not deployed" && s.status != "Unknown"
                                    })
                                    .map(|s| (s.deployment_name.clone(), s.ejected))
                                    .unwrap_or((None, false))
                            };

                            if dep.is_some() && ejected {
                                popup = Some(Popup {
                                    service_name: String::new(),
                                    action:   PopupAction::ShellBlocked,
                                    selected: 0,
                                });
                            } else if let Some(dep_name) = dep {
                                disable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    LeaveAlternateScreen,
                                    DisableMouseCapture,
                                    Clear(ClearType::All),
                                    MoveTo(0, 0)
                                )?;
                                terminal.show_cursor()?;
                                io::stdout().flush()?;

                                let _ = shell_into_pod(&dep_name).await;

                                enable_raw_mode()?;
                                execute!(
                                    terminal.backend_mut(),
                                    EnterAlternateScreen,
                                    EnableMouseCapture
                                )?;
                                terminal.hide_cursor()?;
                                terminal.clear()?;
                            }
                        }

                        /* ── Eject / uneject ────────────────────────────── */
                        KeyCode::Char('e') => {
                            if has_deployment && has_lang {
                                let svc_name = selected
                                    .as_ref()
                                    .map(|s| s.meta_name.clone())
                                    .unwrap_or_default();
                                popup = Some(Popup {
                                    service_name: svc_name,
                                    action:   if is_ejected_now {
                                        PopupAction::Uneject
                                    } else {
                                        PopupAction::Eject
                                    },
                                    selected: 0,
                                });
                            }
                        }

                        _ => {}
                    }
                }

                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}