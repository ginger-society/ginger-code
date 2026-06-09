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

use crate::shared::{
    core::types::{DbSchema, K8sService, Package},
    tui::{
        popup::render_popup,
        types::{Focus, Popup, SidebarItem},
    },
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
/// Each item occupies 2 rows; section headers also occupy 2 rows.
/// `scroll_offset` is the number of list items scrolled off the top.
pub fn click_sidebar_item(
    col:           u16,
    row:           u16,
    sidebar_area:  Rect,
    scroll_offset: usize,
    svc_count:     usize,
    pkg_count:     usize,
    db_count:      usize,
) -> Option<SidebarItem> {
    let x0 = sidebar_area.x + 1;
    let y0 = sidebar_area.y + 2; // border + "Services" header row
    let x1 = sidebar_area.x + sidebar_area.width - 1;
    let y1 = sidebar_area.y + sidebar_area.height - 1;

    if col < x0 || col >= x1 || row < y0 || row >= y1 {
        return None;
    }

    let row_in_list = (row - y0) as usize;
    let raw_row     = scroll_offset * 2 + row_in_list;

    let svc_end = svc_count * 2;

    if raw_row < svc_end {
        let idx = raw_row / 2;
        return if idx < svc_count { Some(SidebarItem::Service(idx)) } else { None };
    }

    if pkg_count > 0 {
        let pkg_header_start = svc_end;
        let pkg_header_end   = pkg_header_start + 2;
        let pkg_end          = pkg_header_end + pkg_count * 2;

        if raw_row < pkg_header_end {
            return None;
        }
        if raw_row < pkg_end {
            let idx = (raw_row - pkg_header_end) / 2;
            return if idx < pkg_count { Some(SidebarItem::Package(idx)) } else { None };
        }

        if db_count > 0 {
            let db_header_start = pkg_end;
            let db_header_end   = db_header_start + 2;
            let db_end          = db_header_end + db_count * 2;

            if raw_row < db_header_end {
                return None;
            }
            if raw_row < db_end {
                let idx = (raw_row - db_header_end) / 2;
                return if idx < db_count { Some(SidebarItem::DbSchema(idx)) } else { None };
            }
        }
    } else if db_count > 0 {
        let db_header_start = svc_end;
        let db_header_end   = db_header_start + 2;
        let db_end          = db_header_end + db_count * 2;

        if raw_row < db_header_end {
            return None;
        }
        if raw_row < db_end {
            let idx = (raw_row - db_header_end) / 2;
            return if idx < db_count { Some(SidebarItem::DbSchema(idx)) } else { None };
        }
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
                "↑/↓ navigate  |  m mount/unmount  |  c VS Code (if mounted)  |  q quit"
                    .to_string()
            }
            SidebarItem::DbSchema(_) => {
                "↑/↓ navigate  |  → logs  |  q quit".to_string()
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
    pub sidebar_scroll: usize,
    pub logs:           Rect,
}

/* ================================================================
   MAIN DRAW
   ================================================================ */

pub fn draw(
    f:              &mut Frame,
    services:       &[K8sService],
    packages:       &[Package],
    db_schemas:     &[DbSchema],
    sidebar_item:   &SidebarItem,
    logs:           &HashMap<String, Vec<String>>,
    db_logs:        Option<&[String]>,
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
    let sidebar_scroll = draw_sidebar(
        f, chunks[0], services, packages, db_schemas, sidebar_item, focus,
    );

    // ── Right pane ────────────────────────────────────────────────────────────
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(chunks[1]);

    let logs_area = match sidebar_item {
        SidebarItem::Package(pkg_idx) => {
            draw_package_detail(f, chunks[1], packages.get(*pkg_idx), focus);
            right_chunks[1]
        }
        SidebarItem::DbSchema(db_idx) => {
            draw_db_schema_detail(f, chunks[1], db_schemas.get(*db_idx), db_logs, focus, scroll_offset, auto_scroll);
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
   SIDEBAR
   ================================================================ */

fn draw_sidebar(
    f:            &mut Frame,
    area:         Rect,
    services:     &[K8sService],
    packages:     &[Package],
    db_schemas:   &[DbSchema],
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

    // ── Full-width highlight helpers ──────────────────────────────────────────
    //
    // Ratatui's List widget only paints the highlight background behind the
    // text content of each Span — it does not extend to the right edge of the
    // list area.  The fix is to append a trailing "fill" Span to every Line of
    // a selected/current item that carries the same background colour.  The
    // Span text is a run of spaces wider than the inner area (ratatui clips it),
    // so the background floods all the way to the right border.

    let row_bg = |is_sel: bool, is_cur: bool| -> Style {
        if is_sel      { Style::default().bg(Color::Yellow) }
        else if is_cur { Style::default().bg(Color::DarkGray) }
        else           { Style::default() }
    };

    // A trailing span that bleeds colour to the right edge.
    let fill = |is_sel: bool, is_cur: bool| -> Span<'static> {
        // inner.width characters is always enough; ratatui clips the rest.
        Span::styled(
            " ".repeat(inner.width as usize),
            row_bg(is_sel, is_cur),
        )
    };

    let mut items: Vec<ListItem> = Vec::new();

    // ── Service items ─────────────────────────────────────────────────────────
    for (i, svc) in services.iter().enumerate() {
        let is_sel = *sidebar_item == SidebarItem::Service(i) && sidebar_focus;
        let is_cur = *sidebar_item == SidebarItem::Service(i);

        let name_style = if is_sel {
            Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else if is_cur {
            Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        // Status icon: always carry the row background so it doesn't "punch
        // through" to the terminal default when the row is highlighted.
        let icon_style = if is_sel {
            Style::default().bg(Color::Yellow).fg(status_color(&svc.status))
        } else if is_cur {
            Style::default().bg(Color::DarkGray).fg(status_color(&svc.status))
        } else {
            Style::default().fg(status_color(&svc.status))
        };

        let eject_style = if is_sel {
            Style::default().bg(Color::Yellow).fg(Color::Magenta).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        };

        let eject = if svc.ejected {
            Span::styled(" [EJECTED]", eject_style)
        } else {
            Span::raw("")
        };

        let sub_style = if is_sel {
            Style::default().bg(Color::Yellow).fg(Color::DarkGray)
        } else if is_cur {
            Style::default().bg(Color::DarkGray).fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::styled(format!("{} ", status_icon(&svc.status)), icon_style),
                Span::styled(svc.meta_name.clone(), name_style),
                eject,
                fill(is_sel, is_cur),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("  ready: {}  {}", svc.ready, svc.status),
                    sub_style,
                ),
                fill(is_sel, is_cur),
            ]),
        ]));
    }

    // ── Packages section header ───────────────────────────────────────────────
    if !packages.is_empty() {
        items.push(ListItem::new(vec![
            Line::from(vec![Span::styled(
                "── Packages & Executables ──",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            )]),
            Line::from(""),
        ]));

        for (i, pkg) in packages.iter().enumerate() {
            let is_sel = *sidebar_item == SidebarItem::Package(i) && sidebar_focus;
            let is_cur = *sidebar_item == SidebarItem::Package(i);

            let name_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let dot_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(if pkg.mounted { Color::Green } else { Color::DarkGray })
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(if pkg.mounted { Color::Green } else { Color::DarkGray })
            } else if pkg.mounted {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let type_color = match pkg.package_type.as_str() {
                "lib" | "library"    => Color::Cyan,
                "bin" | "executable" => Color::Magenta,
                _                    => Color::DarkGray,
            };

            let sub_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(type_color)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(type_color)
            } else {
                Style::default().fg(type_color)
            };

            let short = pkg.identifier.split('/').last().unwrap_or(&pkg.identifier);

            let mounted_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            };

            let mounted = if pkg.mounted {
                Span::styled(" [MOUNTED]", mounted_style)
            } else {
                Span::raw("")
            };

            items.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled(if pkg.mounted { "● " } else { "○ " }, dot_style),
                    Span::styled(short.to_string(), name_style),
                    mounted,
                    fill(is_sel, is_cur),
                ]),
                Line::from(vec![
                    Span::styled(
                        format!("  {}  ·  {}", pkg.package_type, pkg.lang),
                        sub_style,
                    ),
                    fill(is_sel, is_cur),
                ]),
            ]));
        }
    }

    // ── DB Schemas section header ─────────────────────────────────────────────
    if !db_schemas.is_empty() {
        items.push(ListItem::new(vec![
            Line::from(vec![Span::styled(
                "── DB Schemas ──────────────",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            )]),
            Line::from(""),
        ]));

        for (i, schema) in db_schemas.iter().enumerate() {
            let is_sel = *sidebar_item == SidebarItem::DbSchema(i) && sidebar_focus;
            let is_cur = *sidebar_item == SidebarItem::DbSchema(i);

            let name_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let icon_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::Cyan)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };

            let db_type = schema.db_type.as_deref().unwrap_or("db");

            let sub_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::DarkGray)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Replace the current db schema ListItem push with:
            let k8s_dot = match schema.k8s_status.as_str() {
                "Running"      => "● ",
                "Degraded"     => "◐ ",
                "Pending"      => "○ ",
                "Not deployed" => "· ",
                _              => "✗ ",
            };
            let k8s_dot_color = match schema.k8s_status.as_str() {
                "Running"            => Color::Green,
                "Degraded"|"Pending" => Color::Yellow,
                _                    => Color::DarkGray,
            };

            let dot_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(k8s_dot_color)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(k8s_dot_color)
            } else {
                Style::default().fg(k8s_dot_color)
            };

            items.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled(k8s_dot, dot_style),
                    Span::styled(schema.name.clone(), name_style),
                    fill(is_sel, is_cur),
                ]),
                Line::from(vec![
                    Span::styled(
                        format!("  {}  ·  {} tables", db_type, schema.tables.len()),
                        sub_style,
                    ),
                    fill(is_sel, is_cur),
                ]),
            ]));
        }
    }

    // ── Compute selected flat index ───────────────────────────────────────────
    let pkg_header = if packages.is_empty()  { 0 } else { 1 };
    let db_header  = if db_schemas.is_empty() { 0 } else { 1 };

    let selected_flat = match sidebar_item {
        SidebarItem::Service(i)  => *i,
        SidebarItem::Package(i)  => services.len() + pkg_header + i,
        SidebarItem::DbSchema(i) => {
            services.len() + pkg_header + packages.len() + db_header + i
        }
    };

    let mut state = ListState::default();
    state.select(Some(selected_flat));

    // Use a no-op highlight_style — all highlighting is painted via fill spans
    // above, so we don't want the List widget to apply its own background on
    // top (which would only cover the text-content portion of each row).
    //
    // Shrink the list area by 1 column on the right to leave room for the
    // scrollbar — otherwise it renders on top of the list content.
    let list_area = Rect {
        width: inner.width.saturating_sub(1),
        ..inner
    };
    let list = List::new(items).highlight_style(Style::default());
    f.render_stateful_widget(list, list_area, &mut state);

    // ── Sidebar scrollbar ─────────────────────────────────────────────────────
    //
    // Total flat item count (headers count as 1 slot each, items as 1 slot).
    let pkg_header_slots = if packages.is_empty()   { 0usize } else { 1 };
    let db_header_slots  = if db_schemas.is_empty()  { 0usize } else { 1 };
    let total_slots      = services.len()
        + pkg_header_slots + packages.len()
        + db_header_slots  + db_schemas.len();

    // Each slot is 2 terminal rows tall; the inner area gives us this many
    // visible slots (integer division intentional — partial slots don't count).
    let visible_slots = (inner.height as usize / 2).max(1);
    let max_scroll    = total_slots.saturating_sub(visible_slots);
    let scroll_pos    = state.offset();

    if max_scroll > 0 {
        let mut sb_state = ScrollbarState::new(max_scroll).position(scroll_pos);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█"),
            Rect {
                x:      inner.x + inner.width.saturating_sub(1),
                y:      inner.y,
                width:  1,
                height: inner.height,
            },
            &mut sb_state,
        );
    }

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
        if has_deployment             { hints.push("[s] shell"); }
        if has_deployment && has_lang { hints.push(if svc.ejected { "[e] uneject" } else { "[e] eject" }); }
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
   LOGS PANEL
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
            x:      area.x + area.width.saturating_sub(1),
            y:      area.y + 1,
            width:  1,
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

