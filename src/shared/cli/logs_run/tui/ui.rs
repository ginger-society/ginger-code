// src/bin/logs_run/tui/ui.rs

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use super::state::{AppState, Focus, LogRow, PipelineState};
use crate::shared::cli::logs_run::wire::RunStatus;

// ── Status visuals ───────────────────────────────────────────────────────

fn status_style(status: &RunStatus) -> (Style, &'static str) {
    match status {
        RunStatus::Succeeded => (Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),  "✓"),
        RunStatus::Failed    => (Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),    "✗"),
        RunStatus::Running   => (Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD), "●"),
        RunStatus::Pending   => (Style::default().fg(Color::DarkGray), "○"),
        RunStatus::Unknown   => (Style::default().fg(Color::DarkGray), "?"),
    }
}

fn task_label_style(status: &RunStatus) -> Style {
    match status {
        RunStatus::Succeeded => Style::default().fg(Color::Green),
        RunStatus::Failed    => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        RunStatus::Running   => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        RunStatus::Pending   => Style::default().fg(Color::DarkGray),
        RunStatus::Unknown   => Style::default().fg(Color::DarkGray),
    }
}

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

fn panel_block(title_spans: Vec<Span<'static>>, focused: bool, borders: Borders) -> Block<'static> {
    Block::default()
        .title(Line::from(title_spans))
        .borders(borders)
        .border_style(focused_border_style(focused))
}

// ── Top-level draw ────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, state: &AppState) -> usize {
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

    if state.pipelines.len() > 1 {
        draw_pipeline_list(frame, state, body[0]);
    }

    let mut log_visible_height = 0;
    if let Some(pipeline) = state.current() {
        draw_task_pane(frame, pipeline, state.focus == Focus::TaskLog, body[1]);
        log_visible_height = draw_log_pane(frame, pipeline, state.focus == Focus::Logs, body[2]);
    }

    draw_footer(frame, state, vertical[2]);
    log_visible_height
}

// ── Header ────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let (run_label, pipeline_label, current_status, source_tag, duration, commit) =
        match state.current() {
            None => ("—".to_string(), "—".to_string(), RunStatus::Pending, "", String::new(), None),
            Some(p) => {
                let src = match p.source {
                    crate::shared::cli::logs_run::wire::RunSource::Tekton  => " [live]",
                    crate::shared::cli::logs_run::wire::RunSource::Archive => " [archived]",
                };
                let dur = p.duration_seconds.map(|d| format!("  ({d}s)")).unwrap_or_default();
                (
                    p.run_name.clone(),
                    p.pipeline_name.clone().unwrap_or_else(|| p.run_name.clone()),
                    p.run_status,
                    src,
                    dur,
                    p.commit_sha.as_ref().map(|sha| (sha.clone(), p.commit_message.clone())),
                )
            }
        };

    let (status_sty, status_icon) = status_style(&current_status);

    let mut spans = vec![
        Span::styled(format!("{status_icon} "), status_sty),
        Span::styled(pipeline_label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {run_label}"), Style::default().fg(Color::Cyan)),
        Span::styled(format!("{source_tag}{duration}"), Style::default().fg(Color::DarkGray)),
    ];

    if state.pipelines.len() > 1 {
        spans.push(Span::styled(
            format!("  [{}/{}]", state.selected_pipeline + 1, state.pipelines.len()),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if let Some((sha, message)) = commit {
        spans.push(Span::styled(
            format!("  {} ", &sha[..sha.len().min(8)]),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ));
        if let Some(msg) = message.filter(|m| !m.is_empty()) {
            spans.push(Span::styled(msg, Style::default().fg(Color::White)));
        }
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
        let label = p.pipeline_name.as_deref().unwrap_or(p.run_name.as_str());
        let label = if label.len() > 16 { format!("{}…", &label[..15]) } else { label.to_string() };
        let done_tag  = if p.run_done { " ✓" } else { "" };
        let error_tag = if p.error.is_some() { " !" } else { "" };
        let selected  = i == state.selected_pipeline;

        ListItem::new(Line::from(vec![
            Span::styled(icon, status_sty),
            Span::raw(" "),
            Span::styled(
                format!("{label}{done_tag}{error_tag}"),
                if selected { Style::default().add_modifier(Modifier::BOLD) }
                else        { Style::default().fg(Color::Reset) },
            ),
        ]))
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_pipeline));

    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        area,
        &mut list_state,
    );
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
        let step_count = if task.steps.is_empty() {
            String::new()
        } else {
            format!(" ({})", task.steps.len())
        };
        ListItem::new(Line::from(vec![
            Span::styled(task.name.as_str(), task_label_style(&task.status)),
            Span::styled(step_count, Style::default().fg(Color::DarkGray)),
        ]))
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(Some(pipeline.cursor_pos));

    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        area,
        &mut list_state,
    );

    if pipeline.tasks.is_empty() {
        let msg = if pipeline.error.is_some() { "Connection error" } else { "Connecting…" };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    }
}

