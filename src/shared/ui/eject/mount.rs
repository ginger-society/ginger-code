//! Dev-container mount / unmount operations for packages.
//!
//! A "mounted" package gets:
//!   * a fresh k8s `Deployment` running the appropriate builder image
//!   * a `PersistentVolumeClaim` for the `/workspace` volume
//!   * the repo cloned into `/workspace/<slug>` on the active branch
//!   * a daemon-managed port-forward on a free port in 2200–2299
//!   * an SSH alias block in `~/.ssh/config`
//!
//! "Unmounting" tears all of that down in reverse.
//!
//! The deployment name / PVC slug is derived solely from the package identifier
//! (the last path segment, lowercased) — org_id is *not* included because all
//! packages are assumed to live in a single organisation.

use std::fs;

use tokio::io::AsyncWriteExt as _;

use crate::shared::ui::eject::{
    daemon::{assert_daemon_reachable, daemon_register, daemon_remove},
    git_ops::{
        copy_ssh_keys_to_dev, delete_dev_ssh_keys, setup_repo_branch, write_pod_ssh_config,
    },
    image::{builder_image, pkg_to_slug, supports_ssh},
    k8s_ops::{
        apply_pvc, delete_deployment, delete_pvc, is_workspace_empty,
        wait_for_pod_scheduled, write_ssh_principal,
    },
    port::find_free_22xx_port,
    ssh_config::{add_source_ssh_config, add_ssh_config, remove_ssh_config},
};

// ── Name helpers ──────────────────────────────────────────────────────────────

/// Kubernetes deployment name for a package.
///
/// e.g. `"@ginger-society/IAMService"` → `"iamservice"`
fn deployment_name(pkg_identifier: &str) -> String {
    pkg_to_slug(pkg_identifier)
}

/// Workspace PVC name for a package.
fn workspace_pvc_name(slug: &str) -> String {
    format!("{}-mount-pvc", slug)
}

/// SSH principals PVC name for a package.
fn principals_pvc_name(slug: &str) -> String {
    format!("{}-mount-ssh-principals-pvc", slug)
}

// ── Active branch (same source as eject) ─────────────────────────────────────

fn active_branch() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".ginger-society").join("code.toml");
    let raw  = fs::read_to_string(&path).ok()?;
    let val: toml::Value = toml::from_str(&raw).ok()?;
    val.get("active_branch")?.as_str().map(|s| s.to_string())
}

// ── Session user ──────────────────────────────────────────────────────────────

fn session_user() -> Result<String, Box<dyn std::error::Error>> {
    let user_file = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ginger-society")
        .join("user.json");

    let raw: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&user_file).map_err(|e| {
            format!("Could not read {}: {}", user_file.display(), e)
        })?)?;

    raw["sub"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "sub missing or not a string in user.json".into())
}

// ── Mount ─────────────────────────────────────────────────────────────────────

