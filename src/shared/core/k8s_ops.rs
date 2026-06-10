//! Low-level kube-rs / pod helpers shared by eject and mount.
//!
//! All operations use kube-rs typed APIs — no kubectl subprocesses remain.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, Pod, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client};

use super::k8s_exec::{sh_exec, sh_output};

// ── Client helper ─────────────────────────────────────────────────────────────

async fn client() -> Client {
    Client::try_default().await.expect("kube client")
}

// ── PVC creation ──────────────────────────────────────────────────────────────

/// Apply a `PersistentVolumeClaim` via the kube-rs API (idempotent).
pub async fn apply_pvc(
    pvc_name:     &str,
    storage_size: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let api: Api<PersistentVolumeClaim> = Api::default_namespaced(client().await);

    if api.get(pvc_name).await.is_ok() {
        println!("  PVC '{}' already exists, skipping", pvc_name);
        return Ok(());
    }

    let mut requests = BTreeMap::new();
    requests.insert("storage".to_string(), Quantity(storage_size.to_string()));

    let pvc = PersistentVolumeClaim {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(pvc_name.to_string()),
            ..Default::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(requests),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    api.create(&PostParams::default(), &pvc).await?;
    println!("✓ Created PVC '{}'", pvc_name);
    Ok(())
}

/// Delete a `PersistentVolumeClaim`; ignores "not found" errors.
pub async fn delete_pvc(pvc_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let api: Api<PersistentVolumeClaim> = Api::default_namespaced(client().await);

    match api.delete(pvc_name, &DeleteParams::default()).await {
        Ok(_) => println!("✓ Deleted PVC '{}'", pvc_name),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            println!("  PVC '{}' not found, skipping", pvc_name);
        }
        Err(e) => eprintln!("Warning: could not delete PVC '{}': {}", pvc_name, e),
    }
    Ok(())
}

// ── Deployment helpers ────────────────────────────────────────────────────────

/// Delete a deployment; ignores "not found" errors.
pub async fn delete_deployment(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let api: Api<Deployment> = Api::default_namespaced(client().await);

    match api.delete(deployment_name, &DeleteParams::default()).await {
        Ok(_) => println!("✓ Deleted deployment '{}'", deployment_name),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            println!("  Deployment '{}' not found, skipping", deployment_name);
        }
        Err(e) => {
            return Err(
                format!("Could not delete deployment '{}': {}", deployment_name, e).into(),
            )
        }
    }
    Ok(())
}

/// Read a field from a Deployment using common jsonpath patterns.
///
/// All patterns are resolved via typed struct traversal — no kubectl subprocess.
///
/// Supported patterns:
///   .metadata.annotations['key']
///   .spec.template.metadata.annotations['key']
///   .spec.template.spec.containers[?(@.name=='<name>')].image
///   .spec.template.spec.containers[?(@.ports[0].containerPort==22)].name
pub async fn get_deployment_annotation(
    deployment_name: &str,
    jsonpath:        &str,
) -> Option<String> {
    let api: Api<Deployment> = Api::default_namespaced(client().await);
    let d = api.get(deployment_name).await.ok()?;

    // Pattern 1: .metadata.annotations['key']
    if let Some(rest) = jsonpath.strip_prefix(".metadata.annotations['") {
        if let Some(key) = rest.strip_suffix("']") {
            return d.metadata.annotations.as_ref()?.get(key).cloned();
        }
    }

    // Pattern 2: .spec.template.metadata.annotations['key']
    if let Some(rest) = jsonpath.strip_prefix(".spec.template.metadata.annotations['") {
        if let Some(key) = rest.strip_suffix("']") {
            return d
                .spec.as_ref()?
                .template
                .metadata.as_ref()?
                .annotations.as_ref()?
                .get(key)
                .cloned();
        }
    }

    // Pattern 3: .spec.template.spec.containers[?(@.name=='<name>')].image
    if let Some(rest) =
        jsonpath.strip_prefix(".spec.template.spec.containers[?(@.name=='")
    {
        if let Some(name) = rest.strip_suffix("')].image") {
            return d
                .spec.as_ref()?
                .template
                .spec.as_ref()?
                .containers
                .iter()
                .find(|c| c.name == name)?
                .image
                .clone();
        }
    }

    // Pattern 4: port-by-value → container name
    // .spec.template.spec.containers[?(@.ports[0].containerPort==22)].name
    if jsonpath.contains("containerPort==22") {
        return d
            .spec.as_ref()?
            .template
            .spec.as_ref()?
            .containers
            .iter()
            .find(|c| {
                c.ports.as_ref().map_or(false, |ports| {
                    ports.iter().any(|p| p.container_port == 22)
                })
            })
            .map(|c| c.name.clone());
    }

    None
}

