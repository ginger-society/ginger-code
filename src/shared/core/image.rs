//! Language-to-builder-image mapping and related predicates.

/// Returns the builder container image for the given language tag.
pub fn builder_image(lang: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    match lang {
        "TS"   => Ok("gingersociety/dev-container-node:5"),
        "Rust" => Ok("gingersociety/dev-container-rust:7"),
        other  => Err(format!("builder image not yet defined for lang: {}", other).into()),
    }
}

/// Returns `true` if the builder image for `lang` includes an SSH daemon and
/// therefore supports direct SSH port-forwarding.
pub fn supports_ssh(lang: &str) -> bool {
    matches!(lang, "TS" | "Rust")
}

/// Convert any package/service identifier into the gitolite repo name.
///
/// The gitolite server stores repos with the org prefix, using `-` as
/// separator.  Both eject and mount use this for the clone URL.
///
/// Examples:
/// * `"@ginger-society/dev-portal"` → `"ginger-society-dev-portal"`
/// * `"@ginger-society/IAMService"` → `"ginger-society-iamservice"`
pub fn meta_to_repo_name(org_id: &str, meta_name: &str) -> String {
    format!("{}-{}", org_id, meta_name.to_lowercase())
}

/// Derive a k8s deployment / PVC slug from a package identifier.
///
/// Strips the org scope and lowercases — used for k8s resource names only,
/// NOT for the gitolite clone URL (use [`meta_to_repo_name`] for that).
///
/// Examples:
/// * `"@ginger-society/IAMService"` → `"iamservice"`
/// * `"my-cool-lib"`               → `"my-cool-lib"`
pub fn pkg_to_slug(pkg_identifier: &str) -> String {
    pkg_identifier
        .split('/')
        .last()
        .unwrap_or(pkg_identifier)
        .to_lowercase()
}