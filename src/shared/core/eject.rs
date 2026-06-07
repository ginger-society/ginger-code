//! Eject / uneject operations for k8s services.
//!
//! All low-level helpers (daemon, git, SSH, kubectl) live in
//! `crate::shared::ui::eject::*` and are shared with `mount.rs`.
//!
//! # Main-container annotation
//!
//! To tell ginger-code which container in a multi-container pod is the "main"
//! one to eject/uneject, add this annotation to the **Pod template**:
//!
//! ```yaml
//! spec:
//!   template:
//!     metadata:
//!       annotations:
//!         x-ginger-code-ejectable: "<container-name>"
//! ```
//!
//! If the annotation is absent, `deployment_name` is used as the fallback,
//! which keeps single-container deployments working with zero config.

use std::fs;

use tokio::io::AsyncWriteExt as _;

use crate::shared::core::{
    daemon::{assert_daemon_reachable, daemon_register, daemon_remove},
    git_ops::{
        copy_ssh_keys_to_dev, delete_dev_ssh_keys, setup_repo_branch, write_pod_ssh_config,
    },
    image::{builder_image, meta_to_repo_name, supports_ssh},
    k8s_ops::{
        apply_pvc, get_deployment_annotation, is_workspace_empty, wait_for_pod_scheduled,
        write_ssh_principal,
    },
    port::find_free_22xx_port,
    ssh_config::{add_source_ssh_config, add_ssh_config, remove_ssh_config},
};

// ── Branch config helpers (eject-specific) ────────────────────────────────────

fn active_branch() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".ginger-society").join("code.toml");
    let raw  = fs::read_to_string(&path).ok()?;
    let val: toml::Value = toml::from_str(&raw).ok()?;
    val.get("active_branch")?.as_str().map(|s| s.to_string())
}

fn remove_from_branch_config(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let home     = dirs::home_dir().ok_or("Could not locate home directory")?;
    let cfg_path = home.join(".ginger-society").join("code.toml");

    let raw = fs::read_to_string(&cfg_path).unwrap_or_default();
    let cfg: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    let branch = match cfg.get("active_branch").and_then(|v| v.as_str()) {
        Some(b) => b.to_string(),
        None => {
            println!("  No active branch in code.toml, nothing to remove from branch config");
            return Ok(());
        }
    };

    let slug        = branch.replace('/', "-");
    let branch_path = home
        .join(".ginger-society")
        .join("branches")
        .join(format!("{}.toml", slug));

    if !branch_path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&branch_path)?;
    let mut bc: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    if let Some(deps) = bc.get_mut("deployments").and_then(|d| d.as_array_mut()) {
        deps.retain(|d| {
            d.get("deployment_name")
                .and_then(|v| v.as_str())
                .map_or(true, |n| n != deployment_name)
        });
    }

    fs::write(&branch_path, toml::to_string_pretty(&bc)?)?;
    println!("✓ Removed '{}' from branch config ({})", deployment_name, branch);
    Ok(())
}

// ── Main-container resolution ─────────────────────────────────────────────────

/// Returns the name of the container to eject/uneject.
///
/// Reads `x-ginger-code-ejectable` from the Pod template annotations using
/// kubectl jsonpath single-quote bracket syntax, which handles hyphenated keys.
/// Falls back to `deployment_name` if the annotation is absent, keeping
/// single-container deployments working with zero configuration.
async fn resolve_main_container(deployment_name: &str) -> String {
    get_deployment_annotation(
        deployment_name,
        // Single-quote bracket syntax is required by kubectl jsonpath
        // for annotation keys that contain hyphens.
        ".spec.template.metadata.annotations['x-ginger-code-ejectable']",
    )
    .await
    .unwrap_or_else(|| {
        println!(
            "  x-ginger-code-ejectable annotation not found — \
             falling back to container name '{}'",
            deployment_name
        );
        deployment_name.to_string()
    })
}

// ── Eject ─────────────────────────────────────────────────────────────────────

