#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use kube::api::ListParams;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

mod tray;
mod shared;

// Import the shared hook + client-rebuild logic.
use shared::core::k8s_client::{handle_unauthorized_and_get, is_unauthorized, run_auth_refresh_hook};

// ── Logging ────────────────────────────────────────────────────────────────────

const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const LOG_TRIM_SLACK_BYTES: u64 = 256 * 1024;

fn log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".ginger-society")
        .join("logs")
        .join("ginger-code.log")
}

struct CappedFileWriter {
    path:      PathBuf,
    max_bytes: u64,
}

impl CappedFileWriter {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self { path, max_bytes }
    }

    fn open_append(&self) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(&self.path)
    }

    fn trim(&self) -> std::io::Result<()> {
        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let len = file.metadata()?.len();
        if len <= self.max_bytes { return Ok(()); }

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let target    = self.max_bytes as usize;
        let bytes     = contents.as_bytes();
        if bytes.len() <= target { return Ok(()); }

        let cut_from  = bytes.len() - target;
        let keep_from = match contents[cut_from..].find('\n') {
            Some(rel) => cut_from + rel + 1,
            None      => cut_from,
        };

        let trimmed = &contents[keep_from..];
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(trimmed.as_bytes())?;
        file.flush()?;
        Ok(())
    }
}

impl Write for CappedFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = self.open_append()?;
        let written  = file.write(buf)?;
        if let Ok(meta) = file.metadata() {
            if meta.len() > self.max_bytes + LOG_TRIM_SLACK_BYTES {
                drop(file);
                let _ = self.trim();
            }
        }
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

#[derive(Clone)]
struct CappedFileWriterFactory {
    path:      PathBuf,
    max_bytes: u64,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CappedFileWriterFactory {
    type Writer = CappedFileWriter;
    fn make_writer(&'a self) -> Self::Writer {
        CappedFileWriter::new(self.path.clone(), self.max_bytes)
    }
}

fn init_logging() {
    let factory = CappedFileWriterFactory {
        path:      log_path(),
        max_bytes: LOG_MAX_BYTES,
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(factory)
        .with_ansi(false)
        .with_target(false)
        .init();
}

// ── Shared kube client ────────────────────────────────────────────────────────
//
// The daemon needs its own Arc<Mutex<Client>> so that concurrent forward tasks
// can all swap to the refreshed client atomically after a 401. The actual
// hook + kubeconfig-reload logic now lives in shared::core::k8s_client.

pub type SharedClient = Arc<tokio::sync::Mutex<Client>>;

// ── Active-connection counter (NEW) ───────────────────────────────────────────
//
// One shared counter across ALL forwards, not one per forward. It exists for
// exactly one purpose: let the centralized heartbeat (see run_heartbeat below)
// know whether ANY tunnel anywhere currently has a live, accepted TCP
// connection flowing through it, so the heartbeat can skip probing the
// apiserver while real traffic might be in flight.
//
// IMPORTANT: this counter never gates accept()ing new local connections and
// is never used to close existing ones. It only gates whether the heartbeat
// task makes an extra apiserver call. Already-open local sockets (e.g. an
// active VS Code SSH session) are completely unaffected by this counter, by
// the heartbeat, or by auth refreshes — exactly as today, a stalled portforward
// just means the data path is slow until the client/portforward recovers; the
// local listener and any accepted socket are never torn down because of it.
pub type ActiveConnCounter = Arc<AtomicU32>;

// ── Config (code.toml) ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Config {
    pub active_branch: Option<String>,
    pub active_env:    Option<String>,
    pub active_url:    Option<String>,
}

impl Config {
    fn load(path: &PathBuf) -> Self {
        if !path.exists() { return Config::default(); }
        toml::from_str(&fs::read_to_string(path).unwrap_or_default())
            .unwrap_or_default()
    }

    fn save(&self, path: &PathBuf) {
        if let Some(p) = path.parent() { fs::create_dir_all(p).ok(); }
        fs::write(path, toml::to_string_pretty(self).expect("toml"))
            .expect("write config");
    }
}

// ── Branch config ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct BranchConfig {
    #[serde(default)]
    pub deployments: Vec<DeploymentEntry>,
}

impl BranchConfig {
    fn path_for(cfg_path: &PathBuf, branch: &str) -> PathBuf {
        let slug = branch.replace('/', "-");
        cfg_path.parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("branches")
            .join(format!("{}.toml", slug))
    }

    fn load(cfg_path: &PathBuf, branch: &str) -> Self {
        let path = Self::path_for(cfg_path, branch);
        if !path.exists() { return BranchConfig::default(); }
        toml::from_str(&fs::read_to_string(&path).unwrap_or_default())
            .unwrap_or_default()
    }

