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
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
    Terminal,
};
use tokio::time::sleep;

use MetadataService::{
    apis::{
        configuration::Configuration as MetadataConfiguration,
        default_api::{metadata_get_services_and_envs, MetadataGetServicesAndEnvsParams},
    },
};
use IAMService::apis::configuration::Configuration as IAMConfiguration;
use ginger_shared_rs::read_service_config_file;

/* ================================================================
   DATA TYPES
   ================================================================ */

#[derive(Clone, Debug)]
struct K8sService {
    /// e.g. "@ginger-society/dev-portal"
    meta_name: String,
    /// k8s deployment name if matched, e.g. "dev-portal"
    deployment_name: Option<String>,
    /// "Running" / "Pending" / "Unknown" / "Not deployed"
    status: String,
    /// Number of ready pods, e.g. "1/1"
    ready: String,
}

#[derive(PartialEq, Clone)]
enum Focus {
    Services,
    Logs,
}

/* ================================================================
   KUBERNETES HELPERS
   ================================================================ */

/// Returns the short deployment name from a metadata service identifier.
/// "@ginger-society/dev-portal" → "dev-portal"
fn meta_to_deployment_name(meta_name: &str) -> String {
    meta_name
        .split('/')
        .last()
        .unwrap_or(meta_name)
        .to_string()
}

async fn get_k8s_deployments() -> HashMap<String, (String, String)> {
    // Returns map: deployment_name -> (status, ready)
    let output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "deployments",
            "-o", "custom-columns=NAME:.metadata.name,READY:.status.readyReplicas,DESIRED:.spec.replicas",
            "--no-headers",
        ])
        .output()
        .await;

    let mut map = HashMap::new();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let name = parts[0].to_string();
                let ready_count = parts[1];
                let desired = parts[2];
                let ready_str = format!("{}/{}", ready_count, desired);
                let status = if ready_count == desired {
                    "Running".to_string()
                } else if ready_count == "<none>" || ready_count == "0" {
                    "Pending".to_string()
                } else {
                    "Degraded".to_string()
                };
                map.insert(name, (status, ready_str));
            }
        }
    }
    map
}

async fn get_pod_logs(deployment_name: &str) -> Vec<String> {
    // Get logs from the most recent pod for this deployment
    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "-l", &format!("app={}", deployment_name),
            "--no-headers",
            "-o", "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await;

    let pod_name = match pod_output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter(|l| !l.is_empty())
                .next()
                .map(|l| l.trim().to_string())
        }
        Err(_) => None,
    };

    let Some(pod) = pod_name else {
        return vec!["No pods found for this deployment.".to_string()];
    };

    let log_output = tokio::process::Command::new("kubectl")
        .args(&["logs", "--tail=500", &pod])
        .output()
        .await;

    match log_output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            stdout
                .lines()
                .chain(stderr.lines())
                .map(|s| s.to_string())
                .collect()
        }
        Err(e) => vec![format!("Failed to fetch logs: {}", e)],
    }
}

