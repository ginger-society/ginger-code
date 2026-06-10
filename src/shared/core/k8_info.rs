use std::collections::{HashMap, HashSet};

use tokio::io::AsyncBufReadExt;
use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use kube::api::{ListParams, LogParams};
use tokio_util::compat::FuturesAsyncReadCompatExt;

// ── Client helper ─────────────────────────────────────────────────────────────

async fn client() -> Client {
    Client::try_default().await.expect("kube client")
}

// ── meta_to_deployment_name ───────────────────────────────────────────────────

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
// Shared by get_pod_containers and stream_pod_logs.
// Tries label selectors first, falls back to pod-name prefix matching.

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

// ── stream_pod_logs ───────────────────────────────────────────────────────────
//
// Streams log lines for `deployment_name` into `line_tx`.
// - Seeds with the last 500 lines via `tail_lines: Some(500)`.
// - Then follows the live stream (`follow: true`) until the sender is dropped
//   or the pod stream ends, at which point it returns so the caller can retry.
//
// The caller is responsible for the retry/reconnect loop (see bg.rs).

pub async fn stream_pod_logs(
    deployment_name: &str,
    container:       Option<String>,
    line_tx:         tokio::sync::mpsc::UnboundedSender<String>,
) {
    let pod_name = match resolve_pod_name(deployment_name).await {
        Some(p) => p,
        None => return,
    };

    let api: Api<Pod> = Api::default_namespaced(client().await);

    let params = LogParams {
        follow:     true,
        tail_lines: Some(500),
        container:  container.clone(),
        ..Default::default()
    };

    let stream = match api.log_stream(&pod_name, &params).await {
        Ok(s)  => s,
        Err(e) => {
            let _ = line_tx.send(format!("log stream error: {}", e));
            return;
        }
    };

    let reader = tokio::io::BufReader::new(stream.compat());
    let mut lines = reader.lines();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line_tx.send(line).is_err() {
                    // Receiver dropped — container switched or app closed.
                    return;
                }
            }
            Ok(None) => {
                // Stream ended cleanly (pod restarted / completed).
                return;
            }
            Err(e) => {
                let _ = line_tx.send(format!("log stream error: {}", e));
                return;
            }
        }
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