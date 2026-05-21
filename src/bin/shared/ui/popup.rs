use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear as RatatuiClear, Paragraph},
};

use crate::shared::ui::types::{Popup, PopupAction};


pub fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = r.x + (r.width.saturating_sub(popup_width)) / 2;
    let y = r.y + (r.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: popup_width.min(r.width),
        height: height.min(r.height),
    }
}

pub fn render_popup(f: &mut ratatui::Frame, popup: &Popup, area: Rect) {
    let popup_area = centered_rect(55, 7, area);
    f.render_widget(RatatuiClear, popup_area);

    let (action_label, action_color) = match popup.action {
        PopupAction::Eject => ("EJECT", Color::Magenta),
        PopupAction::Uneject => ("UNEJECT", Color::Cyan),
        PopupAction::Quit => ("QUIT", Color::Red),
    };

    let yes_style = if popup.selected == 0 {
        Style::default()
            .fg(Color::Black)
            .bg(action_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let no_style = if popup.selected == 1 {
        Style::default()
            .fg(Color::Black)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let question = if popup.action == PopupAction::Quit {
        "Are you sure you want to quit?".to_string()
    } else {
        format!("{} {}?", action_label, popup.service_name)
    };

    let body = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(question, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("        "),
            Span::styled("  Yes  ", yes_style),
            Span::raw("   "),
            Span::styled("  No  ", no_style),
        ]),
        Line::from(""),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(action_color))
        .title(Span::styled(
            format!(" Confirm {} ", action_label),
            Style::default()
                .fg(action_color)
                .add_modifier(Modifier::BOLD),
        ));

    f.render_widget(Paragraph::new(body).block(block), popup_area);
}