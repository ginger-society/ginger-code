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
use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{Patch, PatchParams, PostParams};
use kube::{Api, Client};
use tokio::io::AsyncWriteExt as _;

use crate::shared::core::{
    daemon::{assert_daemon_reachable, daemon_register, daemon_remove},
    git_ops::{
        copy_ssh_keys_to_dev, delete_dev_ssh_keys, setup_repo_branch, write_pod_ssh_config,
    },
    image::{builder_image, meta_to_repo_name, supports_ssh},
    k8s_ops::{
        apply_pvc, get_deployment_annotation, is_workspace_empty, wait_for_pod_ready, wait_for_pod_scheduled, write_ssh_principal
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

/// Returns the current image of the named container inside a deployment.
///
/// Uses a filter expression so it works regardless of container order —
/// unlike the fragile `.spec.template.spec.containers[0].image` approach.
async fn get_container_image(
    deployment_name: &str,
    container_name:  &str,
) -> Option<String> {
    let path = format!(
        ".spec.template.spec.containers[?(@.name=='{}')].image",
        container_name
    );
    get_deployment_annotation(deployment_name, &path).await
}

/// Detects whether port 22 is already claimed by a container other than
/// `main_container`. If so, the dev container must use 2222 instead.
async fn resolve_ssh_port(deployment_name: &str, main_container: &str) -> u16 {
    let port_22_in_use = get_deployment_annotation(
        deployment_name,
        ".spec.template.spec.containers[?(@.ports[0].containerPort==22)].name",
    )
    .await
    .map(|name| name != main_container)
    .unwrap_or(false);

    if port_22_in_use {
        println!("  Port 22 already claimed by another container — using 2222 for dev SSH");
        2222
    } else {
        22
    }
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
    let main_container = resolve_main_container(deployment_name).await;
    println!("  Main container: {}", main_container);

    // Read the image of the specific container being ejected, not containers[0]
    let original_image = get_container_image(deployment_name, &main_container)
        .await
        .ok_or_else(|| format!(
            "Could not read image for container '{}' in deployment '{}'",
            main_container, deployment_name
        ))?;
    println!("  Original image: {}", original_image);

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

    // Detect SSH port — use 2222 if port 22 is already taken by another container
    // (e.g. gitolite running alongside ginger-gitter-service).
    // The dev container image reads SSH_PORT env var: `sshd -D -e -p ${SSH_PORT:-22}`
    let ssh_port = resolve_ssh_port(deployment_name, &main_container).await;
    println!("  SSH port: {}", ssh_port);

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
        container["ports"] = serde_json::json!([{ "containerPort": ssh_port }]);
        // SSH_PORT tells the entrypoint which port sshd should bind to.
        // Defaults to 22 in the image; only set explicitly when using 2222.
        container["env"] = serde_json::json!([{
            "name":  "SSH_PORT",
            "value": ssh_port.to_string(),
        }]);
    }

    // ── Patch deployment ──────────────────────────────────────────────────────
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        "true",
                "ginger-original-image": original_image,
                "ginger-branch":         branch,
                // Store resolved values so uneject is reliable even if the
                // Pod-template annotations change in the meantime.
                "ginger-main-container": main_container,
                "ginger-ssh-port":       ssh_port.to_string(),
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

    let client = Client::try_default().await.expect("kube client");
    let api: Api<Deployment> = Api::default_namespaced(client);

    api.patch(
        deployment_name,
        &PatchParams::default(),
        &Patch::Strategic(patch),
    )
    .await
    .map_err(|e| format!("patch failed for {}: {}", deployment_name, e))?;

    println!("✓ Patched {} → {} (branch: {})", deployment_name, image, branch);

    if ssh {
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("\n⏸  Sleeping 5s to allow rollout to stabilize...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let final_pod = wait_for_pod_scheduled(deployment_name).await?;

        wait_for_pod_ready(&final_pod).await?;
        
        println!("✓ Pod ready: {}", final_pod);

        write_ssh_principal(&final_pod, &main_container, &session_user).await?;

        println!("⏳ Writing pod SSH config for git push...");
        if let Err(e) = write_pod_ssh_config(&final_pod, &main_container).await {
            eprintln!("Warning: {e}");
        }

        println!("⏳ Checking workspace and branch...");
        let workspace_empty = is_workspace_empty(&final_pod, &main_container)
            .await
            .unwrap_or(true);

        if workspace_empty {
            println!("⏳ Copying SSH keys into pod for initial clone...");
            match copy_ssh_keys_to_dev(&final_pod, &main_container).await {
                Err(e) => eprintln!("Warning: could not copy SSH keys into pod: {e}"),
                Ok(()) => {
                    setup_repo_branch(
                        &final_pod, &main_container,
                        &git_repo,
                        &git_repo,
                        &branch,
                    ).await?;
                    if let Err(e) = delete_dev_ssh_keys(&final_pod, &main_container).await {
                        eprintln!("Warning: {e}");
                    }
                }
            }
        } else {
            println!("  /workspace is not empty — checking branch...");
            setup_repo_branch(
                &final_pod, &main_container,
                &git_repo,
                &git_repo,
                &branch,
            ).await?;
        }

        if let Err(e) = add_source_ssh_config() {
            eprintln!("Warning: could not add source host to local ~/.ssh/config: {e}");
        }

        let forwarding_port = find_free_22xx_port()?;

        if let Err(e) = daemon_register(deployment_name, ssh_port, forwarding_port, organization_id) {
            eprintln!(
                "Warning: {e}\n\
                Register manually:\n  \
                ginger-code register --deployment-name {} --deployment-port {} \
                --forwarding-port {}",
                deployment_name, ssh_port, forwarding_port
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
    // Note: `ports` is intentionally omitted so k8s retains the original
    // port definitions from the deployment spec rather than clearing them.
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        null,
                "ginger-original-image": null,
                "ginger-branch":         null,
                "ginger-main-container": null,
                "ginger-ssh-port":       null,
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
                    }],
                    "volumes": []
                }
            }
        }
    });

    let client = Client::try_default().await.expect("kube client");
    let api: Api<Deployment> = Api::default_namespaced(client);

    api.patch(
        deployment_name,
        &PatchParams::default(),
        &Patch::Strategic(patch),
    )
    .await
    .map_err(|e| format!("patch failed for {}: {}", deployment_name, e))?;

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