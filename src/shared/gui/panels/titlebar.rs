use eframe::egui;
use super::super::types::AppState;

pub fn draw_titlebar(state: &AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 28.0),
        egui::Sense::click_and_drag(),
    );
    if response.dragged() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    let dot_y      = rect.center().y;
    let dot_colors = [
        egui::Color32::from_rgb(255, 95,  86),
        egui::Color32::from_rgb(255, 189, 46),
        egui::Color32::from_rgb(39,  201, 63),
    ];

    // Allocate click zones before painting (painter borrows ui).
    let dot_data: Vec<(egui::Pos2, bool)> = dot_colors.iter().enumerate().map(|(i, _)| {
        let center   = egui::pos2(rect.min.x + 14.0 + i as f32 * 20.0, dot_y);
        let dot_rect = egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0));
        let clicked  = ui.allocate_rect(dot_rect, egui::Sense::click()).clicked();
        (center, clicked)
    }).collect();

    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(30, 30, 30));
    for ((center, _), color) in dot_data.iter().zip(dot_colors.iter()) {
        painter.circle_filled(*center, 6.0, *color);
    }

    let title = state.services
        .get(state.selected_idx)
        .map(|s| format!("Ginger Code  —  {}", s.meta_name))
        .unwrap_or_else(|| "Ginger Code".to_string());
    painter.text(
        rect.center(), egui::Align2::CENTER_CENTER, title,
        egui::FontId::proportional(12.0), egui::Color32::from_rgb(160, 160, 160),
    );
    drop(painter);

    for (i, (_, clicked)) in dot_data.iter().enumerate() {
        if *clicked {
            match i {
                0 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                1 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                2 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true)),
                _ => {}
            }
        }
    }
}