    fn save(&self, cfg_path: &PathBuf, branch: &str) {
        let path = Self::path_for(cfg_path, branch);
        if let Some(p) = path.parent() { fs::create_dir_all(p).ok(); }
        fs::write(&path, toml::to_string_pretty(self).expect("toml"))
            .expect("write branch config");
    }
}

// ── Deployment entry ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DeploymentEntry {
    pub deployment_name: String,
    pub deployment_port: u16,
    pub forwarding_port: u16,
    pub organization_id: String,
}

// ── Forward status ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ForwardStatus {
    Connected,
    Offline,
    Retrying { attempt: u32 },
}

// ── ForwardState ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ForwardState {
    pub status:          ForwardStatus,
    pub restarts:        u32,
    pub forwarding_port: u16,
    pub deployment_port: u16,
    pub token:           CancellationToken,
    pub task:            Option<tokio::task::JoinHandle<()>>,
}

impl ForwardState {
    fn new(
        entry: &DeploymentEntry,
        token: CancellationToken,
        task:  tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            status:          ForwardStatus::Retrying { attempt: 0 },
            restarts:        0,
            forwarding_port: entry.forwarding_port,
            deployment_port: entry.deployment_port,
            token,
            task: Some(task),
        }
    }
}

pub type StateMap = Arc<Mutex<HashMap<String, ForwardState>>>;

// ── Backoff ───────────────────────────────────────────────────────────────────

fn backoff(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_secs(1),
        1 => Duration::from_secs(3),
        2 => Duration::from_secs(8),
        3 => Duration::from_secs(15),
        _ => Duration::from_secs(30),
    }
}

// ── Resolve error ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ResolveError {
    Unauthorized,
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Unauthorized => write!(f, "Unauthorized (401)"),
            ResolveError::Other(e)     => write!(f, "{}", e),
        }
    }
}

// ── Resolve running pod ───────────────────────────────────────────────────────

async fn resolve_pod(
    pods:            &Api<Pod>,
    deployment_name: &str,
) -> Result<String, ResolveError> {
    let lp   = ListParams::default().labels(&format!("app={}", deployment_name));
    let list = pods.list(&lp).await.map_err(|e| {
        if is_unauthorized(&e) { return ResolveError::Unauthorized; }
        ResolveError::Other(Box::new(e))
    })?;

    list.items
        .into_iter()
        .find(|p| {
            p.status.as_ref()
                .and_then(|s| s.phase.as_deref())
                == Some("Running")
        })
        .and_then(|p| p.metadata.name)
        .ok_or_else(|| ResolveError::Other(
            format!("no running pod found for '{}'", deployment_name).into()
        ))
}

