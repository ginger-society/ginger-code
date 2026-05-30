//! Pure-UI drawing functions split into focused sub-modules.
//! Each function returns data for decisions rather than mutating state
//! directly, keeping business logic in `app.rs`.

mod infostrip;
mod logspane;
pub mod sidebar;
mod statusbar;
mod tabbar;
mod terminalpane;
mod titlebar;

// Re-export every public surface so callers use `panels::*` as before.
pub use infostrip::{draw_info_strip, InfoStripAction};
pub use logspane::draw_logs_pane;
pub use sidebar::draw_service_list;
pub use statusbar::draw_statusbar;
pub use tabbar::{draw_tab_bar, TabBarAction};
pub use terminalpane::draw_terminal_pane;
pub use titlebar::draw_titlebar;