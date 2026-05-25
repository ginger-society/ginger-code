use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
    Frame,
};
use std::collections::HashMap;

use crate::shared::ui::{
    popup::render_popup,
    types::{Focus, K8sService, Popup},
};

/* ================================================================
   STATUS COLOUR / ICON
   ================================================================ */

pub fn status_color(status: &str) -> Color {
    match status {
        "Running"      => Color::Green,
        "Degraded"     => Color::Yellow,
        "Pending"      => Color::Yellow,
        "Not deployed" => Color::DarkGray,
        _              => Color::Red,
    }
}

pub fn status_icon(status: &str) -> &'static str {
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

pub fn click_service_index(
    col: u16,
    row: u16,
    list_area: Rect,
    count: usize,
) -> Option<usize> {
    if col < list_area.x
        || col >= list_area.x + list_area.width
        || row < list_area.y + 1
        || row >= list_area.y + list_area.height - 1
    {
        return None;
    }
    let idx = (row - list_area.y - 1) as usize / 2;
    if idx < count { Some(idx) } else { None }
}

pub fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x
        && col < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}

/* ================================================================
   HELP TEXT
   ================================================================ */

pub fn help_text(
    focus: &Focus,
    has_deployment: bool,
    has_lang: bool,
    ejected: bool,
) -> String {
    match focus {
        Focus::Services => {
            let mut parts = vec!["↑/↓ select", "←/→ switch focus"];
            if has_deployment {
                parts.push("s shell");
                if has_lang {
                    parts.push(if ejected { "e uneject" } else { "e eject" });
                }
                if ejected {
                    parts.push("c VS Code / Codium");
                }
            }
            parts.push("q quit");
            parts.join("  |  ")
        }
        Focus::Logs => {
            "PgUp top  |  PgDn follow  |  ↑/↓  j/k scroll  |  g/G jump  |  ←/→ switch"
                .to_string()
        }
    }
}

/* ================================================================
   DRAW — returns the two layout areas needed for hit-testing
   ================================================================ */

pub struct DrawnAreas {
    pub services_list: Rect,
    pub logs: Rect,
}

pub fn draw(
    f: &mut Frame,
    services_snap: &[K8sService],
    current_idx: usize,
    selected: Option<&K8sService>,
    logs: &HashMap<String, Vec<String>>,
    focus: &Focus,
    auto_scroll: bool,
    scroll_offset: usize,
    has_deployment: bool,
    has_lang: bool,
    is_ejected_now: bool,
    popup: Option<&Popup>,
) -> DrawnAreas {
    let area = f.size();

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(root[0]);

    /* ── Service list ─────────────────────────────────────────────────── */
    let items: Vec<ListItem> = services_snap
        .iter()
        .enumerate()
        .map(|(i, svc)| {
            let is_selected = i == current_idx;
            let base_style = if is_selected {
                if *focus == Focus::Services {
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

            let icon       = status_icon(&svc.status);
            let icon_style = Style::default().fg(status_color(&svc.status));

            let eject_badge = if svc.ejected {
                Span::styled(
                    " [EJECTED]",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("")
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", icon), icon_style),
                    Span::styled(svc.meta_name.clone(), base_style),
                    eject_badge,
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
                .border_style(if *focus == Focus::Services {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }),
        ),
        chunks[0],
    );

    /* ── Right: info + logs ───────────────────────────────────────────── */
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(chunks[1]);

    /* ── Info panel ───────────────────────────────────────────────────── */
    let info_lines = if let Some(svc) = selected {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Name:    ", Style::default().fg(Color::Cyan)),
                Span::raw(svc.meta_name.clone()),
            ]),
            Line::from(vec![
                Span::styled("Deploy:  ", Style::default().fg(Color::Cyan)),
                Span::raw(svc.deployment_name.as_deref().unwrap_or("—").to_string()),
            ]),
            Line::from(vec![
                Span::styled("Status:  ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    svc.status.clone(),
                    Style::default().fg(status_color(&svc.status)),
                ),
            ]),
            Line::from(vec![
                Span::styled("Ejected: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    if svc.ejected {
                        "YES (builder mode)"
                    } else {
                        "no"
                    }
                    .to_string(),
                    Style::default().fg(if svc.ejected {
                        Color::Magenta
                    } else {
                        Color::DarkGray
                    }),
                ),
            ]),
        ];

        let mut hints = vec![];
        if has_deployment {
            hints.push("[s] shell");
        }
        if has_deployment && has_lang {
            hints.push(if svc.ejected { "[e] uneject" } else { "[e] eject" });
        }
        if has_deployment && svc.ejected {
            hints.push("[c] VS Code / Codium");
        }
        if !hints.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", hints.join("   ")),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
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

    /* ── Logs / Dev-mode panel ────────────────────────────────────────── */
    let is_ejected = selected.map(|s| s.ejected).unwrap_or(false);

    if is_ejected {
        // The container runs `sleep infinity` — no application logs to show.
        let dev_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "⚡ Service is in dev mode (ejected)",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "The container is running  sleep infinity  — there are no application logs.",
                    Style::default().fg(Color::Gray),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Press  c  to open the workspace in VS Code / Codium.",
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ];

        f.render_widget(
            Paragraph::new(dev_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Dev Mode ")
                        .border_style(Style::default().fg(Color::Magenta)),
                )
                .wrap(Wrap { trim: false }),
            right_chunks[1],
        );
    } else {
        let log_text = if let Some(svc) = selected {
            logs.get(&svc.meta_name)
                .map(|l| l.join("\n"))
                .unwrap_or_else(|| "Fetching logs...".to_string())
        } else {
            "No service selected".to_string()
        };

        let num_lines  = log_text.lines().count();
        let height     = right_chunks[1].height.saturating_sub(2) as usize;
        let max_scroll = num_lines.saturating_sub(height);

        let effective_offset = if auto_scroll {
            max_scroll
        } else {
            scroll_offset.min(max_scroll)
        };

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
                        .title(if auto_scroll {
                            " Logs [FOLLOW] "
                        } else {
                            " Logs [PAUSED] "
                        })
                        .border_style(if *focus == Focus::Logs {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                )
                .wrap(Wrap { trim: false })
                .scroll((effective_offset as u16, 0)),
            inner_log_area,
        );

        let mut scrollbar_state =
            ScrollbarState::new(max_scroll.max(1)).position(effective_offset);
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
    }

    /* ── Help bar ─────────────────────────────────────────────────────── */
    f.render_widget(
        Paragraph::new(help_text(focus, has_deployment, has_lang, is_ejected_now))
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP)),
        root[1],
    );

    /* ── Popup overlay ────────────────────────────────────────────────── */
    if let Some(p) = popup {
        render_popup(f, p, area);
    }

    DrawnAreas {
        services_list: chunks[0],
        logs: right_chunks[1],
    }
}