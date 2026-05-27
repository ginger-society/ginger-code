#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod tray;
mod shared;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DeploymentEntry {
    pub deployment_name: String,
    pub deployment_port: u16,
    pub forwarding_port: u16,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub deployments: Vec<DeploymentEntry>,
}

impl Config {
    fn load(path: &PathBuf) -> Self {
        if !path.exists() {
            return Config::default();
        }
        toml::from_str(&fs::read_to_string(path).unwrap_or_default()).unwrap_or_default()
    }

    fn save(&self, path: &PathBuf) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).ok();
        }
        fs::write(path, toml::to_string_pretty(self).expect("toml")).expect("write config");
    }
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ForwardStatus {
    Connected,
    Offline,
    Retrying { attempt: u32 },
}

// ── ForwardState ──────────────────────────────────────────────────────────────
//
// The kubectl Child is now OWNED by the per-deployment thread, not stored here.
// This struct holds only the observable state that the watcher / tray / socket
// handler need to read, plus the two handles required to stop the thread cleanly.

#[derive(Debug)]
pub struct ForwardState {
    pub status:          ForwardStatus,
    pub restarts:        u32,
    pub forwarding_port: u16,
    pub deployment_port: u16,
    pub pid:             Option<u32>,       // set by the thread so List can report it
    /// Set to true to ask the thread to stop.  The thread kills its child and exits.
    pub stop_flag:       Arc<AtomicBool>,
    /// Join handle — taken (set to None) when we join the thread on removal.
    pub thread_handle:   Option<std::thread::JoinHandle<()>>,
}

impl ForwardState {
    fn new(entry: &DeploymentEntry, stop_flag: Arc<AtomicBool>, handle: std::thread::JoinHandle<()>) -> Self {
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

// ── Network reachability ──────────────────────────────────────────────────────

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:53")?;
            s.local_addr()
        })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
}

// ── TCP probe ─────────────────────────────────────────────────────────────────

fn probe_port(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
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

    eprintln!(
        "[ginger-code] spawning kubectl port-forward {} {}",
        target, ports
    );

    match Command::new(&kubectl)
        .args(["port-forward", &target, &ports])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => {
            println!(
                "[ginger-code] started  {} → localhost:{} (pid {})",
                entry.deployment_name,
                entry.forwarding_port,
                child.id()
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

// ── Per-deployment supervised thread ─────────────────────────────────────────
//
// Each deployment gets exactly one thread.  The thread owns the kubectl Child
// for its lifetime.  When stop_flag is set (by the watcher or shutdown path)
// the thread kills the child and exits cleanly — no zombie processes.

fn spawn_forward_thread(
    entry:      DeploymentEntry,
    stop_flag:  Arc<AtomicBool>,
    offline:    Arc<AtomicBool>,   // shared with net-monitor
    state_map:  StateMap,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("forward-{}", entry.deployment_name))
        .spawn(move || {
            let name      = entry.deployment_name.clone();
            let mut child: Option<Child> = None;
            let mut spawned_at: Option<Instant> = None;
            let mut attempt: u32 = 0;

            loop {
                // ── Stop requested ────────────────────────────────────────────
                if stop_flag.load(Ordering::Relaxed) {
                    if let Some(ref mut c) = child {
                        kill_child(c);
                        println!("[ginger-code] thread stopped, killed child for {}", name);
                    }
                    // Remove pid from map so List doesn't show stale data
                    if let Ok(mut map) = state_map.lock() {
                        if let Some(fw) = map.get_mut(&name) {
                            fw.pid = None;
                        }
                    }
                    return;
                }

                // ── Network offline — kill child, wait ────────────────────────
                if offline.load(Ordering::Relaxed) {
                    if let Some(ref mut c) = child {
                        kill_child(c);
                        child      = None;
                        spawned_at = None;
                        attempt    = 0;
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                fw.status = ForwardStatus::Offline;
                                fw.pid    = None;
                            }
                        }
                    }
                    // Sleep in short bursts so we notice stop_flag quickly
                    interruptible_sleep(Duration::from_secs(1), &stop_flag);
                    continue;
                }

                // ── Check if existing child is still alive ────────────────────
                let alive = child
                    .as_mut()
                    .map_or(false, |c| c.try_wait().ok().flatten().is_none());

                if !alive {
                    // Kill cleanly if it exited on its own
                    if let Some(ref mut c) = child {
                        kill_child(c);
                    }
                    child      = None;
                    spawned_at = None;

                    // Update status before sleeping
                    {
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                fw.status = ForwardStatus::Retrying { attempt };
                                fw.pid    = None;
                                fw.restarts += if attempt > 0 { 1 } else { 0 };
                            }
                        }
                    }

                    let delay = backoff(attempt);
                    eprintln!(
                        "[ginger-code] retrying {} (attempt {}, wait {:?})",
                        name, attempt, delay
                    );
                    attempt += 1;

                    // Sleep interruptibly so stop_flag wakes us
                    interruptible_sleep(delay, &stop_flag);

                    if stop_flag.load(Ordering::Relaxed) { continue; } // will exit top of loop

                    // Spawn new child
                    child = spawn_kubectl_child(&entry);
                    spawned_at = Some(Instant::now());

                    if let Some(ref c) = child {
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                fw.pid = Some(c.id());
                            }
                        }
                    }

                } else {
                    // ── Child alive — probe after settle window ────────────────
                    let settled = spawned_at
                        .map_or(false, |t| t.elapsed() > Duration::from_secs(3));

                    if settled && probe_port(entry.forwarding_port) {
                        if let Ok(mut map) = state_map.lock() {
                            if let Some(fw) = map.get_mut(&name) {
                                if fw.status != ForwardStatus::Connected {
                                    println!(
                                        "[ginger-code] connected {} on :{}",
                                        name, entry.forwarding_port
                                    );
                                    attempt = 0; // reset backoff on successful connect
                                }
                                fw.status = ForwardStatus::Connected;
                            }
                        }
                    }

                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        })
        .expect("spawn forward thread")
}

