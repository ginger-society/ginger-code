//! Centralised metadata fetches shared by the GUI (`bg.rs`) and TUI (`tui/mod.rs`).
//!
//! Both callers need the same two API calls mapped to the same structs; only
//! their error-handling strategy differs, so we expose the two fetches as
//! independent `async fn`s and let each caller decide what to do on failure.

use MetadataService::apis::{
        configuration::Configuration as MetadataConfiguration,
        default_api::{
            MetadataGetDbschemasAndTablesParams, MetadataGetDbschemasParams, MetadataGetServicesAndEnvsParams, MetadataGetUserPackagesParams, metadata_get_current_workspace, metadata_get_dbschemas, metadata_get_dbschemas_and_tables, metadata_get_services_and_envs, metadata_get_user_packages
        },
    };

use crate::shared::core::types::DbSchema;

use super::{
    k8_info::meta_to_deployment_name,
    types::{K8sService, Package},
};

// ── Error type ────────────────────────────────────────────────────────────────

pub type DataSourceError = Box<dyn std::error::Error + Send + Sync>;

// ── Package fetch ─────────────────────────────────────────────────────────────

/// Fetch and map all packages for the given org + env.
///
/// Returns `Err` only on a network / API failure; an empty list is `Ok(vec![])`.
pub async fn fetch_packages(
    config: &MetadataConfiguration,
    org_id: &str,
    env: &str,
) -> Result<Vec<Package>, DataSourceError> {
    let raw = metadata_get_user_packages(
        config,
        MetadataGetUserPackagesParams {
            org_id: org_id.to_string(),
            env: env.to_string(),
        },
    )
    .await?;

    let packages = raw
        .into_iter()
        .map(|p| Package {
            identifier: p.identifier,
            package_type: p.package_type,
            lang: p.lang,
            description: p.description,
            organization_id: p.organization_id,
            mounted: false,
            dependencies: p.dependencies,
        })
        .collect();

    Ok(packages)
}

// ── Service fetch ─────────────────────────────────────────────────────────────

/// Fetch and map all services/envs for the given org.
///
/// `page_size` is caller-controlled so the GUI (100) and TUI (50) can keep
/// their existing limits without hard-coding them here.
pub async fn fetch_services(
    config: &MetadataConfiguration,
    org_id: &str,
    page_size: u32,
) -> Result<Vec<K8sService>, DataSourceError> {
    let raw = metadata_get_services_and_envs(
        config,
        MetadataGetServicesAndEnvsParams {
            page_number: Some("1".to_string()),
            page_size: Some(page_size.to_string()),
            org_id: org_id.to_string(),
        },
    )
    .await?;

    let services = raw
        .iter()
        .map(|s| {
            let meta_name = s.identifier.to_string();
            let deployment_name = meta_to_deployment_name(&meta_name);
            let lang = s.lang.as_ref().and_then(|l| l.as_ref()).cloned();
            let pod_name = Some(deployment_name.to_lowercase().replace('_', "-"));
            K8sService {
                meta_name,
                organization_id: s.organization_id.clone(),
                deployment_name: Some(deployment_name),
                status: "Unknown".into(),
                ready: "–".into(),
                lang,
                ejected: false,
                ssh_host: pod_name,
            }
        })
        .collect();

    Ok(services)
}

pub async fn fetch_dbs(
    config:    &MetadataConfiguration,
    org_id:    &str,
    _page_size: u32,
) -> Result<Vec<DbSchema>, DataSourceError> {
    use super::types::DbSchema;

    let raw = metadata_get_dbschemas_and_tables(
        config,
        MetadataGetDbschemasAndTablesParams {
            org_id: org_id.to_string(),
            env:    "stage".to_string(),
        },
    )
    .await?;

    let schemas = raw
        .into_iter()
        .map(|s| DbSchema {
            id:              s.id,
            name:            s.name,
            identifier:      s.identifier.and_then(|o| o),
            db_type:         s.db_type.and_then(|o| o),
            organization_id: s.organization_id,
            tables:          s.tables,
            description:     s.description.and_then(|o| o),
            version:         s.version.and_then(|o| o),
            pipeline_status: s.pipeline_status.and_then(|o| o),
            updated_at:      s.updated_at,
        })
        .collect();

    Ok(schemas)
}


pub async fn fetch_current_workspace(
    config: &MetadataConfiguration,
) -> Result<String, DataSourceError> {
    let raw = metadata_get_current_workspace(
        config,
    )
    .await?;

    Ok(raw.org_id)
}