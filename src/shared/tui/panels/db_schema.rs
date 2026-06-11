//! DB schema detail panel: info header, container tabs, log area.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};

use crate::shared::core::types::DbSchema;
use crate::shared::tui::types::Focus;
use super::status_color;

/// Returns the db_last_max_scroll value so the caller can update state.
pub fn draw(
    f:                     &mut Frame,
    area:                  Rect,
    schema:                Option<&DbSchema>,
    db_logs:               Option<&[String]>,
    focus:                 &Focus,
    scroll_offset:         usize,
    auto_scroll:           bool,
    db_containers:         &[String],
    db_selected_container: Option<&str>,
) -> usize {
    let Some(schema) = schema else {
        f.render_widget(
            Paragraph::new("No schema selected")
                .block(Block::default().borders(Borders::ALL).title(" DB Schema ")),
            area,
        );
        return 0;
    };

    let has_tabs    = db_containers.len() > 1;
    let constraints = if has_tabs {
        vec![Constraint::Length(7), Constraint::Length(2), Constraint::Min(0)]
    } else {
        vec![Constraint::Length(7), Constraint::Min(0)]
    };

    let chunks     = Layout::default().direction(Direction::Vertical).constraints(constraints).split(area);
    let logs_chunk = if has_tabs { chunks[2] } else { chunks[1] };

    draw_info(f, chunks[0], schema);

    if has_tabs {
        draw_container_tabs(f, chunks[1], db_containers, db_selected_container);
    }

    draw_logs(f, logs_chunk, schema, db_logs, focus, scroll_offset, auto_scroll, db_selected_container, db_containers)
}

// ── Schema info ───────────────────────────────────────────────────────────────

fn draw_info(f: &mut Frame, area: Rect, schema: &DbSchema) {
    let db_type = schema.db_type.as_deref().unwrap_or("db");

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(&schema.name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(format!("[{}]", db_type), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("identifier: ", Style::default().fg(Color::DarkGray)),
            Span::styled(schema.identifier.as_deref().unwrap_or("—"), Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("tables: ", Style::default().fg(Color::DarkGray)),
            Span::styled(schema.tables.len().to_string(), Style::default().fg(Color::Cyan)),
            Span::raw("   "),
            Span::styled("org: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&schema.organization_id, Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::styled("k8s: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&schema.k8s_status, Style::default().fg(status_color(&schema.k8s_status))),
            Span::raw("   "),
            Span::styled("ready: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&schema.k8s_ready, Style::default().fg(Color::Gray)),
        ]),
    ];

    if let Some(ref desc) = schema.description {
        if !desc.is_empty() {
            lines.push(Line::from(vec![Span::styled(desc.as_str(), Style::default().fg(Color::DarkGray))]));
        }
    }
    if let Some(ref ps) = schema.pipeline_status {
        lines.push(Line::from(vec![
            Span::styled("pipeline: ", Style::default().fg(Color::DarkGray)),
            Span::styled(ps.as_str(), Style::default().fg(Color::Yellow)),
        ]));
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" DB Schema Info ")
                .border_style(Style::default().fg(Color::Cyan)))
            .wrap(Wrap { trim: true }),
        area,
    );
}

// ── Container tab bar ─────────────────────────────────────────────────────────

fn draw_container_tabs(
    f:          &mut Frame,
    area:       Rect,
    containers: &[String],
    selected:   Option<&str>,
) {
    if containers.len() <= 1 { return; }

    let tab_w    = (area.width as usize / containers.len().max(1)).max(1) as u16;
    let mut spans: Vec<Span> = Vec::new();

    for (i, name) in containers.iter().enumerate() {
        let is_active = selected == Some(name.as_str());
        let label = if name.len() + 2 > tab_w as usize {
            format!(" {:.width$} ", name, width = (tab_w as usize).saturating_sub(2))
        } else {
            format!(" {:<width$} ", name, width = (tab_w as usize).saturating_sub(2))
        };
        spans.push(if is_active {
            Span::styled(label, Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(label, Style::default().fg(Color::DarkGray))
        });
        if i + 1 < containers.len() {
            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        }
    }
    spans.push(Span::styled("  ⇧←/⇧→", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)));

    f.render_widget(
        Paragraph::new(Line::from(spans))
            .block(Block::default().borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray))),
        area,
    );
}

// ── Log area ──────────────────────────────────────────────────────────────────

fn draw_logs(
    f:                     &mut Frame,
    area:                  Rect,
    schema:                &DbSchema,
    db_logs:               Option<&[String]>,
    focus:                 &Focus,
    scroll_offset:         usize,
    auto_scroll:           bool,
    db_selected_container: Option<&str>,
    db_containers:         &[String],
) -> usize {
    match db_logs {
        None => {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![Span::styled("  Looking for deployment…", Style::default().fg(Color::Cyan))]),
                ])
                .block(Block::default().borders(Borders::ALL).title(" Logs ")
                    .border_style(Style::default().fg(Color::DarkGray))),
                area,
            );
            0
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
                .block(Block::default().borders(Borders::ALL).title(" Logs — No Deployment ")
                    .border_style(Style::default().fg(Color::DarkGray))),
                area,
            );
            0
        }

        Some(lines) => {
            let log_text   = lines.join("\n");
            let num_lines  = log_text.lines().count();
            let height     = area.height.saturating_sub(2) as usize;
            let max_scroll = num_lines.saturating_sub(height);
            let offset     = if auto_scroll { max_scroll } else { scroll_offset.min(max_scroll) };

            let title = if let Some(name) = db_selected_container {
                if db_containers.len() > 1 { format!(" Logs [{}] ", name) }
                else { " Logs [FOLLOW] ".to_string() }
            } else {
                " Logs [FOLLOW] ".to_string()
            };

            let inner_area = Rect { width: area.width.saturating_sub(1), ..area };

            f.render_widget(
                Paragraph::new(log_text)
                    .block(Block::default().borders(Borders::ALL).title(title)
                        .border_style(if *focus == Focus::Logs {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().fg(Color::Cyan)
                        }))
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
                    x:      area.x + area.width.saturating_sub(1),
                    y:      area.y + 1,
                    width:  1,
                    height: area.height.saturating_sub(2),
                },
                &mut sb,
            );

            max_scroll
        }
    }
}