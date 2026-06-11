//! Package detail panel.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::shared::core::types::Package;
use crate::shared::tui::types::Focus;

pub fn draw(f: &mut Frame, area: Rect, pkg: Option<&Package>, _focus: &Focus) {
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
            Span::styled(&pkg.identifier, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
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
        lines.push(Line::from(vec![Span::styled(&pkg.description, Style::default().fg(Color::Gray))]));
    }

    if !pkg.dependencies.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled("dependencies:", Style::default().fg(Color::DarkGray))]));
        for dep in &pkg.dependencies {
            lines.push(Line::from(vec![Span::raw("  · "), Span::styled(dep, Style::default().fg(Color::Gray))]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled("─────────────────────────────", Style::default().fg(Color::DarkGray))]));
    lines.push(Line::from(""));

    if pkg.mounted {
        lines.push(Line::from(vec![
            Span::styled("  [m] Unmount", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled("[c] Open VS Code", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  [m] Mount Dev Container", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
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