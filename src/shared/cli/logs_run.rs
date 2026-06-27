// src/bin/logs_run.rs

pub mod raw;
pub mod tui;
pub mod wire;

/// A single pipeline run to watch, with its namespace.
#[derive(Debug, Clone)]
pub struct RunTarget {
    pub namespace: String,
    pub run_name: String,
}

pub async fn stream_run_logs(
    base_url: &str,
    targets: Vec<RunTarget>,
    raw: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if raw {
        self::raw::run(base_url, targets).await
    } else {
        self::tui::run(base_url, targets).await
    }
}