use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};


// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ServiceEntry {
    pub service_name:    String,
    pub service_port:    u16,
    pub forwarding_port: u16,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub services: Vec<ServiceEntry>,
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
//
// The single source of truth is whether the kubectl child process is alive.
// TCP probing the forwarding port is wrong — kubectl port-forward only completes
// a connection when there is actual traffic, so a bare TCP connect always fails.

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ForwardStatus {
    /// kubectl child is running and has not exited
    Connected,
    /// No routable network interface detected (wifi off etc.)
    Offline,
    /// kubectl is not running; we are between retries
    Retrying { attempt: u32 },
}

#[derive(Debug)]
pub struct ForwardState {
    pub child:      Option<Child>,
    pub status:     ForwardStatus,
    pub restarts:   u32,
    /// Don't attempt another spawn until this instant
    pub retry_at:   Option<Instant>,
}

impl ForwardState {
    fn new_pending() -> Self {
        Self { child: None, status: ForwardStatus::Retrying { attempt: 0 }, restarts: 0, retry_at: None }
    }
}

type StateMap = Arc<Mutex<HashMap<String, ForwardState>>>;

// ── Network reachability ──────────────────────────────────────────────────────
//
// UDP "connect" to a well-known address — no packet is sent, but the OS must
// find a route. If it can't, we have no default gateway → offline.

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
}

// ── Backoff: 0→2s  1→5s  2→10s  3→20s  4+→30s ───────────────────────────────

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

fn spawn_forward(entry: &ServiceEntry) -> Option<Child> {
    let target = format!("svc/{}", entry.service_name);
    let ports  = format!("{}:{}", entry.forwarding_port, entry.service_port);
    match Command::new("kubectl")
        .args(["port-forward", &target, &ports])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            println!("[ginger-code] started  {} → localhost:{} (pid {})",
                entry.service_name, entry.forwarding_port, child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[ginger-code] kubectl spawn failed for '{}': {e}", entry.service_name);
            None
        }
    }
}

fn kill_child(child: &mut Child) {
    libc_kill(child.id() as i32, 15); // SIGTERM
    std::thread::sleep(Duration::from_millis(200));
    let _ = child.kill();
    let _ = child.wait();
}

extern "C" { fn kill(pid: libc::pid_t, sig: libc::c_int) -> libc::c_int; }
fn libc_kill(pid: i32, sig: i32) { unsafe { kill(pid, sig); } }

// ── Network monitor thread ────────────────────────────────────────────────────
//
// Polls every second. On offline→online transition: clears all backoff timers
// so the watcher fires immediately — same snap-back as Chrome's offline page.