pub async fn eject(
    deployment_name: &str,
    lang:            &str,
    meta_name:       &str,
    organization_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_daemon_reachable()?;

    let branch = active_branch()
        .ok_or("No active branch set — run `ginger-code -b <branch>` first")?;
    println!("  Active branch: {}", branch);

    let user_file = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ginger-society")
        .join("user.json");

    let user_json    = fs::read_to_string(&user_file)
        .map_err(|e| format!("Could not read {}: {}", user_file.display(), e))?;
    let user_details: serde_json::Value = serde_json::from_str(&user_json)
        .map_err(|e| format!("Could not parse user.json: {}", e))?;
    let session_user = user_details["sub"]
        .as_str()
        .ok_or("sub missing or not a string in user.json")?
        .to_string();

    let image    = builder_image(lang)?;
    let ssh      = supports_ssh(lang);
    // git_repo: org-prefixed name used for the gitolite clone URL
    // e.g. "@ginger-society/dev-portal" → "ginger-society-dev-portal"
    let git_repo = meta_to_repo_name(organization_id, meta_name);
    // dir_name: same as deployment_name (already org-free slug from caller)
    // e.g. "dev-portal"
    let dir_name = deployment_name.to_string();

    // ── Resolve which container to eject ──────────────────────────────────────
    //
    // For single-container deployments with no annotation this falls back to
    // deployment_name — identical behaviour to the original containers[0] approach.
    // For multi-container pods, the annotation names the container to target.
    let main_container = resolve_main_container(deployment_name).await;
    println!("  Main container: {}", main_container);

    let original_image =
        get_deployment_annotation(deployment_name, ".spec.template.spec.containers[0].image")
            .await
            .ok_or("Could not read current image from deployment")?;

    // ── PVCs ──────────────────────────────────────────────────────────────────
    let pvc_name            = format!("{}-eject-pvc", deployment_name);
    let principals_pvc_name = format!("{}-ssh-principals-pvc", deployment_name);

    apply_pvc(&pvc_name, "4Gi").await?;
    apply_pvc(&principals_pvc_name, "16Mi").await?;
    println!("✓ SSH principals PVC ready: {}", principals_pvc_name);

    // ── Build the patched container spec ──────────────────────────────────────
    //
    // Strategic merge patch uses `name` as the merge key for `containers[]`,
    // so only the named container is touched — any sidecars are left unchanged.
    let command = if ssh {
        serde_json::json!(["/entrypoint.sh"])
    } else {
        serde_json::json!(["sleep", "infinity"])
    };

    let mut container = serde_json::json!({
        "name":    main_container,   // ← strategic merge key: only this container is patched
        "image":   image,
        "command": command,
        "volumeMounts": [
            { "name": "workspace",      "mountPath": "/workspace" },
            { "name": "ssh-principals", "mountPath": "/etc/ssh/auth_principals" },
        ]
    });
    if ssh {
        container["ports"] = serde_json::json!([{ "containerPort": 22 }]);
    }

    // ── Patch deployment ──────────────────────────────────────────────────────
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        "true",
                "ginger-original-image": original_image,
                "ginger-branch":         branch,
                // Store the resolved container name so uneject can find it
                // even if the Pod-template annotation changes in the meantime.
                "ginger-main-container": main_container,
            }
        },
        "spec": {
            "template": {
                "metadata": { "annotations": { "kubectl.kubernetes.io/restartedAt": null } },
                "spec": {
                    "containers": [container],
                    "volumes": [
                        { "name": "workspace",      "persistentVolumeClaim": { "claimName": pvc_name } },
                        { "name": "ssh-principals", "persistentVolumeClaim": { "claimName": principals_pvc_name } },
                    ]
                }
            }
        }
    });

    let status = tokio::process::Command::new("kubectl")
        .args([
            "patch", "deployment", deployment_name,
            "--type", "strategic",
            "-p", &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }
    println!("✓ Patched {} → {} (branch: {})", deployment_name, image, branch);

    if ssh {
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("\n⏸  Sleeping 5s to allow rollout to stabilize...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let final_pod = wait_for_pod_scheduled(deployment_name).await?;

        tokio::process::Command::new("kubectl")
            .args([
                "wait", &format!("pod/{}", final_pod),
                "--for=condition=Ready", "--timeout=300s",
            ])
            .status()
            .await?;
        println!("✓ Pod ready: {}", final_pod);

        write_ssh_principal(&final_pod, &main_container, &session_user).await?;

        println!("⏳ Writing pod SSH config for git push...");
        if let Err(e) = write_pod_ssh_config(&final_pod, deployment_name).await {
            eprintln!("Warning: {e}");
        }

        println!("⏳ Checking workspace and branch...");
        let workspace_empty = is_workspace_empty(&final_pod, &main_container)
            .await
            .unwrap_or(true);

        if workspace_empty {
            println!("⏳ Copying SSH keys into pod for initial clone...");
            match copy_ssh_keys_to_dev(&final_pod, deployment_name).await {
                Err(e) => eprintln!("Warning: could not copy SSH keys into pod: {e}"),
                Ok(()) => {
                    setup_repo_branch(
                        &final_pod, deployment_name,
                        &git_repo,
                        &git_repo,
                        &branch,
                    ).await?;
                    if let Err(e) = delete_dev_ssh_keys(&final_pod, deployment_name).await {
                        eprintln!("Warning: {e}");
                    }
                }
            }
        } else {
            println!("  /workspace is not empty — checking branch...");
            setup_repo_branch(
                &final_pod, deployment_name,
                &git_repo,
                &git_repo,
                &branch,
            ).await?;
        }

        if let Err(e) = add_source_ssh_config() {
            eprintln!("Warning: could not add source host to local ~/.ssh/config: {e}");
        }

        let forwarding_port = find_free_22xx_port()?;

        if let Err(e) = daemon_register(deployment_name, 22, forwarding_port, organization_id) {
            eprintln!(
                "Warning: {e}\n\
                Register manually:\n  \
                ginger-code register --deployment-name {} --deployment-port 22 \
                --forwarding-port {}",
                deployment_name, forwarding_port
            );
        }

        if let Err(e) = add_ssh_config(deployment_name, forwarding_port) {
            eprintln!("Warning: could not update local ~/.ssh/config: {e}");
        }

        println!("\nConnect with:  ssh {}-local", deployment_name);
        println!("Then push with: git push  (agent forwarding carries your key)");
    }

    println!("\n⏸  Sleeping 2s...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    Ok(())
}

// ── Uneject ───────────────────────────────────────────────────────────────────

pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    assert_daemon_reachable()?;

    let original_image =
        get_deployment_annotation(deployment_name, ".metadata.annotations['ginger-original-image']")
            .await
            .ok_or("No ginger-original-image annotation — was this deployment ejected?")?;

    // Recover which container was ejected from the Deployment-level annotation
    // stored at eject time — reliable even if the Pod-template annotation changes.
    let main_container =
        get_deployment_annotation(deployment_name, ".metadata.annotations['ginger-main-container']")
            .await
            .unwrap_or_else(|| {
                println!(
                    "  ginger-main-container annotation not found — \
                     falling back to deployment name '{}'",
                    deployment_name
                );
                deployment_name.to_string()
            });
    println!("  Restoring container: {}", main_container);

    // Strategic merge patch: only the named container is restored.
    // Sidecars are left completely unchanged.
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        null,
                "ginger-original-image": null,
                "ginger-branch":         null,
                "ginger-main-container": null,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name":         main_container,  // ← strategic merge key
                        "image":        original_image,
                        "command":      null,
                        "volumeMounts": [],
                        "ports":        [],
                    }],
                    "volumes": []
                }
            }
        }
    });

    let status = tokio::process::Command::new("kubectl")
        .args([
            "patch", "deployment", deployment_name,
            "--type", "strategic",
            "-p", &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }
    println!("✓ Unejected {} → restored {}", deployment_name, original_image);

    if let Err(e) = remove_from_branch_config(deployment_name) {
        eprintln!("Warning: could not update branch config: {e}");
    }

    daemon_remove(deployment_name);

    if let Err(e) = remove_ssh_config(deployment_name) {
        eprintln!("Warning: could not clean up local ~/.ssh/config: {e}");
    }

    Ok(())
}