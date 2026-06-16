//! Background worker threads / tasks and the channel message type.
//!
//! Design: a single background thread owns one Tokio runtime. Log streams run
//! as tasks inside that runtime and are cancelled (not just orphaned) when the
//! user navigates away. This eliminates the file-descriptor leak that came from
//! spawning a new runtime + thread on every navigation event.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc as async_mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::shared::core::{
    k8_info::{get_k8s_deployments, get_pod_containers, is_ejected, stream_pod_logs},
    k8s_ops::get_deployment_annotation,
    types::K8sService,
};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum TuiMsg {
    ServiceLogs { lines: Vec<String>, generation: u64 },
    DbLogs      { lines: Vec<String>, generation: u64 },
    Containers  { svc_idx: usize,    containers: Vec<String> },
    DbContainers { schema_idx: usize, containers: Vec<String> },
}

// ── Shared background runtime ─────────────────────────────────────────────────
//
// All stream tasks run inside this single runtime so we never open more than
// O(1) kqueue/epoll handles regardless of how many times the user navigates.

lazy_static::lazy_static! {
    static ref BG_RT: tokio::runtime::Runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("background tokio runtime");
}

// ── Service log stream ────────────────────────────────────────────────────────

pub fn spawn_service_log_stream(
    tx:              std::sync::mpsc::Sender<TuiMsg>,
    deployment_name: String,
    container:       Option<String>,
    generation:      u64,
    cancel:          CancellationToken,
) {
    BG_RT.spawn(async move {
        let mut lines: Vec<String> = Vec::new();
        loop {
            if cancel.is_cancelled() { return; }

            let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();
            let dep  = deployment_name.clone();
            let cont = container.clone();
            let cancel2 = cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel2.cancelled() => {}
                    _ = stream_pod_logs(&dep, cont, line_tx) => {}
                }
            });

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    msg = line_rx.recv() => match msg {
                        None => break,
                        Some(line) => {
                            lines.push(line);
                            if lines.len() > 2000 { lines.drain(0..500); }
                            if tx.send(TuiMsg::ServiceLogs { lines: lines.clone(), generation }).is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            if tx.send(TuiMsg::ServiceLogs { lines: lines.clone(), generation }).is_err() {
                return;
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sleep(Duration::from_secs(2)) => {}
            }
        }
    });
}

// ── DB log stream ─────────────────────────────────────────────────────────────

pub fn spawn_db_log_stream(
    tx:         std::sync::mpsc::Sender<TuiMsg>,
    slug:       String,
    container:  Option<String>,
    generation: u64,
    cancel:     CancellationToken,
) {
    BG_RT.spawn(async move {
        let mut lines: Vec<String> = Vec::new();
        loop {
            if cancel.is_cancelled() { return; }

            let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();
            let dep    = slug.clone();
            let cont   = container.clone();
            let cancel2 = cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel2.cancelled() => {}
                    _ = stream_pod_logs(&dep, cont, line_tx) => {}
                }
            });

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    msg = line_rx.recv() => match msg {
                        None => break,
                        Some(line) => {
                            lines.push(line);
                            if lines.len() > 2000 { lines.drain(0..500); }
                            let normalised = if lines.len() == 1 && lines[0].starts_with("No pods found") {
                                vec![]
                            } else {
                                lines.clone()
                            };
                            if tx.send(TuiMsg::DbLogs { lines: normalised, generation }).is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            if tx.send(TuiMsg::DbLogs { lines: lines.clone(), generation }).is_err() {
                return;
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sleep(Duration::from_secs(2)) => {}
            }
        }
    });
}

// ── Container fetch ───────────────────────────────────────────────────────────

pub fn spawn_container_fetch(
    tx:              std::sync::mpsc::Sender<TuiMsg>,
    deployment_name: String,
    svc_idx:         usize,
) {
    BG_RT.spawn(async move {
        if let Some((_pod, containers)) = get_pod_containers(&deployment_name).await {
            let _ = tx.send(TuiMsg::Containers { svc_idx, containers });
        }
    });
}

pub fn spawn_db_container_fetch(
    tx:         std::sync::mpsc::Sender<TuiMsg>,
    slug:       String,
    schema_idx: usize,
) {
    BG_RT.spawn(async move {
        if let Some((_pod, containers)) = get_pod_containers(&slug).await {
            let _ = tx.send(TuiMsg::DbContainers { schema_idx, containers });
        }
    });
}

// ── Deployment status watcher ─────────────────────────────────────────────────

pub fn spawn_deployment_watcher(services: Arc<Mutex<Vec<K8sService>>>) {
    BG_RT.spawn(async move {
        loop {
            let deployments = get_k8s_deployments().await;
            {
                let mut svcs = services.lock().unwrap();
                for svc in svcs.iter_mut() {
                    if let Some(ref dep) = svc.deployment_name {
                        if let Some((status, ready)) = deployments.get(dep) {
                            svc.status = status.clone();
                            svc.ready  = ready.clone();
                        } else {
                            svc.status = "Not deployed".to_string();
                            svc.ready  = "-".to_string();
                        }
                    }
                }
            }
            let deps: Vec<(usize, String)> = {
                let svcs = services.lock().unwrap();
                svcs.iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.deployment_name.clone().map(|d| (i, d)))
                    .collect()
            };
            for (i, dep) in deps {
                let ejected = is_ejected(&dep).await;
                let ejected_container = if ejected {
                    get_deployment_annotation(
                        &dep,
                        ".metadata.annotations['ginger-main-container']",
                    ).await
                } else {
                    None
                };
                if let Some(svc) = services.lock().unwrap().get_mut(i) {
                    svc.ejected           = ejected;
                    svc.ejected_container = ejected_container;
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    });
}