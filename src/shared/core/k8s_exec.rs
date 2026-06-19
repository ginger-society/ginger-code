//! kube-rs based exec / attach helpers.
//!
//! Two modes:
//!   * `exec_in_pod`   — one-shot command, captures stdout/stderr, returns output.
//!   * `attach_to_pod` — interactive PTY session wired to a `TermPerformer`
//!                       (replaces the kubectl-exec + portable_pty approach in terminal.rs).

use std::sync::Arc;
use super::k8s_client::{get_client, handle_unauthorized, is_unauthorized};

use k8s_openapi::api::core::v1::Pod;
use kube::api::{AttachParams, ListParams};
use kube::{Api, Client};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::shared::gui::terminal::{SshSession, TermPerformer};

// ── Client helper ─────────────────────────────────────────────────────────────

async fn client() -> Client {
    Client::try_default().await.expect("kube client")
}

// ── Pod resolution ────────────────────────────────────────────────────────────

pub async fn resolve_running_pod(deployment_name: &str) -> Option<String> {
    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    let label_strategies = [
        format!("app={}", deployment_name),
        format!("app.kubernetes.io/instance={}", deployment_name),
        format!("app.kubernetes.io/name={}", deployment_name),
    ];

    for label in &label_strategies {
        let lp = ListParams::default().labels(label);
        match api.list(&lp).await {
            Ok(list) => {
                if let Some(name) = list.items.into_iter()
                    .find(|p| {
                        p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running")
                            && p.metadata.deletion_timestamp.is_none()
                    })
                    .and_then(|p| p.metadata.name)
                {
                    return Some(name);
                }
            }
            Err(ref e) if is_unauthorized(e) => {
                handle_unauthorized().await;
                return None;
            }
            Err(_) => {}
        }
    }

    match api.list(&ListParams::default()).await {
        Ok(all) => all.items.into_iter()
            .find(|p| {
                let matches = p.metadata.name.as_deref()
                    .map(|n| n == deployment_name || n.starts_with(&format!("{}-", deployment_name)))
                    .unwrap_or(false);
                let running = p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running");
                matches && running && p.metadata.deletion_timestamp.is_none()
            })
            .and_then(|p| p.metadata.name),
        Err(ref e) if is_unauthorized(e) => {
            handle_unauthorized().await;
            None
        }
        Err(_) => None,
    }
}

// ── One-shot exec ─────────────────────────────────────────────────────────────

/// Run a command in a pod container and return (stdout, stderr, success).
///
/// `command` is a slice of owned strings so callers can pass either
/// `&["sh", "-c", "..."]` or a `Vec<String>` — the `.iter().map(|s| s.as_str())`
/// pattern is handled internally.
pub async fn exec_in_pod(
    pod_name:  &str,
    container: &str,
    command:   &[&str],
) -> Result<(String, String, bool), Box<dyn std::error::Error + Send + Sync>> {
    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    let ap = AttachParams {
        container: Some(container.to_string()),
        stdin:     false,
        stdout:    true,
        stderr:    true,
        tty:       false,
        ..Default::default()
    };

    // api.exec expects IntoIterator<Item: Into<String>>.
    // Convert &[&str] → Vec<String> so the bound is satisfied cleanly.
    let cmd: Vec<String> = command.iter().map(|s| s.to_string()).collect();
    let mut attached = api.exec(pod_name, cmd, &ap).await?;

    let stdout_str = match attached.stdout() {
        Some(mut r) => {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).await?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        None => String::new(),
    };

    let stderr_str = match attached.stderr() {
        Some(mut r) => {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).await?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        None => String::new(),
    };

    let success = match attached.take_status() {
        Some(status_fut) => status_fut
            .await
            .and_then(|s| s.status)
            .map(|s| s == "Success")
            .unwrap_or(false),
        None => true,
    };

    Ok((stdout_str, stderr_str, success))
}

/// Run `sh -c "<cmd>"` and return whether it succeeded.
pub async fn sh_exec(
    pod_name:  &str,
    container: &str,
    cmd:       &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let (_, stderr, ok) = exec_in_pod(pod_name, container, &["sh", "-c", cmd]).await?;
    if !ok && !stderr.trim().is_empty() {
        eprintln!("  [exec stderr] {}", stderr.trim());
    }
    Ok(ok)
}

