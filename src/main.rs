#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod tray;
mod shared;

// ── Config (code.toml — active branch pointer only) ──────────────────────────

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

// ── Branch config (branches/<slug>.toml) ──────────────────────────────────────

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
    pub pid:             Option<u32>,
    pub stop_flag:       Arc<AtomicBool>,
    pub thread_handle:   Option<std::thread::JoinHandle<()>>,
}

impl ForwardState {
    fn new(
        entry:     &DeploymentEntry,
        stop_flag: Arc<AtomicBool>,
        handle:    std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            status:          ForwardStatus::Retrying { attempt: 0 },
            restarts:        0,
            forwarding_port: entry.forwarding_port,
            deployment_port: entry.deployment_port,
            pid:             None,
            stop_flag,
            thread_handle:   Some(handle),
        }
    }
}

pub type StateMap = Arc<Mutex<HashMap<String, ForwardState>>>;

// ── Probe constants ───────────────────────────────────────────────────────────

/// After this many seconds without a successful probe on a previously-connected
/// forward AND at least MIN_PROBE_FAIL_COUNT consecutive failures, kill and
/// restart. Raised from 15 s → 60 s so transient VS Code I/O stalls don't
/// trigger a restart.
const MAX_PROBE_FAIL_SECS: u64 = 60;

/// Minimum number of consecutive probe failures required (in addition to the
/// time threshold) before we declare the stream dead. A single slow banner
/// read resets this counter and won't cause a restart on its own.
const MIN_PROBE_FAIL_COUNT: u32 = 3;

/// Hard ceiling on kubectl process lifetime — restart regardless of probe
/// status. Prevents SPDY streams from going stale silently over long sessions.
const MAX_FORWARD_LIFETIME_SECS: u64 = 6000; // 100 minutes

// ── Network reachability ──────────────────────────────────────────────────────

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
}

// ── TCP probe — for non-SSH forwards ─────────────────────────────────────────

fn probe_port(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )
    .is_ok()
}

// ── SSH banner probe — for SSH forwards ──────────────────────────────────────
//
// A plain TCP connect to kubectl port-forward always succeeds even when the
// internal SPDY streams have died — kubectl is still listening.  The only
// reliable way to detect a dead stream is to actually attempt the SSH
// handshake: the SSH server sends its banner immediately on connect.
// If we read at least one byte we know the stream end-to-end is alive.
//
// Timeouts are intentionally generous (2 s connect, 3 s read) so that
// VS Code heavy I/O — which can momentarily stall the banner — does not
// cause false negatives that accumulate toward the kill threshold.

fn probe_ssh(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(2000)) {
        Err(_) => false,
        Ok(mut stream) => {
            stream.set_read_timeout(Some(Duration::from_millis(3000))).ok();
            let mut buf = [0u8; 8];
            matches!(stream.read(&mut buf), Ok(n) if n > 0)
        }
    }
}

// ── Choose probe based on deployment port ─────────────────────────────────────

fn probe(entry: &DeploymentEntry) -> bool {
    if entry.deployment_port == 22 {
        probe_ssh(entry.forwarding_port)
    } else {
        probe_port(entry.forwarding_port)
    }
}

// ── Backoff ───────────────────────────────────────────────────────────────────

fn backoff(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_secs(2),
        1 => Duration::from_secs(5),
        2 => Duration::from_secs(10),
        3 => Duration::from_secs(20),
        _ => Duration::from_secs(30),
    }
}

// ── kubectl helpers ───────────────────────────────────────────────────────────

