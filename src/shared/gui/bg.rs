//! Background task helpers and channel message types.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use ginger_shared_rs::utils::get_token_from_file_storage;
use MetadataService::get_configuration as get_metadata_configuration;

use crate::shared::core::{
    data_source::{fetch_current_workspace, fetch_dbs_enriched, fetch_packages, fetch_services},
    k8_info::{get_k8s_deployments, get_pod_containers, get_transitioning_deployments, is_ejected},
    k8s_ops::get_deployment_annotation,
    mount, types::{DbSchema, K8sService, Package}, unmount,
};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum BgMsg {
    Services(Vec<K8sService>),
    Packages(Vec<Package>),
    DbSchemas(Vec<DbSchema>),
    K8sStatuses(HashMap<String, (String, String)>),
    EjectedFlag { idx: usize, ejected: bool, ejected_container: Option<String> },
    Logs { lines: Vec<String>, generation: u64 },
    DbSchemaLogs { lines: Vec<String>, schema_idx: usize, generation: u64 },
    Error(String),
    EjectResult { success: bool, message: String, idx: usize },
    MountResult { success: bool, message: String, pkg_idx: usize, mounted: bool },
    Containers { svc_idx: usize, containers: Vec<String> },
    TransitioningSet(std::collections::HashSet<String>),
    DbContainers { schema_idx: usize, containers: Vec<String> },
    TermConnected {
        tab_idx: usize,
        session: crate::shared::gui::terminal::SshSession,
    },
    TermError {
        tab_idx: usize,
        message: String,
    },
}

// ── How many new lines to buffer before sending a snapshot to the UI.
//
// Lower  = more responsive tail, more channel traffic + repaints.
// Higher = less GPU churn, slightly more lag on fast log bursts.
// 8 is a good middle ground: at 60 fps that's ~480 lines/s visible
// with essentially zero perceptible lag.
const LOG_BATCH_SIZE: usize = 8;

// ── Spawn helpers ─────────────────────────────────────────────────────────────

/// One-shot: fetch packages, services, and DB schemas from the metadata API.
pub fn spawn_metadata_fetch(tx: mpsc::Sender<BgMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            let token  = get_token_from_file_storage();
            let config = get_metadata_configuration(Some(token));

            let org_id = match fetch_current_workspace(&config).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(BgMsg::Error(format!("Workspace fetch error: {e:?}")));
                    return;
                }
            };

            // ── Packages (non-fatal) ──────────────────────────────────────────
            match fetch_packages(&config, &org_id, "stage").await {
                Ok(mut packages) => {
                    for pkg in &mut packages {
                        let slug = crate::shared::core::image::pkg_to_slug(&pkg.identifier);
                        pkg.mounted = crate::shared::core::k8_info::is_mounted(&slug).await;
                    }
                    let _ = tx.send(BgMsg::Packages(packages));
                    ctx.request_repaint();
                }
                Err(e) => eprintln!("Package fetch error: {e:?}"),
            }

            // ── Services ─────────────────────────────────────────────────────
            match fetch_services(&config, &org_id, 100).await {
                Ok(services) => {
                    let _ = tx.send(BgMsg::Services(services));
                    ctx.request_repaint();
                }
                Err(e) => {
                    let _ = tx.send(BgMsg::Error(format!("{e:?}")));
                    ctx.request_repaint();
                }
            }

            // ── DB Schemas (non-fatal) ────────────────────────────────────────
            match fetch_dbs_enriched(&config, &org_id).await {
                Ok(schemas) => {
                    let _ = tx.send(BgMsg::DbSchemas(schemas));
                    ctx.request_repaint();
                }
                Err(e) => eprintln!("DB schema fetch error: {e:?}"),
            }
        });
    });
}

