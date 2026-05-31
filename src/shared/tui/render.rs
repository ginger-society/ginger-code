use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Frame,
};
use std::collections::HashMap;

use crate::shared::tui::{
    popup::render_popup,
    types::{Focus, K8sService, Package, Popup, SidebarItem},
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

pub fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x
        && col < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}

/// Translate a mouse click in the sidebar to a `SidebarItem`.
/// Each item occupies 2 rows (name + subline).
/// `scroll_offset` is the number of items scrolled off the top.
pub fn click_sidebar_item(
    col:           u16,
    row:           u16,
    sidebar_area:  Rect,
    scroll_offset: usize,
    svc_count:     usize,
    pkg_count:     usize,
) -> Option<SidebarItem> {
    // inner area: 1px border on each side, 1 row title
    let x0 = sidebar_area.x + 1;
    let y0 = sidebar_area.y + 2; // border + "Services" header row
    let x1 = sidebar_area.x + sidebar_area.width - 1;
    let y1 = sidebar_area.y + sidebar_area.height - 1;

    if col < x0 || col >= x1 || row < y0 || row >= y1 {
        return None;
    }

    let row_in_list = (row - y0) as usize;
    // Each item takes 2 rows
    let list_item   = scroll_offset + row_in_list / 2;

    // Is the row inside a separator line (odd lines between sections)?
    // We account for the section-header row that sits between services and packages.
    // Layout inside the list widget (after "Services" header):
    //   items 0..svc_count  → rows 0..svc_count*2
    //   separator           → row svc_count*2  (1 row, the "Packages" header)
    //   items 0..pkg_count  → rows svc_count*2+1 ..
    let svc_rows = svc_count * 2;
    let raw_row  = scroll_offset * 2 + row_in_list; // row within the unscrolled list content

    if raw_row < svc_rows {
        let idx = raw_row / 2;
        if idx < svc_count { return Some(SidebarItem::Service(idx)); }
    } else if raw_row == svc_rows {
        // Clicked the "Packages" section header — ignore
        return None;
    } else {
        let pkg_raw = raw_row - svc_rows - 1;
        let idx     = pkg_raw / 2;
        if idx < pkg_count { return Some(SidebarItem::Package(idx)); }
    }
    None
}

/* ================================================================
   HELP TEXT
   ================================================================ */

pub fn help_text(
    focus:          &Focus,
    sidebar_item:   &SidebarItem,
    has_deployment: bool,
    has_lang:       bool,
    ejected:        bool,
) -> String {
    match focus {
        Focus::Sidebar => match sidebar_item {
            SidebarItem::Service(_) => {
                let mut parts = vec!["↑/↓ navigate", "→ logs"];
                if has_deployment {
                    parts.push("s shell");
                    if has_lang {
                        parts.push(if ejected { "e uneject" } else { "e eject" });
                    }
                    if ejected { parts.push("c VS Code"); }
                }
                parts.push("q quit");
                parts.join("  |  ")
            }
            SidebarItem::Package(_) => {
                "↑/↓ navigate  |  → detail  |  m mount/unmount  |  c VS Code (if mounted)  |  q quit"
                    .to_string()
            }
        },
        Focus::Logs => {
            "PgUp top  |  PgDn follow  |  ↑/↓  j/k scroll  |  g/G jump  |  ← sidebar  |  q quit"
                .to_string()
        }
    }
}

/* ================================================================
   RETURNED LAYOUT AREAS
   ================================================================ */

pub struct DrawnAreas {
    pub sidebar:        Rect,
    pub sidebar_scroll: usize,   // current scroll offset for hit-testing
    pub logs:           Rect,
}

/* ================================================================
   MAIN DRAW
   ================================================================ */

pub fn draw(
    f:              &mut Frame,
    services:       &[K8sService],
    packages:       &[Package],
    sidebar_item:   &SidebarItem,
    logs:           &HashMap<String, Vec<String>>,
    focus:          &Focus,
    auto_scroll:    bool,
    scroll_offset:  usize,
    has_deployment: bool,
    has_lang:       bool,
    is_ejected_now: bool,
    popup:          Option<&Popup>,
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

    // ── Sidebar ───────────────────────────────────────────────────────────────
    let sidebar_scroll = draw_sidebar(f, chunks[0], services, packages, sidebar_item, focus);

    // ── Right pane ────────────────────────────────────────────────────────────
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(chunks[1]);

    let logs_area = match sidebar_item {
        SidebarItem::Package(pkg_idx) => {
            // Package selected → full right side shows package detail.
            draw_package_detail(f, chunks[1], packages.get(*pkg_idx), focus);
            right_chunks[1]
        }
        SidebarItem::Service(svc_idx) => {
            let selected = services.get(*svc_idx);
            draw_service_info(f, right_chunks[0], selected, has_deployment, has_lang, is_ejected_now);
            draw_logs(f, right_chunks[1], selected, logs, focus, auto_scroll, scroll_offset);
            right_chunks[1]
        }
    };

    // ── Help bar ──────────────────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(help_text(focus, sidebar_item, has_deployment, has_lang, is_ejected_now))
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP)),
        root[1],
    );

    // ── Popup overlay ─────────────────────────────────────────────────────────
    if let Some(p) = popup {
        render_popup(f, p, area);
    }

    DrawnAreas { sidebar: chunks[0], sidebar_scroll, logs: logs_area }
}

