use eframe::egui;

pub const COLOR_BG:          egui::Color32 = egui::Color32::from_rgb(15,  15,  15);
pub const COLOR_FG:          egui::Color32 = egui::Color32::from_rgb(0,   255, 65);
pub const COLOR_CURSOR:      egui::Color32 = egui::Color32::from_rgb(0,   255, 65);
pub const COLOR_PANEL_BG:    egui::Color32 = egui::Color32::from_rgb(20,  20,  20);
pub const COLOR_SIDEBAR_BG:  egui::Color32 = egui::Color32::from_rgb(18,  18,  18);
pub const COLOR_TAB_ACTIVE:  egui::Color32 = egui::Color32::from_rgb(0,   200, 50);
pub const COLOR_TAB_INACTIVE:egui::Color32 = egui::Color32::from_rgb(90,  90,  90);
pub const COLOR_TAB_BAR:     egui::Color32 = egui::Color32::from_rgb(25,  25,  25);
pub const COLOR_SELECTED_BG: egui::Color32 = egui::Color32::from_rgb(30,  50,  30);
pub const COLOR_BORDER:      egui::Color32 = egui::Color32::from_rgb(50,  50,  50);
pub const COLOR_DIM:         egui::Color32 = egui::Color32::from_rgb(80,  80,  80);
pub const COLOR_MUTED:       egui::Color32 = egui::Color32::from_rgb(120, 120, 120);
pub const COLOR_CYAN:        egui::Color32 = egui::Color32::from_rgb(80,  180, 220);
pub const COLOR_MAGENTA:     egui::Color32 = egui::Color32::from_rgb(200, 80,  200);
pub const COLOR_YELLOW:      egui::Color32 = egui::Color32::from_rgb(230, 180, 40);
pub const COLOR_RED:         egui::Color32 = egui::Color32::from_rgb(220, 70,  70);

/// Standard 16-colour ANSI palette
pub const ANSI_COLORS: [egui::Color32; 16] = [
    egui::Color32::from_rgb(0,   0,   0),
    egui::Color32::from_rgb(194, 54,  33),
    egui::Color32::from_rgb(37,  188, 36),
    egui::Color32::from_rgb(173, 173, 39),
    egui::Color32::from_rgb(73,  46,  225),
    egui::Color32::from_rgb(211, 56,  211),
    egui::Color32::from_rgb(51,  187, 200),
    egui::Color32::from_rgb(203, 204, 205),
    egui::Color32::from_rgb(129, 131, 131),
    egui::Color32::from_rgb(252, 57,  31),
    egui::Color32::from_rgb(49,  231, 34),
    egui::Color32::from_rgb(234, 236, 35),
    egui::Color32::from_rgb(88,  51,  255),
    egui::Color32::from_rgb(249, 53,  248),
    egui::Color32::from_rgb(20,  240, 240),
    egui::Color32::from_rgb(233, 235, 235),
];

/// Resolve an xterm-256 colour index to `Color32`.
pub fn ansi256(idx: u8) -> egui::Color32 {
    match idx {
        0..=15  => ANSI_COLORS[idx as usize],
        16..=231 => {
            let v = idx - 16;
            let b = (v % 6) * 51;
            let g = ((v / 6) % 6) * 51;
            let r = (v / 36) * 51;
            egui::Color32::from_rgb(r, g, b)
        }
        232..=255 => {
            let gray = (idx - 232) * 10 + 8;
            egui::Color32::from_rgb(gray, gray, gray)
        }
    }
}