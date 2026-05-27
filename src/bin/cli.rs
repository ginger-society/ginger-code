use clap::{Parser, Subcommand};
use ginger_code::shared;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use ginger_shared_rs::utils::get_token_from_file_storage;
use IAMService::apis::configuration::Configuration as IAMConfiguration;
use IAMService::apis::default_api::identity_validate_api_token;
use IAMService::get_configuration as get_iam_configuration;
use MetadataService::apis::configuration::Configuration as MetadataConfiguration;
use MetadataService::get_configuration as get_metadata_configuration;
use shared::ui::fetch_metadata_and_process;

// ── ANSI colours ──────────────────────────────────────────────────────────────

const GREEN:  &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED:    &str = "\x1b[31m";
const CYAN:   &str = "\x1b[36m";
const BOLD:   &str = "\x1b[1m";
const RESET:  &str = "\x1b[0m";

fn is_tty() -> bool { unsafe { libc::isatty(1) == 1 } }

struct Colour { on: bool }
impl Colour {
    fn new() -> Self { Self { on: is_tty() } }
    fn paint(&self, code: &'static str, s: &str) -> String {
        if self.on { format!("{code}{s}{RESET}") } else { s.to_string() }
    }
}

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "ginger-code",
    about   = "Manage ephemeral dev environments and kubectl port-forwards",
    version,
    propagate_version = true,
)]
struct Cli {
    /// Set or switch the active branch (creates ephemeral env if needed)
    #[arg(short = 'b', long = "branch", value_name = "BRANCH")]
    branch: Option<String>,

    /// Environment name to associate with the branch (e.g. feat01)
    #[arg(short = 'e', long = "env", value_name = "ENV", requires = "branch")]
    env: Option<String>,

    /// Public URL for the ephemeral env (e.g. feat01.ginger-society.test-env.rackmint.com)
    #[arg(short = 'u', long = "url", value_name = "URL", requires = "branch")]
    url: Option<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check the daemon is alive
    Ping,

    /// Register a deployment into the active branch and start forwarding
    Register {
        #[arg(long)]
        deployment_name: String,
        #[arg(long)]
        deployment_port: u16,
        #[arg(long)]
        forwarding_port: u16,
    },

    /// Show active branch, env URL, and all registered deployments
    List,

    /// Remove a deployment and tear down its forward
    Remove {
        #[arg(long)]
        deployment_name: String,
    },

    /// Show the current active branch and env info
    Status,

    #[command(hide = true)]
    Config,
}

// ── Paths ─────────────────────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("code.toml")
}

fn branches_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("branches")
}

fn branch_slug(branch: &str) -> String {
    branch.replace('/', "-")
}

fn branch_toml_path(branch: &str) -> PathBuf {
    branches_dir().join(format!("{}.toml", branch_slug(branch)))
}

// ── code.toml read/write (minimal, no daemon needed) ─────────────────────────

#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct CodeConfig {
    pub active_branch: Option<String>,
    pub active_env:    Option<String>,
    pub active_url:    Option<String>,
}

impl CodeConfig {
    fn load() -> Self {
        let path = config_path();
        if !path.exists() { return Self::default(); }
        toml::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = config_path();
        if let Some(p) = path.parent() { std::fs::create_dir_all(p).ok(); }
        std::fs::write(&path, toml::to_string_pretty(self).expect("toml"))
            .expect("write code.toml");
    }
}

// ── Socket I/O ────────────────────────────────────────────────────────────────

fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

fn send(payload: &str) -> serde_json::Value {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).unwrap_or_else(|e| {
        eprintln!("error: cannot connect to daemon at {}: {e}", path.display());
        eprintln!("hint : start the daemon with `ginger-code` (via the tray app)");
        std::process::exit(1);
    });
    stream.write_all(format!("{payload}\n").as_bytes()).expect("write");
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).expect("read");
    serde_json::from_str(&resp)
        .unwrap_or_else(|_| serde_json::json!({"status":"error","message":"malformed response"}))
}

// ── -b / --branch handler ─────────────────────────────────────────────────────
//
// This just writes code.toml.  The daemon watches the file and handles the
// rest (stop old forwards, kill GUI, start new forwards) within ~2 seconds.
// Works whether the daemon is running or not.

