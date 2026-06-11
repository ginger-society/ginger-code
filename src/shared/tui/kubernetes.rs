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

    // Always explicit — multi-container pods 400 on container: None.
    let container_name: Option<String> = api
        .get(&pod_name)
        .await
        .ok()
        .and_then(|p| p.spec)
        .and_then(|s| s.containers.into_iter().next())
        .map(|c| c.name);

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // tty:true merges stderr into stdout at the pty level — that is correct
    // kernel behaviour. stderr:false is required when tty:true.
    // We exec the shell directly (no sh -c wrapper) so it is PID 1 and
    // receives signals (Ctrl+C etc.) directly.
    // The COLUMNS/LINES/stty args are passed via the shell's own -c flag
    // then we exec the interactive shell so that becomes PID 1.
    let init = format!(
        "stty rows {rows} cols {cols}; \
         export TERM=xterm-256color COLUMNS={cols} LINES={rows}; \
         exec \"$0\" -i",
    );

    for shell in &["/bin/bash", "/bin/sh"] {
        let ap = AttachParams {
            container: container_name.clone(),
            stdin:     true,
            stdout:    true,
            stderr:    false, // must be false when tty:true
            tty:       true,
            ..Default::default()
        };

        // shell -c "stty ...; exec shell -i"
        // "$0" expands to the shell binary itself so we don't hardcode it twice.
        let mut attached = match api
            .exec(&pod_name, vec![*shell, "-c", &init], &ap)
            .await
        {
            Ok(a)  => a,
            Err(e) => {
                eprintln!("exec {shell}: {e}");
                continue;
            }
        };

        let mut pod_stdout = match attached.stdout() {
            Some(s) => s,
            None    => { eprintln!("no stdout"); continue; }
        };
        let mut pod_stdin = match attached.stdin() {
            Some(s) => s,
            None    => { eprintln!("no stdin"); continue; }
        };

        crossterm::terminal::enable_raw_mode()?;

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        // stdin: local terminal → pod
        let stdin_task = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf   = [0u8; 256];
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
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

        // stdout (+ pty-merged stderr): pod → local terminal
        let mut out = tokio::io::stdout();
        let mut buf = [0u8; 4096];
        loop {
            match pod_stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = out.flush().await;
                }
            }
        }

        let _ = stop_tx.send(());
        let _ = stdin_task.await;

        crossterm::terminal::disable_raw_mode()?;
        eprintln!();
        return Ok(());
    }

    eprintln!("Could not start shell in pod {}", pod_name);
    Ok(())
}