//! Dev-container mount / unmount operations for packages.
//!
//! Mirrors the shape of `eject.rs` / `uneject.rs` so the calling pattern in
//! `bg.rs` is identical.  Fill in the real implementation once the underlying
//! tooling is ready; the stubs below compile and let the rest of the UI work
//! end-to-end today.

/// Mount a dev container for `pkg_identifier`.
///
/// On success returns `Ok(())`; on failure returns a human-readable error.
pub async fn mount(org_id: &str, pkg_identifier: &str, lang: &str) -> Result<(), String> {
    // TODO: invoke the real mount command, e.g.:
    //   ginger-connector mount --org <org_id> --pkg <pkg_identifier> --lang <lang>
    println!("[mount] org={org_id}  pkg={pkg_identifier}  lang={lang}");
    Ok(())
}

/// Unmount the dev container for `pkg_identifier`.
///
/// On success returns `Ok(())`; on failure returns a human-readable error.
pub async fn unmount(org_id: &str, pkg_identifier: &str) -> Result<(), String> {
    // TODO: invoke the real unmount command, e.g.:
    //   ginger-connector unmount --org <org_id> --pkg <pkg_identifier>
    println!("[unmount] org={org_id}  pkg={pkg_identifier}");
    Ok(())
}