fn handle_branch(branch: &str, env: Option<&str>, url: Option<&str>) {
    let c = Colour::new();

    let mut cfg = CodeConfig::load();

    let prev = cfg.active_branch.clone();
    let switching = prev.as_deref() != Some(branch);

    cfg.active_branch = Some(branch.to_string());
    cfg.active_env    = env.map(|s| s.to_string())
        .or_else(|| if switching { None } else { cfg.active_env.clone() });
    cfg.active_url    = url.map(|s| s.to_string())
        .or_else(|| if switching { None } else { cfg.active_url.clone() });

    cfg.save();

    // Ensure branches/ dir and an empty branch toml exist so the daemon
    // has something to load even before any Register calls.
    let branch_path = branch_toml_path(branch);
    if !branch_path.exists() {
        if let Some(p) = branch_path.parent() { std::fs::create_dir_all(p).ok(); }
        std::fs::write(&branch_path, "# deployments for branch\n[[deployments]]\n")
            .unwrap_or_else(|e| eprintln!("warn: could not create branch toml: {e}"));
        // Write an empty valid toml (no deployments yet)
        std::fs::write(&branch_path, "")
            .unwrap_or_else(|e| eprintln!("warn: could not init branch toml: {e}"));
    }

    if switching {
        if let Some(ref prev_branch) = prev {
            println!(
                "{}  Switched branch:  {} → {}{}",
                c.paint(CYAN, "⎇"),
                c.paint(YELLOW, prev_branch),
                c.paint(GREEN, branch),
                RESET,
            );
        } else {
            println!(
                "{}  Active branch set to: {}",
                c.paint(CYAN, "⎇"),
                c.paint(GREEN, branch),
            );
        }
    } else {
        println!(
            "{}  Already on branch: {}  (env/url updated)",
            c.paint(CYAN, "⎇"),
            c.paint(GREEN, branch),
        );
    }

    if let Some(e) = &cfg.active_env {
        println!("   env : {}", c.paint(CYAN, e));
    }
    if let Some(u) = &cfg.active_url {
        println!("   url : {}", c.paint(CYAN, u));
    }

    if daemon_running() {
        println!("\n   Daemon detected — it will pick up the change within ~2s.");
        println!("   Reopen the dashboard to see the new branch.");
    } else {
        println!("\n   {}Daemon not running{} — changes will take effect when it starts.", YELLOW, RESET);
        println!("   Launch it via the ginger-code tray app.");
    }
}

// ── List rendering ────────────────────────────────────────────────────────────

fn print_deployments(val: &serde_json::Value) {
    let c = Colour::new();

    // Show branch / url header
    if let Some(branch) = val["active_branch"].as_str() {
        println!("{}branch:{} {}", BOLD, RESET, c.paint(GREEN, branch));
    }
    if let Some(url) = val["active_url"].as_str() {
        println!("{}url:   {}{}", BOLD, RESET, c.paint(CYAN, url));
    }
    println!();

    let deps = match val["deployments"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => { println!("(no deployments registered for this branch)"); return; }
    };

    let name_w = deps.iter()
        .map(|d| d["deployment_name"].as_str().unwrap_or("").len())
        .max().unwrap_or(16).max(16);

    println!("{}{:<name_w$}  {:>9}  {:>8}  {:>8}  {:<13}  {}{}",
        BOLD,
        "DEPLOYMENT", "DEP PORT", "FWD PORT", "RESTARTS", "STATUS", "PID",
        RESET,
        name_w = name_w,
    );
    println!("{}", "─".repeat(name_w + 60));

    for d in deps {
        let name     = d["deployment_name"].as_str().unwrap_or("?");
        let dport    = d["deployment_port"].as_u64().unwrap_or(0);
        let fport    = d["forwarding_port"].as_u64().unwrap_or(0);
        let restarts = d["restarts"].as_u64().unwrap_or(0);
        let pid_str  = d["pid"].as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        let fst = &d["forward_status"];
        let tag = fst["status"].as_str().unwrap_or("retrying");
        let (icon, label) = match tag {
            "connected" => (c.paint(GREEN,  "●"), c.paint(GREEN,  "CONNECTED")),
            "offline"   => (c.paint(RED,    "○"), c.paint(RED,    "OFFLINE")),
            _ => {
                let attempt = fst["attempt"].as_u64().unwrap_or(0);
                (c.paint(YELLOW, "↻"), c.paint(YELLOW, &format!("RETRYING (#{attempt})")))
            }
        };

        println!("{:<name_w$}  {:>9}  {:>8}  {:>8}  {} {:<12}  {}",
            name, dport, fport, restarts,
            icon, label, pid_str,
            name_w = name_w,
        );
    }
}

