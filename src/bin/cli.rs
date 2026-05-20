use clap::{Parser, Subcommand};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

// ── ANSI colours (only when stdout is a tty) ──────────────────────────────────

const GREEN:  &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED:    &str = "\x1b[31m";
const BOLD:   &str = "\x1b[1m";
const RESET:  &str = "\x1b[0m";

fn is_tty() -> bool {
    // SAFETY: trivial libc call
    unsafe { libc::isatty(1) == 1 }
}

struct Colour {
    on: bool,
}
impl Colour {
    fn new() -> Self { Self { on: is_tty() } }
    fn paint<'a>(&self, code: &'static str, s: &'a str) -> String {
        if self.on { format!("{code}{s}{RESET}") } else { s.to_string() }
    }
}

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "ginger-code",
    about   = "Manage resilient kubectl port-forwards",
    version,
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check the daemon is alive
    Ping,

    /// Register a service and start forwarding immediately
    Register {
        /// Kubernetes service name (must match `kubectl get svc`)
        #[arg(long)]
        service_name: String,
        /// Port exposed by the Kubernetes service
        #[arg(long)]
        service_port: u16,
        /// Local port kubectl will bind
        #[arg(long)]
        forwarding_port: u16,
    },

    /// Show all registered services with live connection status
    List,

    /// Remove a service and tear down its forward
    Remove {
        /// Kubernetes service name to remove
        #[arg(long)]
        service_name: String,
    },
}

// ── Socket I/O ────────────────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

fn send(payload: &str) -> serde_json::Value {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).unwrap_or_else(|e| {
        eprintln!("error: cannot connect to daemon at {}: {e}", path.display());
        eprintln!("hint : start the daemon with `ginger-code`");
        std::process::exit(1);
    });
    stream.write_all(format!("{payload}\n").as_bytes()).expect("write failed");
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).expect("read failed");
    serde_json::from_str(&resp)
        .unwrap_or_else(|_| serde_json::json!({"status":"error","message":"malformed response"}))
}

// ── List rendering ────────────────────────────────────────────────────────────

fn print_services(val: &serde_json::Value) {
    let c = Colour::new();

    let svcs = match val["services"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => { println!("(no services registered)"); return; }
    };

    let name_w = svcs.iter()
        .map(|s| s["service_name"].as_str().unwrap_or("").len())
        .max().unwrap_or(12).max(12);

    // Header
    println!("{}{:<name_w$}  {:>9}  {:>8}  {:>8}  {:<13}  {}{}",
        BOLD,
        "SERVICE", "SVC PORT", "FWD PORT", "RESTARTS", "STATUS", "PID",
        RESET,
        name_w = name_w);
    println!("{}", "─".repeat(name_w + 9 + 8 + 8 + 13 + 20));

    for s in svcs {
        let name     = s["service_name"].as_str().unwrap_or("?");
        let sport    = s["service_port"].as_u64().unwrap_or(0);
        let fport    = s["forwarding_port"].as_u64().unwrap_or(0);
        let restarts = s["restarts"].as_u64().unwrap_or(0);
        let pid_str  = s["pid"].as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        // ForwardStatus is a serde internally-tagged enum: {"status":"connected"}
        // {"status":"offline"} or {"status":"retrying","attempt":N}
        let fst = &s["forward_status"];
        let tag = fst["status"].as_str().unwrap_or("retrying");
        let (icon, label) = match tag {
            "connected" => (c.paint(GREEN,  "●"), c.paint(GREEN,  "CONNECTED")),
            "offline"   => (c.paint(RED,    "○"), c.paint(RED,    "OFFLINE")),
            _           => {
                let attempt = fst["attempt"].as_u64().unwrap_or(0);
                (c.paint(YELLOW, "↻"), c.paint(YELLOW, &format!("RETRYING (#{attempt})")))
            }
        };

        // icon is 1 visible char + ANSI codes; pad label to fixed visible width
        println!("{:<name_w$}  {:>9}  {:>8}  {:>8}  {} {:<12}  {}",
            name, sport, fport, restarts,
            icon, label, pid_str,
            name_w = name_w);
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let val = match cli.command {
        Cmd::Ping => send(r#"{"cmd":"ping"}"#),

        Cmd::List => send(r#"{"cmd":"list"}"#),

        Cmd::Register { service_name, service_port, forwarding_port } => {
            send(&serde_json::json!({
                "cmd":             "register",
                "service_name":    service_name,
                "service_port":    service_port,
                "forwarding_port": forwarding_port,
            }).to_string())
        }

        Cmd::Remove { service_name } => {
            send(&serde_json::json!({
                "cmd":          "remove",
                "service_name": service_name,
            }).to_string())
        }
    };

    match val["status"].as_str() {
        Some("ok")       => println!("✓  {}", val["message"].as_str().unwrap_or("ok")),
        Some("services") => print_services(&val),
        Some("error")    => {
            eprintln!("✗  {}", val["message"].as_str().unwrap_or("unknown error"));
            std::process::exit(1);
        }
        _ => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
    }
}