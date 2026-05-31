//! Port-discovery helpers shared between eject and mount.

use std::net::TcpListener;

use crate::shared::ui::eject::daemon::daemon_used_ports;


/// Find a free port in 2200–2299 that is not already registered with the daemon.
pub fn find_free_22xx_port() -> Result<u16, Box<dyn std::error::Error>> {
    let used = daemon_used_ports();

    for port in 2200u16..=2299 {
        if used.contains(&port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    Err("No free port available in the 2200–2299 range".into())
}