// ── Interruptible sleep ───────────────────────────────────────────────────────
//
// Sleeps for `dur` but wakes early if `stop_flag` is set.
// Checks every 100 ms so threads are responsive to shutdown.

fn interruptible_sleep(dur: Duration, stop_flag: &Arc<AtomicBool>) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ── Network monitor ───────────────────────────────────────────────────────────
//
// Sets/clears the shared `offline` flag.  Each forward thread reads it and
// kills/restarts its own child — no central coordination needed.

fn run_net_monitor(
    offline:  Arc<AtomicBool>,
    state_map: StateMap,
    shutdown: Arc<AtomicBool>,
) {
    let mut was_online = has_network();

    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        let online = has_network();

        if !was_online && online {
            println!("[ginger-code] network restored");
            offline.store(false, Ordering::Relaxed);
            // Threads will notice offline=false on their next loop tick
            // and resume spawning with backoff.
        } else if was_online && !online {
            println!("[ginger-code] network lost");
            offline.store(true, Ordering::Relaxed);
            // Threads notice offline=true and kill their children themselves.
            // We still update the map status here for the tray tooltip.
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
// Now only responsible for config reconciliation — adding new deployments and
// stopping removed ones.  It never touches kubectl children directly.

fn run_watcher(
    state_map: StateMap,
    cfg_path:  PathBuf,
    offline:   Arc<AtomicBool>,
    shutdown:  Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(2));

        let entries: Vec<DeploymentEntry> = Config::load(&cfg_path).deployments;
        let active: std::collections::HashSet<String> =
            entries.iter().map(|e| e.deployment_name.clone()).collect();

        // ── Stop threads for deployments removed from config ──────────────────
        //
        // We collect the handles outside the lock so we can join without
        // holding the mutex (joining while holding a mutex = deadlock risk).
        let to_stop: Vec<(String, Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>)> = {
            let mut map = state_map.lock().unwrap();
            let names: Vec<String> = map.keys()
                .filter(|k| !active.contains(*k))
                .cloned()
                .collect();

            names.into_iter().filter_map(|name| {
                map.remove(&name).map(|mut fw| {
                    (name, Arc::clone(&fw.stop_flag), fw.thread_handle.take())
                })
            }).collect()
        };

        for (name, stop, handle) in to_stop {
            eprintln!("[ginger-code] removing deployment '{}'", name);
            stop.store(true, Ordering::Relaxed);
            if let Some(h) = handle {
                h.join().ok();
                println!("[ginger-code] thread for '{}' fully stopped", name);
            }
        }

        // ── Start threads for newly added deployments ─────────────────────────
        {
            let mut map = state_map.lock().unwrap();
            for entry in &entries {
                if map.contains_key(&entry.deployment_name) {
                    continue;
                }

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

                println!(
                    "[ginger-code] registered new deployment '{}'",
                    entry.deployment_name
                );
            }
        }
    }
}

// ── Graceful shutdown ─────────────────────────────────────────────────────────
//
// Called from the tray quit handler.  Signals all threads, then joins them
// so every kubectl child is dead before the process exits.

