pub mod app;
pub mod colors;
pub mod panels;
pub mod terminal;
pub mod types;

use eframe::egui;

use types::{AppState, RightPane, TermState};
use app::App;

/// Launch the egui window. Called from your tray icon or CLI.
/// The window will request focus / come to front on open.
pub fn run_gui() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GingerKube")
            .with_inner_size([1100.0, 680.0])
            .with_min_inner_size([600.0, 400.0])
            .with_decorations(false)
            .with_transparent(false)
            // Raise window above everything when first shown
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "GingerKube",
        options,
        Box::new(|cc| {
            // Request focus on the very first frame so the window comes
            // forward even when launched from a background tray process.
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            Box::new(App::new(cc))
        }),
    )
}