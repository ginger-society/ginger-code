use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use tokio::io::AsyncWriteExt as _;

// ── Daemon socket helper ──────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

fn send_to_daemon(payload: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("Cannot connect to ginger-code daemon at {}: {e}", path.display()))?;
    stream.write_all(format!("{payload}\n").as_bytes())?;
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp)?;
    Ok(serde_json::from_str(&resp)?)
}

/// Find an available port in the range 2200..=2299 that is not already
/// registered with the daemon and is not bound on the local machine.
fn find_free_22xx_port() -> Result<u16, Box<dyn std::error::Error>> {
    // Ask the daemon which ports are already in use
    let used_ports: std::collections::HashSet<u16> =
        match send_to_daemon(r#"{"cmd":"list"}"#) {
            Ok(val) => val["deployments"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|d| d["forwarding_port"].as_u64().map(|p| p as u16))
                .collect(),
            // Daemon not running — fall back to OS-only check
            Err(_) => std::collections::HashSet::new(),
        };

    for port in 2200u16..=2299 {
        if used_ports.contains(&port) {
            continue;
        }
        // Try binding; if it succeeds the port is free on the OS side
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    Err("No free port available in the 2200–2299 range".into())
}

// ── Builder image map ─────────────────────────────────────────────────────────

fn builder_image(lang: &str) -> &'static str {
    match lang {
        "TS"   => "gingersociety/dev-container-node:1",
        "Rust" => "gingersociety/rust-rocket-api-builder:latest",
        other  => todo!("builder image not yet defined for lang: {}", other),
    }
}

/// Returns true if the image for this lang supports SSH-based dev mode.
fn supports_ssh(lang: &str) -> bool {
    lang == "TS"
}

// ── Eject ─────────────────────────────────────────────────────────────────────

pub async fn eject(
    deployment_name: &str,
    lang: &str,
) -> Result<(), Box<dyn std::error::Error>> {

    // ── Read session user from ~/.ginger-society/user.json ───────────────────
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
        .ok_or("sub missing or not a string in user.json")?
        .to_string();

    let image = builder_image(lang);
    let ssh   = supports_ssh(lang);

    // ── Read the current image so we can restore it later ────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.spec.template.spec.containers[0].image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
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

    // ── Patch: swap image, set command, mount PVC, annotate ──────────────────
    let command = if ssh {
        serde_json::json!(["/usr/sbin/sshd", "-D", "-e"])
    } else {
        serde_json::json!(["sleep", "infinity"])
    };

    let mut container = serde_json::json!({
        "name":         deployment_name,
        "image":        image,
        "command":      command,
        "volumeMounts": [{ "name": "workspace", "mountPath": "/workspace" }]
    });

    if ssh {
        container["ports"] = serde_json::json!([{ "containerPort": 22 }]);
    }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        "true",
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
        // ── Wait until the pod is scheduled ──────────────────────────────────
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        // ── Wait until the pod container is ready ────────────────────────────
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
                "Timed out waiting for pod '{}' to become ready", pod_name
            ).into());
        }

        println!("✓ Pod ready: {}", pod_name);

        // ── Write the SSH principal for the 'dev' unix user ──────────────────
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
                "Failed to write SSH principal '{}' into pod {}", session_user, pod_name
            ).into());
        }

        println!("✓ SSH principal '{}' written — connect as: ssh dev@<host>", session_user);

        // ── Pick a free local port and register with the daemon ───────────────
        let forwarding_port = find_free_22xx_port()?;

        let register_payload = serde_json::json!({
            "cmd":             "register",
            "deployment_name": deployment_name,
            "deployment_port": 22,
            "forwarding_port": forwarding_port,
        });

        match send_to_daemon(&register_payload.to_string()) {
            Ok(resp) if resp["status"] == "ok" => {
                println!(
                    "✓ Registered '{}' with daemon — SSH available at localhost:{}",
                    deployment_name, forwarding_port
                );
            }
            Ok(resp) => {
                eprintln!("Warning: daemon responded unexpectedly: {}", resp);
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not register with daemon (is ginger-code running?): {e}\n\
                     You can register manually with:\n  \
                     ginger-code register --deployment-name {} --deployment-port 22 --forwarding-port {}",
                    deployment_name, forwarding_port
                );
            }
        }
    }

    Ok(())
}

// ── Uneject ───────────────────────────────────────────────────────────────────

pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ── Read original image from annotation ──────────────────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.metadata.annotations.ginger-original-image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
    if original_image.is_empty() {
        return Err(
            "No ginger-original-image annotation — was this deployment ejected?".into()
        );
    }

    // ── Restore: put original image back, clear builder overrides ────────────
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        null,
                "ginger-original-image": null,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name":         deployment_name,
                        "image":        original_image,
                        "command":      null,
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

    // ── Deregister from daemon ────────────────────────────────────────────────
    let remove_payload = serde_json::json!({
        "cmd":             "remove",
        "deployment_name": deployment_name,
    });

    match send_to_daemon(&remove_payload.to_string()) {
        Ok(resp) if resp["status"] == "ok" => {
            println!("✓ Deregistered '{}' from daemon — port-forward stopped", deployment_name);
        }
        Ok(resp) => {
            eprintln!("Warning: daemon responded unexpectedly: {}", resp);
        }
        Err(e) => {
            eprintln!(
                "Warning: could not deregister from daemon (is ginger-code running?): {e}\n\
                 You can remove manually with:\n  \
                 ginger-code remove --deployment-name {}",
                deployment_name
            );
        }
    }

    Ok(())
}

// ── Pod scheduling wait ───────────────────────────────────────────────────────

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