fn find_kubectl() -> Option<PathBuf> {
    let candidates = [
        "/usr/local/bin/kubectl",
        "/opt/homebrew/bin/kubectl",
        "/usr/bin/kubectl",
    ];
    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn spawn_kubectl_child(entry: &DeploymentEntry) -> Option<Child> {
    let kubectl = find_kubectl().unwrap_or_else(|| {
        eprintln!("[ginger-code] kubectl not found in PATH or known locations");
        PathBuf::from("kubectl")
    });

    let target = format!("deployment/{}", entry.deployment_name);
    let ports  = format!("{}:{}", entry.forwarding_port, entry.deployment_port);

    eprintln!("[ginger-code] spawning kubectl port-forward {} {}", target, ports);

    match Command::new(&kubectl)
        .args(["port-forward", &target, &ports])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()) // suppress stream timeout noise
        .spawn()
    {
        Ok(child) => {
            println!(
                "[ginger-code] started  {} → localhost:{} (pid {})",
                entry.deployment_name, entry.forwarding_port, child.id()
            );
            Some(child)
        }
        Err(e) => {
            eprintln!(
                "[ginger-code] kubectl spawn failed for '{}': {e}",
                entry.deployment_name
            );
            None
        }
    }
}

fn kill_child(child: &mut Child) {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
    std::thread::sleep(Duration::from_millis(200));
    let _ = child.kill();
    let _ = child.wait();
}

// ── Interruptible sleep ───────────────────────────────────────────────────────

fn interruptible_sleep(dur: Duration, stop_flag: &Arc<AtomicBool>) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop_flag.load(Ordering::Relaxed) { return; }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ── Per-deployment supervised thread ─────────────────────────────────────────
//
// Owns the kubectl child for its entire lifetime.
// Detects dead SSH streams via banner probe, not just TCP connect.
// Enforces a hard process lifetime ceiling to prevent SPDY stream rot.
//
// Kill decision requires BOTH:
//   • No successful probe for MAX_PROBE_FAIL_SECS
//   • At least MIN_PROBE_FAIL_COUNT consecutive failures
// This prevents VS Code heavy-I/O stalls from being misread as dead streams.

