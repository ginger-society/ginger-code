use std::collections::{HashMap, HashSet};

use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use kube::api::ListParams;

// ── Client helper ─────────────────────────────────────────────────────────────

async fn client() -> Client {
    Client::try_default().await.expect("kube client")
}

// ── meta_to_deployment_name ───────────────────────────────────────────────────

/// "@ginger-society/dev-portal"  → "dev-portal"
/// "@ginger-society/IAMService"  → "iamservice"
pub fn meta_to_deployment_name(meta_name: &str) -> String {
    meta_name
        .split('/')
        .last()
        .unwrap_or(meta_name)
        .to_lowercase()
}

// ── is_mounted ────────────────────────────────────────────────────────────────

pub async fn is_mounted(deployment_slug: &str) -> bool {
    let api: Api<Deployment> = Api::default_namespaced(client().await);
    api.get(deployment_slug)
        .await
        .ok()
        .and_then(|d| d.metadata.annotations)
        .and_then(|a| a.get("ginger-mounted").cloned())
        .map(|v| v == "true")
        .unwrap_or(false)
}

// ── is_ejected ────────────────────────────────────────────────────────────────

pub async fn is_ejected(deployment_name: &str) -> bool {
    let api: Api<Deployment> = Api::default_namespaced(client().await);
    api.get(deployment_name)
        .await
        .ok()
        .and_then(|d| d.metadata.annotations)
        .and_then(|a| a.get("ginger-ejected").cloned())
        .map(|v| v == "true")
        .unwrap_or(false)
}

// ── get_k8s_deployments ───────────────────────────────────────────────────────

/// Returns map: deployment_name → (status, ready_string)
pub async fn get_k8s_deployments() -> HashMap<String, (String, String)> {
    let api: Api<Deployment> = Api::default_namespaced(client().await);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return HashMap::new();
    };

    list.items
        .into_iter()
        .filter_map(|d| {
            let name    = d.metadata.name?;
            let status  = d.status.as_ref()?;
            let desired = d.spec.as_ref()?.replicas.unwrap_or(0);
            let ready   = status.ready_replicas.unwrap_or(0);

            let ready_str  = format!("{}/{}", ready, desired);
            let status_str = if ready == desired && desired > 0 {
                "Running".to_string()
            } else if ready == 0 {
                "Pending".to_string()
            } else {
                "Degraded".to_string()
            };

            Some((name, (status_str, ready_str)))
        })
        .collect()
}

// ── get_k8s_statefulsets ──────────────────────────────────────────────────────

pub async fn get_k8s_statefulsets() -> HashMap<String, (String, String)> {
    let api: Api<StatefulSet> = Api::default_namespaced(client().await);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return HashMap::new();
    };

    list.items
        .into_iter()
        .filter_map(|ss| {
            let name    = ss.metadata.name?;
            let status  = ss.status.as_ref()?;
            let desired = ss.spec.as_ref()?.replicas.unwrap_or(0) as i32;
            let ready   = status.ready_replicas.unwrap_or(0);

            let ready_str  = format!("{}/{}", ready, desired);
            let status_str = if ready == desired && desired > 0 {
                "Running".to_string()
            } else if ready == 0 {
                "Pending".to_string()
            } else {
                "Degraded".to_string()
            };

            Some((name, (status_str, ready_str)))
        })
        .collect()
}

// ── resolve_pod_name ──────────────────────────────────────────────────────────
//
// Shared by get_pod_containers and get_pod_logs.
// Tries label selectors first, falls back to pod-name prefix matching.
// This is necessary for Helm-managed StatefulSets (e.g. my-db-postgresql-0)
// which don't carry an `app=` label.

async fn resolve_pod_name(deployment_name: &str) -> Option<String> {
    let api: Api<Pod> = Api::default_namespaced(client().await);

    let label_strategies = [
        format!("app={}", deployment_name),
        format!("app.kubernetes.io/instance={}", deployment_name),
        format!("app.kubernetes.io/name={}", deployment_name),
    ];

    for label in &label_strategies {
        let lp = ListParams::default().labels(label);
        if let Ok(list) = api.list(&lp).await {
            if let Some(name) = list.items.into_iter()
                .find(|p| {
                    p.status.as_ref()
                        .and_then(|s| s.phase.as_deref())
                        == Some("Running")
                    && p.metadata.deletion_timestamp.is_none()
                })
                .and_then(|p| p.metadata.name)
            {
                return Some(name);
            }
        }
    }

    // Pod-name prefix fallback — covers StatefulSets like my-db-postgresql-0
    if let Ok(all) = api.list(&ListParams::default()).await {
        return all.items.into_iter()
            .find(|p| {
                let matches = p.metadata.name.as_deref()
                    .map(|n| n == deployment_name
                        || n.starts_with(&format!("{}-", deployment_name)))
                    .unwrap_or(false);
                let running = p.status.as_ref()
                    .and_then(|s| s.phase.as_deref())
                    == Some("Running");
                let not_terminating = p.metadata.deletion_timestamp.is_none();
                matches && running && not_terminating
            })
            .and_then(|p| p.metadata.name);
    }

    None
}

// ── get_pod_containers ────────────────────────────────────────────────────────

pub async fn get_pod_containers(deployment_name: &str) -> Option<(String, Vec<String>)> {
    let api: Api<Pod> = Api::default_namespaced(client().await);
    let pod_name = resolve_pod_name(deployment_name).await?;

    let pod = api.get(&pod_name).await.ok()?;
    let containers = pod.spec?
        .containers
        .into_iter()
        .map(|c| c.name)
        .collect::<Vec<_>>();

    if containers.is_empty() { None } else { Some((pod_name, containers)) }
}

// ── get_pod_logs — kubectl shell-out using resolved pod name ──────────────────

pub async fn get_pod_logs(deployment_name: &str, container: Option<String>) -> Vec<String> {
    let pod_name = match resolve_pod_name(deployment_name).await {
        Some(p) => p,
        None    => return vec![format!("No pods found for deployment '{}'.", deployment_name)],
    };

    let mut args: Vec<String> = vec![
        "logs".into(),
        "--tail=500".into(),
        pod_name,
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

// ── get_transitioning_deployments ─────────────────────────────────────────────

pub async fn get_transitioning_deployments() -> HashSet<String> {
    let api: Api<Pod> = Api::default_namespaced(client().await);
    let Ok(list) = api.list(&ListParams::default()).await else {
        return HashSet::new();
    };

    list.items
        .into_iter()
        .filter(|p| {
            let terminating = p.metadata.deletion_timestamp.is_some();
            let pending     = p.status.as_ref()
                .and_then(|s| s.phase.as_deref())
                == Some("Pending");
            terminating || pending
        })
        .filter_map(|p| {
            p.metadata
                .labels?
                .get("app")
                .cloned()
        })
        .collect()
}

// ── db helpers ────────────────────────────────────────────────────────────────

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