// ── Core forward loop ─────────────────────────────────────────────────────────
//
// CHANGED: takes `active_conns: ActiveConnCounter` now — the shared, global
// counter (not a per-forward one). This loop's own per-second tick no longer
// makes any apiserver call itself; that's been pulled out into run_heartbeat
// so N forwards don't each redundantly probe the same shared client. This
// loop's only new responsibility is incrementing/decrementing the shared
// counter around each connection's lifetime so the heartbeat knows whether
// it's safe to probe.
//
// Everything about local socket persistence is UNCHANGED: the listener is
// still bound once up front, accepted connections are still handed to their
// own spawned copy task and never torn down by anything in this function.
async fn run_forward(
    shared_client: SharedClient,
    entry:         DeploymentEntry,
    token:         CancellationToken,
    offline:       Arc<AtomicBool>,
    state_map:     StateMap,
    active_conns:  ActiveConnCounter, // NEW
) {
    let name = entry.deployment_name.clone();

    let listener = loop {
        match TcpListener::bind(("127.0.0.1", entry.forwarding_port)).await {
            Ok(l) => break l,
            Err(e) => {
                warn!(port = entry.forwarding_port, deployment = %name, error = %e, "cannot bind port — retrying in 3s");
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
            }
        }
    };

    info!(port = entry.forwarding_port, deployment = %name, deployment_port = entry.deployment_port, "listening");

    let mut attempt: u32 = 0;

    loop {
        if token.is_cancelled() {
            info!(deployment = %name, "stopping forward");
            return;
        }

        if offline.load(Ordering::Relaxed) {
            update_status(&state_map, &name, ForwardStatus::Offline);
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            continue;
        }

        let pods: Api<Pod> = {
            let c = shared_client.lock().await;
            Api::default_namespaced(c.clone())
        };

        let pod_name = match resolve_pod(&pods, &name).await {
            Ok(p) => {
                attempt = 0;
                p
            }

            Err(ResolveError::Unauthorized) => {
                // ── Delegate entirely to shared module ────────────────────────
                warn!(deployment = %name, "401 Unauthorized — running hook and reloading kubeconfig");
                update_status(&state_map, &name, ForwardStatus::Retrying { attempt });

                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }

                // handle_unauthorized_and_get runs the hook (with cooldown)
                // and rebuilds the global client, returning the new one.
                match handle_unauthorized_and_get().await {
                    Some(new_client) => {
                        // Swap into the daemon's own SharedClient arc so all
                        // concurrent forward tasks pick it up.
                        *shared_client.lock().await = new_client;
                        info!(deployment = %name, "kubeconfig reloaded");
                    }
                    None => {
                        error!(deployment = %name, "kubeconfig reload failed");
                        tokio::select! {
                            _ = token.cancelled() => return,
                            _ = tokio::time::sleep(backoff(attempt)) => {}
                        }
                        attempt += 1;
                    }
                }
                continue;
            }

            Err(ResolveError::Other(e)) => {
                warn!(deployment = %name, error = %e, "resolve pod failed");
                update_status(&state_map, &name, ForwardStatus::Retrying { attempt });
                attempt += 1;
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(backoff(attempt)) => {}
                }
                continue;
            }
        };

        info!(deployment = %name, pod = %pod_name, "resolved pod");
        update_status(&state_map, &name, ForwardStatus::Connected);

        // REMOVED the once-per-outer-loop `pods_for_pf` snapshot that used
        // to live here. It was captured exactly once when this lap of the
        // loop started and then reused for every accept() until something
        // forced the loop back around — which meant a heartbeat-triggered
        // refresh of `shared_client` had NO effect on any forward already
        // sitting idle in 'accept: the snapshot just kept pointing at the
        // old, now-401-ing client. `pods_for_pf` is now derived fresh from
        // `shared_client` inside the accept_result arm below, right before
        // each portforward() call — so it always reflects whatever the
        // heartbeat (or anything else) most recently swapped in, with no
        // staleness window at all. The lock + clone is cheap and this is
        // off the hot data path (it only runs once per NEW connection, not
        // per byte), so there's no meaningful cost to dropping the snapshot.

        'accept: loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!(deployment = %name, "stopping forward");
                    return;
                }

                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // CHANGED: no apiserver call here anymore — that's now the
                    // centralized run_heartbeat task's job. This just keeps the
                    // displayed status honest for the common case (no network
                    // loss, no auth issue) without making any API call.
                    if !offline.load(Ordering::Relaxed) {
                        update_status(&state_map, &name, ForwardStatus::Connected);
                    }
                }

                accept_result = listener.accept() => {
                    let (tcp, peer) = match accept_result {
                        Ok(v)  => v,
                        Err(e) => {
                            warn!(deployment = %name, error = %e, "accept error");
                            break 'accept;
                        }
                    };

                    // NEW: build pods_for_pf fresh, from whatever client is
                    // currently in shared_client, right here — not from a
                    // stale snapshot. This is the actual fix for "heartbeat
                    // refreshed the client but green tunnels still 401 on
                    // first real use": there is no more snapshot to go stale.
                    let mut pods_for_pf: Api<Pod> = {
                        let c = shared_client.lock().await;
                        Api::default_namespaced(c.clone())
                    };

                    // FIXED: portforward() can still 401 in the rare window
                    // where the token expires AFTER this snapshot was taken
                    // but BEFORE portforward() completes (a few hundred ms
                    // wide at most, vs. the previous unbounded staleness
                    // window that could last as long as the forward stayed
                    // idle). Kept as defense in depth: detect the 401
                    // specifically, refresh inline, and retry portforward()
                    // ONCE on the SAME accepted `tcp` socket before giving up.
                    // From VS Code's point of view this just looks like a
                    // slow connect, not a failed one.
                    let mut pf = match pods_for_pf
                        .portforward(&pod_name, &[entry.deployment_port])
                        .await
                    {
                        Ok(pf) => pf,
                        Err(e) if is_unauthorized(&e) => {
                            warn!(deployment = %name, pod = %pod_name, "portforward 401 — refreshing inline and retrying same connection");
                            update_status(&state_map, &name, ForwardStatus::Retrying { attempt });

                            let refreshed_pods: Option<Api<Pod>> = match handle_unauthorized_and_get().await {
                                Some(new_client) => {
                                    *shared_client.lock().await = new_client.clone();
                                    info!(deployment = %name, "kubeconfig reloaded (inline, from portforward 401)");
                                    let fresh = Api::default_namespaced(new_client);
                                    pods_for_pf = fresh.clone(); // kept in sync for the retry call just below; harmless since pods_for_pf no longer outlives this accept()
                                    Some(fresh)
                                }
                                None => {
                                    error!(deployment = %name, "inline kubeconfig reload failed — dropping connection");
                                    None
                                }
                            };

                            let retry_result = match refreshed_pods {
                                Some(ref fresh_pods) => {
                                    fresh_pods.portforward(&pod_name, &[entry.deployment_port]).await
                                }
                                None => {
                                    // Reuse the typed error path so the match below
                                    // still has something to report/log against.
                                    Err(e)
                                }
                            };

                            match retry_result {
                                Ok(pf) => pf,
                                Err(e2) => {
                                    warn!(deployment = %name, pod = %pod_name, error = %e2, "portforward retry after refresh also failed — giving up on this connection");
                                    break 'accept;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(deployment = %name, pod = %pod_name, error = %e, "portforward failed");
                            update_status(&state_map, &name, ForwardStatus::Retrying { attempt });
                            break 'accept;
                        }
                    };

                    let stream = match pf.take_stream(entry.deployment_port) {
                        Some(s) => s,
                        None => {
                            warn!(deployment = %name, port = entry.deployment_port, "take_stream returned None");
                            break 'accept;
                        }
                    };

                    info!(deployment = %name, peer = %peer, "new connection");

                    // NEW: mark this connection active in the shared counter
                    // BEFORE spawning, so the heartbeat can never observe a
                    // false "idle" window between accept() and the counter
                    // being bumped.
                    active_conns.fetch_add(1, Ordering::Relaxed);

                    if let Ok(mut map) = state_map.lock() {
                        if let Some(fw) = map.get_mut(&name) {
                            fw.restarts += 1;
                        }
                    }

                    let name_clone = name.clone();
                    let active_conns_for_task = Arc::clone(&active_conns); // NEW
                    tokio::spawn(async move {
                        let (mut tcp_r, mut tcp_w) = tcp.into_split();
                        let (mut pf_r,  mut pf_w)  = tokio::io::split(stream);

                        let client_to_pod = tokio::io::copy(&mut tcp_r, &mut pf_w);
                        let pod_to_client = tokio::io::copy(&mut pf_r,  &mut tcp_w);

                        // UNCHANGED: this select! and everything in it is the
                        // part responsible for "internet drops, VS Code stays
                        // connected, data just stalls". Nothing here checks
                        // offline/auth/heartbeat state — a stalled copy just
                        // sits here until the underlying stream errors or
                        // recovers on its own. That behavior is fully preserved.
                        tokio::select! {
                            r = client_to_pod => {
                                if let Err(e) = r {
                                    warn!(deployment = %name_clone, error = %e, "client→pod copy error");
                                }
                            }
                            r = pod_to_client => {
                                if let Err(e) = r {
                                    warn!(deployment = %name_clone, error = %e, "pod→client copy error");
                                }
                            }
                        }

                        let _ = pf_w.shutdown().await;
                        active_conns_for_task.fetch_sub(1, Ordering::Relaxed); // NEW
                        info!(deployment = %name_clone, "connection closed");
                    });
                }
            }
        }

        if let Ok(mut map) = state_map.lock() {
            if let Some(fw) = map.get_mut(&name) {
                fw.status = ForwardStatus::Retrying { attempt };
            }
        }

        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(backoff(attempt)) => {}
        }
        attempt += 1;
    }
}

