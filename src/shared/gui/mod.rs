pub mod app;
pub mod colors;
pub mod panels;
pub mod terminal;
pub mod types;

use eframe::egui;
use app::App;

const ICON_BYTES: &[u8] = include_bytes!("../../../assets/ginger-code.png");

pub fn run_gui() -> eframe::Result<()> {
    let icon = {
        let img = ::image::load_from_memory(ICON_BYTES)  // :: prefix = crate root
            .expect("Failed to load icon")
            .into_rgba8();
        let (w, h) = img.dimensions();
        egui::viewport::IconData {
            rgba:   img.into_raw(),
            width:  w,
            height: h,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ginger Code")
            .with_icon(std::sync::Arc::new(icon))
            .with_inner_size([1100.0, 680.0])
            .with_min_inner_size([600.0, 400.0])
            .with_decorations(false)
            .with_transparent(false)
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "Ginger Code",
        options,
        Box::new(|cc| {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            Box::new(App::new(cc))
        }),
    )
}