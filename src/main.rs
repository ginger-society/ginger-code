#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

// ── Logging ────────────────────────────────────────────────────────────────────
//
// Replaces println!/eprintln! with `tracing`, writing exclusively to
// ~/.ginger-society/logs/ginger-code.log — never to stdout/stderr — since the
// tray-launched binary has no controlling terminal to write to anyway, and a
// single stable file path means there's always exactly one place to look,
// whether ginger-code was launched from the tray icon, `--daemon`, or a
// terminal during development.
//
// The file is capped at LOG_MAX_BYTES. Once exceeded, oldest *lines* are
// dropped (never a mid-line cut) so the file stays a valid, readable log of
// the most recent activity rather than growing forever.

const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024; // 5MB
// Only run the (more expensive) trim pass once the file has grown this much
// past the cap, so we're not re-scanning the file on every single log line.
const LOG_TRIM_SLACK_BYTES: u64 = 256 * 1024; // 256KB

fn log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".ginger-society")
        .join("logs")
        .join("ginger-code.log")
}

/// A `Write` impl that appends to a fixed log file path and periodically
/// trims it from the front (oldest lines first) once it grows past
/// `max_bytes + LOG_TRIM_SLACK_BYTES`, so the file never grows unbounded but
/// also isn't rewritten on every single write.
struct CappedFileWriter {
    path:      PathBuf,
    max_bytes: u64,
}

impl CappedFileWriter {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        if let Some(parent) = path.parent() {
            // Best-effort — if this fails, the subsequent file open will
            // surface the real error.
            let _ = fs::create_dir_all(parent);
        }
        Self { path, max_bytes }
    }

    fn open_append(&self) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(&self.path)
    }

    /// Drops oldest lines until the file is back under `max_bytes`. Reads the
    /// whole file into memory — fine at a few MB, which is the entire point
    /// of capping it at 5MB in the first place.
    fn trim(&self) -> std::io::Result<()> {
        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let len = file.metadata()?.len();
        if len <= self.max_bytes {
            return Ok(());
        }

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        // Drop oldest lines (from the start) until we're under the cap.
        // Walk forward summing line byte-lengths so we cut on a line
        // boundary, never mid-line.
        let target = self.max_bytes as usize;
        let bytes  = contents.as_bytes();

        if bytes.len() <= target {
            return Ok(());
        }

        // Find the earliest newline at or after (bytes.len() - target), so
        // everything kept is a suffix starting right after a '\n'.
        let cut_from = bytes.len() - target;
        let keep_from = match contents[cut_from..].find('\n') {
            Some(rel_idx) => cut_from + rel_idx + 1,
            None => cut_from, // no newline found in the tail; fall back as-is
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
        let written = file.write(buf)?;

        // Cheap check on every write; only do the expensive trim pass once
        // we've actually grown past cap + slack.
        if let Ok(meta) = file.metadata() {
            if meta.len() > self.max_bytes + LOG_TRIM_SLACK_BYTES {
                drop(file);
                let _ = self.trim();
            }
        }

        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `tracing_subscriber` needs a `MakeWriter`, which means a type that can
/// produce a fresh `Write` instance per log event. `CappedFileWriter` is
/// cheap to construct (just a path + a size cap, no open handle held across
/// calls), so we just clone its (small, Clone) config per call.
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

    // RUST_LOG can still override verbosity (e.g. RUST_LOG=debug) for
    // development; defaults to "info" so routine operational messages
    // (forward status changes, branch switches, hook runs) are always
    // captured without being noisy with trace-level kube-rs internals.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(factory)
        .with_ansi(false) // no color codes in a file you might `cat`/grep
        .with_target(false)
        .init();
}

// ── Shared kube client ────────────────────────────────────────────────────────

pub type SharedClient = Arc<tokio::sync::Mutex<Client>>;
pub type HookCooldown = Arc<tokio::sync::Mutex<Option<tokio::time::Instant>>>;

const AUTH_HOOK_COOLDOWN: Duration = Duration::from_secs(180);

fn hooks_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("hooks")
}

