//! Infra-as-Code detail panel.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::shared::core::types::InfraAsCode;
use crate::shared::tui::types::Focus;

pub fn draw(f: &mut Frame, area: Rect, iac: &InfraAsCode, _focus: &Focus) {
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "⚙  Infra as Code",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            if iac.mounted {
                Span::styled("● mounted", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            } else {
                Span::styled("○ not mounted", Style::default().fg(Color::DarkGray))
            },
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "This is an Infra as Code repo. There is no deployment as such — you can",
            Style::default().fg(Color::Gray),
        )]),
        Line::from(vec![Span::styled(
            "however mount it to make changes in this repo.",
            Style::default().fg(Color::Gray),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Slug:  ", Style::default().fg(Color::Cyan)),
            Span::styled(iac.slug(), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "kubectl is available on the mounted environment which can be used for",
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(vec![Span::styled(
            "debugging and testing.",
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "────────────────────────────────────",
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(""),
    ];

    if iac.mounted {
        lines.push(Line::from(vec![
            Span::styled(
                "  [m] Unmount",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                "[c] Open VS Code",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "  [m] Mount Dev Container",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )]));
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Infra as Code ")
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}