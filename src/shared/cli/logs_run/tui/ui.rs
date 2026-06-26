// src/bin/logs_run/tui/ui.rs
//
// Ratatui draw functions. Called on every frame from the main event loop.
// All state is read from `&AppState` — nothing is mutated here.
//
// Layout:
//   ┌─────────────────────────────────────────────────────────────────┐
//   │  header bar: run name, pipeline, source, overall status         │
//   ├──────────────────────┬──────────────────────────────────────────┤
//   │  LEFT: task/step     │  RIGHT: log lines for selection          │
//   │  list with cursor    │  (scrollable, timestamped)               │
//   │  (≈30% width)        │  (≈70% width)                            │
//   ├──────────────────────┴──────────────────────────────────────────┤
//   │  footer: key hints                                              │
//   └─────────────────────────────────────────────────────────────────┘
//
// Color philosophy: status colors (green/red/yellow/dim) for icons and
// final labels; a small fixed palette for task identity in the left pane
// (same palette as the original flat renderer so muscle-memory carries
// over). The right pane is unstyled except for the timestamp (dim) and
// step header.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::state::{AppState, Selection, TaskState};
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
        RunStatus::Succeeded => (
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            "✓",
        ),
        RunStatus::Failed => (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            "✗",
        ),
        RunStatus::Running => (
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            "●",
        ),
        RunStatus::Pending => (Style::default().fg(Color::DarkGray), "○"),
        RunStatus::Unknown => (Style::default().fg(Color::DarkGray), "?"),
    }
}

// ── Top-level draw ────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, state: &AppState) {
    let area = frame.size();

    // Three horizontal bands: header (3), body (fill), footer (1).
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, state, vertical[0]);

    // Body: two columns, left ~30%, right ~70%.
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(vertical[1]);

    draw_left_pane(frame, state, body[0]);
    draw_right_pane(frame, state, body[1]);

    draw_footer(frame, state, vertical[2]);
}

// ── Header ────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let (status_style, status_icon) = status_style(&state.run_status);

    let source_tag = match state.source {
        crate::shared::cli::logs_run::wire::RunSource::Tekton => " [live: tekton]",
        crate::shared::cli::logs_run::wire::RunSource::Archive => " [archived]",
    };

    let pipeline = state
        .pipeline_name
        .as_deref()
        .unwrap_or(state.run_name.as_str());

    let duration = state
        .duration_seconds
        .map(|d| format!("  ({d}s)"))
        .unwrap_or_default();

    let title_line = Line::from(vec![
        Span::styled(
            format!("{} ", status_icon),
            status_style,
        ),
        Span::styled(
            format!("PipelineRun {pipeline}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", state.run_name),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{source_tag}{duration}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let header = Paragraph::new(title_line)
        .block(Block::default().borders(Borders::BOTTOM))
        .alignment(Alignment::Left);

    frame.render_widget(header, area);
}

// ── Left pane: task/step list ─────────────────────────────────────────────

fn draw_left_pane(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .title(" Tasks ")
        .borders(Borders::RIGHT | Borders::BOTTOM);

    let inner = block.inner(area);

    let items: Vec<ListItem> = state
        .flat_rows
        .iter()
        .enumerate()
        .map(|(i, row)| build_list_item(state, row, i))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.cursor_pos));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut list_state);

    // If no tasks yet, show a waiting message.
    if state.tasks.is_empty() {
        let wait = Paragraph::new("Connecting…")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(wait, inner);
    }
}

fn build_list_item<'a>(state: &'a AppState, row: &'a Selection, idx: usize) -> ListItem<'a> {
    match row {
        Selection::Task(task_name) => {
            let task_idx = state.tasks.iter().position(|t| &t.name == task_name).unwrap_or(0);
            let task = &state.tasks[task_idx];
            let color = task_color(task_idx);
            let (status_sty, icon) = status_style(&task.status);

            // No expand indicator at all — steps are always visible.
            let step_count = if task.steps.is_empty() {
                String::new()
            } else {
                format!(" ({})", task.steps.len())
            };

            let line = Line::from(vec![
                Span::styled(icon, status_sty),
                Span::raw(" "),
                Span::styled(
                    task_name.as_str(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(step_count, Style::default().fg(Color::DarkGray)),
            ]);

            ListItem::new(line)
        }

        Selection::Step { task, step } => {
            // Unchanged — indented step row.
            let task_idx = state.tasks.iter().position(|t| &t.name == task).unwrap_or(0);
            let task_state = &state.tasks[task_idx];
            let color = task_color(task_idx);
            let step_state = task_state.steps.iter().find(|s| &s.name == step);
            let (status_sty, icon) = step_state
                .map(|s| status_style(&s.status))
                .unwrap_or((Style::default().fg(Color::DarkGray), "?"));

            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(icon, status_sty),
                Span::raw(" "),
                Span::styled("▸", Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(step.as_str(), Style::default().fg(Color::Reset)),
            ]);

            ListItem::new(line)
        }
    }
}

// ── Right pane: log lines ─────────────────────────────────────────────────

pub fn draw_right_pane(frame: &mut Frame, state: &AppState, area: Rect) {
    let title = match &state.selection {
        None => " Logs ".to_string(),
        Some(Selection::Task(t)) => format!(" Logs: {t} (all steps) "),
        Some(Selection::Step { task, step }) => format!(" Logs: {task} ▸ {step} "),
    };

    let follow_indicator = if state.log_follow { " [follow]" } else { "" };

    let block = Block::default()
        .title(format!("{title}{follow_indicator}"))
        .borders(Borders::BOTTOM);

    let inner = block.inner(area);
    let visible_height = inner.height as usize;

    let lines = state.visible_log_lines();
    let scroll = state.clamped_log_scroll(visible_height);

    // Build styled lines for the paragraph. We show timestamp + text,
    // with each step's lines prefixed by a dim step tag when showing all
    // steps for a task.
    let show_step_prefix = matches!(&state.selection, Some(Selection::Task(_)));

    let styled_lines: Vec<Line> = lines
        .iter()
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
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ));
            }
            spans.push(Span::raw(
                l.text.split('\r').last().unwrap_or(&l.text).trim_end()
            ));
            Line::from(spans)
        })
        .collect();

    let total = lines.len();
    let showing_from = scroll + 1;
    let showing_to = (scroll + visible_height).min(total);
    let scroll_info = if total > 0 {
        format!("{showing_from}-{showing_to}/{total}")
    } else {
        "no logs".to_string()
    };

    let log_widget = Paragraph::new(styled_lines)
        .block(
            block.title_alignment(Alignment::Left).title_bottom(
                Line::from(Span::styled(
                    format!(" {scroll_info} "),
                    Style::default().fg(Color::DarkGray),
                )),
            ),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(log_widget, area);

    // Empty state.
    if total == 0 {
        let msg = match &state.selection {
            None => "No task selected",
            Some(_) => "No logs yet…",
        };
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        // Render in middle of inner area.
        let mid = Rect {
            x: inner.x,
            y: inner.y + inner.height / 2,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(empty, mid);
    }
}

// ── Footer ────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, state: &AppState, area: Rect) {
    let done_hint = if state.run_done {
        "  Run complete."
    } else {
        ""
    };

    let hints = format!(
        " ↑/↓ navigate  PgUp/PgDn scroll logs  q quit{done_hint}"
    );

    let footer = Paragraph::new(hints)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Left);

    frame.render_widget(footer, area);
}