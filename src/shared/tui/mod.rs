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
use crate::shared::core::data_source::{self, fetch_current_workspace};

use crossterm::{
    cursor::MoveTo,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc as async_mpsc;
use tokio::time::sleep;

use MetadataService::apis::configuration::Configuration as MetadataConfiguration;

use crate::shared::core::{
    eject::{eject, uneject},
    k8_info::{get_k8s_deployments, get_pod_containers, is_ejected, stream_pod_logs},
    mount::{mount, unmount},
    types::{DbSchema, K8sService, Package},
};

use self::{
    kubernetes::shell_into_pod,
    render::draw,
    types::{Focus, Popup, PopupAction, SidebarItem},
};

/* ================================================================
   INTERNAL CHANNEL MESSAGES
   ================================================================ */

enum TuiMsg {
    /// A new batch of log lines for a service (generation-gated).
    ServiceLogs { lines: Vec<String>, generation: u64 },
    /// A new batch of log lines for a DB schema (generation-gated).
    DbLogs { lines: Vec<String>, generation: u64 },
    /// Container list resolved for `svc_idx`.
    Containers { svc_idx: usize, containers: Vec<String> },
    /// Container list resolved for a DB schema.
    DbContainers { schema_idx: usize, containers: Vec<String> },
}

/* ================================================================
   LOG STREAMER
   ================================================================ */

fn spawn_service_log_stream(
    tx:              std::sync::mpsc::Sender<TuiMsg>,
    deployment_name: String,
    container:       Option<String>,
    generation:      u64,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();
            loop {
                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();
                let dep  = deployment_name.clone();
                let cont = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });
                loop {
                    match line_rx.recv().await {
                        None => break,
                        Some(line) => {
                            lines.push(line);
                            if lines.len() > 2000 {
                                lines.drain(0..500);
                            }
                            if tx
                                .send(TuiMsg::ServiceLogs {
                                    lines:      lines.clone(),
                                    generation,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                if tx
                    .send(TuiMsg::ServiceLogs {
                        lines:      lines.clone(),
                        generation,
                    })
                    .is_err()
                {
                    return;
                }
                sleep(Duration::from_secs(2)).await;
            }
        });
    });
}

fn spawn_db_log_stream(
    tx:         std::sync::mpsc::Sender<TuiMsg>,
    slug:       String,
    container:  Option<String>,
    generation: u64,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();
            loop {
                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();
                let dep  = slug.clone();
                let cont = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });
                loop {
                    match line_rx.recv().await {
                        None => break,
                        Some(line) => {
                            lines.push(line);
                            if lines.len() > 2000 {
                                lines.drain(0..500);
                            }
                            let normalised =
                                if lines.len() == 1 && lines[0].starts_with("No pods found") {
                                    vec![]
                                } else {
                                    lines.clone()
                                };
                            if tx
                                .send(TuiMsg::DbLogs {
                                    lines:      normalised,
                                    generation,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                if tx
                    .send(TuiMsg::DbLogs {
                        lines:      lines.clone(),
                        generation,
                    })
                    .is_err()
                {
                    return;
                }
                sleep(Duration::from_secs(2)).await;
            }
        });
    });
}

fn spawn_container_fetch(
    tx:              std::sync::mpsc::Sender<TuiMsg>,
    deployment_name: String,
    svc_idx:         usize,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        rt.block_on(async move {
            if let Some((_pod, containers)) = get_pod_containers(&deployment_name).await {
                let _ = tx.send(TuiMsg::Containers { svc_idx, containers });
            }
        });
    });
}

fn spawn_db_container_fetch(
    tx:         std::sync::mpsc::Sender<TuiMsg>,
    slug:       String,
    schema_idx: usize,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        rt.block_on(async move {
            if let Some((_pod, containers)) = get_pod_containers(&slug).await {
                let _ = tx.send(TuiMsg::DbContainers { schema_idx, containers });
            }
        });
    });
}

/* ================================================================
   ENTRY POINT
   ================================================================ */
