//! Low-level kubectl / pod helpers shared by eject and mount.

use tokio::io::AsyncWriteExt as _;

// ── PVC creation ──────────────────────────────────────────────────────────────

/// Apply a `PersistentVolumeClaim` via `kubectl apply -f -`.
pub async fn apply_pvc(
    pvc_name:     &str,
    storage_size: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let yaml = format!(
        "apiVersion: v1\n\
         kind: PersistentVolumeClaim\n\
         metadata:\n  name: {pvc_name}\n\
         spec:\n  accessModes: [ReadWriteOnce]\n  \
           resources:\n    requests:\n      storage: {storage_size}\n"
    );

    let mut child = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(yaml.as_bytes()).await?;
    }
    child.wait().await?;
    Ok(())
}

/// Delete a `PersistentVolumeClaim`; ignores "not found" errors.
pub async fn delete_pvc(pvc_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = tokio::process::Command::new("kubectl")
        .args(["delete", "pvc", pvc_name, "--ignore-not-found"])
        .status()
        .await?;

    if status.success() {
        println!("✓ Deleted PVC '{}'", pvc_name);
    } else {
        eprintln!("Warning: could not delete PVC '{}'", pvc_name);
    }
    Ok(())
}

// ── Deployment helpers ────────────────────────────────────────────────────────

/// Read an arbitrary `jsonpath` field from a deployment.
pub async fn get_deployment_annotation(
    deployment_name: &str,
    jsonpath:        &str,
) -> Option<String> {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "get",
            "deployment",
            deployment_name,
            "-o",
            &format!("jsonpath={{{jsonpath}}}"),
        ])
        .output()
        .await
        .ok()?;

    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Delete a deployment; ignores "not found" errors.
pub async fn delete_deployment(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = tokio::process::Command::new("kubectl")
        .args(["delete", "deployment", deployment_name, "--ignore-not-found"])
        .status()
        .await?;

    if status.success() {
        println!("✓ Deleted deployment '{}'", deployment_name);
    } else {
        eprintln!("Warning: could not delete deployment '{}'", deployment_name);
    }
    Ok(())
}

// ── Pod scheduling ────────────────────────────────────────────────────────────

/// Poll until a pod for `deployment_name` is scheduled and not terminating.
/// Returns the pod name.
pub async fn wait_for_pod_scheduled(
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let label = format!("app={}", deployment_name);
    for attempt in 1..=40 {
        let out = tokio::process::Command::new("kubectl")
            .args([
                "get",
                "pods",
                "-l",
                &label,
                "--field-selector=status.phase!=Failed",
                "--no-headers",
                "-o",
                "custom-columns=NAME:.metadata.name,DELETED:.metadata.deletionTimestamp",
            ])
            .output()
            .await?;

        if let Some(name) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty() && l.contains("<none>"))
            .filter_map(|l| l.split_whitespace().next().map(|s| s.trim().to_string()))
            .next()
        {
            return Ok(name);
        }

        println!(
            "  … pod not scheduled yet (attempt {}), retrying in 3s",
            attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod for '{}'", deployment_name).into())
}

// ── Workspace emptiness check ─────────────────────────────────────────────────

pub async fn is_workspace_empty(
    pod_name:  &str,
    container: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            "find /workspace -mindepth 1 -maxdepth 1 | head -1",
        ])
        .output()
        .await?;

    Ok(String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

// ── SSH principal ─────────────────────────────────────────────────────────────

/// Write `session_user` into `/etc/ssh/auth_principals/dev` inside the pod.
pub async fn write_ssh_principal(
    pod_name:     &str,
    container:    &str,
    session_user: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cmd = format!(
        "mkdir -p /etc/ssh/auth_principals && \
         chown root:root /etc/ssh/auth_principals && \
         chmod 755 /etc/ssh/auth_principals && \
         echo '{session_user}' > /etc/ssh/auth_principals/dev && \
         chown root:root /etc/ssh/auth_principals/dev && \
         chmod 644 /etc/ssh/auth_principals/dev",
    );

    let status = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "-c", container, "--", "sh", "-c", &cmd])
        .status()
        .await?;

    if !status.success() {
        return Err(format!(
            "Failed to write SSH principal '{}' into pod {}",
            session_user, pod_name
        )
        .into());
    }
    println!("✓ SSH principal '{}' written", session_user);
    Ok(())
}