// ── Right panel: log accordion ────────────────────────────────────────────
//
// Layout (no Borders::TOP):
//   row 0        : title bar (manual 1-row paragraph)
//   rows 1..h-2  : log content  ← visible_height = h-2 (exact, no guessing)
//   row h-1      : bottom border with scroll-position label
//
// No Wrap on the log Paragraph — wrapping makes logical row count diverge
// from visual row count, which breaks scroll math. Long lines are truncated
// at the right edge (standard terminal log viewer behaviour); use
// Shift+←/→ to pan horizontally.
//
// Accordion headers are padded to full content width so the inverted bar
// spans the entire line, making section boundaries unmistakable.
//
// While collapsed, ↑/↓ navigate a step cursor (highlighted in yellow) and
// Enter expands directly to that step. Space always toggles all sections.
fn draw_log_pane(frame: &mut Frame, pipeline: &PipelineState, focused: bool, area: Rect) -> usize {
    // ── Layout ────────────────────────────────────────────────────────────
    let outer_block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(focused_border_style(focused));
    let outer_inner = outer_block.inner(area); // height = area.height - 1

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(outer_inner);
    let title_area   = split[0]; // exactly 1 row
    let content_area = split[1]; // area.height - 2 rows

    let visible_height = content_area.height as usize;
    let content_width  = content_area.width  as usize;

    // ── Data ──────────────────────────────────────────────────────────────
    let rows   = pipeline.visible_log_rows();
    let total  = rows.len();
    let scroll = pipeline.clamped_log_scroll(visible_height);
    let h      = pipeline.log_h_scroll;

    // Precompute which step name (if any) is the focused step cursor while
    // collapsed, so the row renderer can highlight it without re-borrowing.
    let selected_step: Option<String> = pipeline.selected_step_name().map(|s| s.to_string());

    // ── Scroll-position label ─────────────────────────────────────────────
    let scroll_info = if total > 0 {
        let from = scroll + 1;
        let to   = (scroll + visible_height).min(total);
        if h > 0 {
            format!("{from}-{to}/{total}  →{h}")
        } else {
            format!("{from}-{to}/{total}")
        }
    } else {
        "no logs".to_string()
    };

    // ── Border + bottom label ─────────────────────────────────────────────
    frame.render_widget(
        outer_block.title_bottom(Line::from(Span::styled(
            format!(" {scroll_info} "),
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );

    // ── Title bar ─────────────────────────────────────────────────────────
    let task_label = match &pipeline.selected_task {
        None       => " Logs ".to_string(),
        Some(task) => format!(" Logs: {task} "),
    };
    let mode_hint = if pipeline.logs_collapsed {
        " [↑/↓ step  Enter expand  Space expand all]"
    } else if pipeline.log_follow {
        " [follow  Shift+←/→ h-scroll]"
    } else {
        " [manual  Ctrl+↓ follow  Shift+←/→ h-scroll]"
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(task_label, panel_title_style(focused)),
            Span::styled(mode_hint, Style::default().fg(Color::DarkGray)),
        ])),
        title_area,
    );

    // ── Log rows (no Wrap — see module doc) ──────────────────────────────
    let styled_lines: Vec<Line> = rows
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|row| match row {
            LogRow::Header { step, status } => {
                let (_, icon) = status_style(status);
                let arrow = if pipeline.logs_collapsed { "▸" } else { "▾" };

                // Is this the step currently under the cursor (collapsed only)?
                let is_cursor = selected_step.as_deref() == Some(step.as_str());

                let raw_text = format!(" {arrow} {icon} {step} ");
                // Pad to full content width so the background fills the row.
                let pad_len = content_width.saturating_sub(raw_text.chars().count());
                let text = format!("{raw_text}{}", " ".repeat(pad_len));

                let style = if is_cursor {
                    // Bright yellow: "this step is selected, press Enter"
                    Style::default()
                        .bg(Color::Yellow)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else {
                    // Standard dimmed-grey section divider
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                };

                Line::from(vec![Span::styled(text, style)])
            }

            LogRow::Line(l) => {
                // The timestamp column is frozen (always visible at the left).
                // Only the log text itself shifts with h-scroll so the user
                // always knows when each line was emitted.
                let text = l.text.split('\r').last().unwrap_or(&l.text).trim_end();
                let scrolled: String = text.chars().skip(h).collect();

                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{} ", l.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(scrolled),
                ])
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(styled_lines), content_area);

    // ── Vertical scrollbar ────────────────────────────────────────────────
    if total > visible_height {
        let sb_area = Rect {
            x:      content_area.x + content_area.width.saturating_sub(1),
            y:      content_area.y,
            width:  1,
            height: content_area.height,
        };
        let mut sb_state =
            ScrollbarState::new(total.saturating_sub(visible_height)).position(scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓"))
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .style(if focused {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
            sb_area,
            &mut sb_state,
        );
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
        frame.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            Rect {
                x:      content_area.x,
                y:      content_area.y + content_area.height / 2,
                width:  content_area.width,
                height: 1,
            },
        );
    }

    visible_height
}

// ── Footer ────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, state: &AppState, area: Rect) {
    let collapsed = state.current().map(|p| p.logs_collapsed).unwrap_or(false);

    let focus_hints = match state.focus {
        Focus::PipelineList => "↑/↓ select pipeline  → tasks  q quit",
        Focus::TaskLog      => "← pipelines  ↑/↓ navigate  → logs  q quit",
        Focus::Logs if collapsed =>
            "← tasks  ↑/↓ step  Enter expand step  Space expand all  q quit",
        Focus::Logs =>
            "← tasks  ↑/↓ scroll  PgUp/Dn page  Shift+←/→ h-scroll  Space collapse  Ctrl+↓ follow  q quit",
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