/* ================================================================
   SIDEBAR  (scrollable unified list)
   ================================================================ */

/// Returns the scroll offset used, for hit-test back in mod.rs.
fn draw_sidebar(
    f:            &mut Frame,
    area:         Rect,
    services:     &[K8sService],
    packages:     &[Package],
    sidebar_item: &SidebarItem,
    focus:        &Focus,
) -> usize {
    let sidebar_focus = *focus == Focus::Sidebar;
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" Services ")
        .border_style(if sidebar_focus {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // Build a flat list of ListItems.
    // Structure:
    //   [service rows …]
    //   [section header if packages exist]
    //   [package rows …]
    //
    // The "selected" index in ListState maps directly to this flat list.

    let mut items: Vec<ListItem> = Vec::new();

    // ── Service items ─────────────────────────────────────────────────────────
    for (i, svc) in services.iter().enumerate() {
        let is_sel  = *sidebar_item == SidebarItem::Service(i) && sidebar_focus;
        let is_cur  = *sidebar_item == SidebarItem::Service(i);
        let base    = if is_sel {
            Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else if is_cur {
            Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let icon_s  = Style::default().fg(status_color(&svc.status));
        let eject   = if svc.ejected {
            Span::styled(" [EJECTED]", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("")
        };
        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::styled(format!("{} ", status_icon(&svc.status)), icon_s),
                Span::styled(svc.meta_name.clone(), base),
                eject,
            ]),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("ready: {}  {}", svc.ready, svc.status),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ]));
    }

    // ── Packages section header ───────────────────────────────────────────────
    let pkg_header_idx = if packages.is_empty() {
        None
    } else {
        let idx = items.len();
        items.push(ListItem::new(vec![
            Line::from(vec![Span::styled(
                "── Packages & Executables ──",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            )]),
            Line::from(""), // blank second row so 2-row rhythm is preserved
        ]));
        Some(idx)
    };

    // ── Package items ─────────────────────────────────────────────────────────
    for (i, pkg) in packages.iter().enumerate() {
        let is_sel = *sidebar_item == SidebarItem::Package(i) && sidebar_focus;
        let is_cur = *sidebar_item == SidebarItem::Package(i);
        let base   = if is_sel {
            Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else if is_cur {
            Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let dot_style = if pkg.mounted {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let type_color = match pkg.package_type.as_str() {
            "lib" | "library"    => Color::Cyan,
            "bin" | "executable" => Color::Magenta,
            _                    => Color::DarkGray,
        };
        let short = pkg.identifier.split('/').last().unwrap_or(&pkg.identifier);
        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::styled(if pkg.mounted { "● " } else { "○ " }, dot_style),
                Span::styled(short.to_string(), base),
            ]),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{}  ·  {}", pkg.package_type, pkg.lang),
                    Style::default().fg(type_color),
                ),
            ]),
        ]));
    }

    // ── Compute which flat list index is selected ─────────────────────────────
    let selected_flat = match sidebar_item {
        SidebarItem::Service(i) => *i,
        SidebarItem::Package(i) => {
            // services + optional header + package index
            services.len() + if packages.is_empty() { 0 } else { 1 } + i
        }
    };

    // ── Render with ListState so ratatui handles scrolling ───────────────────
    let mut state = ListState::default();
    state.select(Some(selected_flat));

    let list = List::new(items).highlight_style(Style::default()); // styling done per-item
    f.render_stateful_widget(list, inner, &mut state);

    // Derive scroll offset from ListState for hit-test.
    state.offset()
}

/* ================================================================
   SERVICE INFO PANEL
   ================================================================ */

