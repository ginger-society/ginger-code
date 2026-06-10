// kubernetes.rs

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

    // Get the current terminal dimensions so the remote shell
    // has the right size from the start (avoids immediate exit with tty:true).
    let (cols, rows) = crossterm::terminal::size().unwrap_or((220, 50));

    // Wrap in sh -c so we can set TERM + stty before exec'ing the real shell.
    // This mirrors what attach_to_pod() does and prevents the shell from
    // exiting immediately due to missing TERM / zero-size pty.
    let shell_cmd = format!(
        "export TERM=xterm-256color COLUMNS={cols} LINES={rows}; \
         stty rows {rows} cols {cols} 2>/dev/null; \
         exec /bin/bash 2>/dev/null || exec /bin/sh",
        cols = cols,
        rows = rows,
    );

    let ap = AttachParams {
        container: None,
        stdin:     true,
        stdout:    true,
        stderr:    false, // merged into stdout when tty:true
        tty:       true,
        ..Default::default()
    };

    let mut attached = match api
        .exec(&pod_name, vec!["sh", "-c", &shell_cmd], &ap)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Could not exec into pod {}: {}", pod_name, e);
            return Ok(());
        }
    };

    let mut pod_stdout = match attached.stdout() {
        Some(s) => s,
        None => {
            eprintln!("No stdout on exec attach for pod {}", pod_name);
            return Ok(());
        }
    };
    let mut pod_stdin = match attached.stdin() {
        Some(s) => s,
        None => {
            eprintln!("No stdin on exec attach for pod {}", pod_name);
            return Ok(());
        }
    };

    // Enter raw mode so every keystroke is forwarded immediately and
    // signals (Ctrl+C etc.) pass through to the pod rather than the host.
    crossterm::terminal::enable_raw_mode()?;

    // Oneshot to stop the stdin task when the pod closes stdout.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    // Stdin forwarder: local terminal → pod
    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf   = [0u8; 256];
        let mut stop  = stop_rx;
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                result = stdin.read(&mut buf) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if pod_stdin.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    });

    // Stdout forwarder: pod → local terminal  (blocks until pod closes)
    let mut stdout = tokio::io::stdout();
    let mut buf    = [0u8; 4096];
    loop {
        match pod_stdout.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                let _ = stdout.flush().await;
            }
        }
    }

    // Tell the stdin task to stop, then wait for it to finish.
    let _ = stop_tx.send(());
    let _ = stdin_task.await;

    // Restore cooked mode before the TUI resumes.
    crossterm::terminal::disable_raw_mode()?;

    Ok(())
}