/// Infinite loop: poll k8s deployment statuses every 5 seconds.
pub fn spawn_k8s_poller(tx: mpsc::Sender<BgMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            loop {
                let deployments   = get_k8s_deployments().await;
                let transitioning = get_transitioning_deployments().await;
                let _ = tx.send(BgMsg::K8sStatuses(deployments));
                let _ = tx.send(BgMsg::TransitioningSet(transitioning));
                // Single repaint for the k8s status update — fires every 5 s.
                ctx.request_repaint();
                sleep(Duration::from_secs(5)).await;
            }
        });
    });
}

/// Check ejected flag, then start a log poller if not ejected.
pub fn spawn_service_refresh(
    tx:              mpsc::Sender<BgMsg>,
    ctx:             egui::Context,
    idx:             usize,
    deployment_name: String,
    generation:      u64,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let ejected = is_ejected(&deployment_name).await;
            let ejected_container = if ejected {
                get_deployment_annotation(
                    &deployment_name,
                    ".metadata.annotations['ginger-main-container']",
                ).await
            } else {
                None
            };
            let _ = tx.send(BgMsg::EjectedFlag { idx, ejected, ejected_container });
            ctx.request_repaint();
        });
    });
}

/// Bulk ejected check for sidebar badges (services 1..N).
pub fn spawn_bulk_ejected_check(
    tx:       mpsc::Sender<BgMsg>,
    ctx:      egui::Context,
    services: Vec<(usize, String)>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            for (idx, deployment_name) in services {
                let ejected = is_ejected(&deployment_name).await;
                let ejected_container = if ejected {
                    get_deployment_annotation(
                        &deployment_name,
                        ".metadata.annotations['ginger-main-container']",
                    ).await
                } else {
                    None
                };
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected, ejected_container });
                // One repaint per service — these fire once at startup, acceptable.
                ctx.request_repaint();
            }
        });
    });
}

/// Mount a dev container for `pkg_idx`.
pub fn spawn_mount(
    tx:         mpsc::Sender<BgMsg>,
    ctx:        egui::Context,
    pkg_idx:    usize,
    org_id:     String,
    identifier: String,
    lang:       String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let result = mount(&org_id, &identifier, &lang).await;
            let (success, message) = match result {
                Ok(())  => (true,  format!("✓ Mounted dev container for {}", identifier)),
                Err(e)  => (false, format!("✗ Mount failed for {}: {}", identifier, e)),
            };
            let _ = tx.send(BgMsg::MountResult { success, message, pkg_idx, mounted: true });
            ctx.request_repaint();
        });
    });
}

/// Unmount the dev container for `pkg_idx`.
pub fn spawn_unmount(
    tx:         mpsc::Sender<BgMsg>,
    ctx:        egui::Context,
    pkg_idx:    usize,
    org_id:     String,
    identifier: String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let result = unmount(&org_id, &identifier).await;
            let (success, message) = match result {
                Ok(())  => (true,  format!("✓ Unmounted dev container for {}", identifier)),
                Err(e)  => (false, format!("✗ Unmount failed for {}: {}", identifier, e)),
            };
            let _ = tx.send(BgMsg::MountResult { success, message, pkg_idx, mounted: false });
            ctx.request_repaint();
        });
    });
}

pub fn spawn_db_container_fetch(
    tx:         mpsc::Sender<BgMsg>,
    ctx:        egui::Context,
    schema_idx: usize,
    slug:       String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            if let Some((_pod, containers)) = get_pod_containers(&slug).await {
                let _ = tx.send(BgMsg::DbContainers { schema_idx, containers });
                ctx.request_repaint();
            }
        });
    });
}

pub fn spawn_container_fetch(
    tx:              mpsc::Sender<BgMsg>,
    ctx:             egui::Context,
    svc_idx:         usize,
    deployment_name: String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            if let Some((_pod, containers)) = get_pod_containers(&deployment_name).await {
                let _ = tx.send(BgMsg::Containers { svc_idx, containers });
                ctx.request_repaint();
            }
        });
    });
}

pub use spawn_service_logs as spawn_logs_for_container;