// ── Centralized heartbeat (NEW) ───────────────────────────────────────────────
//
// Replaces the idea of a per-forward version check with ONE check for the
// whole daemon, because every forward shares the same `shared_client` — auth
// staleness is a fact about that one client, not about any individual
// forward. Runs every 5s, skips entirely if:
//   - the daemon is currently marked offline (run_net_monitor already owns
//     that signal), or
//   - any forward anywhere has a live accepted connection right now
//     (active_conns > 0) — we don't want to spend an extra apiserver call
//     while real traffic might be in flight, even though the check itself
//     wouldn't touch the data path.
//
// On success: clears any forward stuck in `Offline` (we now know the client
// itself is healthy) but deliberately does NOT touch `Retrying { attempt }`
// forwards — those are mid pod-resolution for reasons unrelated to auth
// (e.g. no running pod yet), and that transition is already owned by
// run_forward's own loop once resolve_pod succeeds. Overwriting it here would
// blur two different failure dimensions (auth vs. pod-availability) together.
//
// On 401: runs the existing hook+rebuild path exactly once for the whole
// daemon and swaps the shared client — every forward picks up the refreshed
// client on its next lock acquisition, no per-forward duplication.
async fn run_heartbeat(
    shared_client: SharedClient,
    active_conns:  ActiveConnCounter,
    state_map:     StateMap,
    offline:       Arc<AtomicBool>,
    token:         CancellationToken,
) {
    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }

        if offline.load(Ordering::Relaxed) {
            continue;
        }

        if active_conns.load(Ordering::Relaxed) > 0 {
            // Real traffic may be in flight somewhere — skip this tick
            // entirely. Does NOT affect any open local socket either way;
            // this is purely "don't bother probing right now."
            continue;
        }

        let probe_result = {
            let client = shared_client.lock().await;
            client.apiserver_version().await
        };

        match probe_result {
            Ok(_) => {
                if let Ok(mut map) = state_map.lock() {
                    for fw in map.values_mut() {
                        if fw.status == ForwardStatus::Offline {
                            fw.status = ForwardStatus::Connected;
                        }
                    }
                }
            }
            Err(ref e) if is_unauthorized(e) => {
                warn!("heartbeat detected 401 — refreshing shared client");
                match handle_unauthorized_and_get().await {
                    Some(new_client) => {
                        *shared_client.lock().await = new_client;
                        info!("kubeconfig reloaded via heartbeat");
                    }
                    None => {
                        error!("heartbeat-triggered kubeconfig reload failed");
                    }
                }
            }
            Err(e) => {
                // Transient error (network blip, apiserver hiccup) — not
                // necessarily auth-related. Leave forward statuses alone;
                // next tick retries. run_net_monitor owns the actual
                // online/offline signal independently of this.
                warn!(error = %e, "heartbeat probe failed (non-auth) — will retry");
            }
        }
    }
}

