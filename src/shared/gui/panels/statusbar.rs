use eframe::egui;
use super::super::colors::{COLOR_BORDER, COLOR_DIM, COLOR_RED, COLOR_TAB_ACTIVE};
use super::super::types::{AppState, RightPane, TermState};

pub fn draw_statusbar(state: &AppState, ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(18, 18, 18));
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );

    let (text, color) = if let RightPane::TerminalTab(i) = state.right_pane {
        if let Some(tab) = state.term_tabs.get(i) {
            match &tab.state {
                TermState::Idle         => ("  ○ Ready".to_string(), COLOR_DIM),
                TermState::Connected(_) => (
                    format!("  ● {}  {}×{}", tab.label, tab.term_cols, tab.term_rows),
                    COLOR_TAB_ACTIVE,
                ),
                TermState::Error(e) => (format!("  ✗ {}", e), COLOR_RED),
            }
        } else {
            ("".to_string(), COLOR_DIM)
        }
    } else {
        ("  ○ Ready".to_string(), COLOR_DIM)
    };

    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.0),
        color,
    );
}