/// Run ~/.ginger-society/hooks/k8-auth-refresh.sh if present and executable,
/// before reloading the kubeconfig on a 401 — but at most once per
/// AUTH_HOOK_COOLDOWN window, since many forwards can hit 401 at once when a
/// shared credential expires and they all converge on the same fix. 3 minutes
/// is on the high end deliberately: it's meant to comfortably outlast the time
/// it takes every forward loop to notice the reloaded kubeconfig and recover,
/// not to bound the hook's own runtime.
async fn run_auth_refresh_hook(cooldown: &HookCooldown) {
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
            warn!(
                path = %hook.display(),
                "hook exists but is not executable — skipping (chmod +x it to enable)"
            );
            return;
        }
    }

    {
        let mut last_run = cooldown.lock().await;
        let should_run = match *last_run {
            None => true,
            Some(t) => t.elapsed() >= AUTH_HOOK_COOLDOWN,
        };

        if !should_run {
            info!(
                seconds_ago = last_run.unwrap().elapsed().as_secs_f64(),
                cooldown_secs = AUTH_HOOK_COOLDOWN.as_secs(),
                "auth-refresh hook skipped — within cooldown"
            );
            return;
        }

        *last_run = Some(tokio::time::Instant::now());
    }

    info!(path = %hook.display(), "running auth-refresh hook");

    let run = tokio::process::Command::new(&hook).output();

    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Ok(Ok(output)) => {
            if !output.stdout.is_empty() {
                info!(stdout = %String::from_utf8_lossy(&output.stdout), "hook stdout");
            }
            if !output.stderr.is_empty() {
                warn!(stderr = %String::from_utf8_lossy(&output.stderr), "hook stderr");
            }
            if output.status.success() {
                info!("auth-refresh hook completed successfully");
            } else {
                warn!(
                    code = ?output.status.code(),
                    "auth-refresh hook exited non-zero — continuing with kubeconfig reload anyway"
                );
            }
        }
        Ok(Err(e)) => {
            error!(error = %e, "failed to spawn auth-refresh hook");
        }
        Err(_) => {
            warn!("auth-refresh hook timed out after 30s — continuing");
        }
    }
}

/// Rebuild a kube Client from the latest kubeconfig on disk.
async fn fresh_client() -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    let config = kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions::default())
        .await?;
    Ok(Client::try_from(config)?)
}

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
        cfg_path
            .parent()
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

// ── Resolve running pod for a deployment ─────────────────────────────────────

