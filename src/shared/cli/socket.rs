use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

pub fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

pub fn send(payload: &str) -> serde_json::Value {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).unwrap_or_else(|e| {
        eprintln!("error: cannot connect to daemon at {}: {e}", path.display());
        eprintln!("hint : start the daemon with `ginger-code` (via the tray app)");
        std::process::exit(1);
    });
    stream
        .write_all(format!("{payload}\n").as_bytes())
        .expect("write");
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp).expect("read");
    serde_json::from_str(&resp)
        .unwrap_or_else(|_| serde_json::json!({"status":"error","message":"malformed response"}))
}