// ── Pod scheduling ────────────────────────────────────────────────────────────

/// Poll via kube-rs until a non-terminating, non-failed pod for
/// `deployment_name` is scheduled. Returns the pod name.
pub async fn wait_for_pod_scheduled(
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let api: Api<Pod> = Api::default_namespaced(client().await);
    let lp = ListParams::default().labels(&format!("app={}", deployment_name));

    for attempt in 1..=40 {
        let list = api.list(&lp).await?;

        if let Some(name) = list
            .items
            .into_iter()
            .find(|p| {
                p.metadata.deletion_timestamp.is_none()
                    && p.status.as_ref().and_then(|s| s.phase.as_deref())
                        != Some("Failed")
            })
            .and_then(|p| p.metadata.name)
        {
            return Ok(name);
        }

        println!(
            "  … pod not scheduled yet (attempt {}), retrying in 3s",
            attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod for '{}'", deployment_name).into())
}

/// Poll until the named pod has `condition=Ready`. Replaces `kubectl wait`.
pub async fn wait_for_pod_ready(
    pod_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let api: Api<Pod> = Api::default_namespaced(client().await);

    for attempt in 1..=100 {
        let pod = api.get(pod_name).await?;

        let ready = pod
            .status.as_ref()
            .and_then(|s| s.conditions.as_ref())
            .map(|conds| {
                conds
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
            })
            .unwrap_or(false);

        if ready {
            println!("✓ Pod ready: {}", pod_name);
            return Ok(());
        }

        println!(
            "  … pod not ready yet (attempt {}), retrying in 3s",
            attempt
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for pod '{}' to be Ready", pod_name).into())
}

// ── Workspace emptiness check ─────────────────────────────────────────────────

pub async fn is_workspace_empty(
    pod_name:  &str,
    container: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    // sh_output returns None when stdout is empty — workspace is empty.
    let out = sh_output(
        pod_name,
        container,
        "find /workspace -mindepth 1 -maxdepth 1 | head -1",
    )
    .await;

    Ok(out.is_none())
}

// ── SSH principal ─────────────────────────────────────────────────────────────

pub async fn write_ssh_principal(
    pod_name:     &str,
    container:    &str,
    session_user: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cmd = format!(
        "mkdir -p /etc/ssh/auth_principals && \
         chown root:root /etc/ssh/auth_principals && \
         chmod 755 /etc/ssh/auth_principals && \
         echo '{session_user}' > /etc/ssh/auth_principals/dev && \
         chown root:root /etc/ssh/auth_principals/dev && \
         chmod 644 /etc/ssh/auth_principals/dev",
    );

    let ok = sh_exec(pod_name, container, &cmd)
        .await
        .map_err(|e| format!(
            "Failed to write SSH principal '{}' into pod {}: {}",
            session_user, pod_name, e
        ))?;

    if !ok {
        return Err(format!(
            "Failed to write SSH principal '{}' into pod {}",
            session_user, pod_name
        )
        .into());
    }

    println!("✓ SSH principal '{}' written", session_user);
    Ok(())
}