// ── Log stream helpers ────────────────────────────────────────────────────────
//
// Each log-streaming function now accepts a `CancellationToken`. The inner
// loop selects between a new log line and cancellation so the thread exits
// promptly when the user switches service/schema/container — instead of
// running forever and silently discarding every message it sends.

use tokio::sync::mpsc as async_mpsc;
use crate::shared::core::k8_info::stream_pod_logs;

pub fn spawn_service_logs(
    tx:              mpsc::Sender<BgMsg>,
    ctx:             egui::Context,
    deployment_name: String,
    container:       Option<String>,
    generation:      u64,
    cancel:          CancellationToken,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();
            let mut since_repaint: usize = 0;

            loop {
                // ── Check for cancellation before starting a new stream ────
                if cancel.is_cancelled() { return; }

                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();

                let dep  = deployment_name.clone();
                let cont = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });

                loop {
                    tokio::select! {
                        // Cancellation wins immediately — exit the thread.
                        _ = cancel.cancelled() => return,

                        msg = line_rx.recv() => {
                            match msg {
                                None => break, // stream ended, restart after delay
                                Some(line) => {
                                    lines.push(line);
                                    // Cap buffer to avoid unbounded memory growth.
                                    if lines.len() > 2000 {
                                        lines.drain(0..500);
                                    }
                                    since_repaint += 1;

                                    if tx.send(BgMsg::Logs {
                                        lines: lines.clone(),
                                        generation,
                                    }).is_err() {
                                        return; // app closed
                                    }

                                    // Only wake egui after every LOG_BATCH_SIZE lines,
                                    // or immediately for the very first line so the UI
                                    // doesn't stay blank.
                                    if since_repaint == 1 || since_repaint >= LOG_BATCH_SIZE {
                                        ctx.request_repaint();
                                        since_repaint = 0;
                                    }
                                }
                            }
                        }
                    }
                }

                // Stream ended — send final snapshot and repaint once.
                if tx.send(BgMsg::Logs { lines: lines.clone(), generation }).is_err() {
                    return;
                }
                ctx.request_repaint();
                since_repaint = 0;

                // Wait before reconnecting, but bail immediately if cancelled.
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = sleep(Duration::from_secs(2)) => {}
                }
            }
        });
    });
}

// ── spawn_db_schema_logs ──────────────────────────────────────────────────────

pub fn spawn_db_schema_logs(
    tx:         mpsc::Sender<BgMsg>,
    ctx:        egui::Context,
    schema_idx: usize,
    slug:       String,
    container:  Option<String>,
    generation: u64,
    cancel:     CancellationToken,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();
            let mut since_repaint: usize = 0;

            loop {
                // ── Check for cancellation before starting a new stream ────
                if cancel.is_cancelled() { return; }

                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();

                let dep  = slug.clone();
                let cont = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,

                        msg = line_rx.recv() => {
                            match msg {
                                None => break,
                                Some(line) => {
                                    lines.push(line);
                                    if lines.len() > 2000 {
                                        lines.drain(0..500);
                                    }
                                    since_repaint += 1;

                                    let normalised = if lines.len() == 1
                                        && lines[0].starts_with("No pods found")
                                    {
                                        vec![]
                                    } else {
                                        lines.clone()
                                    };

                                    if tx.send(BgMsg::DbSchemaLogs {
                                        lines: normalised,
                                        schema_idx,
                                        generation,
                                    }).is_err() {
                                        return;
                                    }

                                    if since_repaint == 1 || since_repaint >= LOG_BATCH_SIZE {
                                        ctx.request_repaint();
                                        since_repaint = 0;
                                    }
                                }
                            }
                        }
                    }
                }

                if tx.send(BgMsg::DbSchemaLogs {
                    lines: lines.clone(),
                    schema_idx,
                    generation,
                }).is_err() {
                    return;
                }
                ctx.request_repaint();
                since_repaint = 0;

                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = sleep(Duration::from_secs(2)) => {}
                }
            }
        });
    });
}