fn draw_service_info(
    f:              &mut Frame,
    area:           Rect,
    selected:       Option<&K8sService>,
    has_deployment: bool,
    has_lang:       bool,
    is_ejected_now: bool,
) {
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
                Span::styled(svc.status.clone(), Style::default().fg(status_color(&svc.status))),
            ]),
            Line::from(vec![
                Span::styled("Ejected: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    if svc.ejected { "YES (builder mode)" } else { "no" }.to_string(),
                    Style::default().fg(if svc.ejected { Color::Magenta } else { Color::DarkGray }),
                ),
            ]),
        ];
        let mut hints = vec![];
        if has_deployment               { hints.push("[s] shell"); }
        if has_deployment && has_lang   { hints.push(if svc.ejected { "[e] uneject" } else { "[e] eject" }); }
        if has_deployment && svc.ejected { hints.push("[c] VS Code"); }
        if !hints.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", hints.join("   ")),
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )));
        }
        lines
    } else {
        vec![Line::from("No service selected")]
    };

    f.render_widget(
        Paragraph::new(info_lines)
            .block(Block::default().borders(Borders::ALL).title(" Service Info ")
                .border_style(Style::default().fg(Color::Blue)))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/* ================================================================
   LOGS / DEV-MODE PANEL
   ================================================================ */

fn draw_logs(
    f:             &mut Frame,
    area:          Rect,
    selected:      Option<&K8sService>,
    logs:          &HashMap<String, Vec<String>>,
    focus:         &Focus,
    auto_scroll:   bool,
    scroll_offset: usize,
) {
    if selected.map(|s| s.ejected).unwrap_or(false) {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(vec![Span::raw("  "), Span::styled(
                    "⚡ Service is in dev mode (ejected)",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )]),
                Line::from(""),
                Line::from(vec![Span::raw("  "), Span::styled(
                    "The container is running  sleep infinity  — no application logs.",
                    Style::default().fg(Color::Gray),
                )]),
                Line::from(""),
                Line::from(vec![Span::raw("  "), Span::styled(
                    "Press  c  to open the workspace in VS Code / Codium.",
                    Style::default().fg(Color::Cyan),
                )]),
            ])
            .block(Block::default().borders(Borders::ALL).title(" Dev Mode ")
                .border_style(Style::default().fg(Color::Magenta)))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let log_text = if let Some(svc) = selected {
        logs.get(&svc.meta_name)
            .map(|l| l.join("\n"))
            .unwrap_or_else(|| "Fetching logs...".to_string())
    } else {
        "No service selected".to_string()
    };

    let num_lines        = log_text.lines().count();
    let height           = area.height.saturating_sub(2) as usize;
    let max_scroll       = num_lines.saturating_sub(height);
    let effective_offset = if auto_scroll { max_scroll } else { scroll_offset.min(max_scroll) };

    let inner_area = Rect { width: area.width.saturating_sub(1), ..area };

    f.render_widget(
        Paragraph::new(log_text)
            .block(Block::default().borders(Borders::ALL)
                .title(if auto_scroll { " Logs [FOLLOW] " } else { " Logs [PAUSED] " })
                .border_style(if *focus == Focus::Logs {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }))
            .wrap(Wrap { trim: false })
            .scroll((effective_offset as u16, 0)),
        inner_area,
    );

    let mut sb = ScrollbarState::new(max_scroll.max(1)).position(effective_offset);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲")).end_symbol(Some("▼"))
            .track_symbol(Some("│")).thumb_symbol("█"),
        Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        },
        &mut sb,
    );
}

/* ================================================================
   PACKAGE DETAIL PANEL
   ================================================================ */

fn draw_package_detail(f: &mut Frame, area: Rect, pkg: Option<&Package>, focus: &Focus) {
    let Some(pkg) = pkg else {
        f.render_widget(
            Paragraph::new("No package selected")
                .block(Block::default().borders(Borders::ALL).title(" Package ")),
            area,
        );
        return;
    };

    let type_color = match pkg.package_type.as_str() {
        "lib" | "library"    => Color::Cyan,
        "bin" | "executable" => Color::Magenta,
        _                    => Color::DarkGray,
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(&pkg.identifier,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(format!("[{}]", pkg.package_type), Style::default().fg(type_color)),
            if pkg.mounted {
                Span::styled("  ● mounted", Style::default().fg(Color::Green))
            } else {
                Span::styled("  ○ not mounted", Style::default().fg(Color::DarkGray))
            },
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("lang:  ", Style::default().fg(Color::Cyan)),
            Span::raw(&pkg.lang),
        ]),
    ];

    if !pkg.description.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(&pkg.description, Style::default().fg(Color::Gray)),
        ]));
    }

    if !pkg.dependencies.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("dependencies:", Style::default().fg(Color::DarkGray)),
        ]));
        for dep in &pkg.dependencies {
            lines.push(Line::from(vec![
                Span::raw("  · "),
                Span::styled(dep, Style::default().fg(Color::Gray)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "─────────────────────────────",
        Style::default().fg(Color::DarkGray),
    )]));
    lines.push(Line::from(""));

    if pkg.mounted {
        lines.push(Line::from(vec![
            Span::styled("  [m] Unmount",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled("[c] Open VS Code",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  [m] Mount Dev Container",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ]));
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Package Detail ")
                .border_style(Style::default().fg(Color::Yellow)))
            .wrap(Wrap { trim: false }),
        area,
    );
}