// ── Status update ─────────────────────────────────────────────────────────────

fn update_status(state_map: &StateMap, name: &str, status: ForwardStatus) {
    if let Ok(mut map) = state_map.lock() {
        if let Some(fw) = map.get_mut(name) {
            if fw.status != status {
                info!(deployment = name, status = ?status, "status changed");
                fw.status = status;
            }
        }
    }
}

// ── Start forward task ────────────────────────────────────────────────────────
//
// CHANGED: now takes & threads through the shared `active_conns` counter.
fn start_forward(
    entry:         &DeploymentEntry,
    shared_client: SharedClient,
    offline:       &Arc<AtomicBool>,
    state_map:     &StateMap,
    active_conns:  &ActiveConnCounter, // NEW
    rt:            &tokio::runtime::Handle,
) {
    let token  = CancellationToken::new();
    let handle = rt.spawn(run_forward(
        Arc::clone(&shared_client),
        entry.clone(),
        token.clone(),
        Arc::clone(offline),
        Arc::clone(state_map),
        Arc::clone(active_conns), // NEW
    ));

    state_map.lock().unwrap().insert(
        entry.deployment_name.clone(),
        ForwardState::new(entry, token, handle),
    );
}

// ── Stop all forwards ─────────────────────────────────────────────────────────

pub async fn stop_all_forwards(state_map: &StateMap) {
    let tasks: Vec<tokio::task::JoinHandle<()>> = {
        let mut map = state_map.lock().unwrap();
        map.values().for_each(|fw| fw.token.cancel());
        map.values_mut()
            .filter_map(|fw| fw.task.take())
            .collect()
    };
    for task in tasks { task.await.ok(); }
    state_map.lock().unwrap().clear();
    info!("all forwards stopped");
}

pub fn shutdown_all_threads(state_map: &StateMap) {
    {
        let mut map = state_map.lock().unwrap();
        map.values().for_each(|fw| fw.token.cancel());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    state_map.lock().unwrap().clear();
    info!("all forwards stopped");
}

// ── Start forwards for a branch ───────────────────────────────────────────────
//
// CHANGED: threads active_conns through to start_forward.
async fn start_branch_forwards(
    cfg_path:      &PathBuf,
    branch:        &str,
    shared_client: &SharedClient,
    state_map:     &StateMap,
    offline:       &Arc<AtomicBool>,
    active_conns:  &ActiveConnCounter, // NEW
) {
    let entries = BranchConfig::load(cfg_path, branch).deployments;
    if entries.is_empty() {
        info!(branch = branch, "no deployments in branch");
        return;
    }

    let rt = tokio::runtime::Handle::current();
    let map = state_map.lock().unwrap();
    let new_entries: Vec<DeploymentEntry> = entries
        .into_iter()
        .filter(|e| !map.contains_key(&e.deployment_name))
        .collect();
    drop(map);

    let count = new_entries.len();
    for entry in new_entries {
        start_forward(&entry, Arc::clone(shared_client), offline, state_map, active_conns, &rt);
    }
    info!(branch = branch, count = count, "started forward(s)");
}

// ── Network monitor ───────────────────────────────────────────────────────────
//
// UNCHANGED. Still only flips the `offline` flag and labels forwards
// `Offline` for display purposes — never touches an accepted connection or
// the listener. A network blip still leaves VS Code's local socket open;
// only the data path stalls, exactly as before.
async fn run_net_monitor(
    offline:   Arc<AtomicBool>,
    state_map: StateMap,
    token:     CancellationToken,
) {
    let mut was_online = has_network();
    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        let online = has_network();
        if !was_online && online {
            info!("network restored");
            offline.store(false, Ordering::Relaxed);
        } else if was_online && !online {
            warn!("network lost");
            offline.store(true, Ordering::Relaxed);
            if let Ok(mut map) = state_map.lock() {
                for fw in map.values_mut() {
                    fw.status = ForwardStatus::Offline;
                }
            }
        }
        was_online = online;
    }
}

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
}

