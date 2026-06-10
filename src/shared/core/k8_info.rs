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


// In k8_info.rs — replace get_pod_logs with this version that uses
// -l selector instead of name prefix matching, which is more reliable
pub async fn get_pod_logs(deployment_name: &str, container: Option<String>) -> Vec<String> {
    // Use label selector — more reliable than pod name prefix matching
    let label = format!("app={}", deployment_name);
    
    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "-l", &label,
            "--field-selector=status.phase=Running",
            "--no-headers",
            "-o", "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await;

    let pod_name = match pod_output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines()
                .filter(|l| !l.trim().is_empty())
                .next()
                .map(|l| l.trim().to_string())
        }
        Err(_) => None,
    };

    let Some(pod) = pod_name else {
        return vec![format!("No pods found for deployment '{}'.", deployment_name)];
    };

    let mut args: Vec<String> = vec![
        "logs".into(),
        "--tail=500".into(),
        pod.clone(),
    ];
    if let Some(ref c) = container {
        args.push("--container".into());
        args.push(c.clone());
    }

    match tokio::process::Command::new("kubectl")
        .args(&args)
        .output()
        .await
    {
        Ok(out) => {
            if !out.stderr.is_empty() {
                // If kubectl errors (e.g. wrong container name), surface it
                let err = String::from_utf8_lossy(&out.stderr);
                return vec![format!("kubectl logs error: {}", err.trim())];
            }
            String::from_utf8_lossy(&out.stdout)
                .lines()
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


/// Returns the set of deployment names (via `app=` label) that have at least
/// one pod in a transient state: Terminating, ContainerCreating, Pending.
pub async fn get_transitioning_deployments() -> std::collections::HashSet<String> {
    let out = tokio::process::Command::new("kubectl")
        .args(&[
            "get", "pods",
            "--no-headers",
            "-o", "custom-columns=\
                APP:.metadata.labels.app,\
                PHASE:.status.phase,\
                DELETED:.metadata.deletionTimestamp",
        ])
        .output()
        .await;

    let mut set = std::collections::HashSet::new();
    let Ok(out) = out else { return set };

    for line in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
    {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 { continue; }
        let app     = parts[0];
        let phase   = parts[1]; // "Running", "Pending", "Succeeded", "Failed"
        let deleted = parts[2]; // "<none>" or a timestamp

        if deleted != "<none>" || phase == "Pending" {
            set.insert(app.to_string());
        }
    }
    set
}