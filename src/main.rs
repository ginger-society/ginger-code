#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]


use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
mod tray;

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
        if !path.exists() { return Config::default(); }
        toml::from_str(&fs::read_to_string(path).unwrap_or_default()).unwrap_or_default()
    }
    fn save(&self, path: &PathBuf) {
        if let Some(p) = path.parent() { fs::create_dir_all(p).ok(); }
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

#[derive(Debug)]
pub struct ForwardState {
    pub child:    Option<Child>,
    pub status:   ForwardStatus,
    pub restarts: u32,
    pub retry_at: Option<Instant>,
}

impl ForwardState {
    fn new_pending() -> Self {
        Self { child: None, status: ForwardStatus::Retrying { attempt: 0 }, restarts: 0, retry_at: None }
    }
}

pub type StateMap = Arc<Mutex<HashMap<String, ForwardState>>>;

// ── Network reachability ──────────────────────────────────────────────────────

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
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

fn kill_existing_forward(forwarding_port: u16) {
    // Find and kill any kubectl port-forward holding this port
    let _ = Command::new("pkill")
        .args(["-f", &format!("kubectl port-forward.*{}:", forwarding_port)])
        .status();
    // Give it a moment to release the port
    std::thread::sleep(Duration::from_millis(300));
}

// ── kubectl helpers ───────────────────────────────────────────────────────────

fn spawn_forward(entry: &DeploymentEntry) -> Option<Child> {

    let kubectl = find_kubectl().unwrap_or_else(|| {
        eprintln!("[ginger-code] kubectl not found in PATH or known locations");
        std::path::PathBuf::from("kubectl")
    });

    let target = format!("deployment/{}", entry.deployment_name);
    let ports  = format!("{}:{}", entry.forwarding_port, entry.deployment_port);

    kill_existing_forward(entry.forwarding_port); 
    eprintln!("[ginger-code] spawning kubectl port-forward {} {}", target, ports);

    match Command::new(&kubectl)
        .args(["port-forward", &target, &ports])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => {
            println!("[ginger-code] started  {} → localhost:{} (pid {})",
                entry.deployment_name, entry.forwarding_port, child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[ginger-code] kubectl spawn failed for '{}': {e}", entry.deployment_name);
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

// ── Network monitor ───────────────────────────────────────────────────────────

fn run_net_monitor(state_map: StateMap, shutdown: Arc<AtomicBool>) {
    let mut was_online = has_network();
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        let online = has_network();

        if !was_online && online {
            println!("[ginger-code] network restored");
            let mut map = state_map.lock().unwrap();
            for fw in map.values_mut() {
                fw.retry_at = None;
                if fw.status == ForwardStatus::Offline {
                    fw.status = ForwardStatus::Retrying { attempt: 0 };
                }
            }
        } else if was_online && !online {
            println!("[ginger-code] network lost");
            let mut map = state_map.lock().unwrap();
            for fw in map.values_mut() {
                if let Some(ref mut c) = fw.child { kill_child(c); }
                fw.child    = None;
                fw.status   = ForwardStatus::Offline;
                fw.retry_at = None;
            }
        }
        was_online = online;
    }
}

// ── Watcher ───────────────────────────────────────────────────────────────────

fn run_watcher(state_map: StateMap, cfg_path: PathBuf, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));

        let entries: Vec<DeploymentEntry> = Config::load(&cfg_path).deployments;
        let active: std::collections::HashSet<String> =
            entries.iter().map(|e| e.deployment_name.clone()).collect();

        let mut map = state_map.lock().unwrap();

        map.retain(|name, fw| {
            if active.contains(name) { return true; }
            eprintln!("[ginger-code] removed  {} — killing forward", name);
            if let Some(ref mut c) = fw.child { kill_child(c); }
            false
        });

        for entry in &entries {
            let dep = &entry.deployment_name;
            map.entry(dep.clone()).or_insert_with(ForwardState::new_pending);
            let fw = map.get_mut(dep).unwrap();

            if fw.status == ForwardStatus::Offline { continue; }
            if fw.retry_at.map_or(false, |t| Instant::now() < t) { continue; }

            let child_alive = fw.child.as_mut()
                .map_or(false, |c| c.try_wait().ok().flatten().is_none());

            if child_alive {
                fw.status   = ForwardStatus::Connected;
                fw.retry_at = None;
            } else {
                if let Some(ref mut c) = fw.child { kill_child(c); }
                fw.child = None;

                let attempt = match fw.status {
                    ForwardStatus::Retrying { attempt } => attempt,
                    _ => 0,
                };

                let delay = backoff(attempt);
                eprintln!("[ginger-code] retrying {} (attempt {}, wait {:?})", dep, attempt, delay);
                fw.status   = ForwardStatus::Retrying { attempt: attempt + 1 };
                fw.retry_at = Some(Instant::now() + delay);
                fw.restarts += 1;
                fw.child    = spawn_forward(entry);
            }
        }
    }
}

// ── Protocol ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Ping,
    Register { deployment_name: String, deployment_port: u16, forwarding_port: u16 },
    List,
    Remove   { deployment_name: String },
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

