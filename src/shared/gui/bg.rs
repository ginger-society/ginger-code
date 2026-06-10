//! Background task helpers and channel message types.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use tokio::time::sleep;

use ginger_shared_rs::utils::get_token_from_file_storage;
use MetadataService::get_configuration as get_metadata_configuration;

use crate::shared::core::{
    data_source::{fetch_current_workspace, fetch_dbs, fetch_dbs_enriched, fetch_packages, fetch_services}, k8_info::{get_k8s_deployments, get_pod_containers, get_transitioning_deployments, is_ejected}, k8s_ops::get_deployment_annotation, mount, types::{DbSchema, K8sService, Package}, unmount
};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum BgMsg {
    Services(Vec<K8sService>),
    Packages(Vec<Package>),
    DbSchemas(Vec<DbSchema>),
    K8sStatuses(HashMap<String, (String, String)>),
    EjectedFlag { idx: usize, ejected: bool, ejected_container: Option<String> },
    Logs { lines: Vec<String>, generation: u64 },
    /// Logs for the selected DB schema deployment (empty vec = no deployment found).
    DbSchemaLogs { lines: Vec<String>, schema_idx: usize, generation: u64 },
    Error(String),
    EjectResult { success: bool, message: String, idx: usize },
    /// Result of a mount or unmount operation for a package.
    MountResult { success: bool, message: String, pkg_idx: usize, mounted: bool },
    /// Container names for a service's running pod.
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

             // We hardcode "stage" here since our current MetadataService doesn't support multiple envs;
             // this will need to be revisited if/when we add that support.

            // ── Packages (non-fatal) ──────────────────────────────────────
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

            // ── Services ─────────────────────────────────────────────────
            match fetch_services(&config, &org_id, 100).await {
                Ok(services) => {
                    let _ = tx.send(BgMsg::Services(services));
                }
                Err(e) => {
                    let _ = tx.send(BgMsg::Error(format!("{e:?}")));
                }
            }

            // ── DB Schemas (non-fatal) ────────────────────────────────────
            match fetch_dbs_enriched(&config, &org_id).await {
                Ok(schemas) => {
                    let _ = tx.send(BgMsg::DbSchemas(schemas));
                    ctx.request_repaint();
                }
                Err(e) => eprintln!("DB schema fetch error: {e:?}"),
            }

            ctx.request_repaint();
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
                let deployments    = get_k8s_deployments().await;
                let transitioning  = get_transitioning_deployments().await;   // ← new
                let _ = tx.send(BgMsg::K8sStatuses(deployments));
                let _ = tx.send(BgMsg::TransitioningSet(transitioning));       // ← new
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
            // No log polling here at all — EjectedFlag handler starts
            // container fetch, Containers handler starts the log poller
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

/// Poll logs for a DB schema's deployment (by identifier slug).
/// Sends `DbSchemaLogs` with an empty vec if no deployment exists.
/// Runs until the sender is dropped (i.e. the user switches away).


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


// ── Replacement log spawners for bg.rs ───────────────────────────────────────
//
// Drop-in replacements for spawn_service_logs, spawn_logs_for_container,
// and spawn_db_schema_logs. Everything else in bg.rs stays the same.
//
// How it works:
//   1. An unbounded channel (line_tx / line_rx) is created per spawn call.
//   2. A tokio task calls stream_pod_logs, which seeds 500 tail lines then
//      follows live. Each line is sent on line_tx.
//   3. A second tokio task drains line_rx, appends to a local Vec<String>,
//      sends a BgMsg::Logs (snapshot of all lines so far) on every new line,
//      and requests a repaint.
//   4. When the stream ends (pod restart, container switch, generation bump)
//      stream_pod_logs returns. The spawner sleeps 2 s and retries — the
//      line_rx drain task exits when line_tx is dropped (i.e. on retry or
//      when the outer thread exits because the mpsc::Sender was dropped).
//
// Generation gating (same as before):
//   BgMsg::Logs carries `generation`; app.rs drops messages whose generation
//   doesn't match the current one, killing stale pollers automatically.

use tokio::sync::mpsc as async_mpsc;

use crate::shared::core::k8_info::stream_pod_logs;


pub fn spawn_service_logs(
    tx:              mpsc::Sender<BgMsg>,
    ctx:             egui::Context,
    deployment_name: String,
    container:       Option<String>,
    generation:      u64,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();

            loop {
                // Fresh channel each attempt so old line_tx drops cleanly.
                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();

                // Stream task — runs until pod stream ends or line_tx is dropped.
                let dep   = deployment_name.clone();
                let cont  = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });

                // Drain task — forwards each line to the UI immediately.
                loop {
                    match line_rx.recv().await {
                        None => break, // stream ended
                        Some(line) => {
                            lines.push(line);
                            // Cap buffer so the UI doesn't grow unbounded.
                            if lines.len() > 2000 {
                                lines.drain(0..500);
                            }
                            if tx.send(BgMsg::Logs {
                                lines: lines.clone(),
                                generation,
                            }).is_err() {
                                return; // app closed
                            }
                            ctx.request_repaint();
                        }
                    }
                }

                // Stream ended (pod restarted etc.) — check if still wanted,
                // then wait before reconnecting.
                if tx.send(BgMsg::Logs { lines: lines.clone(), generation }).is_err() {
                    return;
                }
                sleep(Duration::from_secs(2)).await;
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
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let mut lines: Vec<String> = Vec::new();

            loop {
                let (line_tx, mut line_rx) = async_mpsc::unbounded_channel::<String>();

                let dep  = slug.clone();
                let cont = container.clone();
                tokio::spawn(async move {
                    stream_pod_logs(&dep, cont, line_tx).await;
                });

                loop {
                    match line_rx.recv().await {
                        None => break,
                        Some(line) => {
                            lines.push(line);
                            if lines.len() > 2000 {
                                lines.drain(0..500);
                            }
                            // Normalise "no pods" sentinel to empty vec so the
                            // UI shows "No deployment found" rather than an error.
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
                            ctx.request_repaint();
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
                sleep(Duration::from_secs(2)).await;
            }
        });
    });
}