//! Background task helpers and channel message types.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use tokio::time::sleep;

use ginger_shared_rs::utils::get_token_from_file_storage;
use MetadataService::get_configuration as get_metadata_configuration;

use crate::shared::core::{
    data_source::{fetch_current_workspace, fetch_dbs, fetch_dbs_enriched, fetch_packages, fetch_services}, k8_info::{get_k8s_deployments, get_pod_logs, is_ejected}, mount, types::{DbSchema, K8sService, Package}, unmount
};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum BgMsg {
    Services(Vec<K8sService>),
    Packages(Vec<Package>),
    DbSchemas(Vec<DbSchema>),
    K8sStatuses(HashMap<String, (String, String)>),
    EjectedFlag { idx: usize, ejected: bool },
    Logs { lines: Vec<String>, generation: u64 },
    /// Logs for the selected DB schema deployment (empty vec = no deployment found).
    DbSchemaLogs { lines: Vec<String>, schema_idx: usize },
    Error(String),
    EjectResult { success: bool, message: String, idx: usize },
    /// Result of a mount or unmount operation for a package.
    MountResult { success: bool, message: String, pkg_idx: usize, mounted: bool },
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
                let deployments = get_k8s_deployments().await;
                let _ = tx.send(BgMsg::K8sStatuses(deployments));
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
            let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
            ctx.request_repaint();

            if ejected { return; }

            let lines = get_pod_logs(&deployment_name).await;
            let _ = tx.send(BgMsg::Logs { lines, generation });
            ctx.request_repaint();

            loop {
                sleep(Duration::from_secs(2)).await;
                let lines = get_pod_logs(&deployment_name).await;
                if tx.send(BgMsg::Logs { lines, generation }).is_err() { break; }
                ctx.request_repaint();
            }
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
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
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
pub fn spawn_db_schema_logs(
    tx:         mpsc::Sender<BgMsg>,
    ctx:        egui::Context,
    schema_idx: usize,
    slug:       String,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            loop {
                let lines = get_pod_logs(&slug).await;
                // get_pod_logs returns a single "No pods found" string when absent —
                // we normalise that into our "no deployment" indicator.
                let normalised = if lines.len() == 1
                    && (lines[0].starts_with("No pods found") || lines[0].starts_with("No pods found for deployment"))
                {
                    vec![]
                } else {
                    lines
                };

                if tx.send(BgMsg::DbSchemaLogs { lines: normalised, schema_idx }).is_err() {
                    break;
                }
                ctx.request_repaint();
                sleep(Duration::from_secs(3)).await;
            }
        });
    });
}