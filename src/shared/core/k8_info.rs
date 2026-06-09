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

/// Returns (pod_name, vec_of_container_names) for the running pod of a deployment.
/// Returns None if no running pod is found.
pub async fn get_pod_containers(deployment_name: &str) -> Option<(String, Vec<String>)> {
    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "--field-selector=status.phase=Running",
            "--no-headers",
            "-o", "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await
        .ok()?;

    let pod_name = String::from_utf8_lossy(&pod_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .find(|l| l.trim().starts_with(deployment_name))
        .map(|l| l.trim().to_string())?;

    let container_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pod", &pod_name,
            "-o", "jsonpath={.spec.containers[*].name}",
        ])
        .output()
        .await
        .ok()?;

    let containers = String::from_utf8_lossy(&container_output.stdout)
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();

    if containers.is_empty() { None } else { Some((pod_name, containers)) }
}

/// Fetch container list for a service once — fires once per service selection.


pub async fn get_pod_logs(deployment_name: &str, container: Option<&str>) -> Vec<String> {
    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "--field-selector=status.phase=Running",
            "--no-headers",
            "-o", "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await;

    let pod_name = match pod_output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .find(|l| l.trim().starts_with(deployment_name))
            .map(|l| l.trim().to_string()),
        Err(_) => None,
    };

    let Some(pod) = pod_name else {
        return vec![format!("No pods found for deployment '{}'.", deployment_name)];
    };

    let mut args = vec!["logs", "--tail=500", &pod];
    // Only add --container when explicitly requested — avoids the
    // "Defaulted container" warning without breaking single-container pods.
    if let Some(c) = container {
        args.push("--container");
        args.push(c);
    }

    match tokio::process::Command::new("kubectl")
        .args(&args)
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