/// Run `sh -c "<cmd>"` and return trimmed stdout, or `None` if empty/failed.
pub async fn sh_output(
    pod_name:  &str,
    container: &str,
    cmd:       &str,
) -> Option<String> {
    let (stdout, _, ok) = exec_in_pod(pod_name, container, &["sh", "-c", cmd])
        .await
        .ok()?;
    let s = stdout.trim().to_string();
    if ok && !s.is_empty() { Some(s) } else { None }
}

// ── Interactive PTY attach ────────────────────────────────────────────────────

/// Attach to a running pod with a TTY and wire it to `performer`.
/// Returns an `SshSession` whose `writer` sends keystrokes into the pod.
pub async fn attach_to_pod(
    deployment_name: &str,
    rows:            u16,
    cols:            u16,
    performer:       Arc<Mutex<TermPerformer>>,
    ctx:             eframe::egui::Context,
    container:       Option<String>,
) -> Result<SshSession, Box<dyn std::error::Error>> {
    let client = get_client().await;
    let api: Api<Pod> = Api::default_namespaced(client);

    let pod_name = resolve_running_pod(deployment_name)
        .await
        .ok_or_else(|| format!("No running pod found for '{}'", deployment_name))?;

    let container_name = container.unwrap_or_else(|| {
        deployment_name.to_lowercase().replace('_', "-")
    });

    // terminal_size is not a field in this version of kube-rs — send an
    // in-band stty resize via the shell command instead.
    let ap = AttachParams {
        container: Some(container_name),
        stdin:     true,
        stdout:    true,
        stderr:    false, // merged into stdout when tty:true
        tty:       true,
        ..Default::default()
    };

    let shell_cmd: Vec<String> = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "export COLUMNS={cols} LINES={rows} TERM=xterm-256color \
             PYTHONDONTWRITEBYTECODE=1; \
             stty rows {rows} cols {cols}; \
             exec /bin/sh -i",
        ),
    ];

    let mut attached = api.exec(&pod_name, shell_cmd, &ap).await?;

    let mut pod_stdout = attached.stdout().ok_or("no stdout on attach")?;

    // stdin() returns `impl AsyncWrite + Unpin` (not `Send`).
    // We need it in a Mutex for the SyncWriter, so box it first.
    let pod_stdin = attached.stdin().ok_or("no stdin on attach")?;
    let pod_stdin_box: Box<dyn tokio::io::AsyncWrite + Unpin + Send> =
        Box::new(UnsendWrapper(pod_stdin));

    let handle = tokio::runtime::Handle::current(); // valid here — we're inside rt.spawn
    let writer_arc: Arc<Mutex<Box<dyn std::io::Write + Send>>> =
        Arc::new(Mutex::new(Box::new(SyncWriter(
            Arc::new(tokio::sync::Mutex::new(pod_stdin_box)),
            handle,
        ))));

    tokio::spawn(async move {
        let mut parser = vte::Parser::new();
        let mut buf    = [0u8; 4096];
        loop {
            match pod_stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut p = performer.lock();
                    for &b in &buf[..n] {
                        parser.advance(&mut *p, b);
                    }
                    drop(p);
                    ctx.request_repaint();
                }
            }
        }
    });

    Ok(SshSession::from_kube(writer_arc))
}

// ── UnsendWrapper ─────────────────────────────────────────────────────────────
//
// kube-rs stdin() returns `impl AsyncWrite + Unpin` without a `Send` bound.
// We need `Send` for the Mutex inside SyncWriter. Since we only ever write
// from one task at a time (the GUI thread via block_in_place), this is safe.

struct UnsendWrapper<T>(T);

// SAFETY: we ensure single-threaded access via the Mutex in SyncWriter.
unsafe impl<T> Send for UnsendWrapper<T> {}

impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for UnsendWrapper<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

// ── SyncWriter ────────────────────────────────────────────────────────────────
//
// Bridges tokio::io::AsyncWrite (kube-rs pod stdin) to std::io::Write
// (what terminal.rs holds for keystroke writes).

// AFTER
struct SyncWriter(
    Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>>,
    tokio::runtime::Handle,
);

impl std::io::Write for SyncWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let writer = self.0.clone();
        let bytes  = buf.to_vec();
        let handle = self.1.clone();
        std::thread::spawn(move || {
            handle.block_on(async move {
                let mut w = writer.lock().await;
                w.write_all(&bytes).await?;
                Ok::<usize, std::io::Error>(bytes.len())
            })
        })
        .join()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "write thread panicked"))?
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}