fn dispatch(req: Request, path: &PathBuf, state_map: &StateMap) -> Response {
    match req {
        Request::Ping => Response::Ok { message: "pong".to_string() },

        Request::Register { deployment_name, deployment_port, forwarding_port } => {
            let mut cfg = Config::load(path);
            let is_new = !cfg.deployments.iter().any(|d| d.deployment_name == deployment_name);
            cfg.deployments.retain(|d| d.deployment_name != deployment_name);
            cfg.deployments.push(DeploymentEntry {
                deployment_name: deployment_name.clone(), deployment_port, forwarding_port,
            });
            cfg.save(path);
            if is_new {
                state_map.lock().unwrap()
                    .insert(deployment_name.clone(), ForwardState::new_pending());
            }
            Response::Ok {
                message: format!("Registered '{}' — forward starting (:{} → deployment:{})",
                    deployment_name, forwarding_port, deployment_port),
            }
        }

        Request::List => {
            let cfg = Config::load(path);
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
                    pid:      fw.and_then(|f| f.child.as_ref().map(|c| c.id())),
                }
            }).collect();
            Response::Deployments { deployments }
        }

        Request::Remove { deployment_name } => {
            let mut cfg = Config::load(path);
            let before = cfg.deployments.len();
            cfg.deployments.retain(|d| d.deployment_name != deployment_name);
            if cfg.deployments.len() == before {
                return Response::Error {
                    message: format!("Deployment '{}' not found", deployment_name),
                };
            }
            cfg.save(path);
            Response::Ok {
                message: format!("Removed '{}' — forward will be torn down shortly", deployment_name),
            }
        }
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

fn handle_client(stream: UnixStream, cfg_path: PathBuf, state_map: StateMap) {
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { eprintln!("[ginger-code] clone stream: {e}"); return; }
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::Error { message: format!("Parse error: {e}") },
            Ok(req) => dispatch(req, &cfg_path, &state_map),
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

// ── Spawn a background thread, always restart it ─────────────────────────────

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
                    eprintln!("[ginger-code] thread '{}' panicked: {} — restarting in 2s", name, msg);
                }
                // Always wait before restarting (panic or clean exit).
                // The shutdown check at the top of the loop prevents restarting after quit.
                if !shutdown.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        })
        .expect("spawn thread");
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn cleanup_stale_forwards(cfg: &Config) {
    for dep in &cfg.deployments {
        kill_existing_forward(dep.forwarding_port);
    }
}

fn find_kubectl() -> Option<std::path::PathBuf> {
    // Try common locations explicitly if which fails
    let candidates = [
        "/usr/local/bin/kubectl",
        "/opt/homebrew/bin/kubectl",
        "/usr/bin/kubectl",
        "/usr/local/bin/kubectl",
    ];
    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    None
}

fn main() {

    #[cfg(target_os = "macos")]
    {
        let current = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!(
            "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}",
            current
        ));
    }

    #[cfg(target_os = "macos")]
    unsafe {
        // Detach from the terminal — no visible window
        libc::setsid();
    }
    

    // Suppress macOS IMK noise
    #[cfg(target_os = "macos")]
    unsafe {
        std::env::set_var("CFPREFERENCES_AVOID_DAEMON", "1");
    }

    let sock_path = socket_path();
    let cfg_path  = config_path();

    // Clean up any stale socket from a previous crash.
    // The socket thread will do the actual bind.
    if sock_path.exists() {
        fs::remove_file(&sock_path).expect("remove stale socket");
    }

    println!("[ginger-code] started\n  socket : {}\n  config : {}",
        sock_path.display(), cfg_path.display());

    let state_map: StateMap = Arc::new(Mutex::new(HashMap::new()));
    let shutdown = Arc::new(AtomicBool::new(false));

    // Seed state from config
    {
        let cfg = Config::load(&cfg_path);
        cleanup_stale_forwards(&cfg);

        let mut map = state_map.lock().unwrap();
        for dep in &cfg.deployments {
            map.insert(dep.deployment_name.clone(), ForwardState::new_pending());
        }
        if !cfg.deployments.is_empty() {
            println!("[ginger-code] resuming {} deployment(s)", cfg.deployments.len());
        }
    }

    // Network monitor
    spawn_resilient("net-monitor", Arc::clone(&shutdown), {
        let sm = Arc::clone(&state_map);
        let sd = Arc::clone(&shutdown);
        move || run_net_monitor(Arc::clone(&sm), Arc::clone(&sd))
    });

    // Watcher
    spawn_resilient("watcher", Arc::clone(&shutdown), {
        let sm = Arc::clone(&state_map);
        let cp = cfg_path.clone();
        let sd = Arc::clone(&shutdown);
        move || run_watcher(Arc::clone(&sm), cp.clone(), Arc::clone(&sd))
    });

    // Socket listener — owns the bind entirely, re-binds on each restart
    spawn_resilient("socket", Arc::clone(&shutdown), {
        let cp = cfg_path.clone();
        let sm = Arc::clone(&state_map);
        let sp = sock_path.clone();
        move || {
            // Remove stale socket from a previous restart of this thread
            if sp.exists() { let _ = fs::remove_file(&sp); }

            let listener = match UnixListener::bind(&sp) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[ginger-code] socket bind failed: {e}");
                    // spawn_resilient will sleep 2s and retry
                    return;
                }
            };

            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sp, fs::Permissions::from_mode(0o600)).ok();
            }

            println!("[ginger-code] socket listening on {}", sp.display());

            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let cp = cp.clone();
                        let sm = Arc::clone(&sm);
                        std::thread::spawn(move || handle_client(s, cp, sm));
                    }
                    Err(e) => {
                        eprintln!("[ginger-code] accept error: {e}");
                        break; // exit → spawn_resilient restarts
                    }
                }
            }
        }
    });
    

    // Tray — owns the main thread (required by macOS)
    tray::run_tray(Arc::clone(&state_map), Arc::clone(&shutdown), sock_path.clone());
}