/// Mount a dev container for `pkg_identifier`.
///
/// Steps (mirrors `eject`):
/// 1. Verify the daemon is reachable.
/// 2. Create workspace + SSH-principals PVCs.
/// 3. Create a new `Deployment` running the builder image.
/// 4. Wait for the pod, write the SSH principal, clone / checkout the repo.
/// 5. Register the port-forward with the daemon and update `~/.ssh/config`.
pub async fn mount(
    org_id:         &str,
    pkg_identifier: &str,
    lang:           &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_daemon_reachable()?;

    let branch = active_branch()
        .ok_or("No active branch set — run `ginger-code -b <branch>` first")?;
    println!("  Active branch: {}", branch);

    let user      = session_user()?;
    let slug      = deployment_name(pkg_identifier);
    let image     = builder_image(lang)?;
    let ssh       = supports_ssh(lang);

    // The repo name on the git server is the same slug (last segment of the
    // identifier, lowercased), consistent with the eject convention.
    let repo_name = slug.clone();

    let ws_pvc   = workspace_pvc_name(&slug);
    let prin_pvc = principals_pvc_name(&slug);

    // ── PVCs ──────────────────────────────────────────────────────────────────
    apply_pvc(&ws_pvc, "4Gi").await?;
    println!("✓ Workspace PVC ready: {}", ws_pvc);

    apply_pvc(&prin_pvc, "16Mi").await?;
    println!("✓ SSH principals PVC ready: {}", prin_pvc);

    // ── Deployment ────────────────────────────────────────────────────────────
    let command = if ssh {
        serde_json::json!(["/entrypoint.sh"])
    } else {
        serde_json::json!(["sleep", "infinity"])
    };

    let mut container_spec = serde_json::json!({
        "name":    slug,
        "image":   image,
        "command": command,
        "volumeMounts": [
            { "name": "workspace",      "mountPath": "/workspace" },
            { "name": "ssh-principals", "mountPath": "/etc/ssh/auth_principals" },
        ]
    });
    if ssh {
        container_spec["ports"] = serde_json::json!([{ "containerPort": 22 }]);
    }

    let deployment_manifest = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind":       "Deployment",
        "metadata": {
            "name": slug,
            "labels": { "app": slug },
            "annotations": {
                "ginger-mounted":     "true",
                "ginger-pkg":         pkg_identifier,
                "ginger-branch":      branch,
                "ginger-org":         org_id,
            }
        },
        "spec": {
            "replicas": 1,
            "selector": { "matchLabels": { "app": slug } },
            "template": {
                "metadata": { "labels": { "app": slug } },
                "spec": {
                    "containers": [container_spec],
                    "volumes": [
                        { "name": "workspace",      "persistentVolumeClaim": { "claimName": ws_pvc } },
                        { "name": "ssh-principals", "persistentVolumeClaim": { "claimName": prin_pvc } },
                    ]
                }
            }
        }
    });

    let mut apply = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = apply.stdin.take() {
        stdin
            .write_all(serde_json::to_string_pretty(&deployment_manifest)?.as_bytes())
            .await?;
    }
    let apply_status = apply.wait().await?;
    if !apply_status.success() {
        return Err(format!("kubectl apply failed for deployment '{}'", slug).into());
    }
    println!("✓ Deployment '{}' created with image {}", slug, image);

    // ── Wait for pod + SSH setup (SSH-capable images only) ───────────────────
    if ssh {
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(&slug).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("\n⏸  Sleeping 5s to allow rollout to stabilize...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let final_pod = wait_for_pod_scheduled(&slug).await?;

        tokio::process::Command::new("kubectl")
            .args([
                "wait",
                &format!("pod/{}", final_pod),
                "--for=condition=Ready",
                "--timeout=300s",
            ])
            .status()
            .await?;
        println!("✓ Pod ready: {}", final_pod);

        write_ssh_principal(&final_pod, &slug, &user).await?;

        println!("⏳ Writing pod SSH config for git push...");
        if let Err(e) = write_pod_ssh_config(&final_pod, &slug).await {
            eprintln!("Warning: {e}");
        }

        println!("⏳ Checking workspace and cloning if needed...");
        let workspace_empty = is_workspace_empty(&final_pod, &slug).await.unwrap_or(true);

        if workspace_empty {
            println!("⏳ Copying SSH keys into pod for initial clone...");
            match copy_ssh_keys_to_dev(&final_pod, &slug).await {
                Err(e) => eprintln!("Warning: could not copy SSH keys into pod: {e}"),
                Ok(()) => {
                    setup_repo_branch(&final_pod, &slug, &repo_name, &branch).await?;
                    if let Err(e) = delete_dev_ssh_keys(&final_pod, &slug).await {
                        eprintln!("Warning: {e}");
                    }
                }
            }
        } else {
            println!("  /workspace is not empty — checking branch...");
            setup_repo_branch(&final_pod, &slug, &repo_name, &branch).await?;
        }

        // ── Local SSH config ──────────────────────────────────────────────────
        if let Err(e) = add_source_ssh_config() {
            eprintln!("Warning: could not add source host to local ~/.ssh/config: {e}");
        }

        let forwarding_port = find_free_22xx_port()?;

        if let Err(e) = daemon_register(&slug, 22, forwarding_port, org_id) {
            eprintln!(
                "Warning: {e}\n\
                 Register manually:\n  \
                 ginger-code register --deployment-name {slug} \
                 --deployment-port 22 --forwarding-port {forwarding_port}"
            );
        }

        if let Err(e) = add_ssh_config(&slug, forwarding_port) {
            eprintln!("Warning: could not update local ~/.ssh/config: {e}");
        }

        println!("\nConnect with:  ssh {slug}-local");
        println!("Then push with: git push  (agent forwarding carries your key)");
    }

    println!("\n⏸  Sleeping 2s...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    Ok(())
}

// ── Unmount ───────────────────────────────────────────────────────────────────

/// Unmount the dev container for `pkg_identifier`.
///
/// Steps (mirrors `uneject`):
/// 1. Delete the `Deployment`.
/// 2. Delete both PVCs.
/// 3. Notify the daemon to stop port-forwarding.
/// 4. Remove the SSH alias block from `~/.ssh/config`.
pub async fn unmount(
    _org_id:        &str,
    pkg_identifier: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Daemon connectivity is best-effort for unmount: we still clean up locally
    // even if the daemon is unreachable.
    let slug     = deployment_name(pkg_identifier);
    let ws_pvc   = workspace_pvc_name(&slug);
    let prin_pvc = principals_pvc_name(&slug);

    // Delete the deployment first so no new pods are created while PVCs are
    // still bound.
    delete_deployment(&slug).await?;

    // Give the pod a moment to terminate before releasing the PVCs so we don't
    // hit a "volume in use" error.
    println!("⏸  Waiting 5s for pod termination before deleting PVCs...");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    delete_pvc(&ws_pvc).await?;
    delete_pvc(&prin_pvc).await?;

    // Notify the daemon (non-fatal)
    daemon_remove(&slug);

    if let Err(e) = remove_ssh_config(&slug) {
        eprintln!("Warning: could not clean up local ~/.ssh/config: {e}");
    }

    println!("✓ Unmounted dev container for '{}'", pkg_identifier);
    Ok(())
}