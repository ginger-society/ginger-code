//! Detail panel rendered in the central area when a package is selected.

use eframe::egui;

use super::super::colors::{COLOR_BORDER, COLOR_CYAN, COLOR_DIM, COLOR_FG, COLOR_MAGENTA, COLOR_MUTED};
use super::super::types::Package;

// ── Action returned to app.rs ─────────────────────────────────────────────────

pub struct PackageDetailAction {
    pub mount_clicked:       bool,
    pub unmount_clicked:     bool,
    pub open_editor_clicked: bool,
}

// ── Main draw function ────────────────────────────────────────────────────────

/// Draw the full detail view for `pkg`.
/// `mounting` is `Some("…msg…")` while a mount/unmount op is in flight.
pub fn draw_package_detail(
    pkg:      &Package,
    mounting: Option<&str>,
    ui:       &mut egui::Ui,
) -> PackageDetailAction {
    let mut action = PackageDetailAction {
        mount_clicked:       false,
        unmount_clicked:     false,
        open_editor_clicked: false,
    };

    egui::ScrollArea::vertical()
        .id_source("pkg_detail_scroll")
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let pad = 20.0;
            ui.add_space(pad);

            // ── Header: identifier + type badge ───────────────────────────────
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new(&pkg.identifier)
                        .font(egui::FontId::new(18.0, egui::FontFamily::Monospace))
                        .color(egui::Color32::WHITE),
                );
                ui.add_space(8.0);
                let badge_color = match pkg.package_type.as_str() {
                    "lib" | "library"    => COLOR_CYAN,
                    "bin" | "executable" => COLOR_MAGENTA,
                    _                    => COLOR_DIM,
                };
                ui.label(
                    egui::RichText::new(format!("[{}]", pkg.package_type))
                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                        .color(badge_color),
                );
            });

            ui.add_space(4.0);

            // ── Lang ──────────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.add_space(pad);
                kv_inline(ui, "lang", &pkg.lang);
            });

            ui.add_space(12.0);
            separator(ui);
            ui.add_space(12.0);

            // ── Description ───────────────────────────────────────────────────
            if !pkg.description.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.label(
                        egui::RichText::new(&pkg.description)
                            .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                            .color(COLOR_MUTED),
                    );
                });
                ui.add_space(12.0);
            }

            // ── Dependencies ──────────────────────────────────────────────────
            if !pkg.dependencies.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.label(
                        egui::RichText::new("dependencies")
                            .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
                            .color(COLOR_DIM),
                    );
                });
                ui.add_space(4.0);
                for dep in &pkg.dependencies {
                    ui.horizontal(|ui| {
                        ui.add_space(pad + 8.0);
                        ui.label(
                            egui::RichText::new(format!("· {}", dep))
                                .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                .color(COLOR_MUTED),
                        );
                    });
                }
                ui.add_space(12.0);
            }

            ui.add_space(8.0);
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
                } else if pkg.mounted {
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

fn kv_inline(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(
        egui::RichText::new(key)
            .font(egui::FontId::new(10.0, egui::FontFamily::Monospace))
            .color(COLOR_DIM),
    );
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(value)
            .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
            .color(COLOR_FG),
    );
}

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