//! Low-level kube-rs / pod helpers shared by eject and mount.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, Pod, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::Api;

use super::k8s_client::{get_client, handle_unauthorized, is_unauthorized};
use super::k8s_exec::{sh_exec, sh_output};

// ── PVC creation ──────────────────────────────────────────────────────────────

pub async fn apply_pvc(
    pvc_name:     &str,
    storage_size: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = get_client().await;
    let api: Api<PersistentVolumeClaim> = Api::default_namespaced(client);

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

pub async fn delete_pvc(pvc_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = get_client().await;
    let api: Api<PersistentVolumeClaim> = Api::default_namespaced(client);

    match api.delete(pvc_name, &DeleteParams::default()).await {
        Ok(_) => println!("✓ Deleted PVC '{}'", pvc_name),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            println!("  PVC '{}' not found, skipping", pvc_name);
        }
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            return Err(format!("Unauthorized deleting PVC '{}' — credentials refreshed, please retry", pvc_name).into());
        }
        Err(e) => eprintln!("Warning: could not delete PVC '{}': {}", pvc_name, e),
    }
    Ok(())
}

// ── Deployment helpers ────────────────────────────────────────────────────────

pub async fn delete_deployment(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = get_client().await;
    let api: Api<Deployment> = Api::default_namespaced(client);

    match api.delete(deployment_name, &DeleteParams::default()).await {
        Ok(_) => println!("✓ Deleted deployment '{}'", deployment_name),
        Err(kube::Error::Api(e)) if e.code == 404 => {
            println!("  Deployment '{}' not found, skipping", deployment_name);
        }
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            return Err(format!("Unauthorized — credentials refreshed, please retry").into());
        }
        Err(e) => {
            return Err(format!("Could not delete deployment '{}': {}", deployment_name, e).into());
        }
    }
    Ok(())
}

pub async fn get_deployment_annotation(
    deployment_name: &str,
    jsonpath:        &str,
) -> Option<String> {
    for attempt in 0..2u8 {
        let client = get_client().await;
        let api: Api<Deployment> = Api::default_namespaced(client);
        match api.get(deployment_name).await {
            Ok(d) => return extract_jsonpath(&d, jsonpath),
            Err(ref e) if is_unauthorized(e) && attempt == 0 => {
                handle_unauthorized().await;
            }
            Err(_) => return None,
        }
    }
    None
}