pub async fn fetch_metadata_and_process(
    metadata_config: &MetadataConfiguration,
    session_user:    &str,
) {
    let org_id = match fetch_current_workspace(metadata_config).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Workspace fetch error: {e:?}");
            exit(1);
        }
    };

    let packages: Vec<Package> =
        match data_source::fetch_packages(metadata_config, &org_id, "stage").await {
            Ok(mut pkgs) => {
                for pkg in &mut pkgs {
                    let slug = crate::shared::core::image::pkg_to_slug(&pkg.identifier);
                    pkg.mounted = crate::shared::core::k8_info::is_mounted(&slug).await;
                }
                pkgs
            }
            Err(e) => {
                eprintln!("Warning: package fetch failed: {e:?}");
                vec![]
            }
        };

    let initial_services: Vec<K8sService> =
        match data_source::fetch_services(metadata_config, &org_id, 50).await {
            Ok(svcs) => svcs,
            Err(e) => {
                eprintln!("{e:?}\nUnable to get metadata");
                exit(1);
            }
        };

    let initial_db_schemas: Vec<DbSchema> =
        match data_source::fetch_dbs_enriched(metadata_config, &org_id).await {
            Ok(schemas) => schemas,
            Err(e) => {
                eprintln!("Warning: DB schema fetch failed: {e:?}");
                vec![]
            }
        };

    if let Err(e) =
        run_tui(initial_services, packages, initial_db_schemas, session_user).await
    {
        eprintln!("TUI error: {}", e);
        exit(1);
    }
}

/* ================================================================
   TUI LOOP
   ================================================================ */

