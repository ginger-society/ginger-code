// kubernetes.rs — full replacement

use std::sync::Arc;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use kube::api::AttachParams;

use crate::shared::core::k8s_exec::resolve_running_pod;

pub async fn shell_into_pod(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pod_name = match resolve_running_pod(deployment_name).await {
        Some(p) => p,
        None => {
            eprintln!("No running pod found for deployment: {}", deployment_name);
            return Ok(());
        }
    };

    let client = Client::try_default().await?;
    let api: Api<Pod> = Api::default_namespaced(client);

    // Try bash first, fall back to sh
    for shell in &["/bin/bash", "/bin/sh"] {
        let ap = AttachParams {
            container:     None,
            stdin:         true,
            stdout:        true,
            stderr:        false, // merged into stdout when tty:true
            tty:           true,
            ..Default::default()
        };

        let mut attached = match api.exec(&pod_name, vec![*shell], &ap).await {
            Ok(a)  => a,
            Err(_) => continue,
        };

        let mut pod_stdout = match attached.stdout() {
            Some(s) => s,
            None    => continue,
        };
        let mut pod_stdin = match attached.stdin() {
            Some(s) => s,
            None    => continue,
        };

        // Spawn stdin forwarder: terminal → pod
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 256];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if pod_stdin.write_all(&buf[..n]).await.is_err() { break; }
                    }
                }
            }
        });

        // stdout forwarder: pod → terminal (blocking here until pod closes)
        let mut stdout = tokio::io::stdout();
        let mut buf = [0u8; 4096];
        loop {
            match pod_stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() { break; }
                    let _ = stdout.flush().await;
                }
            }
        }

        return Ok(());
    }

    eprintln!("Could not start shell in pod {}", pod_name);
    Ok(())
}