fn extract_jsonpath(d: &Deployment, jsonpath: &str) -> Option<String> {
    // Pattern 1: .metadata.annotations['key']
    if let Some(rest) = jsonpath.strip_prefix(".metadata.annotations['") {
        if let Some(key) = rest.strip_suffix("']") {
            return d.metadata.annotations.as_ref()?.get(key).cloned();
        }
    }

    // Pattern 2: .spec.template.metadata.annotations['key']
    if let Some(rest) = jsonpath.strip_prefix(".spec.template.metadata.annotations['") {
        if let Some(key) = rest.strip_suffix("']") {
            return d.spec.as_ref()?
                .template.metadata.as_ref()?
                .annotations.as_ref()?
                .get(key).cloned();
        }
    }

    // Pattern 3: .spec.template.spec.containers[?(@.name=='<name>')].image
    if let Some(rest) = jsonpath.strip_prefix(".spec.template.spec.containers[?(@.name=='") {
        if let Some(name) = rest.strip_suffix("')].image") {
            return d.spec.as_ref()?
                .template.spec.as_ref()?
                .containers.iter()
                .find(|c| c.name == name)?
                .image.clone();
        }
    }

    // Pattern 4: port-by-value → container name
    if jsonpath.contains("containerPort==22") {
        return d.spec.as_ref()?
            .template.spec.as_ref()?
            .containers.iter()
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

pub async fn wait_for_pod_scheduled(
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let lp = ListParams::default().labels(&format!("app={}", deployment_name));

    for attempt in 1..=40 {
        let client = get_client().await;
        let api: Api<Pod> = Api::default_namespaced(client);

        match api.list(&lp).await {
            Ok(list) => {
                if let Some(name) = list.items.into_iter()
                    .find(|p| {
                        p.metadata.deletion_timestamp.is_none()
                            && p.status.as_ref()
                                .and_then(|s| s.phase.as_deref())
                                != Some("Failed")
                    })
                    .and_then(|p| p.metadata.name)
                {
                    return Ok(name);
                }
            }
            Err(ref e) if is_unauthorized(e) => {
                handle_unauthorized().await;
            }
            Err(e) => return Err(e.into()),
        }

        println!("  … pod not scheduled yet (attempt {}), retrying in 3s", attempt);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod for '{}'", deployment_name).into())
}

pub async fn wait_for_pod_ready(
    pod_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 1..=100 {
        let client = get_client().await;
        let api: Api<Pod> = Api::default_namespaced(client);

        match api.get(pod_name).await {
            Ok(pod) => {
                let ready = pod.status.as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .map(|conds| conds.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
                    .unwrap_or(false);

                if ready {
                    println!("✓ Pod ready: {}", pod_name);
                    return Ok(());
                }
            }
            Err(ref e) if is_unauthorized(e) => {
                handle_unauthorized().await;
            }
            Err(e) => return Err(e.into()),
        }

        println!("  … pod not ready yet (attempt {}), retrying in 3s", attempt);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for pod '{}' to be Ready", pod_name).into())
}

// ── Workspace emptiness check ─────────────────────────────────────────────────

pub async fn is_workspace_empty(
    pod_name:  &str,
    container: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
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
        .map_err(|e| format!("Failed to write SSH principal: {}", e))?;

    if !ok {
        return Err(format!(
            "Failed to write SSH principal '{}' into pod {}",
            session_user, pod_name
        ).into());
    }

    println!("✓ SSH principal '{}' written", session_user);
    Ok(())
}

// ── Pod image inspection ──────────────────────────────────────────────────────

pub async fn wait_for_pod_running_image(
    deployment_name: &str,
    container_name:  &str,
    expected_image:  &str,
    timeout:         std::time::Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    let lp    = ListParams::default().labels(&format!("app={}", deployment_name));
    let start = std::time::Instant::now();
    let mut last_seen_image: Option<String> = None;

    loop {
        if start.elapsed() > timeout {
            return Err(format!(
                "Timed out after {:?} waiting for '{}' container '{}' to run image '{}' \
                 (last seen: {:?})",
                timeout, deployment_name, container_name, expected_image, last_seen_image
            ).into());
        }

        let client = get_client().await;
        let api: Api<Pod> = Api::default_namespaced(client);

        match api.list(&lp).await {
            Ok(list) => {
                let matching = list.items.into_iter()
                    .filter(|p| {
                        p.metadata.deletion_timestamp.is_none()
                            && p.status.as_ref()
                                .and_then(|s| s.phase.as_deref())
                                != Some("Failed")
                    })
                    .find(|p| {
                        p.spec.as_ref()
                            .and_then(|s| s.containers.iter().find(|c| c.name == container_name))
                            .and_then(|c| c.image.as_deref())
                            == Some(expected_image)
                    });

                if let Some(pod) = matching {
                    let pod_name = pod.metadata.name.clone().ok_or("pod missing metadata.name")?;
                    let ready = pod.status.as_ref()
                        .and_then(|s| s.conditions.as_ref())
                        .map(|conds| conds.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
                        .unwrap_or(false);

                    if ready {
                        println!("✓ Pod '{}' running expected image and ready", pod_name);
                        return Ok(pod_name);
                    }

                    last_seen_image = Some(expected_image.to_string());
                    println!("  … '{}' has new image but isn't Ready yet", pod_name);
                } else {
                    println!(
                        "  … no pod running '{}' yet (last seen: {:?}), retrying in 3s",
                        expected_image, last_seen_image
                    );
                }
            }
            Err(ref e) if is_unauthorized(e) => {
                handle_unauthorized().await;
            }
            Err(e) => return Err(e.into()),
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}