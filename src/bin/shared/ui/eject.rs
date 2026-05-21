use tokio::io::AsyncWriteExt as _;

/// Map a language string to its builder image.
fn builder_image(lang: &str) -> Option<&'static str> {
    match lang {
        "Rust" => Some("gingersociety/rust-rocket-api-builder:latest"),
        "TS"   => Some("gingersociety/vite-react-builder:latest"),
        other  => todo!("builder image not yet defined for lang: {}", other),
    }
}

/// Eject a deployment: swap it to a long-running builder image backed by a PVC.
///
/// The original image is preserved in an annotation so `uneject` can restore it.
pub async fn eject(
    deployment_name: &str,
    lang: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(builder_img) = builder_image(lang) else {
        return Err(format!("No builder image for lang: {}", lang).into());
    };

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

    // FIX: use tokio::process::Command throughout so stdin is a tokio ChildStdin,
    // then write to it directly with the AsyncWriteExt trait — no from_std() needed.
    let mut apply = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = apply.stdin.take() {
        // `stdin` is already `tokio::process::ChildStdin` — write directly.
        stdin.write_all(pvc_yaml.as_bytes()).await?;
        // Drop stdin to close the pipe so kubectl sees EOF.
    }

    apply.wait().await?;

    // ── Patch: swap image, set sleep command, mount PVC, annotate ─────────────
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
                    "containers": [{
                        "name": deployment_name,
                        "image": builder_img,
                        "command": ["sleep", "infinity"],
                        "volumeMounts": [{
                            "name": "workspace",
                            "mountPath": "/workspace"
                        }]
                    }],
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
            "patch",
            "deployment",
            deployment_name,
            "--type",
            "strategic",
            "-p",
            &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }

    println!("✓ Ejected {} → {}", deployment_name, builder_img);
    Ok(())
}

/// Restore a deployment to its original image by reading the saved annotation.
pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ── Read original image from annotation ──────────────────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get",
            "deployment",
            deployment_name,
            "-o",
            "jsonpath={.metadata.annotations.ginger-original-image}",
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
            "patch",
            "deployment",
            deployment_name,
            "--type",
            "strategic",
            "-p",
            &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }

    println!(
        "✓ Unejected {} → restored {}",
        deployment_name, original_image
    );
    Ok(())
}