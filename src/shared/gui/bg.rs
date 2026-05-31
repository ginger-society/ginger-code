//! Background task helpers and channel message types.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use tokio::time::sleep;

use MetadataService::{
    apis::{
        default_api::{
            metadata_get_services_and_envs,
            metadata_get_user_packages,
            MetadataGetServicesAndEnvsParams,
            MetadataGetUserPackagesParams,
        },
    },
    get_configuration as get_metadata_configuration,
};
use ginger_shared_rs::utils::get_token_from_file_storage;

use crate::shared::tui::kubernetes::{get_k8s_deployments, get_pod_logs, is_ejected, meta_to_deployment_name};
use crate::shared::core::{mount, unmount};
use super::types::{K8sService, Package};

// ── Channel messages ──────────────────────────────────────────────────────────

pub enum BgMsg {
    Services(Vec<K8sService>),
    Packages(Vec<Package>),
    K8sStatuses(HashMap<String, (String, String)>),
    EjectedFlag { idx: usize, ejected: bool },
    Logs { lines: Vec<String>, generation: u64 },
    Error(String),
    EjectResult  { success: bool, message: String, idx: usize },
    /// Result of a mount or unmount operation for a package.
    MountResult  { success: bool, message: String, pkg_idx: usize, mounted: bool },
}

// ── Spawn helpers ─────────────────────────────────────────────────────────────

/// One-shot: fetch packages then services from the metadata API.
pub fn spawn_metadata_fetch(tx: mpsc::Sender<BgMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("tokio rt");

        rt.block_on(async move {
            let token           = get_token_from_file_storage();
            let metadata_config = get_metadata_configuration(Some(token));

            // ── Packages ────────────────────────────────────────────────────
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
                            description:     p.description,
                            organization_id: p.organization_id,
                            mounted:         false,
                            dependencies:    p.dependencies,
                        })
                        .collect();
                    let _ = tx.send(BgMsg::Packages(packages));
                    ctx.request_repaint();
                }
                Err(e) => {
                    // Non-fatal — services still load.
                    eprintln!("Package fetch error: {e:?}");
                }
            }

            // ── Services ────────────────────────────────────────────────────
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
                Err(e) => { let _ = tx.send(BgMsg::Error(format!("{e:?}"))); }
                Ok(raw) => {
                    let services = raw.iter().map(|s| {
                        let meta_name       = s.identifier.to_string();
                        let deployment_name = meta_to_deployment_name(&meta_name);
                        let lang            = s.lang.as_ref().and_then(|l| l.as_ref()).cloned();
                        let pod_name        = Some(deployment_name.to_lowercase().replace('_', "-"));
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
                    }).collect();
                    let _ = tx.send(BgMsg::Services(services));
                }
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
    tx:          mpsc::Sender<BgMsg>,
    ctx:         egui::Context,
    pkg_idx:     usize,
    org_id:      String,
    identifier:  String,
    lang:        String,
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
    tx:          mpsc::Sender<BgMsg>,
    ctx:         egui::Context,
    pkg_idx:     usize,
    org_id:      String,
    identifier:  String,
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