use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::shared::core::types::{DbSchema, K8sService, Package};
use crate::shared::tui::types::{Focus, SidebarItem};
use super::{status_color, status_icon};

pub struct DrawnAreas {
    pub sidebar_scroll: usize,
}

pub fn draw(
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
        .border_style(if sidebar_focus { Style::default().fg(Color::Yellow) } else { Style::default() });
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let fill = |is_sel: bool, is_cur: bool| -> Span<'static> {
        let style = if is_sel      { Style::default().bg(Color::Yellow) }
                    else if is_cur { Style::default().bg(Color::DarkGray) }
                    else           { Style::default() };
        Span::styled(" ".repeat(inner.width as usize), style)
    };

    let mut items: Vec<ListItem> = Vec::new();

    // ── Services ──────────────────────────────────────────────────────────────
    for (i, svc) in services.iter().enumerate() {
        let is_sel = *sidebar_item == SidebarItem::Service(i) && sidebar_focus;
        let is_cur = *sidebar_item == SidebarItem::Service(i);

        let name_style = selected_style(is_sel, is_cur);
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
        let eject = if svc.ejected { Span::styled(" [EJECTED]", eject_style) } else { Span::raw("") };

        let container_badge = if svc.containers.len() > 1 {
            let badge_style = if is_sel {
                Style::default().bg(Color::Yellow).fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };
            Span::styled(format!(" [{}c]", svc.containers.len()), badge_style)
        } else { Span::raw("") };

        let sub_style = sub_text_style(is_sel, is_cur);

        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::styled(format!("{} ", status_icon(&svc.status)), icon_style),
                Span::styled(svc.meta_name.clone(), name_style),
                eject, container_badge,
                fill(is_sel, is_cur),
            ]),
            Line::from(vec![
                Span::styled(format!("  ready: {}  {}", svc.ready, svc.status), sub_style),
                fill(is_sel, is_cur),
            ]),
        ]));
    }

    // ── Packages ──────────────────────────────────────────────────────────────
    if !packages.is_empty() {
        items.push(section_header("── Packages & Executables ──"));

        for (i, pkg) in packages.iter().enumerate() {
            let is_sel = *sidebar_item == SidebarItem::Package(i) && sidebar_focus;
            let is_cur = *sidebar_item == SidebarItem::Package(i);

            let name_style = selected_style(is_sel, is_cur);
            let dot_color  = if pkg.mounted { Color::Green } else { Color::DarkGray };
            let dot_style  = if is_sel {
                Style::default().bg(Color::Yellow).fg(dot_color)
            } else if is_cur {
                Style::default().bg(Color::DarkGray).fg(dot_color)
            } else {
                Style::default().fg(dot_color)
            };

            let type_color = pkg_type_color(&pkg.package_type);
            let sub_style  = if is_sel {
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
            let mounted = if pkg.mounted { Span::styled(" [MOUNTED]", mounted_style) } else { Span::raw("") };

            items.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled(if pkg.mounted { "● " } else { "○ " }, dot_style),
                    Span::styled(short.to_string(), name_style),
                    mounted, fill(is_sel, is_cur),
                ]),
                Line::from(vec![
                    Span::styled(format!("  {}  ·  {}", pkg.package_type, pkg.lang), sub_style),
                    fill(is_sel, is_cur),
                ]),
            ]));
        }
    }

    // ── DB Schemas ────────────────────────────────────────────────────────────
    if !db_schemas.is_empty() {
        items.push(section_header("── DB Schemas ──────────────"));

        for (i, schema) in db_schemas.iter().enumerate() {
            let is_sel = *sidebar_item == SidebarItem::DbSchema(i) && sidebar_focus;
            let is_cur = *sidebar_item == SidebarItem::DbSchema(i);

            let name_style = selected_style(is_sel, is_cur);
            let sub_style  = sub_text_style(is_sel, is_cur);
            let db_type    = schema.db_type.as_deref().unwrap_or("db");

            let (k8s_dot, k8s_dot_color) = db_status_dot(&schema.k8s_status);
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
                    Span::styled(format!("  {}  ·  {} tables", db_type, schema.tables.len()), sub_style),
                    fill(is_sel, is_cur),
                ]),
            ]));
        }
    }

    // ── Render list + scrollbar ───────────────────────────────────────────────
    let pkg_header = if packages.is_empty()   { 0usize } else { 1 };
    let db_header  = if db_schemas.is_empty() { 0usize } else { 1 };

    let selected_flat = match sidebar_item {
        SidebarItem::Service(i)  => *i,
        SidebarItem::Package(i)  => services.len() + pkg_header + i,
        SidebarItem::DbSchema(i) => services.len() + pkg_header + packages.len() + db_header + i,
    };

    // Each item occupies 2 rows in the list.
    let visible_rows  = (inner.height as usize / 2).max(1);
    let total_slots   = services.len() + pkg_header + packages.len() + db_header + db_schemas.len();
    let max_offset    = total_slots.saturating_sub(visible_rows);

    let mut list_state = ListState::default();
    list_state.select(Some(selected_flat));

    // Override ratatui's default "stick selected to bottom" behaviour:
    // keep the selected item one row from the top of the viewport so
    // scrolling up doesn't leave it anchored at the bottom.
    let desired_offset = selected_flat.saturating_sub(1);
    *list_state.offset_mut() = desired_offset.min(max_offset);

    let list_area = Rect { width: inner.width.saturating_sub(1), ..inner };
    f.render_stateful_widget(List::new(items).highlight_style(Style::default()), list_area, &mut list_state);

    if max_offset > 0 {
        let scroll_pos   = list_state.offset();
        let mut sb_state = ScrollbarState::new(max_offset).position(scroll_pos);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲")).end_symbol(Some("▼"))
                .track_symbol(Some("│")).thumb_symbol("█"),
            Rect { x: inner.x + inner.width.saturating_sub(1), y: inner.y, width: 1, height: inner.height },
            &mut sb_state,
        );
    }

    list_state.offset()
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn selected_style(is_sel: bool, is_cur: bool) -> Style {
    if is_sel      { Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD) }
    else if is_cur { Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD) }
    else           { Style::default().fg(Color::Gray) }
}

fn sub_text_style(is_sel: bool, is_cur: bool) -> Style {
    if is_sel      { Style::default().bg(Color::Yellow).fg(Color::DarkGray) }
    else if is_cur { Style::default().bg(Color::DarkGray).fg(Color::DarkGray) }
    else           { Style::default().fg(Color::DarkGray) }
}

fn section_header(label: &'static str) -> ListItem<'static> {
    ListItem::new(vec![
        Line::from(vec![Span::styled(label, Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM))]),
        Line::from(""),
    ])
}

fn pkg_type_color(t: &str) -> Color {
    match t {
        "lib" | "library"    => Color::Cyan,
        "bin" | "executable" => Color::Magenta,
        _                    => Color::DarkGray,
    }
}

fn db_status_dot(status: &str) -> (&'static str, Color) {
    match status {
        "Running"      => ("● ", Color::Green),
        "Degraded"     => ("◐ ", Color::Yellow),
        "Pending"      => ("○ ", Color::Yellow),
        "Not deployed" => ("· ", Color::DarkGray),
        _              => ("✗ ", Color::DarkGray),
    }
}