//! Infra-as-Code detail panel for the GUI.

use eframe::egui;

use crate::shared::core::types::InfraAsCode;
use super::super::colors::{
    COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_MUTED,
};

// ── Action returned to app.rs ─────────────────────────────────────────────────

pub struct IacDetailAction {
    pub mount_clicked:        bool,
    pub unmount_clicked:      bool,
    pub open_editor_clicked:  bool,
}

// ── Main draw function ────────────────────────────────────────────────────────

pub fn draw_iac_detail(
    iac:      &InfraAsCode,
    mounting: Option<&str>,
    ui:       &mut egui::Ui,
) -> IacDetailAction {
    let mut action = IacDetailAction {
        mount_clicked:       false,
        unmount_clicked:     false,
        open_editor_clicked: false,
    };

    egui::ScrollArea::vertical()
        .id_source("iac_detail_scroll")
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let pad = 20.0;
            ui.add_space(pad);

            // ── Header ────────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new("⚙  Infra as Code")
                        .font(egui::FontId::new(18.0, egui::FontFamily::Monospace))
                        .color(COLOR_CYAN),
                );
                ui.add_space(10.0);
                if iac.mounted {
                    ui.label(
                        egui::RichText::new("● mounted")
                            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            .color(egui::Color32::from_rgb(39, 201, 63)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("○ not mounted")
                            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            .color(COLOR_DIM),
                    );
                }
            });

            ui.add_space(12.0);
            separator(ui);
            ui.add_space(12.0);

            // ── Description ───────────────────────────────────────────────────
            ui.horizontal_wrapped(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new(
                        "This is an Infra as Code repo. There is no deployment as such — \
                         you can however mount it to make changes in this repo.",
                    )
                    .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                    .color(COLOR_MUTED),
                );
            });

            ui.add_space(12.0);

            ui.horizontal_wrapped(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new(
                        "kubectl is available on the mounted environment which can be \
                         used for debugging and testing.",
                    )
                    .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                    .color(COLOR_DIM),
                );
            });

            ui.add_space(12.0);

            // ── Slug row ──────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new("slug: ")
                        .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                        .color(COLOR_DIM),
                );
                ui.label(
                    egui::RichText::new("iac")
                        .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                        .color(egui::Color32::WHITE),
                );
            });

            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new("git remote: ")
                        .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                        .color(COLOR_DIM),
                );
                ui.label(
                    egui::RichText::new(format!("source:{}-iac.git", iac.organization_id))
                        .font(egui::FontId::new(10.5, egui::FontFamily::Monospace))
                        .color(COLOR_CYAN),
                );
            });

            ui.add_space(16.0);
            separator(ui);
            ui.add_space(16.0);

            // ── Action buttons ────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.add_space(pad);

                if let Some(msg) = mounting {
                    let t    = ui.input(|i| i.time);
                    let dots = match ((t * 2.0) as usize) % 4 { 0=>"", 1=>".", 2=>"..", _=>"..." };
                    ui.label(
                        egui::RichText::new(format!("{}{}", msg, dots))
                            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            .color(COLOR_CYAN),
                    );
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                } else if iac.mounted {
                    if ui.add(action_button("↩ Unmount", egui::Color32::from_rgb(130, 85, 10))).clicked() {
                        action.unmount_clicked = true;
                    }
                    ui.add_space(8.0);
                    if ui.add(action_button("[>] Open VS Code", egui::Color32::from_rgb(20, 75, 140))).clicked() {
                        action.open_editor_clicked = true;
                    }
                } else {
                    if ui.add(action_button("⬡ Mount Dev Container", egui::Color32::from_rgb(30, 100, 60))).clicked() {
                        action.mount_clicked = true;
                    }
                }
            });

            ui.add_space(24.0);
        });

    action
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        egui::Stroke::new(0.5, COLOR_BORDER),
    );
}

fn action_button(label: &str, bg: egui::Color32) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let desired  = egui::vec2(label.len() as f32 * 7.5 + 16.0, 24.0);
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
        let color = if response.hovered() { bg.linear_multiply(1.3) } else { bg };
        ui.painter().rect_filled(rect, 4.0, color);
        ui.painter().text(
            rect.center(), egui::Align2::CENTER_CENTER, label,
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            egui::Color32::WHITE,
        );
        response
    }
}