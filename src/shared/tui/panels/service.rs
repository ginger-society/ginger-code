//! Service info panel, log panel, and container tab bar.

use std::collections::HashMap;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};

use crate::shared::core::types::K8sService;
use crate::shared::tui::types::Focus;
use super::status_color;

// ── Service info ──────────────────────────────────────────────────────────────

pub fn draw_info(
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
                    if svc.ejected { "YES (builder mode)" } else { "no" },
                    Style::default().fg(if svc.ejected { Color::Magenta } else { Color::DarkGray }),
                ),
            ]),
        ];
        let mut hints = vec![];
        if has_deployment && has_lang       { hints.push(if svc.ejected { "[e] uneject" } else { "[e] eject" }); }
        if has_deployment && svc.ejected    { hints.push("[c] VS Code"); }
        if svc.containers.len() > 1         { hints.push("⇧←/⇧→ container  |  → logs for [s] shell"); }
        else if has_deployment              { hints.push("→ logs panel for [s] shell"); }
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

// ── Container tab bar ─────────────────────────────────────────────────────────

pub fn draw_container_tabs(
    f:            &mut Frame,
    area:         Rect,
    selected_svc: Option<&K8sService>,
    active_idx:   usize,
    _active:      Option<&str>,
) {
    let svc = match selected_svc {
        Some(s) if !s.containers.is_empty() => s,
        _ => return,
    };

    let containers   = &svc.containers;
    let ejected_name = svc.ejected_container.as_deref().unwrap_or("");
    let tab_w        = (area.width as usize / containers.len().max(1)).max(1) as u16;
    let mut spans    = tab_spans(containers, active_idx, ejected_name, svc.ejected, tab_w);
    spans.push(Span::styled("  ⇧←/⇧→", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)));

    f.render_widget(
        Paragraph::new(Line::from(spans))
            .block(Block::default().borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray))),
        area,
    );
}

fn tab_spans<'a>(
    containers:   &'a [String],
    active_idx:   usize,
    ejected_name: &str,
    svc_ejected:  bool,
    tab_w:        u16,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    for (i, name) in containers.iter().enumerate() {
        let is_active      = i == active_idx;
        let is_ejected_tab = svc_ejected && name.as_str() == ejected_name;

        let raw_label = if is_ejected_tab { format!("⚡ {}", name) } else { name.clone() };
        let label = if raw_label.len() + 2 > tab_w as usize {
            format!(" {:.width$} ", raw_label, width = (tab_w as usize).saturating_sub(2))
        } else {
            format!(" {:<width$} ", raw_label, width = (tab_w as usize).saturating_sub(2))
        };

        spans.push(if is_active {
            let fg = if is_ejected_tab { Color::Magenta } else { Color::Black };
            Span::styled(label, Style::default().fg(fg).bg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else if is_ejected_tab {
            Span::styled(label, Style::default().fg(Color::Magenta))
        } else {
            Span::styled(label, Style::default().fg(Color::DarkGray))
        });

        if i + 1 < containers.len() {
            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        }
    }
    spans
}

// ── Logs panel ────────────────────────────────────────────────────────────────

pub fn draw_logs(
    f:                &mut Frame,
    area:             Rect,
    selected:         Option<&K8sService>,
    logs:             &HashMap<String, Vec<String>>,
    focus:            &Focus,
    auto_scroll:      bool,
    scroll_offset:    usize,
    active_container: Option<&str>,
) {
    // Ejected-container splash
    let viewing_ejected = selected.map(|s| {
        if !s.ejected { return false; }
        match (active_container, s.ejected_container.as_deref()) {
            (Some(ac), Some(ec)) => ac == ec,
            _                    => s.containers.len() <= 1,
        }
    }).unwrap_or(false);

    if viewing_ejected {
        let container_label = selected
            .and_then(|s| s.ejected_container.as_deref())
            .unwrap_or("container");

        f.render_widget(
            Paragraph::new(ejected_splash(container_label))
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
            .unwrap_or_else(|| "Fetching logs…".to_string())
    } else {
        "No service selected".to_string()
    };

    let num_lines        = log_text.lines().count();
    let height           = area.height.saturating_sub(2) as usize;
    let max_scroll       = num_lines.saturating_sub(height);
    let effective_offset = if auto_scroll { max_scroll } else { scroll_offset.min(max_scroll) };
    let inner_area       = Rect { width: area.width.saturating_sub(1), ..area };

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

// ── Ejected splash ────────────────────────────────────────────────────────────

fn ejected_splash(container_label: &str) -> Vec<Line<'static>> {
    let label = container_label.to_string();
    vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("⚡ Service is in dev mode (ejected)",
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Container '{}' is running  sleep infinity  — no application logs.", label),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Use ⇧← / ⇧→ to switch to a sidecar container to view its logs.",
                Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Press  c  to open the workspace in VS Code / Codium.",
                Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("To shell into a sidecar: switch container tab, then press  s  in the logs panel.",
                Style::default().fg(Color::DarkGray)),
        ]),
    ]
}