fn print_status() {
    let c   = Colour::new();
    let cfg = CodeConfig::load();

    println!("{}ginger-code status{}", BOLD, RESET);
    println!();

    match cfg.active_branch {
        Some(ref b) => println!("  branch : {}", c.paint(GREEN, b)),
        None        => println!("  branch : {}", c.paint(YELLOW, "(none — run `ginger-code -b <branch>`)")),
    }
    match cfg.active_env {
        Some(ref e) => println!("  env    : {}", c.paint(CYAN, e)),
        None        => println!("  env    : -"),
    }
    match cfg.active_url {
        Some(ref u) => println!("  url    : {}", c.paint(CYAN, u)),
        None        => println!("  url    : -"),
    }

    println!();
    if daemon_running() {
        println!("  daemon : {}", c.paint(GREEN, "running"));
    } else {
        println!("  daemon : {}", c.paint(RED, "not running"));
    }
}

// ── Session-guarded async commands ────────────────────────────────────────────

#[tokio::main]
async fn check_session_guard(
    cmd:             &Cmd,
    iam_config:      &IAMConfiguration,
    metadata_config: &MetadataConfiguration,
) {
    match identity_validate_api_token(iam_config).await {
        Ok(session_details) => {
            match cmd {
                Cmd::Config => {
                    fetch_metadata_and_process(metadata_config, &session_details.sub).await;
                }
                _ => unreachable!(),
            }
        }
        Err(e) => {
            eprintln!("Token validation failed: {:?}", e);
            std::process::exit(1);
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    // ── -b / --branch flag (highest priority, no daemon needed) ──────────────
    if let Some(ref branch) = cli.branch {
        handle_branch(branch, cli.env.as_deref(), cli.url.as_deref());
        return;
    }

    let cmd = cli.command.unwrap_or(Cmd::Config);

    // ── Config (session-guarded metadata fetch) ───────────────────────────────
    if matches!(cmd, Cmd::Config) {
        let token           = get_token_from_file_storage();
        let iam_config      = get_iam_configuration(Some(token.clone()));
        let metadata_config = get_metadata_configuration(Some(token.clone()));
        check_session_guard(&cmd, &iam_config, &metadata_config);
        return;
    }

    // ── Status (no daemon needed) ─────────────────────────────────────────────
    if matches!(cmd, Cmd::Status) {
        print_status();
        return;
    }

    // ── All other commands talk to the daemon ─────────────────────────────────
    let val = match cmd {
        Cmd::Ping => send(r#"{"cmd":"ping"}"#),

        Cmd::List => send(r#"{"cmd":"list"}"#),

        Cmd::Register { deployment_name, deployment_port, forwarding_port } => {
            send(&serde_json::json!({
                "cmd":             "register",
                "deployment_name": deployment_name,
                "deployment_port": deployment_port,
                "forwarding_port": forwarding_port,
            }).to_string())
        }

        Cmd::Remove { deployment_name } => {
            send(&serde_json::json!({
                "cmd":             "remove",
                "deployment_name": deployment_name,
            }).to_string())
        }

        Cmd::Config | Cmd::Status => unreachable!(),
    };

    match val["status"].as_str() {
        Some("ok")          => println!("✓  {}", val["message"].as_str().unwrap_or("ok")),
        Some("deployments") => print_deployments(&val),
        Some("error")       => {
            eprintln!("✗  {}", val["message"].as_str().unwrap_or("unknown error"));
            std::process::exit(1);
        }
        _ => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
    }
}