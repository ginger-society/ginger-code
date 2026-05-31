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

/// Convert a metadata package identifier such as `"@ginger-society/dev-portal"`
/// into a k8s-safe name such as `"ginger-society-dev-portal"`.
pub fn meta_to_repo_name(meta_name: &str) -> String {
    meta_name.trim_start_matches('@').replace('/', "-")
}

/// Derive a k8s deployment / PVC slug from a bare package identifier.
///
/// Rules (no org prefix — all packages are assumed to share a single org):
/// * Strip a leading `@scope/` if present, keep only the final segment.
/// * Lowercase the result.
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