/* ================================================================
   DB SCHEMA DETAIL PANEL
   ================================================================ */

fn draw_db_schema_detail(
    f:          &mut Frame,
    area:       Rect,
    schema:     Option<&DbSchema>,
    db_logs:    Option<&[String]>,
    focus:      &Focus,
    scroll_offset: usize,    
    auto_scroll:   bool, 
) {
    let Some(schema) = schema else {
        f.render_widget(
            Paragraph::new("No schema selected")
                .block(Block::default().borders(Borders::ALL).title(" DB Schema ")),
            area,
        );
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    // ── Info strip ────────────────────────────────────────────────────────────
    let db_type = schema.db_type.as_deref().unwrap_or("db");

    let mut info_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                &schema.name,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("[{}]", db_type),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![
            Span::styled("identifier: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                schema.identifier.as_deref().unwrap_or("—"),
                Style::default().fg(Color::Gray),
            ),
            Span::raw("   "),
            Span::styled("tables: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                schema.tables.len().to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("   "),
            Span::styled("org: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &schema.organization_id,
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];
    // After the identifier/tables/org line, add:
    info_lines.push(Line::from(vec![
        Span::styled("k8s: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &schema.k8s_status,
            Style::default().fg(match schema.k8s_status.as_str() {
                "Running"            => Color::Green,
                "Degraded"|"Pending" => Color::Yellow,
                _                    => Color::DarkGray,
            }),
        ),
        Span::raw("   "),
        Span::styled("ready: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&schema.k8s_ready, Style::default().fg(Color::Gray)),
    ]));

    if let Some(ref desc) = schema.description {
        if !desc.is_empty() {
            info_lines.push(Line::from(vec![
                Span::styled(desc.as_str(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if let Some(ref ps) = schema.pipeline_status {
        info_lines.push(Line::from(vec![
            Span::styled("pipeline: ", Style::default().fg(Color::DarkGray)),
            Span::styled(ps.as_str(), Style::default().fg(Color::Yellow)),
        ]));
    }

    f.render_widget(
        Paragraph::new(info_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" DB Schema Info ")
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true }),
        chunks[0],
    );

    // ── Logs pane ─────────────────────────────────────────────────────────────
    match db_logs {
        None => {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  Looking for deployment…",
                        Style::default().fg(Color::Cyan),
                    )]),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Logs ")
                        .border_style(Style::default().fg(Color::DarkGray)),
                ),
                chunks[1],
            );
        }

        Some([]) => {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  ○  No deployment found in the default namespace for this schema.",
                        Style::default().fg(Color::DarkGray),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  Expected a deployment whose name matches the schema identifier.",
                        Style::default().fg(Color::DarkGray),
                    )]),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Logs — No Deployment ")
                        .border_style(Style::default().fg(Color::DarkGray)),
                ),
                chunks[1],
            );
        }

        Some(lines) => {
            let log_text   = lines.join("\n");
            let num_lines  = log_text.lines().count();
            let height     = chunks[1].height.saturating_sub(2) as usize;
            let max_scroll = num_lines.saturating_sub(height);
            let offset = if auto_scroll { max_scroll } else { scroll_offset.min(max_scroll) };

            let inner_area = Rect { width: chunks[1].width.saturating_sub(1), ..chunks[1] };

            f.render_widget(
                Paragraph::new(log_text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Logs [FOLLOW] ")
                            .border_style(if *focus == Focus::Logs {
                                Style::default().fg(Color::Yellow)
                            } else {
                                Style::default().fg(Color::Cyan)
                            }),
                    )
                    .wrap(Wrap { trim: false })
                    .scroll((offset as u16, 0)),
                inner_area,
            );

            let mut sb = ScrollbarState::new(max_scroll.max(1)).position(offset);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("▲")).end_symbol(Some("▼"))
                    .track_symbol(Some("│")).thumb_symbol("█"),
                Rect {
                    x:      chunks[1].x + chunks[1].width.saturating_sub(1),
                    y:      chunks[1].y + 1,
                    width:  1,
                    height: chunks[1].height.saturating_sub(2),
                },
                &mut sb,
            );
        }
    }
}