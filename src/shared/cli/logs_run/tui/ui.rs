// src/bin/logs_run/tui/ui.rs

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Frame,
};

use super::state::{AppState, Focus, LogRow, PipelineState};
use crate::shared::cli::logs_run::wire::RunStatus;

// ── Status visuals ───────────────────────────────────────────────────────

/// Icon + style used for pipeline-level / step-header status badges.
fn status_style(status: &RunStatus) -> (Style, &'static str) {
    match status {
        RunStatus::Succeeded => (Style::default().fg(Color::Green).add_modifier(Modifier::BOLD), "✓"),
        RunStatus::Failed    => (Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),   "✗"),
        RunStatus::Running   => (Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),"●"),
        RunStatus::Pending   => (Style::default().fg(Color::DarkGray), "○"),
        RunStatus::Unknown   => (Style::default().fg(Color::DarkGray), "?"),
    }
}

/// Plain status → color/style for the center task panel. No icon, no
/// per-task palette — just a single, unambiguous signal: faded when not
/// started, yellow while running, green on success, red on failure.
fn task_label_style(status: &RunStatus) -> Style {
    match status {
        RunStatus::Succeeded => Style::default().fg(Color::Green),
        RunStatus::Failed    => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        RunStatus::Running   => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        RunStatus::Pending   => Style::default().fg(Color::DarkGray),
        RunStatus::Unknown   => Style::default().fg(Color::DarkGray),
    }
}

/// Title style for a panel: an inverted (filled) bar when focused, so
/// it's unmistakable which panel currently has keyboard focus.
fn panel_title_style(focused: bool) -> Style {
    if focused {
        Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn focused_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Build a bordered block whose title bar is inverted when focused.
fn panel_block(title_spans: Vec<Span<'static>>, focused: bool, borders: Borders) -> Block<'static> {
    Block::default()
        .title(Line::from(title_spans))
        .borders(borders)
        .border_style(focused_border_style(focused))
}

// ── Top-level draw ────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, state: &AppState) {
    let area = frame.size();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, state, vertical[0]);

    // Three columns: pipeline list (20%), task list (25%), logs (55%).
    // If only one pipeline, collapse the pipeline list to 0 width — the
    // user never needs to switch between pipelines so the space is wasted.
    let constraints = if state.pipelines.len() == 1 {
        vec![
            Constraint::Percentage(0),
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ]
    } else {
        vec![
            Constraint::Percentage(20),
            Constraint::Percentage(25),
            Constraint::Percentage(55),
        ]
    };

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(vertical[1]);

    // Only render pipeline list if there are multiple runs
    if state.pipelines.len() > 1 {
        draw_pipeline_list(frame, state, body[0]);
    }

    if let Some(pipeline) = state.current() {
        draw_task_pane(frame, pipeline, state.focus == Focus::TaskLog, body[1]);
        draw_log_pane(frame, pipeline, state.focus == Focus::Logs, body[2]);
    }

    draw_footer(frame, state, vertical[2]);
}

// ── Header ────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let (run_label, pipeline_label, current_status, source_tag, duration) = match state.current() {
        None => (
            "—".to_string(),
            "—".to_string(),
            RunStatus::Pending,
            "",
            String::new(),
        ),
        Some(p) => {
            let src = match p.source {
                crate::shared::cli::logs_run::wire::RunSource::Tekton  => " [live]",
                crate::shared::cli::logs_run::wire::RunSource::Archive => " [archived]",
            };
            let dur = p.duration_seconds
                .map(|d| format!("  ({d}s)"))
                .unwrap_or_default();
            (
                p.run_name.clone(),
                p.pipeline_name.clone().unwrap_or_else(|| p.run_name.clone()),
                p.run_status,
                src,
                dur,
            )
        }
    };

    let (status_sty, status_icon) = status_style(&current_status);

    let mut spans = vec![
        Span::styled(format!("{status_icon} "), status_sty),
        Span::styled(
            pipeline_label,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {run_label}"),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{source_tag}{duration}"),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    // Show run count only when there are multiple pipelines
    if state.pipelines.len() > 1 {
        spans.push(Span::styled(
            format!("  [{}/{}]", state.selected_pipeline + 1, state.pipelines.len()),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let header = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::BOTTOM))
        .alignment(Alignment::Left);

    frame.render_widget(header, area);
}

// ── Left panel: pipeline list ─────────────────────────────────────────────

fn draw_pipeline_list(frame: &mut Frame, state: &AppState, area: Rect) {
    let focused = state.focus == Focus::PipelineList;

    let block = panel_block(
        vec![Span::styled(" Pipelines ", panel_title_style(focused))],
        focused,
        Borders::RIGHT | Borders::BOTTOM,
    );

    let items: Vec<ListItem> = state.pipelines.iter().enumerate().map(|(i, p)| {
        let (status_sty, icon) = status_style(&p.run_status);

        // Show pipeline name if known, fall back to run name
        let label = p.pipeline_name.as_deref().unwrap_or(p.run_name.as_str());

        // Truncate long names so they fit the narrow panel
        let label = if label.len() > 16 {
            format!("{}…", &label[..15])
        } else {
            label.to_string()
        };

        let done_tag = if p.run_done { " ✓" } else { "" };
        let error_tag = if p.error.is_some() { " !" } else { "" };

        let selected = i == state.selected_pipeline;

        ListItem::new(Line::from(vec![
            Span::styled(icon, status_sty),
            Span::raw(" "),
            Span::styled(
                format!("{label}{done_tag}{error_tag}"),
                if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Reset)
                },
            ),
        ]))
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_pipeline));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut list_state);
}