fn run_net_monitor(state_map: StateMap) {
    let mut was_online = has_network();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let online = has_network();

        if !was_online && online {
            println!("[ginger-code] network restored — triggering immediate retry for all services");
            let mut map = state_map.lock().unwrap();
            for fw in map.values_mut() {
                // Reset backoff so watcher fires on its very next tick
                fw.retry_at = None;
                if fw.status == ForwardStatus::Offline {
                    fw.status = ForwardStatus::Retrying { attempt: 0 };
                }
            }
        } else if was_online && !online {
            println!("[ginger-code] network lost — marking all services offline");
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

// ── Watcher thread ────────────────────────────────────────────────────────────
//
// Ticks every second. Health signal = kubectl child process still alive.
// No TCP probing — kubectl holds the port open as long as the tunnel is live.

const TICK: Duration = Duration::from_secs(1);

fn run_watcher(state_map: StateMap, cfg_path: PathBuf) {
    loop {
        std::thread::sleep(TICK);

        let entries: Vec<ServiceEntry> = Config::load(&cfg_path).services;
        let active: std::collections::HashSet<String> =
            entries.iter().map(|e| e.service_name.clone()).collect();

        let mut map = state_map.lock().unwrap();

        // Tear down state for removed services
        map.retain(|name, fw| {
            if active.contains(name) { return true; }
            eprintln!("[ginger-code] removed  {} — killing forward", name);
            if let Some(ref mut c) = fw.child { kill_child(c); }
            false
        });

        for entry in &entries {
            let svc = &entry.service_name;
            map.entry(svc.clone()).or_insert_with(ForwardState::new_pending);
            let fw = map.get_mut(svc).unwrap();

            // Don't touch services waiting on network
            if fw.status == ForwardStatus::Offline { continue; }

            // Don't retry before the backoff window has elapsed
            if fw.retry_at.map_or(false, |t| Instant::now() < t) { continue; }

            // ── Health check: is the child still alive? ───────────────────
            let child_alive = fw.child.as_mut()
                .map_or(false, |c| c.try_wait().ok().flatten().is_none());

            if child_alive {
                // Process is up → connected
                fw.status   = ForwardStatus::Connected;
                fw.retry_at = None;
            } else {
                // Process died or never started → retry with backoff
                if let Some(ref mut c) = fw.child { kill_child(c); }
                fw.child = None;

                let attempt = match fw.status {
                    ForwardStatus::Retrying { attempt } => attempt,
                    _ => 0,
                };

                let delay = backoff(attempt);
                eprintln!("[ginger-code] retrying {} (attempt {}, wait {:?})",
                    svc, attempt, delay);

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
    Register { service_name: String, service_port: u16, forwarding_port: u16 },
    List,
    Remove   { service_name: String },
}

#[derive(Debug, Serialize, Clone)]
pub struct ServiceStatus {
    pub service_name:    String,
    pub service_port:    u16,
    pub forwarding_port: u16,
    pub forward_status:  ForwardStatus,
    pub restarts:        u32,
    pub pid:             Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Ok       { message: String },
    Services { services: Vec<ServiceStatus> },
    Error    { message: String },
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch(req: Request, path: &PathBuf, state_map: &StateMap) -> Response {
    match req {
        Request::Ping => Response::Ok { message: "pong".to_string() },

        Request::Register { service_name, service_port, forwarding_port } => {
            let mut cfg = Config::load(path);
            let is_new = !cfg.services.iter().any(|s| s.service_name == service_name);
            cfg.services.retain(|s| s.service_name != service_name);
            cfg.services.push(ServiceEntry {
                service_name: service_name.clone(), service_port, forwarding_port,
            });
            cfg.save(path);
            if is_new {
                state_map.lock().unwrap()
                    .insert(service_name.clone(), ForwardState::new_pending());
            }
            Response::Ok {
                message: format!("Registered '{}' — forward starting (:{} → svc:{})",
                    service_name, forwarding_port, service_port),
            }
        }

        Request::List => {
            let cfg = Config::load(path);
            let map = state_map.lock().unwrap();
            let services = cfg.services.iter().map(|e| {
                let fw = map.get(&e.service_name);
                ServiceStatus {
                    service_name:    e.service_name.clone(),
                    service_port:    e.service_port,
                    forwarding_port: e.forwarding_port,
                    forward_status:  fw.map_or(
                        ForwardStatus::Retrying { attempt: 0 },
                        |f| f.status.clone(),
                    ),
                    restarts: fw.map_or(0, |f| f.restarts),
                    pid:      fw.and_then(|f| f.child.as_ref().map(|c| c.id())),
                }
            }).collect();
            Response::Services { services }
        }

        Request::Remove { service_name } => {
            let mut cfg = Config::load(path);
            let before = cfg.services.len();
            cfg.services.retain(|s| s.service_name != service_name);
            if cfg.services.len() == before {
                return Response::Error {
                    message: format!("Service '{}' not found", service_name),
                };
            }
            cfg.save(path);
            Response::Ok {
                message: format!("Removed '{}' — forward will be torn down shortly", service_name),
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

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let sock_path = socket_path();
    let cfg_path  = config_path();

    if sock_path.exists() { fs::remove_file(&sock_path).expect("remove stale socket"); }

    let listener = UnixListener::bind(&sock_path).unwrap_or_else(|e| {
        eprintln!("[ginger-code] cannot bind {:?}: {e}", sock_path);
        std::process::exit(1);
    });

    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).ok();
    }

    println!("[ginger-code] started\n  socket : {}\n  config : {}",
        sock_path.display(), cfg_path.display());

    let state_map: StateMap = Arc::new(Mutex::new(HashMap::new()));

    // Re-seed services from config on daemon restart
    {
        let cfg = Config::load(&cfg_path);
        let mut map = state_map.lock().unwrap();
        for svc in &cfg.services {
            map.insert(svc.service_name.clone(), ForwardState::new_pending());
        }
        if !cfg.services.is_empty() {
            println!("[ginger-code] resuming {} service(s)", cfg.services.len());
        }
    }

    // Network monitor thread
    {
        let sm = Arc::clone(&state_map);
        std::thread::spawn(move || run_net_monitor(sm));
    }

    // Watcher thread
    {
        let sm = Arc::clone(&state_map);
        let cp = cfg_path.clone();
        std::thread::spawn(move || run_watcher(sm, cp));
    }

    // Accept loop
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cp = cfg_path.clone();
                let sm = Arc::clone(&state_map);
                std::thread::spawn(move || handle_client(s, cp, sm));
            }
            Err(e) => eprintln!("[ginger-code] accept error: {e}"),
        }
    }
}