// ── Config watcher ────────────────────────────────────────────────────────────
//
// CHANGED: threads active_conns through to start_forward / start_branch_forwards.
// Behaviorally unchanged otherwise — branch switches still tear down and
// restart forwards explicitly (that's a deliberate, user-initiated teardown,
// not something the heartbeat or network monitor does on their own).
async fn run_watcher(
    state_map:      StateMap,
    cfg_path:       PathBuf,
    shared_client:  SharedClient,
    offline:        Arc<AtomicBool>,
    active_conns:   ActiveConnCounter, // NEW
    token:          CancellationToken,
    initial_branch: Option<String>,
) {
    let mut last_branch = initial_branch;
    let mut last_modified: Option<std::time::SystemTime> = fs::metadata(&cfg_path)
        .and_then(|m| m.modified())
        .ok();

    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }

        let modified    = fs::metadata(&cfg_path).and_then(|m| m.modified()).ok();
        let cfg_changed = modified != last_modified;
        last_modified   = modified;

        let cfg    = Config::load(&cfg_path);
        let branch = cfg.active_branch.clone();

        if cfg_changed && branch != last_branch {
            info!(from = ?last_branch, to = ?branch, "branch changed");
            stop_all_forwards(&state_map).await;

            if let Ok(mut guard) = tray::GUI_CHILD.lock() {
                if let Some(mut child) = guard.take() {
                    let _ = child.kill();
                    info!("GUI closed for branch switch");
                }
            }

            last_branch = branch.clone();
            if let Some(ref active) = branch {
                start_branch_forwards(&cfg_path, active, &shared_client, &state_map, &offline, &active_conns).await;
            }
            continue;
        }

        let entries: Vec<DeploymentEntry> = branch
            .as_deref()
            .map(|b| BranchConfig::load(&cfg_path, b).deployments)
            .unwrap_or_default();

        let active_set: std::collections::HashSet<String> =
            entries.iter().map(|e| e.deployment_name.clone()).collect();

        let to_stop: Vec<(String, CancellationToken, Option<tokio::task::JoinHandle<()>>)> = {
            let mut map = state_map.lock().unwrap();
            let names: Vec<String> = map.keys()
                .filter(|k| !active_set.contains(*k))
                .cloned()
                .collect();
            names.into_iter().filter_map(|name| {
                map.remove(&name).map(|mut fw| (name, fw.token.clone(), fw.task.take()))
            }).collect()
        };

        for (name, tok, task) in to_stop {
            info!(deployment = %name, "removing");
            tok.cancel();
            if let Some(t) = task { t.await.ok(); }
            info!(deployment = %name, "forward stopped");
        }

        {
            let rt       = tokio::runtime::Handle::current();
            let existing: Vec<String> = state_map.lock().unwrap().keys().cloned().collect();
            for entry in &entries {
                if existing.contains(&entry.deployment_name) { continue; }
                start_forward(entry, Arc::clone(&shared_client), &offline, &state_map, &active_conns, &rt);
                info!(deployment = %entry.deployment_name, "registered");
            }
        }
    }
}

// ── Protocol ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Ping,
    Register {
        deployment_name: String,
        deployment_port: u16,
        forwarding_port: u16,
        organization_id: String,
    },
    List,
    Remove { deployment_name: String },
}

#[derive(Debug, Serialize, Clone)]
pub struct DeploymentStatus {
    pub deployment_name: String,
    pub deployment_port: u16,
    pub forwarding_port: u16,
    pub organization_id: String,
    pub forward_status:  ForwardStatus,
    pub restarts:        u32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Ok          { message: String },
    Deployments { deployments: Vec<DeploymentStatus>, active_branch: Option<String>, active_url: Option<String> },
    Error       { message: String },
}

