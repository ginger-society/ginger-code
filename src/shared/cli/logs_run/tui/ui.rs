// src/bin/logs_run/tui/ui.rs

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::state::{AppState, Focus, PipelineState, TaskSelection};
use crate::shared::cli::logs_run::wire::RunStatus;

// ── Palette ───────────────────────────────────────────────────────────────

const TASK_COLORS: &[Color] = &[
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Gray,
];

fn task_color(idx: usize) -> Color {
    TASK_COLORS[idx % TASK_COLORS.len()]
}

fn status_style(status: &RunStatus) -> (Style, &'static str) {
    match status {
        RunStatus::Succeeded => (Style::default().fg(Color::Green).add_modifier(Modifier::BOLD), "✓"),
        RunStatus::Failed    => (Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),   "✗"),
        RunStatus::Running   => (Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),"●"),
        RunStatus::Pending   => (Style::default().fg(Color::DarkGray), "○"),
        RunStatus::Unknown   => (Style::default().fg(Color::DarkGray), "?"),
    }
}

fn focused_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
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

    // Three columns: pipeline list (20%), task/step list (25%), logs (55%).
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

    let block = Block::default()
        .title(" Pipelines ")
        .borders(Borders::RIGHT | Borders::BOTTOM)
        .border_style(focused_border_style(focused));

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

// ── Middle panel: task/step list ──────────────────────────────────────────

fn draw_task_pane(frame: &mut Frame, pipeline: &PipelineState, focused: bool, area: Rect) {
    let block = Block::default()
        .title(" Tasks ")
        .borders(Borders::RIGHT | Borders::BOTTOM)
        .border_style(focused_border_style(focused));

    let inner = block.inner(area);

    let items: Vec<ListItem> = pipeline.flat_rows.iter().map(|row| {
        match row {
            TaskSelection::Task(task_name) => {
                let task_idx = pipeline.tasks.iter()
                    .position(|t| &t.name == task_name)
                    .unwrap_or(0);
                let task = &pipeline.tasks[task_idx];
                let color = task_color(task_idx);
                let (status_sty, icon) = status_style(&task.status);

                let step_count = if task.steps.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", task.steps.len())
                };

                ListItem::new(Line::from(vec![
                    Span::styled(icon, status_sty),
                    Span::raw(" "),
                    Span::styled(
                        task_name.as_str(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(step_count, Style::default().fg(Color::DarkGray)),
                ]))
            }

            TaskSelection::Step { task, step } => {
                let task_idx = pipeline.tasks.iter()
                    .position(|t| &t.name == task)
                    .unwrap_or(0);
                let color = task_color(task_idx);
                let step_state = pipeline.tasks[task_idx].steps.iter()
                    .find(|s| &s.name == step);
                let (status_sty, icon) = step_state
                    .map(|s| status_style(&s.status))
                    .unwrap_or((Style::default().fg(Color::DarkGray), "?"));

                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(icon, status_sty),
                    Span::raw(" "),
                    Span::styled("▸", Style::default().fg(color)),
                    Span::raw(" "),
                    Span::styled(step.as_str(), Style::default().fg(Color::Reset)),
                ]))
            }
        }
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

// ── Right panel: log lines ────────────────────────────────────────────────

fn draw_log_pane(frame: &mut Frame, pipeline: &PipelineState, focused: bool, area: Rect) {
    let title = match &pipeline.selection {
        None                                          => " Logs ".to_string(),
        Some(TaskSelection::Task(t))                  => format!(" Logs: {t} (all steps) "),
        Some(TaskSelection::Step { task, step })      => format!(" Logs: {task} ▸ {step} "),
    };

    // Show [follow] or [manual] so the user always knows which mode is active
    let follow_indicator = if pipeline.log_follow { " [follow]" } else { " [manual — Ctrl+↓ to follow]" };

    let block = Block::default()
        .title(format!("{title}{follow_indicator}"))
        .borders(Borders::BOTTOM)
        .border_style(focused_border_style(focused));

    let inner = block.inner(area);
    let visible_height = inner.height as usize;

    let lines = pipeline.visible_log_lines();
    let scroll = pipeline.clamped_log_scroll(visible_height);

    let show_step_prefix = matches!(&pipeline.selection, Some(TaskSelection::Task(_)));

    let styled_lines: Vec<Line> = lines.iter()
        .skip(scroll)
        .take(visible_height)
        .map(|l| {
            let mut spans = vec![
                Span::styled(
                    format!("{} ", l.timestamp),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if show_step_prefix {
                spans.push(Span::styled(
                    format!("[{}] ", l.step),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ));
            }
            spans.push(Span::raw(
                l.text.split('\r').last().unwrap_or(&l.text).trim_end()
            ));
            Line::from(spans)
        })
        .collect();

    let total = lines.len();
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

    // Empty state message centred in the pane
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
        Focus::Logs         => "← tasks  ↑/↓ scroll  PgUp/PgDn page  Ctrl+↓ follow  q quit",
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