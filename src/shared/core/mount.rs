//! Dev-container mount / unmount operations for packages.
//!
//! Two names are derived from `pkg_identifier`:
//!   * `slug`     — k8s resource names (last segment only, lowercased)
//!                  e.g. `"@ginger-society/IAMService"` → `"iamservice"`
//!   * `git_repo` — gitolite clone path (org-prefixed, lowercased)
//!                  e.g. `"@ginger-society/IAMService"` → `"ginger-society-iamservice"`
//!
//! The workspace directory inside the pod is the slug (no org prefix) so paths
//! stay short: `/workspace/iamservice`.  The clone URL uses the org-prefixed
//! name so gitolite can find the repo: `source:ginger-society-iamservice.git`.

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
    image::{builder_image, meta_to_repo_name, pkg_to_slug, supports_ssh},
    k8s_ops::{
        apply_pvc, delete_deployment, delete_pvc, is_workspace_empty,
        wait_for_pod_ready, wait_for_pod_scheduled, write_ssh_principal,
    },
    port::find_free_22xx_port,
    ssh_config::{add_source_ssh_config, add_ssh_config, remove_ssh_config},
};

// ── Name helpers ──────────────────────────────────────────────────────────────

/// k8s deployment / PVC slug — last segment only, no org prefix.
/// e.g. `"@ginger-society/IAMService"` → `"iamservice"`
fn deployment_slug(pkg_identifier: &str) -> String {
    pkg_to_slug(pkg_identifier)
}

/// Gitolite remote repo name — org-prefixed, lowercased.
/// e.g. `"@ginger-society/IAMService"` → `"ginger-society-iamservice"`
fn gitolite_repo(org_id: &str, pkg_identifier: &str) -> String {
    meta_to_repo_name(org_id, pkg_identifier)
}

fn workspace_pvc_name(slug: &str) -> String {
    format!("{}-mount-pvc", slug)
}

fn principals_pvc_name(slug: &str) -> String {
    format!("{}-mount-ssh-principals-pvc", slug)
}

// ── Active branch ─────────────────────────────────────────────────────────────

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

    let raw: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&user_file)
            .map_err(|e| format!("Could not read {}: {}", user_file.display(), e))?,
    )?;

    raw["sub"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "sub missing or not a string in user.json".into())
}

// ── Mount ─────────────────────────────────────────────────────────────────────

pub async fn mount(
    org_id:         &str,
    pkg_identifier: &str,
    lang:           &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_daemon_reachable()?;

    let branch = active_branch()
        .ok_or("No active branch set — run `ginger-code -b <branch>` first")?;
    println!("  Active branch: {}", branch);

    let user     = session_user()?;
    let slug     = deployment_slug(pkg_identifier); // "iamservice"
    let git_repo = gitolite_repo(org_id, pkg_identifier);   // "ginger-society-iamservice"
    let image    = builder_image(lang)?;
    let ssh      = supports_ssh(lang);

    println!("  k8s slug:    {}", slug);
    println!("  git remote:  source:{}.git", git_repo);

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
                "ginger-mounted": "true",
                "ginger-pkg":     pkg_identifier,
                "ginger-branch":  branch,
                "ginger-org":     org_id,
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

    let client = Client::try_default().await.expect("kube client");
    let api: Api<Deployment> = Api::default_namespaced(client);

    let deployment: Deployment = serde_json::from_value(deployment_manifest)?;

    api.patch(
        &slug,
        &PatchParams::apply("ginger-code").force(),
        &Patch::Apply(&deployment),
    )
    .await
    .map_err(|e| format!("kubectl apply failed for deployment '{}': {}", slug, e))?;
    println!("✓ Deployment '{}' created with image {}", slug, image);

    if ssh {
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(&slug).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("\n⏸  Sleeping 5s to allow rollout to stabilize...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let final_pod = wait_for_pod_scheduled(&slug).await?;

        wait_for_pod_ready(&final_pod).await?;
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
                    setup_repo_branch(
                        &final_pod, &slug,
                        &git_repo,
                        &format!("{}-{}", org_id, slug),
                        &branch,
                    ).await?;
                    if let Err(e) = delete_dev_ssh_keys(&final_pod, &slug).await {
                        eprintln!("Warning: {e}");
                    }
                }
            }
        } else {
            println!("  /workspace is not empty — checking branch...");
            setup_repo_branch(
                &final_pod, &slug,
                &git_repo,
                &format!("{}-{}", org_id, slug),
                &branch,
            ).await?;
        }

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

pub async fn unmount(
    _org_id:        &str,
    pkg_identifier: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let slug     = deployment_slug(pkg_identifier);
    let ws_pvc   = workspace_pvc_name(&slug);
    let prin_pvc = principals_pvc_name(&slug);

    delete_deployment(&slug).await?;

    println!("⏸  Waiting 5s for pod termination before deleting PVCs...");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    delete_pvc(&ws_pvc).await?;
    delete_pvc(&prin_pvc).await?;

    daemon_remove(&slug);

    if let Err(e) = remove_ssh_config(&slug) {
        eprintln!("Warning: could not clean up local ~/.ssh/config: {e}");
    }

    println!("✓ Unmounted dev container for '{}'", pkg_identifier);
    Ok(())
}