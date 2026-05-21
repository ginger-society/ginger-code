use std::fs;

use tokio::io::AsyncWriteExt as _;

/// Map a language string to its builder image.
fn builder_image(lang: &str) -> &'static str {
    match lang {
        "TS"   => "gingersociety/dev-container-node:1",
        "Rust" => "gingersociety/rust-rocket-api-builder:latest",
        other  => todo!("builder image not yet defined for lang: {}", other),
    }
}

/// Returns true if the image for this lang supports SSH-based dev mode.
/// The Rust builder is a plain build container; only the TS dev-container has sshd.
fn supports_ssh(lang: &str) -> bool {
    lang == "TS"
}

/// Eject a deployment: swap it to the appropriate builder image backed by a PVC.
/// For SSH-capable images (TS), waits for the pod and writes the SSH principal.
/// For other images (Rust), keeps the original sleep-infinity command.
pub async fn eject(
    deployment_name: &str,
    lang: &str,
) -> Result<(), Box<dyn std::error::Error>> {

    let user_file = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ginger-society")
        .join("user.json");

    let user_json = fs::read_to_string(&user_file)
        .map_err(|e| format!("Could not read {}: {}", user_file.display(), e))?;

    let user_details: serde_json::Value = serde_json::from_str(&user_json)
        .map_err(|e| format!("Could not parse user.json: {}", e))?;

    let session_user = user_details["sub"]
        .as_str()
        .ok_or("user_id missing or not a string in user.json")?
        .to_string();

    let image = builder_image(lang);
    let ssh = supports_ssh(lang);
    // ── Read the current image so we can restore it later ────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get",
            "deployment",
            deployment_name,
            "-o",
            "jsonpath={.spec.template.spec.containers[0].image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout)
        .trim()
        .to_string();

    if original_image.is_empty() {
        return Err("Could not read current image from deployment".into());
    }

    // ── Create PVC (idempotent via `kubectl apply`) ───────────────────────────
    let pvc_name = format!("{}-eject-pvc", deployment_name);
    let pvc_yaml = format!(
        r#"apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc_name}
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 4Gi
"#
    );

    let mut apply = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = apply.stdin.take() {
        stdin.write_all(pvc_yaml.as_bytes()).await?;
    }
    apply.wait().await?;

    // ── Patch: swap image, set command, mount PVC, annotate ─────────────────
    let command = if ssh {
        serde_json::json!(["/usr/sbin/sshd", "-D", "-e"])
    } else {
        serde_json::json!(["sleep", "infinity"])
    };

    let mut container = serde_json::json!({
        "name": deployment_name,
        "image": image,
        "command": command,
        "volumeMounts": [{ "name": "workspace", "mountPath": "/workspace" }]
    });

    if ssh {
        container["ports"] = serde_json::json!([{ "containerPort": 22 }]);
    }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected": "true",
                "ginger-original-image": original_image,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [container],
                    "volumes": [{
                        "name": "workspace",
                        "persistentVolumeClaim": { "claimName": pvc_name }
                    }]
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

    println!("✓ Patched {} → {}", deployment_name, image);

    if ssh {
        // ── Wait until the pod container is actually running ────────────────
        // Strategy: first get the pod name (it may take a moment to be scheduled),
        // then wait on the pod directly with --for=condition=Ready, which only
        // passes once the container is up and its readiness probe succeeds.
        // Waiting on the Deployment's Available condition is not sufficient —
        // it can be satisfied while the new container is still starting.
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("⏳ Waiting for pod container to be ready...");
        let wait_status = tokio::process::Command::new("kubectl")
            .args([
                "wait",
                &format!("pod/{}", pod_name),
                "--for=condition=Ready",
                "--timeout=120s",
            ])
            .status()
            .await?;

        if !wait_status.success() {
            return Err(format!(
                "Timed out waiting for pod '{}' to become ready",
                pod_name
            ).into());
        }

        println!("✓ Pod ready: {}", pod_name);

        // ── Write the SSH principal for the 'dev' unix user ───────────────────
        // The Dockerfile creates a 'dev' user; sshd maps the CA principal to it
        // via /etc/ssh/auth_principals/dev.
        let principal_cmd = format!(
            "echo '{}' > /etc/ssh/auth_principals/dev",
            session_user
        );

        let exec_status = tokio::process::Command::new("kubectl")
            .args(["exec", &pod_name, "--", "sh", "-c", &principal_cmd])
            .status()
            .await?;

        if !exec_status.success() {
            return Err(format!(
                "Failed to write SSH principal '{}' into pod {}",
                session_user, pod_name
            ).into());
        }

        println!("✓ SSH principal '{}' written — connect as: ssh dev@<host>", session_user);
    }

    Ok(())
}

/// Poll until a pod for the deployment appears in the API (is scheduled),
/// then return its name. Does NOT mean the container is running yet.
async fn wait_for_pod_scheduled(
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let label = format!("app={}", deployment_name);
    for attempt in 1..=40 {
        let out = tokio::process::Command::new("kubectl")
            .args([
                "get", "pods",
                "-l", &label,
                "--no-headers",
                "-o", "custom-columns=NAME:.metadata.name",
            ])
            .output()
            .await?;

        if let Some(name) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| !l.is_empty())
            .map(|l| l.trim().to_string())
        {
            return Ok(name);
        }

        println!("  … pod not scheduled yet (attempt {}), retrying in 3s", attempt);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod to be scheduled for '{}'", deployment_name).into())
}

/// Restore a deployment to its original image by reading the saved annotation.
pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ── Read original image from annotation ──────────────────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.metadata.annotations.ginger-original-image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout)
        .trim()
        .to_string();

    if original_image.is_empty() {
        return Err(
            "No ginger-original-image annotation — was this deployment ejected?".into(),
        );
    }

    // ── Restore: put original image back, clear builder overrides ────────────
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected": null,
                "ginger-original-image": null,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": deployment_name,
                        "image": original_image,
                        "command": null,
                        "volumeMounts": []
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
    Ok(())
}