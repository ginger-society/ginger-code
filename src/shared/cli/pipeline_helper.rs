// src/shared/cli/pipeline_helper.rs
//
// Git-ref resolution for `ginger-code pipeline <ref>`. Mirrors the
// conventions in push_helper.rs: `tokio::process::Command` (not
// `std::process`, since this whole CLI runs under `#[tokio::main]` and a
// blocking `std::process::Command::output()` call would stall the
// runtime), `Box<dyn std::error::Error>` for fallible git calls.

use tokio::process::Command;

/// Resolve `<ref>` (e.g. `HEAD`, `HEAD~1`, a branch name, a full/partial
/// SHA) to an 8-character short SHA — forced to exactly 8 chars via
/// `--short=8`, NOT git's own bare `--short` (which defaults to 7 and can
/// grow longer on big repos to stay unique) — since the CI pipeline's
/// `ginger-gitter/sha` label is always written at 8 chars, and a 7-char
/// value would silently never match.
pub async fn resolve_sha(git_ref: &str) -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=8", git_ref])
        .output()
        .await?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("could not resolve git ref '{git_ref}': {stderr}").into());
    }

    Ok(std::str::from_utf8(&out.stdout)?.trim().to_string())
}

/// Repo name = basename of the git repo ROOT (`git rev-parse
/// --show-toplevel`), not just the current working directory — so this
/// still resolves correctly when the command is run from a subdirectory
/// of the repo, matching how the CI side names things off the repo root.
pub async fn resolve_repo_name() -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("not inside a git repository: {stderr}").into());
    }

    let toplevel = std::str::from_utf8(&out.stdout)?.trim().to_string();
    let repo_name = std::path::Path::new(&toplevel)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("could not determine repo folder name from git toplevel path")?
        .to_string();

    Ok(repo_name)
}

/// The namespace convention used by ginger-gitter's Tekton triggers:
/// `tasks-{repo-name}`. Centralized here (rather than inlined at each
/// call site) so if this prefix ever changes, there's exactly one place
/// to update it.
pub fn namespace_for_repo(repo_name: &str) -> String {
    format!("tasks-{repo_name}")
}