fn spawn_forward_thread(
    entry:     DeploymentEntry,
    stop_flag: Arc<AtomicBool>,
    offline:   Arc<AtomicBool>,
    state_map: StateMap,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("forward-{}", entry.deployment_name))
        .spawn(move || {
            let name                              = entry.deployment_name.clone();
            let mut child:        Option<Child>   = None;
            let mut spawned_at:   Option<Instant> = None;
            let mut attempt:      u32             = 0;
            /// Tracks when we last got a successful probe on a connected forward.
            /// Used to detect silently-dead SPDY streams.
            let mut last_good_probe:      Option<Instant> = None;
            /// Number of consecutive probe failures since the last success.
            /// Reset to 0 on any successful probe. Both this AND the elapsed
            /// time must exceed their thresholds before we restart.
            let mut consecutive_failures: u32             = 0;

            loop {
                // ── Stop requested ────────────────────────────────────────────
                if stop_flag.load(Ordering::Relaxed) {
                    if let Some(ref mut c) = child {
                        kill_child(c);
                        println!("[ginger-code] stopped forward for {}", name);
                    }
                    if let Ok(mut map) = state_map.lock() {
                        if let Some(fw) = map.get_mut(&name) { fw.pid = None; }
                    }
                    return;
                }

                // ── Network offline — kill child and wait ─────────────────────
                if offline.load(Ordering::Relaxed) {
                    if child.is_some() {
                        if let Some(ref mut c) = child { kill_child(c); }
                        child                = None;
                        spawned_at           = None;
                        last_good_probe      = None;
                        consecutive_failures = 0;
                        attempt              = 0;
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                fw.status = ForwardStatus::Offline;
                                fw.pid    = None;
                            }
                        }
                    }
                    interruptible_sleep(Duration::from_secs(1), &stop_flag);
                    continue;
                }

                // ── Check if existing child is still alive ────────────────────
                let alive = child
                    .as_mut()
                    .map_or(false, |c| c.try_wait().ok().flatten().is_none());

                if !alive {
                    // ── Process exited — clean up and schedule retry ──────────
                    if let Some(ref mut c) = child { kill_child(c); }
                    child                = None;
                    spawned_at           = None;
                    last_good_probe      = None;
                    consecutive_failures = 0;

                    if let Ok(mut map) = state_map.lock() {
                        if let Some(fw) = map.get_mut(&name) {
                            fw.status = ForwardStatus::Retrying { attempt };
                            fw.pid    = None;
                            if attempt > 0 { fw.restarts += 1; }
                        }
                    }

                    let delay = backoff(attempt);
                    eprintln!(
                        "[ginger-code] retrying {} (attempt {}, wait {:?})",
                        name, attempt, delay
                    );
                    attempt += 1;

                    interruptible_sleep(delay, &stop_flag);
                    if stop_flag.load(Ordering::Relaxed) { continue; }

                    child      = spawn_kubectl_child(&entry);
                    spawned_at = Some(Instant::now());

                    if let Some(ref c) = child {
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                fw.pid = Some(c.id());
                            }
                        }
                    }

                } else {
                    // ── Child alive — run health checks ───────────────────────

                    // Hard lifetime ceiling — restart before SPDY streams rot
                    let too_old = spawned_at
                        .map_or(false, |t| t.elapsed() > Duration::from_secs(MAX_FORWARD_LIFETIME_SECS));

                    if too_old {
                        eprintln!(
                            "[ginger-code] refreshing '{}' — max lifetime ({}s) reached",
                            name, MAX_FORWARD_LIFETIME_SECS
                        );
                        if let Some(ref mut c) = child { kill_child(c); }
                        child                = None;
                        spawned_at           = None;
                        last_good_probe      = None;
                        consecutive_failures = 0;
                        attempt              = 0;
                        continue;
                    }

                    // Probe after settle window only
                    let settled = spawned_at
                        .map_or(false, |t| t.elapsed() > Duration::from_secs(3));

                    if settled {
                        if probe(&entry) {
                            // ── Probe succeeded ───────────────────────────────
                            last_good_probe      = Some(Instant::now());
                            consecutive_failures = 0;

                            if let Ok(mut map) = state_map.lock() {
                                if let Some(fw) = map.get_mut(&name) {
                                    if fw.status != ForwardStatus::Connected {
                                        println!(
                                            "[ginger-code] connected {} on :{}",
                                            name, entry.forwarding_port
                                        );
                                        attempt = 0;
                                    }
                                    fw.status = ForwardStatus::Connected;
                                }
                            }
                        } else {
                            // ── Probe failed ──────────────────────────────────
                            //
                            // Increment the consecutive-failure counter.
                            // Only restart when BOTH thresholds are exceeded:
                            //   1. No good probe for MAX_PROBE_FAIL_SECS
                            //   2. At least MIN_PROBE_FAIL_COUNT failures in a row
                            //
                            // This means a single timeout (VS Code I/O stall,
                            // brief network hiccup) resets nothing harmful and
                            // will not cause a restart on its own.
                            consecutive_failures += 1;

                            let was_connected = state_map.lock().ok()
                                .and_then(|m| m.get(&name)
                                    .map(|fw| fw.status == ForwardStatus::Connected))
                                .unwrap_or(false);

                            let time_exceeded = last_good_probe
                                .map_or(false, |t| t.elapsed() > Duration::from_secs(MAX_PROBE_FAIL_SECS));

                            let count_exceeded = consecutive_failures >= MIN_PROBE_FAIL_COUNT;

                            if was_connected && time_exceeded && count_exceeded {
                                eprintln!(
                                    "[ginger-code] SSH stream dead on '{}' \
                                     (no good probe for {}s, {} consecutive failures) — restarting now",
                                    name, MAX_PROBE_FAIL_SECS, consecutive_failures
                                );
                                if let Some(ref mut c) = child { kill_child(c); }
                                child                = None;
                                spawned_at           = None;
                                last_good_probe      = None;
                                consecutive_failures = 0;
                                attempt              = 0; // immediate retry, no backoff
                                continue;
                            }

                            // Log degraded state without restarting yet
                            if was_connected {
                                eprintln!(
                                    "[ginger-code] probe miss #{} on '{}' ({}s since last good) — watching",
                                    consecutive_failures,
                                    name,
                                    last_good_probe.map_or(0, |t| t.elapsed().as_secs()),
                                );
                            }
                        }
                    }

                    // Slow poll — reduces stream noise in kubectl logs
                    std::thread::sleep(Duration::from_millis(1000));
                }
            }
        })
        .expect("spawn forward thread")
}

// ── Stop all forward threads and join ─────────────────────────────────────────