// ── Middle panel: task list ───────────────────────────────────────────────

fn draw_task_pane(frame: &mut Frame, pipeline: &PipelineState, focused: bool, area: Rect) {
    let block = panel_block(
        vec![Span::styled(" Tasks ", panel_title_style(focused))],
        focused,
        Borders::RIGHT | Borders::BOTTOM,
    );

    let inner = block.inner(area);

    let items: Vec<ListItem> = pipeline.tasks.iter().map(|task| {
        let label_style = task_label_style(&task.status);
        let step_count = if task.steps.is_empty() {
            String::new()
        } else {
            format!(" ({})", task.steps.len())
        };

        ListItem::new(Line::from(vec![
            Span::styled(task.name.as_str(), label_style),
            Span::styled(step_count, Style::default().fg(Color::DarkGray)),
        ]))
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(Some(pipeline.cursor_pos));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut list_state);

    if pipeline.tasks.is_empty() {
        let msg = if pipeline.error.is_some() { "Connection error" } else { "Connecting…" };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    }
}

// ── Right panel: log accordion ────────────────────────────────────────────

fn draw_log_pane(frame: &mut Frame, pipeline: &PipelineState, focused: bool, area: Rect) {
    let title_text = match &pipeline.selected_task {
        None       => " Logs ".to_string(),
        Some(task) => format!(" Logs: {task} "),
    };

    let collapse_indicator = if pipeline.logs_collapsed {
        " [collapsed — space to expand]"
    } else {
        " [expanded — space to collapse]"
    };
    let follow_indicator = if pipeline.log_follow {
        " [follow]"
    } else {
        " [manual — Ctrl+↓ to follow]"
    };

    let block = panel_block(
        vec![
            Span::styled(title_text, panel_title_style(focused)),
            Span::styled(collapse_indicator, Style::default().fg(Color::DarkGray)),
            Span::styled(follow_indicator, Style::default().fg(Color::DarkGray)),
        ],
        focused,
        Borders::BOTTOM,
    );

    let inner = block.inner(area);
    let visible_height = inner.height as usize;

    let rows = pipeline.visible_log_rows();
    let total = rows.len();
    let scroll = pipeline.clamped_log_scroll(visible_height);

    let styled_lines: Vec<Line> = rows.iter()
        .skip(scroll)
        .take(visible_height)
        .map(|row| match row {
            LogRow::Header { step, status } => {
                let (_, icon) = status_style(status);
                // Inverted bar makes each accordion section heading
                // unmistakable amid the surrounding log text.
                let header_style = Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
                let arrow = if pipeline.logs_collapsed { "▸" } else { "▾" };
                Line::from(vec![
                    Span::styled(format!(" {arrow} {icon} {step} "), header_style),
                ])
            }
            LogRow::Line(l) => {
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{} ", l.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ];
                spans.push(Span::raw(
                    l.text.split('\r').last().unwrap_or(&l.text).trim_end()
                ));
                Line::from(spans)
            }
        })
        .collect();

    let scroll_info = if total > 0 {
        let from = scroll + 1;
        let to = (scroll + visible_height).min(total);
        format!("{from}-{to}/{total}")
    } else {
        "no logs".to_string()
    };

    let log_widget = Paragraph::new(styled_lines)
        .block(
            block
                .title_alignment(Alignment::Left)
                .title_bottom(Line::from(Span::styled(
                    format!(" {scroll_info} "),
                    Style::default().fg(Color::DarkGray),
                ))),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(log_widget, area);

    // ── Scrollbar ─────────────────────────────────────────────────────────
    if total > visible_height {
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: inner.y,
            width: 1,
            height: inner.height,
        };

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .style(if focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let mut scrollbar_state = ScrollbarState::new(total.saturating_sub(visible_height))
            .position(scroll);

        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    // ── Empty state ───────────────────────────────────────────────────────
    if total == 0 {
        let msg = if pipeline.error.is_some() {
            pipeline.error.as_deref().unwrap_or("connection error")
        } else if pipeline.tasks.is_empty() {
            "Connecting…"
        } else {
            "No logs yet…"
        };

        let mid = Rect {
            x: inner.x,
            y: inner.y + inner.height / 2,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            mid,
        );
    }
}

// ── Footer ────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, state: &AppState, area: Rect) {
    let focus_hints = match state.focus {
        Focus::PipelineList => "↑/↓ select pipeline  → focus tasks  q quit",
        Focus::TaskLog      => "← pipelines  ↑/↓ navigate  → logs  q quit",
        Focus::Logs         => "← tasks  ↑/↓ scroll  PgUp/PgDn page  space collapse/expand  Ctrl+↓ follow  q quit",
    };

    let done_hint = state.current()
        .filter(|p| p.run_done)
        .map(|_| "  ✓ done")
        .unwrap_or("");

    let error_hint = state.current()
        .and_then(|p| p.error.as_deref())
        .map(|e| format!("  ✗ {e}"))
        .unwrap_or_default();

    let line = if error_hint.is_empty() {
        Line::from(format!(" {focus_hints}{done_hint}"))
    } else {
        Line::from(vec![
            Span::raw(format!(" {focus_hints}")),
            Span::styled(error_hint, Style::default().fg(Color::Red)),
        ])
    };

    frame.render_widget(
        Paragraph::new(line)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Left),
        area,
    );
}