#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
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

mod tray;
mod shared;

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

// ── Resolve running pod for a deployment ─────────────────────────────────────

async fn resolve_pod(
    pods:            &Api<Pod>,
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let lp   = ListParams::default().labels(&format!("app={}", deployment_name));
    let list = pods.list(&lp).await?;

    list.items
        .into_iter()
        .find(|p| {
            p.status.as_ref()
                .and_then(|s| s.phase.as_deref())
                == Some("Running")
        })
        .and_then(|p| p.metadata.name)
        .ok_or_else(|| format!("no running pod found for '{}'", deployment_name).into())
}

// ── Core forward loop ─────────────────────────────────────────────────────────

async fn run_forward(
    client:    Client,
    entry:     DeploymentEntry,
    token:     CancellationToken,
    offline:   Arc<AtomicBool>,
    state_map: StateMap,
) {
    let name = entry.deployment_name.clone();
    let pods: Api<Pod> = Api::default_namespaced(client.clone());

    // Bind the TCP listener once — it survives pod restarts.
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", entry.forwarding_port)).await {
            Ok(l) => break l,
            Err(e) => {
                eprintln!(
                    "[ginger-code] cannot bind :{} for '{}': {e} — retrying in 3s",
                    entry.forwarding_port, name
                );
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
            }
        }
    };

    println!(
        "[ginger-code] listening on :{} → {}:{}",
        entry.forwarding_port, name, entry.deployment_port
    );

    let mut attempt: u32 = 0;

    loop {
        // ── Stop requested ────────────────────────────────────────────────────
        if token.is_cancelled() {
            println!("[ginger-code] stopping forward for '{}'", name);
            return;
        }

        // ── Network offline ───────────────────────────────────────────────────
        if offline.load(Ordering::Relaxed) {
            update_status(&state_map, &name, ForwardStatus::Offline);
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            continue;
        }

        // ── Resolve current pod ───────────────────────────────────────────────
        let pod_name = match resolve_pod(&pods, &name).await {
            Ok(p) => {
                attempt = 0;
                p
            }
            Err(e) => {
                eprintln!("[ginger-code] resolve pod for '{}': {e}", name);
                update_status(&state_map, &name, ForwardStatus::Retrying { attempt });
                attempt += 1;
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(backoff(attempt)) => {}
                }
                continue;
            }
        };

        println!("[ginger-code] '{}' resolved to pod '{}'", name, pod_name);
        update_status(&state_map, &name, ForwardStatus::Connected);

        // ── Accept connections and proxy each one ─────────────────────────────
        'accept: loop {
            tokio::select! {
                _ = token.cancelled() => {
                    println!("[ginger-code] stopping forward for '{}'", name);
                    return;
                }

                accept_result = listener.accept() => {
                    let (tcp, peer) = match accept_result {
                        Ok(v)  => v,
                        Err(e) => {
                            eprintln!("[ginger-code] accept error on '{}': {e}", name);
                            break 'accept;
                        }
                    };

                    let mut pf = match pods
                        .portforward(&pod_name, &[entry.deployment_port])
                        .await
                    {
                        Ok(pf) => pf,
                        Err(e) => {
                            eprintln!(
                                "[ginger-code] portforward failed for '{}' (pod '{}'): {e}",
                                name, pod_name
                            );
                            update_status(&state_map, &name, ForwardStatus::Retrying { attempt });
                            break 'accept;
                        }
                    };

                    let stream = match pf.take_stream(entry.deployment_port) {
                        Some(s) => s,
                        None => {
                            eprintln!(
                                "[ginger-code] take_stream returned None for '{}' port {}",
                                name, entry.deployment_port
                            );
                            break 'accept;
                        }
                    };

                    println!(
                        "[ginger-code] '{}' ← new connection from {}",
                        name, peer
                    );

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
                                    eprintln!("[ginger-code] '{}' client→pod: {e}", name_clone);
                                }
                            }
                            r = pod_to_client => {
                                if let Err(e) = r {
                                    eprintln!("[ginger-code] '{}' pod→client: {e}", name_clone);
                                }
                            }
                        }

                        let _ = pf_w.shutdown().await;
                        println!("[ginger-code] '{}' connection closed", name_clone);
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
                println!("[ginger-code] '{}' status → {:?}", name, status);
                fw.status = status;
            }
        }
    }
}

// ── Start forward task ────────────────────────────────────────────────────────

