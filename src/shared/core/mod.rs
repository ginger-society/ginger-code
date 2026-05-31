//! Shared helpers used by both `eject` and `mount` / `unmount` operations.
//!
//! Sub-modules:
//! * [`daemon`]     — Unix-socket communication with the ginger-code daemon.
//! * [`git_ops`]    — Pod-side SSH-key management, git clone, branch checkout.
//! * [`image`]      — Language → builder-image mapping and slug helpers.
//! * [`k8s_ops`]    — Low-level kubectl / pod helpers (PVCs, deployments, pods).
//! * [`port`]       — Free-port discovery in the 2200–2299 range.
//! * [`ssh_config`] — `~/.ssh/config` block management (add / remove / source).

pub mod daemon;
pub mod eject;
pub mod git_ops;
pub mod image;
pub mod k8s_ops;
pub mod mount;
pub mod port;
pub mod ssh_config;
pub mod types;
pub mod k8_info;

// Re-export functions with unambiguous names.
// Can't use `pub use mount::mount` because `mount` the module shadows it.
pub use eject::eject;
pub use eject::uneject;
pub use mount::mount;
pub use mount::unmount;