// ── Dispatch ──────────────────────────────────────────────────────────────────
//
// CHANGED: takes & threads through active_conns for the Register path's
// start_forward call.
fn dispatch(
    req:           Request,
    cfg_path:      &PathBuf,
    state_map:     &StateMap,
    shared_client: &SharedClient,
    offline:       &Arc<AtomicBool>,
    active_conns:  &ActiveConnCounter, // NEW
    rt:            &tokio::runtime::Handle,
) -> Response {
    match req {
        Request::Ping => Response::Ok { message: "pong".to_string() },

        Request::Register {
            deployment_name,
            deployment_port,
            forwarding_port,
            organization_id,
        } => {
            let cfg = Config::load(cfg_path);
            let Some(ref branch) = cfg.active_branch else {
                return Response::Error {
                    message: "No active branch — run `ginger-code -b <branch>` first".into(),
                };
            };

            let mut bc = BranchConfig::load(cfg_path, branch);
            bc.deployments.retain(|d| d.deployment_name != deployment_name);
            bc.deployments.push(DeploymentEntry {
                deployment_name: deployment_name.clone(),
                deployment_port,
                forwarding_port,
                organization_id: organization_id.clone(),
            });
            bc.save(cfg_path, branch);

            {
                let already_running = state_map.lock().unwrap()
                    .contains_key(&deployment_name);
                if !already_running {
                    let entry = DeploymentEntry {
                        deployment_name: deployment_name.clone(),
                        deployment_port,
                        forwarding_port,
                        organization_id,
                    };
                    start_forward(&entry, Arc::clone(shared_client), offline, state_map, active_conns, rt);
                }
            }

            Response::Ok {
                message: format!(
                    "Registered '{}' in branch '{}' (:{} → deployment:{})",
                    deployment_name, branch, forwarding_port, deployment_port
                ),
            }
        }

        Request::List => {
            let cfg    = Config::load(cfg_path);
            let branch = cfg.active_branch.clone();

            let entries: Vec<DeploymentEntry> = branch
                .as_deref()
                .map(|b| BranchConfig::load(cfg_path, b).deployments)
                .unwrap_or_default();

            let map = state_map.lock().unwrap();
            let deployments = entries.iter().map(|e| {
                let fw = map.get(&e.deployment_name);
                DeploymentStatus {
                    deployment_name: e.deployment_name.clone(),
                    deployment_port: e.deployment_port,
                    forwarding_port: e.forwarding_port,
                    organization_id: e.organization_id.clone(),
                    forward_status:  fw.map_or(
                        ForwardStatus::Retrying { attempt: 0 },
                        |f| f.status.clone(),
                    ),
                    restarts: fw.map_or(0, |f| f.restarts),
                }
            }).collect();

            Response::Deployments {
                deployments,
                active_branch: branch,
                active_url:    cfg.active_url,
            }
        }

        Request::Remove { deployment_name } => {
            let cfg = Config::load(cfg_path);
            let Some(ref branch) = cfg.active_branch else {
                return Response::Error {
                    message: "No active branch set in code.toml".into(),
                };
            };

            let mut bc = BranchConfig::load(cfg_path, branch);
            let before = bc.deployments.len();
            bc.deployments.retain(|d| d.deployment_name != deployment_name);

            if bc.deployments.len() == before {
                return Response::Error {
                    message: format!("'{}' not found in branch '{}'", deployment_name, branch),
                };
            }
            bc.save(cfg_path, branch);

            if let Some(fw) = state_map.lock().unwrap().remove(&deployment_name) {
                fw.token.cancel();
                if let Some(task) = fw.task {
                    rt.spawn(async move { task.await.ok(); });
                }
            }

            Response::Ok {
                message: format!(
                    "Removed '{}' from branch '{}' — forward torn down",
                    deployment_name, branch
                ),
            }
        }
    }
}