fn start_forward(
    entry:     &DeploymentEntry,
    client:    Client,
    offline:   &Arc<AtomicBool>,
    state_map: &StateMap,
) {
    let token  = CancellationToken::new();
    let handle = tokio::spawn(run_forward(
        client,
        entry.clone(),
        token.clone(),
        Arc::clone(offline),
        Arc::clone(state_map),
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
    println!("[ginger-code] all forwards stopped");
}

// ── shutdown_all_threads kept for tray.rs compatibility ──────────────────────

pub fn shutdown_all_threads(state_map: &StateMap) {
    {
        let mut map = state_map.lock().unwrap();
        map.values().for_each(|fw| fw.token.cancel());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    state_map.lock().unwrap().clear();
    println!("[ginger-code] all forwards stopped");
}

// ── Start forwards for a branch ───────────────────────────────────────────────

async fn start_branch_forwards(
    cfg_path:  &PathBuf,
    branch:    &str,
    client:    &Client,
    state_map: &StateMap,
    offline:   &Arc<AtomicBool>,
) {
    let entries = BranchConfig::load(cfg_path, branch).deployments;

    if entries.is_empty() {
        println!("[ginger-code] no deployments in branch '{}'", branch);
        return;
    }

    let map = state_map.lock().unwrap();
    let new_entries: Vec<DeploymentEntry> = entries
        .into_iter()
        .filter(|e| !map.contains_key(&e.deployment_name))
        .collect();
    drop(map);

    let count = new_entries.len();
    for entry in new_entries {
        start_forward(&entry, client.clone(), offline, state_map);
    }

    println!(
        "[ginger-code] started {} forward(s) for branch '{}'",
        count, branch
    );
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

fn has_network() -> bool {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:53")?; s.local_addr() })
        .map(|a| !a.ip().is_unspecified() && !a.ip().is_loopback())
        .unwrap_or(false)
}

// ── Config watcher ────────────────────────────────────────────────────────────

async fn run_watcher(
    state_map: StateMap,
    cfg_path:  PathBuf,
    client:    Client,
    offline:   Arc<AtomicBool>,
    token:     CancellationToken,
) {
    let mut last_branch:   Option<String>                = None;
    let mut last_modified: Option<std::time::SystemTime> = None;

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
            println!(
                "[ginger-code] branch changed: {:?} → {:?}",
                last_branch, branch
            );

            stop_all_forwards(&state_map).await;

            if let Ok(mut guard) = tray::GUI_CHILD.lock() {
                if let Some(mut child) = guard.take() {
                    let _ = child.kill();
                    println!("[ginger-code] GUI closed for branch switch");
                }
            }

            last_branch = branch.clone();

            if let Some(ref active) = branch {
                start_branch_forwards(&cfg_path, active, &client, &state_map, &offline).await;
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
            eprintln!("[ginger-code] removing '{}'", name);
            tok.cancel();
            if let Some(t) = task { t.await.ok(); }
            println!("[ginger-code] forward for '{}' stopped", name);
        }

        // Start new deployments
        {
            let existing: Vec<String> = state_map.lock().unwrap()
                .keys().cloned().collect();

            for entry in &entries {
                if existing.contains(&entry.deployment_name) { continue; }
                start_forward(entry, client.clone(), &offline, &state_map);
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
    req:       Request,
    cfg_path:  &PathBuf,
    state_map: &StateMap,
    client:    &Client,
    offline:   &Arc<AtomicBool>,
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
                    start_forward(&entry, client.clone(), offline, state_map);
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
                    tokio::spawn(async move { task.await.ok(); });
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
    stream:    UnixStream,
    cfg_path:  PathBuf,
    state_map: StateMap,
    client:    Client,
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
            Ok(req) => dispatch(req, &cfg_path, &state_map, &client, &offline),
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
//
// KEY CHANGE from original:
//   We do NOT use #[tokio::main] — instead we build the runtime manually and
//   keep the main thread free so that tray::run_tray() (winit / NSApplication)
//   can run on it. On macOS the OS requires the UI event loop to live on the
//   thread that called main(). spawn_blocking does NOT satisfy this requirement.

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--gui".to_string()) {
        shared::gui::run_gui().unwrap();
        return;
    }

    println!("{:?}", args);

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

    // ── Build tokio runtime manually ──────────────────────────────────────────
    // This lets us control which thread is "main" — the tokio runtime runs on
    // its own thread pool while main() stays free for the tray event loop.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    // ── Shared state ──────────────────────────────────────────────────────────
    let state_map:      StateMap           = Arc::new(Mutex::new(HashMap::new()));
    let shutdown_token: CancellationToken  = CancellationToken::new();
    let offline:        Arc<AtomicBool>    = Arc::new(AtomicBool::new(!has_network()));

    // ── Async setup: kube client + seeding + background tasks ─────────────────
    // block_on runs the future on the runtime but still returns to main().
    let kube_client: Client = rt.block_on(async {
        let cfg_path  = cfg_path.clone();
        let state_map = Arc::clone(&state_map);
        let offline   = Arc::clone(&offline);
        let tok       = shutdown_token.clone();

        // Build kube client
        let client = match Client::try_default().await {
            Ok(c)  => c,
            Err(e) => {
                eprintln!("[ginger-code] failed to build kube client: {e}");
                eprintln!("[ginger-code] check your kubeconfig — continuing without k8s");
                panic!("cannot build kube client — is KUBECONFIG set?");
            }
        };

        // Seed state from active branch
        {
            let cfg = Config::load(&cfg_path);
            if let Some(ref branch) = cfg.active_branch {
                println!("[ginger-code] active branch: '{}'", branch);
                start_branch_forwards(
                    &cfg_path, branch, &client, &state_map, &offline,
                ).await;
            } else {
                println!("[ginger-code] no active branch — run `ginger-code -b <branch>`");
            }
        }

        // Network monitor
        tokio::spawn(run_net_monitor(
            Arc::clone(&offline),
            Arc::clone(&state_map),
            tok.clone(),
        ));

        // Config watcher
        tokio::spawn(run_watcher(
            Arc::clone(&state_map),
            cfg_path.clone(),
            client.clone(),
            Arc::clone(&offline),
            tok.clone(),
        ));

        client
    });

    // ── Socket listener — blocking thread ─────────────────────────────────────
    {
        let sp      = sock_path.clone();
        let cp      = cfg_path.clone();
        let sm      = Arc::clone(&state_map);
        let client  = kube_client.clone();
        let offline = Arc::clone(&offline);

        std::thread::spawn(move || {
            if sp.exists() { let _ = fs::remove_file(&sp); }

            let listener = match UnixListener::bind(&sp) {
                Ok(l)  => l,
                Err(e) => {
                    eprintln!("[ginger-code] socket bind failed: {e}");
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
                        let cp      = cp.clone();
                        let sm      = Arc::clone(&sm);
                        let client  = client.clone();
                        let offline = Arc::clone(&offline);
                        std::thread::spawn(move || {
                            handle_client(s, cp, sm, client, offline);
                        });
                    }
                    Err(e) => {
                        eprintln!("[ginger-code] accept error: {e}");
                        break;
                    }
                }
            }
        });
    }

    println!("{:?}", daemon_mode);

    // ── Tray or daemon — both run on the main thread ──────────────────────────
    if daemon_mode {
        // Daemon mode: block main thread on Ctrl-C signal.
        println!("[ginger-code] running in daemon mode (no tray)");
        rt.block_on(async {
            tokio::signal::ctrl_c().await.expect("set signal handler");
        });
        println!("[ginger-code] signal received, shutting down...");
        shutdown_token.cancel();
    } else {
        // Tray mode: run_tray MUST be called on the main thread (macOS/winit requirement).
        // We bridge the CancellationToken → AtomicBool so tray.rs keeps its existing API.
        let tray_shutdown = Arc::new(AtomicBool::new(false));

        // If the token is cancelled externally (e.g. from watcher), also set the bool.
        {
            let tray_sd = Arc::clone(&tray_shutdown);
            let tok     = shutdown_token.clone();
            rt.spawn(async move {
                tok.cancelled().await;
                tray_sd.store(true, Ordering::Relaxed);
            });
        }

        // ✅ This call blocks the main thread — correct on macOS.
        //    winit / NSApplication EventLoop will now be on the right thread.
        tray::run_tray(
            Arc::clone(&state_map),
            Arc::clone(&tray_shutdown),
            Arc::clone(&offline),
            sock_path.clone(),
            cfg_path.clone(),
        );

        // Tray event loop exited (user chose Quit) — cancel everything.
        shutdown_token.cancel();
    }

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    println!("[ginger-code] shutting down...");
    rt.block_on(stop_all_forwards(&state_map));
    let _ = fs::remove_file(&sock_path);
    println!("[ginger-code] bye");
}