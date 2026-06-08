use std::collections::HashMap;


pub async fn is_mounted(deployment_slug: &str) -> bool {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "get",
            "deployment",
            deployment_slug,
            "-o",
            "jsonpath={.metadata.annotations.ginger-mounted}",
        ])
        .output()
        .await;
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "true")
}

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


pub fn db_to_k8s_name(name: &str, db_type: &str) -> String {
    let slug = name.to_lowercase().replace(' ', "-");
    match db_type {
        "rdbms"        => format!("{}-postgresql", slug),
        "cache"        => format!("{}-redis",      slug),
        "messagequeue" => format!("{}-rabbitmq",   slug),
        _              => slug,
    }
}

pub fn db_is_statefulset(db_type: &str) -> bool {
    db_type == "rdbms"
}

pub async fn get_k8s_statefulsets() -> HashMap<String, (String, String)> {
    let output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "statefulsets",
            "-o", "custom-columns=NAME:.metadata.name,READY:.status.readyReplicas,DESIRED:.spec.replicas",
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