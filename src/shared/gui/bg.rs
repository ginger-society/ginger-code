//! Background task helpers and channel message types.
//!
//! All network/async work is spawned here; results are sent back to the main
//! thread via `mpsc::Sender<BgMsg>`.  `app.rs` owns the receiver and drains it
//! every frame.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use tokio::time::sleep;

use MetadataService::{
    apis::{
        configuration::Configuration as MetadataConfiguration,
        default_api::{
            MetadataGetServicesAndEnvsParams, MetadataGetUserPackagesParams, MetadataGetUserPackagesUserLandParams, metadata_get_services_and_envs, metadata_get_user_packages, metadata_get_user_packages_public, metadata_get_user_packages_user_land
        },
    },
    get_configuration as get_metadata_configuration,
};
use ginger_shared_rs::utils::get_token_from_file_storage;

use crate::shared::ui::kubernetes::{get_k8s_deployments, get_pod_logs, is_ejected, meta_to_deployment_name};
use super::types::{K8sService, Package};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum BgMsg {
    /// Initial services list from metadata.
    Services(Vec<K8sService>),
    /// Packages fetched from metadata (not k8s-deployed).
    Packages(Vec<Package>),
    /// Periodic k8s status poll — deployment_name → (status, ready).
    K8sStatuses(HashMap<String, (String, String)>),
    /// Ejected flag update for one service by index.
    EjectedFlag { idx: usize, ejected: bool },
    /// Fresh log lines for the currently-selected service.
    /// `generation` must match `AppState::log_generation` to be applied.
    Logs { lines: Vec<String>, generation: u64 },
    /// Metadata or network error.
    Error(String),
    /// Result of an eject or uneject operation.
    EjectResult { success: bool, message: String, idx: usize },
}

// ── Spawn helpers ─────────────────────────────────────────────────────────────

/// One-shot: fetch services + packages from the metadata API.
/// Sends `BgMsg::Services`, `BgMsg::Packages`, or `BgMsg::Error`.
pub fn spawn_metadata_fetch(tx: mpsc::Sender<BgMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            let token           = get_token_from_file_storage();
            let metadata_config = get_metadata_configuration(Some(token));

            // ── Fetch packages ──────────────────────────────────────────────
            match metadata_get_user_packages(
                &metadata_config,
                MetadataGetUserPackagesParams {
                    org_id: "ginger-society".to_string(),
                    env:    "stage".to_string(),
                },
            )
            .await
            {
                Ok(raw) => {
                    let packages = raw
                        .into_iter()
                        .map(|p| Package {
                            identifier:      p.identifier,
                            package_type:    p.package_type,
                            lang:            p.lang,
                            version:         p.version,
                            description:     p.description,
                            organization_id: p.organization_id,
                        })
                        .collect();
                    let _ = tx.send(BgMsg::Packages(packages));
                    ctx.request_repaint();
                }
                Err(e) => {
                    // Non-fatal: log but continue so services still load.
                    eprintln!("Package fetch error: {e:?}");
                }
            }

            // ── Fetch services ──────────────────────────────────────────────
            match metadata_get_services_and_envs(
                &metadata_config,
                MetadataGetServicesAndEnvsParams {
                    page_number: Some("1".to_string()),
                    page_size:   Some("100".to_string()),
                    org_id:      "ginger-society".to_string(),
                },
            )
            .await
            {
                Err(e) => {
                    let _ = tx.send(BgMsg::Error(format!("{e:?}")));
                }
                Ok(raw) => {
                    let services = raw
                        .iter()
                        .map(|s| {
                            let meta_name       = s.identifier.to_string();
                            let deployment_name = meta_to_deployment_name(&meta_name);
                            let lang = s.lang
                                .as_ref()
                                .and_then(|l| l.as_ref())
                                .cloned();
                            let pod_name =
                                Some(deployment_name.to_lowercase().replace('_', "-"));
                            K8sService {
                                meta_name,
                                organization_id: s.organization_id.clone(),
                                deployment_name: Some(deployment_name),
                                status:  "Unknown".into(),
                                ready:   "–".into(),
                                lang,
                                ejected: false,
                                ssh_host: pod_name,
                            }
                        })
                        .collect();
                    let _ = tx.send(BgMsg::Services(services));
                }
            }
            ctx.request_repaint();
        });
    });
}

/// Infinite loop: poll k8s deployment statuses every 5 seconds.
/// Sends `BgMsg::K8sStatuses` on each tick.
pub fn spawn_k8s_poller(tx: mpsc::Sender<BgMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

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

/// Check ejected flag, then (if not ejected) fetch logs and start the
/// recurring log poller for `deployment_name`.
///
/// This is the single entry-point for all per-service refresh work.
/// `generation` is captured so stale results from old pollers are silently
/// discarded when the user switches services before results arrive.
pub fn spawn_service_refresh(
    tx:              mpsc::Sender<BgMsg>,
    ctx:             egui::Context,
    idx:             usize,
    deployment_name: String,
    generation:      u64,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            // Step 1: ejected check (always first — no racing with log poller).
            let ejected = is_ejected(&deployment_name).await;
            let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
            ctx.request_repaint();

            if ejected {
                // Do not start a log poller for ejected services.
                return;
            }

            // Step 2: first log fetch.
            let lines = get_pod_logs(&deployment_name).await;
            let _ = tx.send(BgMsg::Logs { lines, generation });
            ctx.request_repaint();

            // Step 3: recurring poll.
            loop {
                sleep(Duration::from_secs(2)).await;
                let lines = get_pod_logs(&deployment_name).await;
                if tx.send(BgMsg::Logs { lines, generation }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    });
}

/// Check the ejected flag for a batch of services (for sidebar badges).
/// Spawns ONE thread that checks each sequentially and sends an
/// `EjectedFlag` message per service.
pub fn spawn_bulk_ejected_check(
    tx:       mpsc::Sender<BgMsg>,
    ctx:      egui::Context,
    services: Vec<(usize, String)>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async move {
            for (idx, deployment_name) in services {
                let ejected = is_ejected(&deployment_name).await;
                let _ = tx.send(BgMsg::EjectedFlag { idx, ejected });
                ctx.request_repaint();
            }
        });
    });
}