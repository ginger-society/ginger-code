//! Shared Kubernetes client with automatic 401-recovery.
//!
//! Used by k8_info, k8s_ops, k8s_exec (GUI/TUI/CLI) and also by main.rs
//! (daemon forward loops) so the hook + kubeconfig-reload logic lives in
//! exactly one place.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kube::Client;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

// ── Global state ──────────────────────────────────────────────────────────────

static CLIENT: OnceCell<Arc<Mutex<Client>>> = OnceCell::const_new();
static HOOK_COOLDOWN: OnceCell<Arc<Mutex<Option<Instant>>>> = OnceCell::const_new();

const AUTH_HOOK_COOLDOWN: Duration = Duration::from_secs(180);

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns a clone of the current shared kube client, initialising it on
/// first call.
pub async fn get_client() -> Client {
    let arc = CLIENT
        .get_or_init(|| async {
            let client = Client::try_default()
                .await
                .expect("failed to build initial kube client");
            Arc::new(Mutex::new(client))
        })
        .await;

    arc.lock().await.clone()
}

/// Run the auth-refresh hook (with cooldown) and rebuild the global client
/// from the latest kubeconfig on disk.
///
/// Call this whenever any kube API returns 401.  After it returns, the next
/// `get_client()` call hands out the refreshed client.
pub async fn handle_unauthorized() {
    run_auth_refresh_hook().await;
    rebuild_client().await;
}

/// Like `handle_unauthorized` but also returns the freshly built `Client` so
/// the daemon's forward loop can swap its own `SharedClient` arc without a
/// second lock round-trip.
///
/// Returns `None` if the kubeconfig reload fails.
pub async fn handle_unauthorized_and_get() -> Option<Client> {
    run_auth_refresh_hook().await;
    rebuild_client_inner().await
}

/// Returns `true` if a `kube::Error` is a 401 Unauthorized.
pub fn is_unauthorized(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(ae) if ae.code == 401)
}

// ── Hook runner ───────────────────────────────────────────────────────────────

fn hooks_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("hooks")
}

pub async fn run_auth_refresh_hook() {
    let cooldown_arc = HOOK_COOLDOWN
        .get_or_init(|| async { Arc::new(Mutex::new(None)) })
        .await;

    {
        let mut last = cooldown_arc.lock().await;
        let should_run = match *last {
            None    => true,
            Some(t) => t.elapsed() >= AUTH_HOOK_COOLDOWN,
        };
        if !should_run {
            eprintln!(
                "[k8s_client] auth-refresh hook skipped — within {}s cooldown",
                AUTH_HOOK_COOLDOWN.as_secs()
            );
            return;
        }
        *last = Some(Instant::now());
    }

    let hook = hooks_path().join("k8-auth-refresh.sh");
    if !hook.exists() {
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = fs::metadata(&hook)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if !executable {
            eprintln!(
                "[k8s_client] hook {} exists but is not executable — skipping",
                hook.display()
            );
            return;
        }
    }

    eprintln!("[k8s_client] running auth-refresh hook: {}", hook.display());

    let run = tokio::process::Command::new(&hook).output();
    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Ok(Ok(out)) => {
            if !out.stdout.is_empty() {
                eprintln!(
                    "[k8s_client] hook stdout: {}",
                    String::from_utf8_lossy(&out.stdout)
                );
            }
            if !out.stderr.is_empty() {
                eprintln!(
                    "[k8s_client] hook stderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            if out.status.success() {
                eprintln!("[k8s_client] auth-refresh hook completed");
            } else {
                eprintln!(
                    "[k8s_client] hook exited {:?} — continuing anyway",
                    out.status.code()
                );
            }
        }
        Ok(Err(e)) => eprintln!("[k8s_client] failed to spawn hook: {e}"),
        Err(_)     => eprintln!("[k8s_client] hook timed out after 30s"),
    }
}

// ── Client rebuild ────────────────────────────────────────────────────────────

async fn fresh_client() -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    let config = kube::Config::from_kubeconfig(
        &kube::config::KubeConfigOptions::default(),
    )
    .await?;
    Ok(Client::try_from(config)?)
}

/// Rebuild and store in the global cell; returns the new client.
async fn rebuild_client_inner() -> Option<Client> {
    eprintln!("[k8s_client] rebuilding kube client from latest kubeconfig");

    match fresh_client().await {
        Ok(new_client) => {
            if let Some(arc) = CLIENT.get() {
                *arc.lock().await = new_client.clone();
                eprintln!("[k8s_client] client refreshed");
            }
            Some(new_client)
        }
        Err(e) => {
            eprintln!("[k8s_client] kubeconfig reload failed: {e}");
            None
        }
    }
}

async fn rebuild_client() {
    rebuild_client_inner().await;
}