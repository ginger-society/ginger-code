use std::collections::{HashMap, HashSet};

use tokio::io::AsyncBufReadExt;
use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use kube::api::{ListParams, LogParams};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use super::k8s_client::{get_client, handle_unauthorized, is_unauthorized};

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
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<Deployment> = Api::default_namespaced(client);
        match api.get(deployment_slug).await {
            Ok(d) => {
                return d.metadata.annotations
                    .and_then(|a| a.get("ginger-mounted").cloned())
                    .map(|v| v == "true")
                    .unwrap_or(false);
            }
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return false,
        }
    }
    false
}

// ── is_ejected ────────────────────────────────────────────────────────────────

pub async fn is_ejected(deployment_name: &str) -> bool {
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<Deployment> = Api::default_namespaced(client);
        match api.get(deployment_name).await {
            Ok(d) => {
                return d.metadata.annotations
                    .and_then(|a| a.get("ginger-ejected").cloned())
                    .map(|v| v == "true")
                    .unwrap_or(false);
            }
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return false,
        }
    }
    false
}

// ── get_k8s_deployments ───────────────────────────────────────────────────────

pub async fn get_k8s_deployments() -> HashMap<String, (String, String)> {
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<Deployment> = Api::default_namespaced(client);
        match api.list(&ListParams::default()).await {
            Ok(list) => {
                return list.items
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
                    .collect();
            }
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return HashMap::new(),
        }
    }
    HashMap::new()
}

// ── get_k8s_statefulsets ──────────────────────────────────────────────────────

pub async fn get_k8s_statefulsets() -> HashMap<String, (String, String)> {
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<StatefulSet> = Api::default_namespaced(client);
        match api.list(&ListParams::default()).await {
            Ok(list) => {
                return list.items
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
                    .collect();
            }
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return HashMap::new(),
        }
    }
    HashMap::new()
}

// ── resolve_pod_name ──────────────────────────────────────────────────────────

async fn resolve_pod_name(deployment_name: &str) -> Option<String> {
    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    let label_strategies = [
        format!("app={}", deployment_name),
        format!("app.kubernetes.io/instance={}", deployment_name),
        format!("app.kubernetes.io/name={}", deployment_name),
    ];

    for label in &label_strategies {
        let lp = ListParams::default().labels(label);
        match api.list(&lp).await {
            Ok(list) => {
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
            Err(ref e) if is_unauthorized(e) => {
                handle_unauthorized().await;
                return None; // caller will retry on next poll cycle
            }
            Err(_) => {}
        }
    }

    // Pod-name prefix fallback
    match api.list(&ListParams::default()).await {
        Ok(all) => {
            all.items.into_iter()
                .find(|p| {
                    let matches = p.metadata.name.as_deref()
                        .map(|n| n == deployment_name
                            || n.starts_with(&format!("{}-", deployment_name)))
                        .unwrap_or(false);
                    let running = p.status.as_ref()
                        .and_then(|s| s.phase.as_deref())
                        == Some("Running");
                    matches && running && p.metadata.deletion_timestamp.is_none()
                })
                .and_then(|p| p.metadata.name)
        }
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            None
        }
        Err(_) => None,
    }
}

// ── get_pod_containers ────────────────────────────────────────────────────────

pub async fn get_pod_containers(deployment_name: &str) -> Option<(String, Vec<String>)> {
    let pod_name = resolve_pod_name(deployment_name).await?;
    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    match api.get(&pod_name).await {
        Ok(pod) => {
            let containers = pod.spec?
                .containers
                .into_iter()
                .map(|c| c.name)
                .collect::<Vec<_>>();
            if containers.is_empty() { None } else { Some((pod_name, containers)) }
        }
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            None
        }
        Err(_) => None,
    }
}

// ── stream_pod_logs ───────────────────────────────────────────────────────────

pub async fn stream_pod_logs(
    deployment_name: &str,
    container:       Option<String>,
    line_tx:         tokio::sync::mpsc::UnboundedSender<String>,
) {
    let pod_name = match resolve_pod_name(deployment_name).await {
        Some(p) => p,
        None    => return,
    };

    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    let params = LogParams {
        follow:     true,
        tail_lines: Some(500),
        container:  container.clone(),
        ..Default::default()
    };

    let stream = match api.log_stream(&pod_name, &params).await {
        Ok(s)  => s,
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            let _ = line_tx.send("log stream: refreshing credentials, retrying…".into());
            return;
        }
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
                if line_tx.send(line).is_err() { return; }
            }
            Ok(None) => return,
            Err(e) => {
                let _ = line_tx.send(format!("log stream error: {}", e));
                return;
            }
        }
    }
}

// ── get_transitioning_deployments ─────────────────────────────────────────────

pub async fn get_transitioning_deployments() -> HashSet<String> {
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<Pod> = Api::default_namespaced(client);
        match api.list(&ListParams::default()).await {
            Ok(list) => {
                return list.items
                    .into_iter()
                    .filter(|p| {
                        p.metadata.deletion_timestamp.is_some()
                            || p.status.as_ref()
                                .and_then(|s| s.phase.as_deref())
                                == Some("Pending")
                    })
                    .filter_map(|p| {
                        p.metadata.labels?.get("app").cloned()
                    })
                    .collect();
            }
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return HashSet::new(),
        }
    }
    HashSet::new()
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