pub fn stop_all_forwards(state_map: &StateMap) {
    let handles: Vec<std::thread::JoinHandle<()>> = {
        let mut map = state_map.lock().unwrap();
        map.iter().for_each(|(_, fw)| {
            fw.stop_flag.store(true, Ordering::Relaxed);
        });
        map.values_mut()
            .filter_map(|fw| fw.thread_handle.take())
            .collect()
    };
    for handle in handles { handle.join().ok(); }
    state_map.lock().unwrap().clear();
    println!("[ginger-code] all forward threads stopped");
}

pub fn shutdown_all_threads(state_map: &StateMap) {
    stop_all_forwards(state_map);
}

// ── Start forwards for a branch ───────────────────────────────────────────────

fn start_branch_forwards(
    cfg_path:  &PathBuf,
    branch:    &str,
    state_map: &StateMap,
    offline:   &Arc<AtomicBool>,
) {
    let entries = BranchConfig::load(cfg_path, branch).deployments;

    if entries.is_empty() {
        println!("[ginger-code] no deployments in branch '{}'", branch);
        return;
    }

    let mut map = state_map.lock().unwrap();
    for entry in &entries {
        if map.contains_key(&entry.deployment_name) { continue; }

        let stop   = Arc::new(AtomicBool::new(false));
        let handle = spawn_forward_thread(
            entry.clone(),
            Arc::clone(&stop),
            Arc::clone(offline),
            Arc::clone(state_map),
        );
        map.insert(
            entry.deployment_name.clone(),
            ForwardState::new(entry, stop, handle),
        );
    }

    println!(
        "[ginger-code] started {} forward(s) for branch '{}'",
        entries.len(), branch
    );
}

// ── Network monitor ───────────────────────────────────────────────────────────