async fn run_tui(
    initial_services:   Vec<K8sService>,
    initial_packages:   Vec<Package>,
    initial_db_schemas: Vec<DbSchema>,
    _session_user:      &str,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend      = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ── Shared state ──────────────────────────────────────────────────────────
    let services:   Arc<Mutex<Vec<K8sService>>> = Arc::new(Mutex::new(initial_services));
    let packages:   Arc<Mutex<Vec<Package>>>    = Arc::new(Mutex::new(initial_packages));
    let db_schemas: Arc<Mutex<Vec<DbSchema>>>   = Arc::new(Mutex::new(initial_db_schemas));

    // ── Internal channel for background → UI messages ─────────────────────────
    let (bg_tx, bg_rx) = std::sync::mpsc::channel::<TuiMsg>();

    // ── Log state ─────────────────────────────────────────────────────────────
    let logs: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut svc_log_generation: u64 = 0;

    // DB logs
    let db_logs:       Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let mut db_log_generation: u64 = 0;
    let mut db_log_schema: Option<usize> = None;

    // ── Background: k8s status + ejected flags every 5 s ─────────────────────
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

    // ── UI state ──────────────────────────────────────────────────────────────
    let mut focus:           Focus       = Focus::Sidebar;
    let mut sidebar_item:    SidebarItem = SidebarItem::Service(0);
    let mut auto_scroll:     bool        = true;
    let mut scroll_offset:   usize       = 0;
    let mut popup:           Option<Popup> = None;
    let mut sidebar_scroll:  usize       = 0;
    let mut db_last_max_scroll: usize    = 0;

    // Container selection per service: svc_idx → selected container index.
    let mut container_selection: HashMap<usize, usize> = HashMap::new();

    // Kick off the initial service log stream (index 0).
    {
        let svcs = services.lock().unwrap();
        if let Some(svc) = svcs.first() {
            if let Some(ref dep) = svc.deployment_name {
                svc_log_generation += 1;
                spawn_service_log_stream(
                    bg_tx.clone(),
                    dep.clone(),
                    None,
                    svc_log_generation,
                );
                spawn_container_fetch(bg_tx.clone(), dep.clone(), 0);
            }
        }
    }

    loop {
        // ── Drain background messages ─────────────────────────────────────────
        loop {
            match bg_rx.try_recv() {
                Ok(TuiMsg::ServiceLogs { lines, generation }) => {
                    if generation == svc_log_generation {
                        let key = {
                            let svcs = services.lock().unwrap();
                            if let SidebarItem::Service(i) = sidebar_item {
                                svcs.get(i).map(|s| s.meta_name.clone())
                            } else {
                                None
                            }
                        };
                        if let Some(k) = key {
                            logs.lock().unwrap().insert(k, lines);
                        }
                    }
                }
                Ok(TuiMsg::DbLogs { lines, generation }) => {
                    if generation == db_log_generation {
                        *db_logs.lock().unwrap() = Some(lines);
                    }
                }
                Ok(TuiMsg::Containers { svc_idx, containers }) => {
                    // Determine the ejected container name before mutating.
                    let ejected_container = {
                        let svcs = services.lock().unwrap();
                        svcs.get(svc_idx).and_then(|s| s.ejected_container.clone())
                    };
                    let is_ejected_svc = {
                        let svcs = services.lock().unwrap();
                        svcs.get(svc_idx).map(|s| s.ejected).unwrap_or(false)
                    };

                    // Write the container list into the service.
                    {
                        let mut svcs = services.lock().unwrap();
                        if let Some(svc) = svcs.get_mut(svc_idx) {
                            svc.containers = containers.clone();
                        }
                    }

                    // If this service is ejected, auto-select the first
                    // non-ejected container so logs start without user
                    // having to manually shift to a sidecar.
                    if is_ejected_svc {
                        let ejected_name = ejected_container.as_deref().unwrap_or("");
                        if let Some(non_ejected_idx) = containers
                            .iter()
                            .position(|c| c.as_str() != ejected_name)
                        {
                            // Only switch if no explicit selection has been
                            // made by the user yet for this service.
                            if !container_selection.contains_key(&svc_idx) {
                                container_selection.insert(svc_idx, non_ejected_idx);

                                // Restart log stream for the active service only.
                                let is_active = matches!(sidebar_item, SidebarItem::Service(i) if i == svc_idx);
                                if is_active {
                                    let dep = {
                                        let svcs = services.lock().unwrap();
                                        svcs.get(svc_idx)
                                            .and_then(|s| s.deployment_name.clone())
                                    };
                                    let container = containers.get(non_ejected_idx).cloned();
                                    if let Some(dep) = dep {
                                        // Clear stale lines so the old ejected-splash
                                        // doesn't linger while the new stream loads.
                                        {
                                            let svcs = services.lock().unwrap();
                                            if let Some(svc) = svcs.get(svc_idx) {
                                                logs.lock().unwrap().remove(&svc.meta_name);
                                            }
                                        }
                                        svc_log_generation += 1;
                                        spawn_service_log_stream(
                                            bg_tx.clone(),
                                            dep,
                                            container,
                                            svc_log_generation,
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        // Non-ejected service: if no container selected yet,
                        // default to the first container and (re)start the stream.
                        if !container_selection.contains_key(&svc_idx) {
                            if let Some(first) = containers.first() {
                                container_selection.insert(svc_idx, 0);

                                let is_active = matches!(sidebar_item, SidebarItem::Service(i) if i == svc_idx);
                                if is_active {
                                    let dep = {
                                        let svcs = services.lock().unwrap();
                                        svcs.get(svc_idx)
                                            .and_then(|s| s.deployment_name.clone())
                                    };
                                    if let Some(dep) = dep {
                                        {
                                            let svcs = services.lock().unwrap();
                                            if let Some(svc) = svcs.get(svc_idx) {
                                                logs.lock().unwrap().remove(&svc.meta_name);
                                            }
                                        }
                                        svc_log_generation += 1;
                                        spawn_service_log_stream(
                                            bg_tx.clone(),
                                            dep,
                                            Some(first.clone()),
                                            svc_log_generation,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // ── DB container list resolved ────────────────────────────────
                Ok(TuiMsg::DbContainers { schema_idx, containers }) => {
                    // Only act if this is still the schema we're viewing.
                    if db_log_schema == Some(schema_idx) {
                        // Pick the first container and restart the log stream
                        // with it so the API receives a concrete container name.
                        if let Some(first_container) = containers.first().cloned() {
                            db_log_generation += 1;
                            *db_logs.lock().unwrap() = None; // clear stale "looking…" state

                            let slug = db_schemas
                                .lock()
                                .unwrap()
                                .get(schema_idx)
                                .and_then(|s| s.k8s_name.clone())
                                .unwrap_or_default();

                            if !slug.is_empty() {
                                spawn_db_log_stream(
                                    bg_tx.clone(),
                                    slug,
                                    Some(first_container),
                                    db_log_generation,
                                );
                            }
                        }
                    }
                }

                Err(_) => break,
            }
        }

        // ── Snapshot for drawing ──────────────────────────────────────────────
        let services_snap   = services.lock().unwrap().clone();
        let packages_snap   = packages.lock().unwrap().clone();
        let db_schemas_snap = db_schemas.lock().unwrap().clone();
        let logs_snap       = logs.lock().unwrap().clone();
        let db_logs_snap    = db_logs.lock().unwrap().clone();

        let (selected_svc, has_deployment, has_lang, is_ejected_now) =
            if let SidebarItem::Service(i) = sidebar_item {
                let svc      = services_snap.get(i);
                let has_dep  = svc
                    .map(|s| s.status != "Not deployed" && s.status != "Unknown")
                    .unwrap_or(false);
                let has_lang = svc.and_then(|s| s.lang.as_ref()).is_some();
                let ejected  = svc.map(|s| s.ejected).unwrap_or(false);
                (svc, has_dep, has_lang, ejected)
            } else {
                (None, false, false, false)
            };

        let (active_container_idx, active_container_name): (usize, Option<String>) =
            if let SidebarItem::Service(i) = sidebar_item {
                if let Some(svc) = services_snap.get(i) {
                    let idx  = *container_selection.get(&i).unwrap_or(&0);
                    let name = svc.containers.get(idx).cloned();
                    (idx, name)
                } else {
                    (0, None)
                }
            } else {
                (0, None)
            };

        let db_logs_opt: Option<&[String]> = match sidebar_item {
            SidebarItem::DbSchema(i) if db_log_schema == Some(i) => {
                db_logs_snap.as_deref()
            }
            _ => None,
        };

        // ── Draw ──────────────────────────────────────────────────────────────
        terminal.draw(|f| {
            if let SidebarItem::Service(_) = sidebar_item {
                if let Some(svc) = selected_svc {
                    let log_text = logs_snap
                        .get(&svc.meta_name)
                        .map(|l| l.join("\n"))
                        .unwrap_or_default();
                    let max_scroll = log_text
                        .lines()
                        .count()
                        .saturating_sub(f.size().height.saturating_sub(10) as usize);
                    if auto_scroll {
                        scroll_offset = max_scroll;
                    } else {
                        scroll_offset = scroll_offset.min(max_scroll);
                        if scroll_offset >= max_scroll {
                            auto_scroll = true;
                        }
                    }
                }
            }

            if let SidebarItem::DbSchema(_) = sidebar_item {
                if let Some(lines) = db_logs_snap.as_deref() {
                    let height = f.size().height.saturating_sub(10) as usize;
                    db_last_max_scroll = lines.len().saturating_sub(height);
                    if auto_scroll {
                        scroll_offset = db_last_max_scroll;
                    }
                }
            }

            let drawn = draw(
                f,
                &services_snap,
                &packages_snap,
                &db_schemas_snap,
                &sidebar_item,
                &logs_snap,
                db_logs_opt,
                &focus,
                auto_scroll,
                scroll_offset,
                has_deployment,
                has_lang,
                is_ejected_now,
                popup.as_ref(),
                active_container_idx,
                active_container_name.as_deref(),
            );
            sidebar_scroll = drawn.sidebar_scroll;
        })?;

        /* ================================================================
           INPUT
           ================================================================ */
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        match event::read()? {
            /* ── Mouse ──────────────────────────────────────────────────── */
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
                        let sidebar_area = {
                            let term      = terminal.size()?;
                            let root_h    = term.height.saturating_sub(2);
                            let sidebar_w = term.width * 35 / 100;
                            ratatui::layout::Rect {
                                x: 0, y: 0, width: sidebar_w, height: root_h,
                            }
                        };
                        if let Some(item) = render::click_sidebar_item(
                            col, row, sidebar_area, sidebar_scroll,
                            services_snap.len(), packages_snap.len(), db_schemas_snap.len(),
                        ) {
                            focus = Focus::Sidebar;
                            match &item {
                                SidebarItem::Service(i) => {
                                    switch_service_logs(
                                        *i,
                                        &services_snap,
                                        &mut svc_log_generation,
                                        &container_selection,
                                        &bg_tx,
                                        &logs,
                                    );
                                    auto_scroll   = true;
                                    scroll_offset = 0;
                                }
                                SidebarItem::DbSchema(i) => {
                                    maybe_start_db_stream(
                                        *i,
                                        &db_schemas_snap,
                                        &mut db_log_schema,
                                        &mut db_log_generation,
                                        &db_logs,
                                        &bg_tx,
                                    );
                                    auto_scroll   = true;
                                    scroll_offset = 0;
                                }
                                SidebarItem::Package(_) => {}
                            }
                            sidebar_item = item;
                        } else if matches!(
                            sidebar_item,
                            SidebarItem::Service(_) | SidebarItem::DbSchema(_)
                        ) {
                            focus = Focus::Logs;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if focus == Focus::Logs {
                            if auto_scroll {
                                scroll_offset = db_last_max_scroll;
                            }
                            auto_scroll   = false;
                            scroll_offset = scroll_offset.saturating_sub(3);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset += 3;
                        }
                    }
                    _ => {}
                }
            }

            /* ── Keyboard ───────────────────────────────────────────────── */
            Event::Key(key) => {
                // ── Popup handling ────────────────────────────────────────
                if let Some(ref mut p) = popup {
                    if p.action == PopupAction::ShellBlocked {
                        popup = None;
                        continue;
                    }
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
                                                let dep     = svc.deployment_name.clone();
                                                let lang    = svc.lang.clone();
                                                let ejected = svc.ejected;
                                                let meta    = svc.meta_name.clone();
                                                let org     = svc.organization_id.clone();
                                                drop(svcs);
                                                if let Some(dep_name) = dep {
                                                    popup = None;
                                                    leave_tui(&mut terminal)?;
                                                    let r = if ejected {
                                                        uneject(&dep_name).await
                                                    } else {
                                                        eject(
                                                            &dep_name,
                                                            lang.as_deref().unwrap_or(""),
                                                            &meta,
                                                            &org,
                                                        )
                                                        .await
                                                    };
                                                    if let Err(e) = r {
                                                        eprintln!("Error: {e}");
                                                    }
                                                    // After eject/uneject, clear container
                                                    // selection so it re-detects on next load.
                                                    container_selection.remove(&svc_i);
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
                                                pkgs.get(pkg_i)
                                                    .map(|p| (
                                                        p.organization_id.clone(),
                                                        p.identifier.clone(),
                                                        p.lang.clone(),
                                                    ))
                                                    .unwrap_or_default()
                                            };
                                            popup = None;
                                            leave_tui(&mut terminal)?;
                                            match mount(&org, &id, &lang).await {
                                                Ok(()) => {
                                                    if let Some(p) =
                                                        packages.lock().unwrap().get_mut(pkg_i)
                                                    {
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
                                                pkgs.get(pkg_i)
                                                    .map(|p| (
                                                        p.organization_id.clone(),
                                                        p.identifier.clone(),
                                                    ))
                                                    .unwrap_or_default()
                                            };
                                            popup = None;
                                            leave_tui(&mut terminal)?;
                                            match unmount(&org, &id).await {
                                                Ok(()) => {
                                                    if let Some(p) =
                                                        packages.lock().unwrap().get_mut(pkg_i)
                                                    {
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

                // ── Normal key handling ───────────────────────────────────
                match key.code {
                    // ── Quit ─────────────────────────────────────────────
                    KeyCode::Char('q') | KeyCode::Esc => {
                        popup = Some(Popup {
                            service_name: String::new(),
                            action:       PopupAction::Quit,
                            selected:     1,
                        });
                    }

                    // ── Panel focus ───────────────────────────────────────
                    KeyCode::Left
                        if key.modifiers != KeyModifiers::SHIFT =>
                    {
                        focus = Focus::Sidebar;
                    }
                    KeyCode::Right
                        if key.modifiers != KeyModifiers::SHIFT =>
                    {
                        if matches!(
                            sidebar_item,
                            SidebarItem::Service(_) | SidebarItem::DbSchema(_)
                        ) {
                            focus = Focus::Logs;
                        }
                    }

                    // ── Container tab navigation: Shift + ← / → ──────────
                    KeyCode::Left
                        if key.modifiers == KeyModifiers::SHIFT =>
                    {
                        if let SidebarItem::Service(svc_i) = sidebar_item {
                            let svcs = services.lock().unwrap();
                            if let Some(svc) = svcs.get(svc_i) {
                                if svc.containers.len() > 1 {
                                    let cur =
                                        *container_selection.get(&svc_i).unwrap_or(&0);
                                    let next = if cur == 0 {
                                        svc.containers.len() - 1
                                    } else {
                                        cur - 1
                                    };
                                    let svc_snapshot = svc.clone();
                                    drop(svcs);
                                    select_container(
                                        svc_i,
                                        next,
                                        &[svc_snapshot],
                                        &mut container_selection,
                                        &mut svc_log_generation,
                                        &bg_tx,
                                        &logs,
                                    );
                                }
                            }
                        }
                    }
                    KeyCode::Right
                        if key.modifiers == KeyModifiers::SHIFT =>
                    {
                        if let SidebarItem::Service(svc_i) = sidebar_item {
                            let svcs = services.lock().unwrap();
                            if let Some(svc) = svcs.get(svc_i) {
                                if svc.containers.len() > 1 {
                                    let cur =
                                        *container_selection.get(&svc_i).unwrap_or(&0);
                                    let next = (cur + 1) % svc.containers.len();
                                    let svc_snapshot = svc.clone();
                                    drop(svcs);
                                    select_container(
                                        svc_i,
                                        next,
                                        &[svc_snapshot],
                                        &mut container_selection,
                                        &mut svc_log_generation,
                                        &bg_tx,
                                        &logs,
                                    );
                                }
                            }
                        }
                    }

                    // ── Sidebar navigation ────────────────────────────────
                    KeyCode::Up | KeyCode::Char('k') => {
                        if focus == Focus::Logs {
                            if auto_scroll {
                                scroll_offset = db_last_max_scroll;
                            }
                            auto_scroll   = false;
                            scroll_offset = scroll_offset.saturating_sub(1);
                        } else {
                            let prev = sidebar_prev(
                                &sidebar_item,
                                &services_snap,
                                &packages_snap,
                                &db_schemas_snap,
                            );
                            if let SidebarItem::Service(i) = prev {
                                switch_service_logs(
                                    i,
                                    &services_snap,
                                    &mut svc_log_generation,
                                    &container_selection,
                                    &bg_tx,
                                    &logs,
                                );
                                auto_scroll   = true;
                                scroll_offset = 0;
                            }
                            if let SidebarItem::DbSchema(i) = prev {
                                maybe_start_db_stream(
                                    i,
                                    &db_schemas_snap,
                                    &mut db_log_schema,
                                    &mut db_log_generation,
                                    &db_logs,
                                    &bg_tx,
                                );
                                auto_scroll   = true;
                                scroll_offset = 0;
                            }
                            sidebar_item = prev;
                        }
                    }

                    KeyCode::Down | KeyCode::Char('j') => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset += 1;
                        } else {
                            let next = sidebar_next(
                                &sidebar_item,
                                &services_snap,
                                &packages_snap,
                                &db_schemas_snap,
                            );
                            if let SidebarItem::Service(i) = next {
                                switch_service_logs(
                                    i,
                                    &services_snap,
                                    &mut svc_log_generation,
                                    &container_selection,
                                    &bg_tx,
                                    &logs,
                                );
                                auto_scroll   = true;
                                scroll_offset = 0;
                            }
                            if let SidebarItem::DbSchema(i) = next {
                                maybe_start_db_stream(
                                    i,
                                    &db_schemas_snap,
                                    &mut db_log_schema,
                                    &mut db_log_generation,
                                    &db_logs,
                                    &bg_tx,
                                );
                                auto_scroll   = true;
                                scroll_offset = 0;
                            }
                            sidebar_item = next;
                        }
                    }

                    // ── Scroll jump ───────────────────────────────────────
                    KeyCode::PageDown => {
                        if focus == Focus::Logs {
                            auto_scroll   = true;
                            scroll_offset = db_last_max_scroll;
                        }
                    }
                    KeyCode::PageUp => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset = 0;
                        }
                    }
                    KeyCode::Char('g') => {
                        if focus == Focus::Logs {
                            auto_scroll   = false;
                            scroll_offset = 0;
                        }
                    }
                    KeyCode::Char('G') => {
                        if focus == Focus::Logs {
                            auto_scroll   = true;
                            scroll_offset = db_last_max_scroll;
                        }
                    }

                    // ── Mount / unmount ───────────────────────────────────
                    KeyCode::Char('m') => {
                        if let SidebarItem::Package(pkg_i) = sidebar_item {
                            if let Some(pkg) = packages_snap.get(pkg_i) {
                                popup = Some(Popup {
                                    service_name: pkg.identifier.clone(),
                                    action:       if pkg.mounted {
                                        PopupAction::Unmount
                                    } else {
                                        PopupAction::Mount
                                    },
                                    selected: 0,
                                });
                            }
                        }
                    }

                    // ── VS Code ───────────────────────────────────────────
                    KeyCode::Char('c') => {
                        match sidebar_item {
                            SidebarItem::Package(pkg_i) => {
                                if let Some(pkg) = packages_snap.get(pkg_i) {
                                    if pkg.mounted {
                                        let alias = format!("{}-local", pkg.identifier);
                                        let uri   = format!(
                                            "vscode-remote://ssh-remote+{}/workspace/{}-{}",
                                            alias,
                                            pkg.organization_id,
                                            pkg.identifier,
                                        );
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
                                                dep, svc.organization_id, dep,
                                            );
                                            open_vscode(&mut terminal, &uri).await?;
                                        }
                                    }
                                }
                            }
                            SidebarItem::DbSchema(_) => {}
                        }
                    }

                    // ── Shell ─────────────────────────────────────────────
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

                    // ── Eject / uneject ───────────────────────────────────
                    KeyCode::Char('e') => {
                        if has_deployment && has_lang {
                            if let SidebarItem::Service(svc_i) = sidebar_item {
                                if let Some(svc) = services_snap.get(svc_i) {
                                    popup = Some(Popup {
                                        service_name: svc.meta_name.clone(),
                                        action:       if svc.ejected {
                                            PopupAction::Uneject
                                        } else {
                                            PopupAction::Eject
                                        },
                                        selected: 0,
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/* ================================================================
   CONTAINER SELECTION HELPER
   ================================================================ */

/// Switch the active container for `svc_i`, bump the log generation,
/// and start a new log stream for the chosen container.
///
/// `services` slice must contain exactly one entry at index 0 corresponding
/// to the service at `svc_i` (pass `&[svc_snapshot]`).
fn select_container(
    svc_i:                usize,
    container_idx:        usize,
    services:             &[K8sService],
    container_selection:  &mut HashMap<usize, usize>,
    svc_log_generation:   &mut u64,
    bg_tx:                &std::sync::mpsc::Sender<TuiMsg>,
    logs:                 &Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    // services is a single-element slice containing the snapshot for svc_i.
    let Some(svc) = services.first() else { return };
    let Some(dep) = svc.deployment_name.clone() else { return };
    let container = svc.containers.get(container_idx).cloned();

    container_selection.insert(svc_i, container_idx);

    // If this container is the ejected one it runs `sleep infinity` —
    // no log stream to start. Clear stale lines so the render layer
    // shows the "dev mode" splash immediately.
    let is_ejected_container = svc.ejected
        && svc.ejected_container.as_deref() == container.as_deref();

    logs.lock().unwrap().remove(&svc.meta_name);

    if is_ejected_container {
        return;
    }

    *svc_log_generation += 1;
    spawn_service_log_stream(
        bg_tx.clone(),
        dep,
        container,
        *svc_log_generation,
    );
}

/* ================================================================
   SERVICE LOG SWITCH HELPER
   ================================================================ */

fn switch_service_logs(
    svc_i:               usize,
    services:            &[K8sService],
    svc_log_generation:  &mut u64,
    container_selection: &HashMap<usize, usize>,
    bg_tx:               &std::sync::mpsc::Sender<TuiMsg>,
    logs:                &Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    let Some(svc) = services.get(svc_i) else { return };
    let Some(dep) = svc.deployment_name.clone() else { return };

    let container_idx = *container_selection.get(&svc_i).unwrap_or(&0);

    // If the service is ejected and we haven't resolved a non-ejected container
    // yet (containers list is empty), don't start a log stream — just fetch
    // the container list. The Containers message handler will start the stream.
    if svc.ejected && svc.containers.is_empty() {
        spawn_container_fetch(bg_tx.clone(), dep, svc_i);
        return;
    }

    // For an ejected service, only stream if the selected container is NOT
    // the ejected one.
    let container = svc.containers.get(container_idx).cloned();
    let is_ejected_container = svc.ejected
        && svc.ejected_container.as_deref() == container.as_deref();

    if is_ejected_container {
        // Clear any stale logs so the render layer shows the dev-mode splash.
        logs.lock().unwrap().remove(&svc.meta_name);
        if svc.containers.is_empty() {
            spawn_container_fetch(bg_tx.clone(), dep, svc_i);
        }
        return;
    }

    *svc_log_generation += 1;
    spawn_service_log_stream(
        bg_tx.clone(),
        dep.clone(),
        container,
        *svc_log_generation,
    );

    if svc.containers.is_empty() {
        spawn_container_fetch(bg_tx.clone(), dep, svc_i);
    }
}

/* ================================================================
   DB SCHEMA LOG STREAM HELPER
   ================================================================ */

fn maybe_start_db_stream(
    idx:               usize,
    db_schemas:        &[DbSchema],
    db_log_schema:     &mut Option<usize>,
    db_log_generation: &mut u64,
    db_logs:           &Arc<Mutex<Option<Vec<String>>>>,
    bg_tx:             &std::sync::mpsc::Sender<TuiMsg>,
) {
    if *db_log_schema == Some(idx) {
        return;
    }

    *db_log_schema    = Some(idx);
    *db_log_generation += 1;
    *db_logs.lock().unwrap() = None;

    let slug = db_schemas
        .get(idx)
        .and_then(|s| s.k8s_name.clone())
        .unwrap_or_default();

    if slug.is_empty() {
        *db_logs.lock().unwrap() = Some(vec![]);
        return;
    }

    // Start a container fetch first — the DbContainers message handler will
    // start the actual log stream once we know a real container name.
    // This mirrors the GUI's spawn_db_container_fetch → BgMsg::DbContainers
    // → spawn_db_schema_logs flow.
    spawn_db_container_fetch(bg_tx.clone(), slug.clone(), idx);

    // Also kick off an initial log stream without a container name as a
    // fallback, in case the pod has only one (unnamed) container.  The
    // DbContainers handler will bump db_log_generation and restart it with
    // the real name moments later, so this fallback stream will be discarded.
    spawn_db_log_stream(bg_tx.clone(), slug, None, *db_log_generation);
}

/* ================================================================
   SIDEBAR NAVIGATION HELPERS
   ================================================================ */

fn sidebar_total(
    services:   &[K8sService],
    packages:   &[Package],
    db_schemas: &[DbSchema],
) -> usize {
    services.len() + packages.len() + db_schemas.len()
}

fn sidebar_flat(item: &SidebarItem, svc_count: usize, pkg_count: usize) -> usize {
    match item {
        SidebarItem::Service(i)  => *i,
        SidebarItem::Package(i)  => svc_count + i,
        SidebarItem::DbSchema(i) => svc_count + pkg_count + i,
    }
}

fn sidebar_from_flat(flat: usize, svc_count: usize, pkg_count: usize) -> SidebarItem {
    if flat < svc_count {
        SidebarItem::Service(flat)
    } else if flat < svc_count + pkg_count {
        SidebarItem::Package(flat - svc_count)
    } else {
        SidebarItem::DbSchema(flat - svc_count - pkg_count)
    }
}

fn sidebar_next(
    current:    &SidebarItem,
    services:   &[K8sService],
    packages:   &[Package],
    db_schemas: &[DbSchema],
) -> SidebarItem {
    let total = sidebar_total(services, packages, db_schemas);
    if total == 0 { return current.clone(); }
    let flat = sidebar_flat(current, services.len(), packages.len());
    let next = (flat + 1).min(total - 1);
    sidebar_from_flat(next, services.len(), packages.len())
}

fn sidebar_prev(
    current:    &SidebarItem,
    services:   &[K8sService],
    packages:   &[Package],
    db_schemas: &[DbSchema],
) -> SidebarItem {
    if sidebar_total(services, packages, db_schemas) == 0 {
        return current.clone();
    }
    let flat = sidebar_flat(current, services.len(), packages.len());
    sidebar_from_flat(
        flat.saturating_sub(1),
        services.len(),
        packages.len(),
    )
}

/* ================================================================
   TUI SUSPEND / RESUME
   ================================================================ */

fn leave_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}

fn enter_tui<B: ratatui::backend::Backend + io::Write>(
    terminal: &mut Terminal<B>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
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
        .arg("--folder-uri")
        .arg(remote_uri)
        .status()
        .await
    {
        Ok(s) if s.success() => println!("✓ VS Code launched"),
        Ok(s)  => eprintln!("VS Code exited: {s}"),
        Err(e) => eprintln!("Failed to launch VS Code: {e}"),
    }
    sleep(Duration::from_secs(1)).await;
    enter_tui(terminal)?;
    Ok(())
}