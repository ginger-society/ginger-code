//! Helpers for communicating with the ginger-code daemon over a Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

// ── Socket path ───────────────────────────────────────────────────────────────

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

// ── Low-level send / receive ──────────────────────────────────────────────────

pub fn send_to_daemon(payload: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let path   = socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        format!(
            "Cannot connect to ginger-code daemon at {}: {e}",
            path.display()
        )
    })?;
    stream.write_all(format!("{payload}\n").as_bytes())?;
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp)?;
    Ok(serde_json::from_str(&resp)?)
}

// ── Convenience wrappers ──────────────────────────────────────────────────────

/// Returns `Err` with a human-readable message if the daemon cannot be reached.
pub fn assert_daemon_reachable() -> Result<(), Box<dyn std::error::Error>> {
    match send_to_daemon(r#"{"cmd":"ping"}"#) {
        Ok(val) if val["status"] == "ok" => Ok(()),
        Ok(val) => Err(format!(
            "Daemon responded unexpectedly to ping: {val}\n\
             Make sure ginger-code is running."
        )
        .into()),
        Err(e) => Err(format!(
            "Cannot reach ginger-code daemon: {e}\n\
             Start it with: ginger-code\n\
             Or check that it is running in the system tray."
        )
        .into()),
    }
}

/// Ask the daemon to start port-forwarding `deployment_name:deployment_port`
/// on `forwarding_port`.
pub fn daemon_register(
    deployment_name: &str,
    deployment_port: u16,
    forwarding_port: u16,
    organization_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::json!({
        "cmd":             "register",
        "deployment_name": deployment_name,
        "deployment_port": deployment_port,
        "forwarding_port": forwarding_port,
        "organization_id": organization_id,
    });

    match send_to_daemon(&payload.to_string()) {
        Ok(resp) if resp["status"] == "ok" => {
            println!(
                "✓ Registered '{}' with daemon — port-forward on localhost:{}",
                deployment_name, forwarding_port
            );
            Ok(())
        }
        Ok(resp) => {
            eprintln!("Warning: daemon responded unexpectedly: {resp}");
            Ok(()) // non-fatal
        }
        Err(e) => Err(format!(
            "Could not register with daemon: {e}\n\
             Register manually:\n  \
             ginger-code register \
             --deployment-name {deployment_name} \
             --deployment-port {deployment_port} \
             --forwarding-port {forwarding_port}"
        )
        .into()),
    }
}

/// Ask the daemon to stop port-forwarding for `deployment_name`.
pub fn daemon_remove(deployment_name: &str) {
    let payload = serde_json::json!({
        "cmd":             "remove",
        "deployment_name": deployment_name,
    });

    match send_to_daemon(&payload.to_string()) {
        Ok(resp) if resp["status"] == "ok" => {
            println!("✓ Notified daemon — port-forward stopped immediately");
        }
        Ok(resp) => eprintln!("Warning: daemon responded unexpectedly: {resp}"),
        Err(_) => {
            println!("  (daemon unreachable — forward will stop on next watcher tick)");
        }
    }
}

/// Return all forwarding ports currently registered with the daemon.
pub fn daemon_used_ports() -> std::collections::HashSet<u16> {
    match send_to_daemon(r#"{"cmd":"list"}"#) {
        Ok(val) => val["deployments"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|d| d["forwarding_port"].as_u64().map(|p| p as u16))
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    }
}