async fn resolve_pod(
    pods:            &Api<Pod>,
    deployment_name: &str,
) -> Result<String, ResolveError> {
    let lp   = ListParams::default().labels(&format!("app={}", deployment_name));
    let list = pods.list(&lp).await.map_err(|e| {
        if let kube::Error::Api(ref ae) = e {
            if ae.code == 401 {
                return ResolveError::Unauthorized;
            }
        }
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

async fn run_forward(
    shared_client: SharedClient,
    entry:         DeploymentEntry,
    token:         CancellationToken,
    offline:       Arc<AtomicBool>,
    state_map:     StateMap,
    hook_cooldown: HookCooldown,
) {
    let name = entry.deployment_name.clone();

    let listener = loop {
        match TcpListener::bind(("127.0.0.1", entry.forwarding_port)).await {
            Ok(l) => break l,
            Err(e) => {
                warn!(
                    port = entry.forwarding_port,
                    deployment = %name,
                    error = %e,
                    "cannot bind port — retrying in 3s"
                );
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
            }
        }
    };

    info!(
        port = entry.forwarding_port,
        deployment = %name,
        deployment_port = entry.deployment_port,
        "listening"
    );

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

        // Build an Api handle from the current (possibly refreshed) client.
        let pods: Api<Pod> = {
            let c = shared_client.lock().await;
            Api::default_namespaced(c.clone())
        };

        let pod_name = match resolve_pod(&pods, &name).await {
            Ok(p) => {
                attempt = 0;
                p
            }

            // ── 401: reload kubeconfig and retry without counting the attempt ──
            Err(ResolveError::Unauthorized) => {
                run_auth_refresh_hook(&hook_cooldown).await;
                warn!(deployment = %name, "401 Unauthorized — reloading kubeconfig");
                update_status(&state_map, &name, ForwardStatus::Retrying { attempt });

                // Brief pause before reloading so we don't hammer the API.
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }

                match fresh_client().await {
                    Ok(new_client) => {
                        *shared_client.lock().await = new_client;
                        info!(deployment = %name, "kubeconfig reloaded");
                    }
                    Err(e) => {
                        error!(deployment = %name, error = %e, "kubeconfig reload failed");
                        // Wait with backoff before retrying the reload.
                        tokio::select! {
                            _ = token.cancelled() => return,
                            _ = tokio::time::sleep(backoff(attempt)) => {}
                        }
                        attempt += 1;
                    }
                }
                // Don't increment attempt on a pure 401 — the credential is
                // transient, not a pod-resolve failure.
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

        // Grab a fresh portforward-capable client snapshot for this pod session.
        let pods_for_pf: Api<Pod> = {
            let c = shared_client.lock().await;
            Api::default_namespaced(c.clone())
        };

        'accept: loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!(deployment = %name, "stopping forward");
                    return;
                }

                // Heartbeat: re-assert Connected every second so that a
                // post-reconnect Retrying/Offline status gets corrected without
                // requiring a full pod re-resolve cycle.
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if !offline.load(Ordering::Relaxed) {
                        update_status(&state_map, &name, ForwardStatus::Connected);
                    }
                    // Stay in 'accept — do not break.
                }

                accept_result = listener.accept() => {
                    let (tcp, peer) = match accept_result {
                        Ok(v)  => v,
                        Err(e) => {
                            warn!(deployment = %name, error = %e, "accept error");
                            break 'accept;
                        }
                    };

                    let mut pf = match pods_for_pf
                        .portforward(&pod_name, &[entry.deployment_port])
                        .await
                    {
                        Ok(pf) => pf,
                        Err(e) => {
                            warn!(
                                deployment = %name,
                                pod = %pod_name,
                                error = %e,
                                "portforward failed"
                            );
                            update_status(&state_map, &name, ForwardStatus::Retrying { attempt });
                            break 'accept;
                        }
                    };

                    let stream = match pf.take_stream(entry.deployment_port) {
                        Some(s) => s,
                        None => {
                            warn!(
                                deployment = %name,
                                port = entry.deployment_port,
                                "take_stream returned None"
                            );
                            break 'accept;
                        }
                    };

                    info!(deployment = %name, peer = %peer, "new connection");

                    if let Ok(mut map) = state_map.lock() {
                        if let Some(fw) = map.get_mut(&name) {
                            fw.restarts += 1;
                        }
                    }

                    let name_clone = name.clone();
                    tokio::spawn(async move {
                        let (mut tcp_r, mut tcp_w) = tcp.into_split();
                        let (mut pf_r, mut pf_w)   = tokio::io::split(stream);

                        let client_to_pod = tokio::io::copy(&mut tcp_r, &mut pf_w);
                        let pod_to_client = tokio::io::copy(&mut pf_r, &mut tcp_w);

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

// ── Status update helper ──────────────────────────────────────────────────────

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

fn start_forward(
    entry:         &DeploymentEntry,
    shared_client: SharedClient,
    offline:       &Arc<AtomicBool>,
    state_map:     &StateMap,
    rt:            &tokio::runtime::Handle,
    hook_cooldown: &HookCooldown,
) {
    let token  = CancellationToken::new();
    let handle = rt.spawn(run_forward(
        Arc::clone(&shared_client),
        entry.clone(),
        token.clone(),
        Arc::clone(offline),
        Arc::clone(state_map),
        Arc::clone(hook_cooldown),
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
    for task in tasks {
        task.await.ok();
    }
    state_map.lock().unwrap().clear();
    info!("all forwards stopped");
}

// ── shutdown_all_threads kept for tray.rs compatibility ──────────────────────

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

async fn start_branch_forwards(
    cfg_path:      &PathBuf,
    branch:        &str,
    shared_client: &SharedClient,
    state_map:     &StateMap,
    offline:       &Arc<AtomicBool>,
    hook_cooldown: &HookCooldown,
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
        start_forward(&entry, Arc::clone(shared_client), offline, state_map, &rt, hook_cooldown);
    }

    info!(branch = branch, count = count, "started forward(s)");
}

// ── Network monitor ───────────────────────────────────────────────────────────

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
            // NOTE: We intentionally do NOT reset statuses to Retrying here.
            // Forward tasks sitting in the 'accept loop will self-correct back
            // to Connected within ~1 second via the heartbeat tick arm, because
            // the local TCP listener never dropped. Resetting to Retrying here
            // was the root cause of the tray staying amber after reconnect.
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

async fn run_watcher(
    state_map:      StateMap,
    cfg_path:       PathBuf,
    shared_client:  SharedClient,
    offline:        Arc<AtomicBool>,
    token:          CancellationToken,
    initial_branch: Option<String>,
    hook_cooldown:  HookCooldown,
) {
    let mut last_branch: Option<String> = initial_branch;

    let mut last_modified: Option<std::time::SystemTime> = fs::metadata(&cfg_path)
        .and_then(|m| m.modified())
        .ok();

    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }

        let modified = fs::metadata(&cfg_path)
            .and_then(|m| m.modified())
            .ok();

        let cfg_changed = modified != last_modified;
        last_modified   = modified;

        let cfg    = Config::load(&cfg_path);
        let branch = cfg.active_branch.clone();

        // ── Branch switched ───────────────────────────────────────────────────
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
                start_branch_forwards(&cfg_path, active, &shared_client, &state_map, &offline, &hook_cooldown).await;
            }

            continue;
        }

        // ── Normal reconcile — same branch ────────────────────────────────────
        let entries: Vec<DeploymentEntry> = branch
            .as_deref()
            .map(|b| BranchConfig::load(&cfg_path, b).deployments)
            .unwrap_or_default();

        let active_set: std::collections::HashSet<String> =
            entries.iter().map(|e| e.deployment_name.clone()).collect();

        // Stop removed deployments
        let to_stop: Vec<(String, CancellationToken, Option<tokio::task::JoinHandle<()>>)> = {
            let mut map = state_map.lock().unwrap();
            let names: Vec<String> = map.keys()
                .filter(|k| !active_set.contains(*k))
                .cloned()
                .collect();
            names.into_iter().filter_map(|name| {
                map.remove(&name).map(|mut fw| {
                    (name, fw.token.clone(), fw.task.take())
                })
            }).collect()
        };

        for (name, tok, task) in to_stop {
            info!(deployment = %name, "removing");
            tok.cancel();
            if let Some(t) = task { t.await.ok(); }
            info!(deployment = %name, "forward stopped");
        }

        // Start new deployments
        {
            let rt = tokio::runtime::Handle::current();
            let existing: Vec<String> = state_map.lock().unwrap()
                .keys().cloned().collect();

            for entry in &entries {
                if existing.contains(&entry.deployment_name) { continue; }
                start_forward(entry, Arc::clone(&shared_client), &offline, &state_map, &rt, &hook_cooldown);
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

fn dispatch(
    req:           Request,
    cfg_path:      &PathBuf,
    state_map:     &StateMap,
    shared_client: &SharedClient,
    offline:       &Arc<AtomicBool>,
    rt:            &tokio::runtime::Handle,
    hook_cooldown: &HookCooldown,
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
                let already_running = state_map
                    .lock().unwrap()
                    .contains_key(&deployment_name);

                if !already_running {
                    let entry = DeploymentEntry {
                        deployment_name: deployment_name.clone(),
                        deployment_port,
                        forwarding_port,
                        organization_id,
                    };
                    start_forward(&entry, Arc::clone(shared_client), offline, state_map, rt, hook_cooldown);
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
                    message: format!(
                        "'{}' not found in branch '{}'",
                        deployment_name, branch
                    ),
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

fn handle_client(
    stream:        UnixStream,
    cfg_path:      PathBuf,
    state_map:     StateMap,
    shared_client: SharedClient,
    offline:       Arc<AtomicBool>,
    rt:            tokio::runtime::Handle,
    hook_cooldown: HookCooldown, // owned — this fn runs on its own spawned thread
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
            Ok(req) => dispatch(req, &cfg_path, &state_map, &shared_client, &offline, &rt, &hook_cooldown),
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
    let hook_cooldown: HookCooldown = Arc::new(tokio::sync::Mutex::new(None));

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

    let shared_client: SharedClient = rt.block_on(async {
        let cfg_path      = cfg_path.clone();
        let state_map     = Arc::clone(&state_map);
        let offline       = Arc::clone(&offline);
        let tok           = shutdown_token.clone();
        let hook_cooldown = Arc::clone(&hook_cooldown);

        let client = match Client::try_default().await {
            Ok(c)  => c,
            Err(e) => {
                error!(error = %e, "failed to build kube client");
                panic!("cannot build kube client — is KUBECONFIG set?");
            }
        };

        // Wrap in a shared, async-lockable handle so tasks can swap it on 401.
        let shared_client: SharedClient = Arc::new(tokio::sync::Mutex::new(client));

        let initial_branch = {
            let cfg = Config::load(&cfg_path);
            if let Some(ref branch) = cfg.active_branch {
                info!(branch = branch, "active branch");
                start_branch_forwards(
                    &cfg_path, branch, &shared_client, &state_map, &offline, &hook_cooldown,
                ).await;
            } else {
                info!("no active branch — run `ginger-code -b <branch>`");
            }
            cfg.active_branch.clone()
        };

        tokio::spawn(run_net_monitor(
            Arc::clone(&offline),
            Arc::clone(&state_map),
            tok.clone(),
        ));

        tokio::spawn(run_watcher(
            Arc::clone(&state_map),
            cfg_path.clone(),
            Arc::clone(&shared_client),
            Arc::clone(&offline),
            tok.clone(),
            initial_branch,
            Arc::clone(&hook_cooldown),
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
        let rt_handle     = rt.handle().clone();
        let hook_cooldown = Arc::clone(&hook_cooldown);

        std::thread::spawn(move || {
            if sp.exists() { let _ = fs::remove_file(&sp); }

            let listener = match UnixListener::bind(&sp) {
                Ok(l)  => l,
                Err(e) => {
                    error!(error = %e, "socket bind failed");
                    return;
                }
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
                        let rt_handle     = rt_handle.clone();
                        let hook_cooldown = Arc::clone(&hook_cooldown);
                        std::thread::spawn(move || {
                            handle_client(s, cp, sm, shared_client, offline, rt_handle, hook_cooldown);
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "accept error");
                        break;
                    }
                }
            }
        });
    }

    info!(daemon_mode = daemon_mode, "mode");

    // ── Tray or daemon ────────────────────────────────────────────────────────
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

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    info!("shutting down...");
    rt.block_on(stop_all_forwards(&state_map));
    let _ = fs::remove_file(&sock_path);
    info!("bye");
}