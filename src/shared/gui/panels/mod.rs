//! Pure-UI drawing functions split into focused sub-modules.
//! Each function returns data for decisions rather than mutating state
//! directly, keeping business logic in `app.rs`.

mod infostrip;
mod logspane;
pub mod packagedetail;
pub mod sidebar;
mod tabbar;
mod terminalpane;
mod titlebar;
pub mod dbschemadetail;
mod log_highlight;

pub use infostrip::{draw_info_strip, InfoStripAction};
pub use logspane::draw_logs_pane;
pub use packagedetail::{draw_package_detail, PackageDetailAction};
pub use sidebar::draw_service_list;
pub use terminalpane::draw_terminal_pane;
pub use titlebar::draw_titlebar;
pub use dbschemadetail::draw_db_schema_detail;
pub use tabbar::{draw_tab_bar, TabBarAction};

