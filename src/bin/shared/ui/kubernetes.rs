use std::collections::HashMap;

/// "@ginger-society/dev-portal"  → "dev-portal"
/// "@ginger-society/IAMService"  → "iamservice"
pub fn meta_to_deployment_name(meta_name: &str) -> String {
    meta_name
        .split('/')
        .last()
        .unwrap_or(meta_name)
        .to_lowercase()
}

/// Returns map: deployment_name → (status, ready_string)
pub async fn get_k8s_deployments() -> HashMap<String, (String, String)> {
    let output = tokio::process::Command::new("kubectl")
        .args(&[
            "get",
            "deployments",
            "-o",
            "custom-columns=NAME:.metadata.name,READY:.status.readyReplicas,DESIRED:.spec.replicas",
            "--no-headers",
        ])
        .output()
        .await;

    let mut map = HashMap::new();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let name        = parts[0].to_string();
                let ready_count = parts[1];
                let desired     = parts[2];
                let ready_str   = format!("{}/{}", ready_count, desired);
                let status = if ready_count == desired {
                    "Running".to_string()
                } else if ready_count == "<none>" || ready_count == "0" {
                    "Pending".to_string()
                } else {
                    "Degraded".to_string()
                };
                map.insert(name, (status, ready_str));
            }
        }
    }
    map
}

pub async fn is_ejected(deployment_name: &str) -> bool {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "get",
            "deployment",
            deployment_name,
            "-o",
            "jsonpath={.metadata.annotations.ginger-ejected}",
        ])
        .output()
        .await;
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "true")
}

pub async fn get_pod_logs(deployment_name: &str) -> Vec<String> {
    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get",
            "pods",
            "-l",
            &format!("app={}", deployment_name),
            "--no-headers",
            "-o",
            "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await;

    let pod_name = match pod_output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .next()
            .map(|l| l.trim().to_string()),
        Err(_) => None,
    };

    let Some(pod) = pod_name else {
        return vec!["No pods found for this deployment.".to_string()];
    };

    match tokio::process::Command::new("kubectl")
        .args(&["logs", "--tail=500", &pod])
        .output()
        .await
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            stdout
                .lines()
                .chain(stderr.lines())
                .map(|s| s.to_string())
                .collect()
        }
        Err(e) => vec![format!("Failed to fetch logs: {}", e)],
    }
}

pub async fn shell_into_pod(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get",
            "pods",
            "-l",
            &format!("app={}", deployment_name),
            "--no-headers",
            "-o",
            "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await?;

    let pod_name = String::from_utf8_lossy(&pod_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .next()
        .map(|l| l.trim().to_string());

    let Some(pod) = pod_name else {
        eprintln!("No running pod found for deployment: {}", deployment_name);
        return Ok(());
    };

    // Try bash first, fall back to sh
    let bash_ok = tokio::process::Command::new("kubectl")
        .args(["exec", "-it", &pod, "--", "bash"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if !bash_ok {
        tokio::process::Command::new("kubectl")
            .args(["exec", "-it", &pod, "--", "sh"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
    }

    Ok(())
}