pub fn shutdown_all_threads(state_map: &StateMap) {
    // 1. Signal all threads to stop
    let handles: Vec<(String, std::thread::JoinHandle<()>)> = {
        let mut map = state_map.lock().unwrap();
        map.iter().for_each(|(_, fw)| {
            fw.stop_flag.store(true, Ordering::Relaxed);
        });
        map.values_mut()
            .filter_map(|fw| {
                fw.thread_handle
                    .take()
                    .map(|h| (String::new(), h)) // name not critical here
            })
            .collect()
    };

    // 2. Join all threads outside the lock
    for (_, handle) in handles {
        handle.join().ok();
    }

    println!("[ginger-code] all forward threads stopped");
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
    Deployments { deployments: Vec<DeploymentStatus> },
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
            let mut cfg = Config::load(cfg_path);
            cfg.deployments.retain(|d| d.deployment_name != deployment_name);
            cfg.deployments.push(DeploymentEntry {
                deployment_name: deployment_name.clone(),
                deployment_port,
                forwarding_port,
            });
            cfg.save(cfg_path);

            // If the watcher hasn't picked it up yet, start the thread immediately
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
                    "Registered '{}' — forward starting (:{} → deployment:{})",
                    deployment_name, forwarding_port, deployment_port
                ),
            }
        }

        Request::List => {
            let cfg = Config::load(cfg_path);
            let map = state_map.lock().unwrap();
            let deployments = cfg.deployments.iter().map(|e| {
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
            Response::Deployments { deployments }
        }

        Request::Remove { deployment_name } => {
            let mut cfg = Config::load(cfg_path);
            let before = cfg.deployments.len();
            cfg.deployments.retain(|d| d.deployment_name != deployment_name);
            if cfg.deployments.len() == before {
                return Response::Error {
                    message: format!("Deployment '{}' not found", deployment_name),
                };
            }
            cfg.save(cfg_path);

            // Stop the thread immediately — don't wait for the watcher tick
            let handle_to_join = {
                let mut map = state_map.lock().unwrap();
                map.remove(&deployment_name).map(|mut fw| {
                    fw.stop_flag.store(true, Ordering::Relaxed);
                    fw.thread_handle.take()
                })
            };

            // Join outside the lock — clone name before moving into closure
            if let Some(Some(handle)) = handle_to_join {
                let name_for_log = deployment_name.clone();
                std::thread::spawn(move || {
                    handle.join().ok();
                    println!(
                        "[ginger-code] thread for '{}' fully stopped after Remove",
                        name_for_log
                    );
                });
            }

            Response::Ok {
                message: format!(
                    "Removed '{}' — forward torn down",
                    deployment_name
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

// ── Spawn a background thread, always restart on panic ───────────────────────

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
                    let msg = e
                        .downcast_ref::<&str>()
                        .copied()
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
    // ── GUI mode (spawned by tray on click) ───────────────────────────────────
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
            format!(
                "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}",
                current
            ),
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
        sock_path.display(),
        cfg_path.display()
    );

    let state_map: StateMap = Arc::new(Mutex::new(HashMap::new()));
    let shutdown             = Arc::new(AtomicBool::new(false));
    // Shared offline flag — written by net-monitor, read by forward threads
    let offline              = Arc::new(AtomicBool::new(!has_network()));

    // ── Seed state from config — start a thread per deployment ───────────────
    {
        let cfg = Config::load(&cfg_path);
        let mut map = state_map.lock().unwrap();

        for entry in &cfg.deployments {
            let stop   = Arc::new(AtomicBool::new(false));
            let handle = spawn_forward_thread(
                entry.clone(),
                Arc::clone(&stop),
                Arc::clone(&offline),
                Arc::clone(&state_map),
            );
            map.insert(entry.deployment_name.clone(), ForwardState::new(entry, stop, handle));
        }

        if !cfg.deployments.is_empty() {
            println!(
                "[ginger-code] resuming {} deployment(s)",
                cfg.deployments.len()
            );
        }
    }

    // ── Network monitor ───────────────────────────────────────────────────────
    spawn_resilient("net-monitor", Arc::clone(&shutdown), {
        let offline   = Arc::clone(&offline);
        let state_map = Arc::clone(&state_map);
        let sd        = Arc::clone(&shutdown);
        move || run_net_monitor(Arc::clone(&offline), Arc::clone(&state_map), Arc::clone(&sd))
    });

    // ── Config watcher (reconciles adds/removes) ──────────────────────────────
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
            if sp.exists() {
                let _ = fs::remove_file(&sp);
            }

            let listener = match UnixListener::bind(&sp) {
                Ok(l)  => l,
                Err(e) => {
                    eprintln!("[ginger-code] socket bind failed: {e}");
                    return;
                }
            };

            #[cfg(unix)]
            {
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
                    Err(e) => {
                        eprintln!("[ginger-code] accept error: {e}");
                        break;
                    }
                }
            }
        }
    });

    // ── Tray — owns the main thread (required by macOS) ──────────────────────
    tray::run_tray(
        Arc::clone(&state_map),
        Arc::clone(&shutdown),
        Arc::clone(&offline),
        sock_path.clone(),
        cfg_path.clone(),
    );

    // ── Graceful shutdown after tray exits ────────────────────────────────────
    println!("[ginger-code] shutting down...");
    shutdown.store(true, Ordering::Relaxed);
    shutdown_all_threads(&state_map);
    let _ = fs::remove_file(&sock_path);
    println!("[ginger-code] bye");
}