fn run_net_monitor(
    offline:   Arc<AtomicBool>,
    state_map: StateMap,
    shutdown:  Arc<AtomicBool>,
) {
    let mut was_online = has_network();

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        let online = has_network();

        if !was_online && online {
            println!("[ginger-code] network restored");
            offline.store(false, Ordering::Relaxed);
        } else if was_online && !online {
            println!("[ginger-code] network lost");
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

// ── Watcher ───────────────────────────────────────────────────────────────────
//
// Watches code.toml for active_branch changes.
// On branch change: stops all forwards, kills GUI, starts new forwards.
// On same branch:   normal add/remove reconcile against branch toml.

fn run_watcher(
    state_map: StateMap,
    cfg_path:  PathBuf,
    offline:   Arc<AtomicBool>,
    shutdown:  Arc<AtomicBool>,
) {
    let mut last_branch:   Option<String>                = None;
    let mut last_modified: Option<std::time::SystemTime> = None;

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(2));

        // ── Detect code.toml modification ─────────────────────────────────────
        let modified = fs::metadata(&cfg_path)
            .and_then(|m| m.modified())
            .ok();

        let cfg_changed = modified != last_modified;
        last_modified   = modified;

        let cfg    = Config::load(&cfg_path);
        let branch = cfg.active_branch.clone();

        // ── Branch switched ───────────────────────────────────────────────────
        if cfg_changed && branch != last_branch {
            println!(
                "[ginger-code] branch changed: {:?} → {:?}",
                last_branch, branch
            );

            // 1. Stop all existing forwards cleanly
            stop_all_forwards(&state_map);

            // 2. Kill GUI — dev reopens to see the new branch
            if let Ok(mut guard) = tray::GUI_CHILD.lock() {
                if let Some(mut child) = guard.take() {
                    let _ = child.kill();
                    println!("[ginger-code] GUI closed for branch switch");
                }
            }

            last_branch = branch.clone();

            // 3. Start forwards for new branch
            if let Some(ref active) = branch {
                start_branch_forwards(&cfg_path, active, &state_map, &offline);
            }

            continue; // skip normal reconcile on branch-switch tick
        }

        // ── Normal reconcile — same branch ────────────────────────────────────
        let entries: Vec<DeploymentEntry> = branch
            .as_deref()
            .map(|b| BranchConfig::load(&cfg_path, b).deployments)
            .unwrap_or_default();

        let active_set: std::collections::HashSet<String> =
            entries.iter().map(|e| e.deployment_name.clone()).collect();

        // Stop removed deployments
        let to_stop: Vec<(String, Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>)> = {
            let mut map = state_map.lock().unwrap();
            let names: Vec<String> = map.keys()
                .filter(|k| !active_set.contains(*k))
                .cloned()
                .collect();
            names.into_iter().filter_map(|name| {
                map.remove(&name).map(|mut fw| {
                    (name, Arc::clone(&fw.stop_flag), fw.thread_handle.take())
                })
            }).collect()
        };

        for (name, stop, handle) in to_stop {
            eprintln!("[ginger-code] removing '{}'", name);
            stop.store(true, Ordering::Relaxed);
            if let Some(h) = handle { h.join().ok(); }
            println!("[ginger-code] thread for '{}' stopped", name);
        }

        // Start new deployments
        {
            let mut map = state_map.lock().unwrap();
            for entry in &entries {
                if map.contains_key(&entry.deployment_name) { continue; }

                let stop   = Arc::new(AtomicBool::new(false));
                let handle = spawn_forward_thread(
                    entry.clone(),
                    Arc::clone(&stop),
                    Arc::clone(&offline),
                    Arc::clone(&state_map),
                );
                map.insert(
                    entry.deployment_name.clone(),
                    ForwardState::new(entry, stop, handle),
                );
                println!("[ginger-code] registered '{}'", entry.deployment_name);
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
    },
    List,
    Remove { deployment_name: String },
}

#[derive(Debug, Serialize, Clone)]
pub struct DeploymentStatus {
    pub deployment_name: String,
    pub deployment_port: u16,
    pub forwarding_port: u16,
    pub forward_status:  ForwardStatus,
    pub restarts:        u32,
    pub pid:             Option<u32>,
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
    req:       Request,
    cfg_path:  &PathBuf,
    state_map: &StateMap,
    offline:   &Arc<AtomicBool>,
) -> Response {
    match req {
        Request::Ping => Response::Ok { message: "pong".to_string() },

        Request::Register { deployment_name, deployment_port, forwarding_port } => {
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
            });
            bc.save(cfg_path, branch);

            {
                let mut map = state_map.lock().unwrap();
                if !map.contains_key(&deployment_name) {
                    let entry = DeploymentEntry {
                        deployment_name: deployment_name.clone(),
                        deployment_port,
                        forwarding_port,
                    };
                    let stop   = Arc::new(AtomicBool::new(false));
                    let handle = spawn_forward_thread(
                        entry.clone(),
                        Arc::clone(&stop),
                        Arc::clone(offline),
                        Arc::clone(state_map),
                    );
                    map.insert(deployment_name.clone(), ForwardState::new(&entry, stop, handle));
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
                    forward_status:  fw.map_or(
                        ForwardStatus::Retrying { attempt: 0 },
                        |f| f.status.clone(),
                    ),
                    restarts: fw.map_or(0, |f| f.restarts),
                    pid:      fw.and_then(|f| f.pid),
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
                        "Deployment '{}' not found in branch '{}'",
                        deployment_name, branch
                    ),
                };
            }
            bc.save(cfg_path, branch);

            let handle_to_join = {
                let mut map = state_map.lock().unwrap();
                map.remove(&deployment_name).map(|mut fw| {
                    fw.stop_flag.store(true, Ordering::Relaxed);
                    fw.thread_handle.take()
                })
            };

            if let Some(Some(handle)) = handle_to_join {
                let name_for_log = deployment_name.clone();
                std::thread::spawn(move || {
                    handle.join().ok();
                    println!("[ginger-code] thread for '{}' stopped after Remove", name_for_log);
                });
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

// ── Connection handler ────────────────────────────────────────────────────────

fn handle_client(
    stream:    UnixStream,
    cfg_path:  PathBuf,
    state_map: StateMap,
    offline:   Arc<AtomicBool>,
) {
    let mut writer = match stream.try_clone() {
        Ok(s)  => s,
        Err(e) => { eprintln!("[ginger-code] clone stream: {e}"); return; }
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e)  => Response::Error { message: format!("Parse error: {e}") },
            Ok(req) => dispatch(req, &cfg_path, &state_map, &offline),
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

// ── Spawn resilient background thread ────────────────────────────────────────

fn spawn_resilient<F>(name: &'static str, shutdown: Arc<AtomicBool>, f: F)
where
    F: Fn() + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&f));
                if let Err(e) = result {
                    let msg = e.downcast_ref::<&str>().copied()
                        .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown panic");
                    eprintln!(
                        "[ginger-code] thread '{}' panicked: {} — restarting in 2s",
                        name, msg
                    );
                }
                if !shutdown.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        })
        .expect("spawn resilient thread");
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--gui".to_string()) {
        shared::gui::run_gui().unwrap();
        return;
    }

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

    if sock_path.exists() {
        fs::remove_file(&sock_path).expect("remove stale socket");
    }

    println!(
        "[ginger-code] started\n  socket : {}\n  config : {}",
        sock_path.display(), cfg_path.display()
    );

    let state_map: StateMap = Arc::new(Mutex::new(HashMap::new()));
    let shutdown             = Arc::new(AtomicBool::new(false));
    let offline              = Arc::new(AtomicBool::new(!has_network()));

    // ── Seed state from active branch ─────────────────────────────────────────
    {
        let cfg = Config::load(&cfg_path);
        if let Some(ref branch) = cfg.active_branch {
            println!("[ginger-code] active branch: '{}'", branch);
            start_branch_forwards(&cfg_path, branch, &state_map, &offline);
        } else {
            println!("[ginger-code] no active branch — run `ginger-code -b <branch>`");
        }
    }

    // ── Network monitor ───────────────────────────────────────────────────────
    spawn_resilient("net-monitor", Arc::clone(&shutdown), {
        let offline   = Arc::clone(&offline);
        let state_map = Arc::clone(&state_map);
        let sd        = Arc::clone(&shutdown);
        move || run_net_monitor(Arc::clone(&offline), Arc::clone(&state_map), Arc::clone(&sd))
    });

    // ── Config watcher ────────────────────────────────────────────────────────
    spawn_resilient("watcher", Arc::clone(&shutdown), {
        let sm      = Arc::clone(&state_map);
        let cp      = cfg_path.clone();
        let offline = Arc::clone(&offline);
        let sd      = Arc::clone(&shutdown);
        move || run_watcher(Arc::clone(&sm), cp.clone(), Arc::clone(&offline), Arc::clone(&sd))
    });

    // ── Socket listener ───────────────────────────────────────────────────────
    spawn_resilient("socket", Arc::clone(&shutdown), {
        let cp      = cfg_path.clone();
        let sm      = Arc::clone(&state_map);
        let sp      = sock_path.clone();
        let offline = Arc::clone(&offline);
        move || {
            if sp.exists() { let _ = fs::remove_file(&sp); }

            let listener = match UnixListener::bind(&sp) {
                Ok(l)  => l,
                Err(e) => { eprintln!("[ginger-code] socket bind failed: {e}"); return; }
            };

            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sp, fs::Permissions::from_mode(0o600)).ok();
            }

            println!("[ginger-code] socket listening on {}", sp.display());

            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let cp      = cp.clone();
                        let sm      = Arc::clone(&sm);
                        let offline = Arc::clone(&offline);
                        std::thread::spawn(move || handle_client(s, cp, sm, offline));
                    }
                    Err(e) => { eprintln!("[ginger-code] accept error: {e}"); break; }
                }
            }
        }
    });

    // ── Tray — owns the main thread ───────────────────────────────────────────
    tray::run_tray(
        Arc::clone(&state_map),
        Arc::clone(&shutdown),
        Arc::clone(&offline),
        sock_path.clone(),
        cfg_path.clone(),
    );

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    println!("[ginger-code] shutting down...");
    shutdown.store(true, Ordering::Relaxed);
    shutdown_all_threads(&state_map);
    let _ = fs::remove_file(&sock_path);
    println!("[ginger-code] bye");
}