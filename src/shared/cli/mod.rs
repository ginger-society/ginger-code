pub mod colour;
pub mod config;
pub mod handlers;
pub mod session;
pub mod socket;

// Flatten the most-used surface for callers
pub use handlers::{handle_branch, print_deployments, print_status};
pub use session::check_session_guard;
pub use socket::{daemon_running, send};