// ── Socket listener ───────────────────────────────────────────────────────────
//
// CHANGED: threads active_conns through to dispatch.
fn handle_client(
    stream:        UnixStream,
    cfg_path:      PathBuf,
    state_map:     StateMap,
    shared_client: SharedClient,
    offline:       Arc<AtomicBool>,
    active_conns:  ActiveConnCounter, // NEW
    rt:            tokio::runtime::Handle,
) {
    let mut writer = match stream.try_clone() {
        Ok(s)  => s,
        Err(e) => { error!(error = %e, "clone stream failed"); return; }
    };
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }

        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e)  => Response::Error { message: format!("Parse error: {e}") },
            Ok(req) => dispatch(req, &cfg_path, &state_map, &shared_client, &offline, &active_conns, &rt),
        };

        let mut json = serde_json::to_string(&resp).unwrap();
        json.push('\n');
        if writer.write_all(json.as_bytes()).is_err() { break; }
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("code.toml")
}

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    init_logging();

    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--gui".to_string()) {
        shared::gui::run_gui().unwrap();
        return;
    }

    info!(args = ?args, "starting");

    let daemon_mode = args.contains(&"--daemon".to_string());

    #[cfg(target_os = "macos")]
    {
        let current = std::env::var("PATH").unwrap_or_default();
        std::env::set_var(
            "PATH",
            format!("/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}", current),
        );
    }

    #[cfg(target_os = "macos")]
    unsafe {
        libc::setsid();
        std::env::set_var("CFPREFERENCES_AVOID_DAEMON", "1");
    }

    let sock_path = socket_path();
    let cfg_path  = config_path();
    info!(log_path = %log_path().display(), "logging to file");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let state_map:      StateMap          = Arc::new(Mutex::new(HashMap::new()));
    let shutdown_token: CancellationToken = CancellationToken::new();
    let offline:        Arc<AtomicBool>   = Arc::new(AtomicBool::new(!has_network()));
    // NEW: single shared counter for "is any tunnel actively in use right now",
    // consumed only by run_heartbeat. Never gates accept() or teardown.
    let active_conns:   ActiveConnCounter = Arc::new(AtomicU32::new(0));

    // Build the initial client via the shared module so it seeds the global
    // OnceCell — after this, get_client() in k8_info/k8s_ops will reuse it.
    let shared_client: SharedClient = rt.block_on(async {
        let client = shared::core::k8s_client::get_client().await;
        let shared_client: SharedClient = Arc::new(tokio::sync::Mutex::new(client));

        let cfg_path_c     = cfg_path.clone();
        let state_map_c    = Arc::clone(&state_map);
        let offline_c      = Arc::clone(&offline);
        let active_conns_c = Arc::clone(&active_conns); // NEW
        let tok            = shutdown_token.clone();
        let sc             = Arc::clone(&shared_client);

        let initial_branch = {
            let cfg = Config::load(&cfg_path_c);
            if let Some(ref branch) = cfg.active_branch {
                info!(branch = branch, "active branch");
                start_branch_forwards(&cfg_path_c, branch, &sc, &state_map_c, &offline_c, &active_conns_c).await;
            } else {
                info!("no active branch — run `ginger-code -b <branch>`");
            }
            cfg.active_branch.clone()
        };

        tokio::spawn(run_net_monitor(
            Arc::clone(&offline_c),
            Arc::clone(&state_map_c),
            tok.clone(),
        ));

        // NEW: the single centralized heartbeat task, replacing any idea of
        // a per-forward periodic apiserver check.
        tokio::spawn(run_heartbeat(
            Arc::clone(&sc),
            Arc::clone(&active_conns_c),
            Arc::clone(&state_map_c),
            Arc::clone(&offline_c),
            tok.clone(),
        ));

        tokio::spawn(run_watcher(
            Arc::clone(&state_map_c),
            cfg_path_c,
            Arc::clone(&sc),
            Arc::clone(&offline_c),
            Arc::clone(&active_conns_c), // NEW
            tok.clone(),
            initial_branch,
        ));

        shared_client
    });

    // ── Socket listener ───────────────────────────────────────────────────────
    {
        let sp            = sock_path.clone();
        let cp            = cfg_path.clone();
        let sm            = Arc::clone(&state_map);
        let shared_client = Arc::clone(&shared_client);
        let offline       = Arc::clone(&offline);
        let active_conns  = Arc::clone(&active_conns); // NEW
        let rt_handle     = rt.handle().clone();

        std::thread::spawn(move || {
            if sp.exists() { let _ = fs::remove_file(&sp); }

            let listener = match UnixListener::bind(&sp) {
                Ok(l)  => l,
                Err(e) => { error!(error = %e, "socket bind failed"); return; }
            };

            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sp, fs::Permissions::from_mode(0o600)).ok();
            }

            info!(path = %sp.display(), "socket listening");

            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let cp            = cp.clone();
                        let sm            = Arc::clone(&sm);
                        let shared_client = Arc::clone(&shared_client);
                        let offline       = Arc::clone(&offline);
                        let active_conns  = Arc::clone(&active_conns); // NEW
                        let rt_handle     = rt_handle.clone();
                        std::thread::spawn(move || {
                            handle_client(s, cp, sm, shared_client, offline, active_conns, rt_handle);
                        });
                    }
                    Err(e) => { error!(error = %e, "accept error"); break; }
                }
            }
        });
    }

    info!(daemon_mode = daemon_mode, "mode");

    if daemon_mode {
        info!("running in daemon mode (no tray)");
        rt.block_on(async {
            tokio::signal::ctrl_c().await.expect("set signal handler");
        });
        info!("signal received, shutting down...");
        shutdown_token.cancel();
    } else {
        let tray_shutdown = Arc::new(AtomicBool::new(false));

        {
            let tray_sd = Arc::clone(&tray_shutdown);
            let tok     = shutdown_token.clone();
            rt.spawn(async move {
                tok.cancelled().await;
                tray_sd.store(true, Ordering::Relaxed);
            });
        }

        tray::run_tray(
            Arc::clone(&state_map),
            Arc::clone(&tray_shutdown),
            Arc::clone(&offline),
            sock_path.clone(),
            cfg_path.clone(),
        );

        shutdown_token.cancel();
    }

    info!("shutting down...");
    rt.block_on(stop_all_forwards(&state_map));
    let _ = fs::remove_file(&sock_path);
    info!("bye");
}