async fn shell_into_pod(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "-l", &format!("app={}", deployment_name),
            "--no-headers",
            "-o", "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await?;

    let pod_name = String::from_utf8_lossy(&pod_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .next()
        .map(|l| l.trim().to_string());

    let Some(pod) = pod_name else {
        eprintln!("No running pod found for deployment: {}", deployment_name);
        return Ok(());
    };

    // Try bash first, fall back to sh
    let status = tokio::process::Command::new("kubectl")
        .args(["exec", "-it", &pod, "--", "bash"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;

    if status.map(|s| s.success()).unwrap_or(false) {
        return Ok(());
    }

    tokio::process::Command::new("kubectl")
        .args(["exec", "-it", &pod, "--", "sh"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    Ok(())
}

/* ================================================================
   STATUS COLOUR
   ================================================================ */

fn status_color(status: &str) -> Color {
    match status {
        "Running"     => Color::Green,
        "Degraded"    => Color::Yellow,
        "Pending"     => Color::Yellow,
        "Not deployed" => Color::DarkGray,
        _             => Color::Red,
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "Running"      => "●",
        "Degraded"     => "◐",
        "Pending"      => "○",
        "Not deployed" => "·",
        _              => "✗",
    }
}

/* ================================================================
   HIT TESTING
   ================================================================ */

fn click_service_index(col: u16, row: u16, list_area: Rect, count: usize) -> Option<usize> {
    if col < list_area.x
        || col >= list_area.x + list_area.width
        || row < list_area.y + 1
        || row >= list_area.y + list_area.height - 1
    {
        return None;
    }
    let relative_row = (row - list_area.y - 1) as usize;
    let lines_per_item = 2usize;
    let idx = relative_row / lines_per_item;
    if idx < count { Some(idx) } else { None }
}

fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/* ================================================================
   HELP TEXT
   ================================================================ */

fn help_text(focus: &Focus, has_deployment: bool) -> String {
    match focus {
        Focus::Services => {
            let mut parts = vec!["↑/↓ select", "←/→ switch focus"];
            if has_deployment {
                parts.push("s shell");
            }
            parts.push("q quit");
            parts.join("  |  ")
        }
        Focus::Logs => {
            "PgUp jump to top  |  PgDn follow  |  ↑/↓  j/k scroll  |  g/G jump  |  ←/→ switch focus".to_string()
        }
    }
}

/* ================================================================
   ENTRY POINT
   ================================================================ */

pub async fn fetch_metadata_and_process(
    config_path: &Path,
    metadata_config: &MetadataConfiguration,
) {
    let mut config = read_service_config_file(config_path).unwrap();

    let raw_services = match metadata_get_services_and_envs(
        metadata_config,
        MetadataGetServicesAndEnvsParams {
            page_number: Some("1".to_string()),
            page_size:   Some("50".to_string()),
            org_id:      config.organization_id.clone(),
        },
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{:?}", e);
            eprintln!("Unable to get metadata");
            exit(1);
        }
    };

    // Build initial service list (no k8s status yet)
    let initial_services: Vec<K8sService> = raw_services
        .iter()
        .map(|s| {
            let meta_name = format!("@{}/{}", s.organization_id, s.identifier);
            let deployment_name = meta_to_deployment_name(&meta_name);
            K8sService {
                meta_name,
                deployment_name: Some(deployment_name),
                status: "Unknown".to_string(),
                ready: "-".to_string(),
            }
        })
        .collect();

    // Launch TUI
    if let Err(e) = run_tui(initial_services).await {
        eprintln!("TUI error: {}", e);
        exit(1);
    }
}

/* ================================================================
   TUI LOOP
   ================================================================ */

async fn run_tui(
    initial_services: Vec<K8sService>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let services: Arc<Mutex<Vec<K8sService>>> = Arc::new(Mutex::new(initial_services));
    let logs: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let selected_idx: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    // ── Background: poll k8s deployment statuses ───────────────
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
                } // guard dropped here
                sleep(Duration::from_secs(5)).await;
            }
        });
    }

    // ── Background: stream logs for selected service ────────────
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
                    // guards dropped here
                };

                if let Some(dep) = dep_name {
                    let new_logs = get_pod_logs(&dep).await;
                    {
                        let mut logs_guard = logs.lock().unwrap();
                        logs_guard.insert(meta_name, new_logs);
                    } // guard dropped here
                }

                sleep(Duration::from_secs(2)).await;
            }
        });
    }

    let mut focus       = Focus::Services;
    let mut auto_scroll = true;
    let mut scroll_offset: usize = 0;

    let mut services_list_area = Rect::default();
    let mut logs_area          = Rect::default();

    loop {
        let services_snap = services.lock().unwrap().clone();
        let current_idx   = *selected_idx.lock().unwrap();
        let selected      = services_snap.get(current_idx).cloned();
        let has_deployment = selected
            .as_ref()
            .map(|s| s.status != "Not deployed")
            .unwrap_or(false);

        let mut new_services_area = Rect::default();
        let mut new_logs_area     = Rect::default();

        terminal.draw(|f| {
            let root = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(2)])
                .split(f.size());

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(root[0]);

            new_services_area = chunks[0];

            /* ── Service list ── */
            let items: Vec<ListItem> = services_snap
                .iter()
                .enumerate()
                .map(|(i, svc)| {
                    let is_selected = i == current_idx;
                    let base_style = if is_selected {
                        if focus == Focus::Services {
                            Style::default()
                                .bg(Color::Yellow)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD)
                        }
                    } else {
                        Style::default().fg(Color::Gray)
                    };

                    let icon = status_icon(&svc.status);
                    let icon_style = Style::default().fg(status_color(&svc.status));

                    let lines = vec![
                        Line::from(vec![
                            Span::styled(format!("{} ", icon), icon_style),
                            Span::styled(svc.meta_name.clone(), base_style),
                        ]),
                        Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("ready: {}  status: {}", svc.ready, svc.status),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]),
                    ];

                    ListItem::new(lines).style(base_style)
                })
                .collect();

            f.render_widget(
                List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Services ")
                        .border_style(if focus == Focus::Services {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                ),
                chunks[0],
            );

            /* ── Right: info + logs ── */
            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(5), Constraint::Min(0)])
                .split(chunks[1]);

            new_logs_area = right_chunks[1];

            /* ── Info panel ── */
            let info_lines = if let Some(ref svc) = selected {
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("Name:   ", Style::default().fg(Color::Cyan)),
                        Span::raw(svc.meta_name.clone()),
                    ]),
                    Line::from(vec![
                        Span::styled("Deploy: ", Style::default().fg(Color::Cyan)),
                        Span::raw(
                            svc.deployment_name
                                .as_deref()
                                .unwrap_or("—")
                                .to_string(),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Status: ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            svc.status.clone(),
                            Style::default().fg(status_color(&svc.status)),
                        ),
                    ]),
                ];
                if svc.deployment_name.is_some() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  [s] shell into pod",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
                lines
            } else {
                vec![Line::from("No service selected")]
            };

            f.render_widget(
                Paragraph::new(info_lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Service Info ")
                            .border_style(Style::default().fg(Color::Blue)),
                    )
                    .wrap(Wrap { trim: true }),
                right_chunks[0],
            );

            /* ── Logs ── */
            let log_text = if let Some(ref svc) = selected {
                let logs_guard = logs.lock().unwrap();
                logs_guard
                    .get(&svc.meta_name)
                    .map(|l| l.join("\n"))
                    .unwrap_or_else(|| "Fetching logs...".to_string())
            } else {
                "No service selected".to_string()
            };

            let num_lines = log_text.lines().count();
            let height    = right_chunks[1].height.saturating_sub(2) as usize;
            let max_scroll = num_lines.saturating_sub(height);

            if auto_scroll {
                scroll_offset = max_scroll;
            } else {
                scroll_offset = scroll_offset.min(max_scroll);
                if scroll_offset >= max_scroll {
                    auto_scroll = true;
                }
            }

            let inner_log_area = Rect {
                x:      right_chunks[1].x,
                y:      right_chunks[1].y,
                width:  right_chunks[1].width.saturating_sub(1),
                height: right_chunks[1].height,
            };

            f.render_widget(
                Paragraph::new(log_text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(if auto_scroll { " Logs [FOLLOW] " } else { " Logs [PAUSED] " })
                            .border_style(if focus == Focus::Logs {
                                Style::default().fg(Color::Yellow)
                            } else {
                                Style::default()
                            }),
                    )
                    .wrap(Wrap { trim: false })
                    .scroll((scroll_offset as u16, 0)),
                inner_log_area,
            );

            let mut scrollbar_state =
                ScrollbarState::new(max_scroll.max(1)).position(scroll_offset);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("▲"))
                    .end_symbol(Some("▼"))
                    .track_symbol(Some("│"))
                    .thumb_symbol("█"),
                Rect {
                    x:      right_chunks[1].x + right_chunks[1].width.saturating_sub(1),
                    y:      right_chunks[1].y + 1,
                    width:  1,
                    height: right_chunks[1].height.saturating_sub(2),
                },
                &mut scrollbar_state,
            );

            /* ── Help bar ── */
            f.render_widget(
                Paragraph::new(help_text(&focus, has_deployment))
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::default().borders(Borders::TOP)),
                root[1],
            );
        })?;

        services_list_area = new_services_area;
        logs_area          = new_logs_area;

        /* ================================================================
           INPUT
           ================================================================ */
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                /* ── Mouse ── */
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let (col, row) = (mouse.column, mouse.row);
                        if let Some(idx) = click_service_index(
                            col, row, services_list_area,
                            services.lock().unwrap().len(),
                        ) {
                            focus = Focus::Services;
                            *selected_idx.lock().unwrap() = idx;
                            auto_scroll = true;
                        } else if point_in_rect(col, row, logs_area) {
                            focus = Focus::Logs;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if focus == Focus::Logs {
                            auto_scroll = false;
                            scroll_offset = scroll_offset.saturating_sub(3);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if focus == Focus::Logs {
                            scroll_offset += 3;
                        }
                    }
                    _ => {}
                },

                /* ── Keyboard ── */
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,

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
                            auto_scroll = false;
                            scroll_offset += 1;
                        }
                    }

                    KeyCode::Up | KeyCode::Char('k') => {
                        if focus == Focus::Services {
                            let mut idx = selected_idx.lock().unwrap();
                            *idx = idx.saturating_sub(1);
                            auto_scroll = true;
                        } else {
                            auto_scroll = false;
                            scroll_offset = scroll_offset.saturating_sub(1);
                        }
                    }

                    KeyCode::PageDown => {
                        if focus == Focus::Logs {
                            auto_scroll = true;
                        }
                    }
                    KeyCode::PageUp => {
                        if focus == Focus::Logs {
                            auto_scroll = false;
                            scroll_offset = 0;
                        }
                    }
                    KeyCode::Char('g') => {
                        if focus == Focus::Logs {
                            auto_scroll = false;
                            scroll_offset = 0;
                        }
                    }
                    KeyCode::Char('G') => {
                        if focus == Focus::Logs {
                            auto_scroll = true;
                        }
                    }

                    // Shell into pod
                    KeyCode::Char('s') => {
                        let dep = {
                            let svcs = services.lock().unwrap();
                            let idx  = *selected_idx.lock().unwrap();
                            svcs.get(idx)
                                .and_then(|s| s.deployment_name.clone())
                                .filter(|_| {
                                    svcs.get(idx)
                                        .map(|s| s.status != "Not deployed")
                                        .unwrap_or(false)
                                })
                        };

                        if let Some(dep_name) = dep {
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

                    _ => {}
                },

                _ => {}
            }
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