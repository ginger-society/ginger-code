// src/bin/logs_run.rs

pub mod raw;
pub mod tui;
pub mod wire;

/// A single pipeline run to watch, with its namespace.
#[derive(Debug, Clone)]
pub struct RunTarget {
    pub namespace: String,
    pub run_name: String,
    /// Short (8-char) commit SHA this run was triggered for, if known.
    pub commit_sha: Option<String>,
    /// Subject line of that commit, if known.
    pub commit_message: Option<String>,
}

impl RunTarget {
    /// Plain constructor — used by call sites that don't have commit
    /// info available (LogsRun, Push).
    pub fn new(namespace: String, run_name: String) -> Self {
        RunTarget {
            namespace,
            run_name,
            commit_sha: None,
            commit_message: None,
        }
    }

    /// Attach commit info — used by the `pipeline` subcommand, which
    /// resolves a sha/message before looking up matching runs.
    pub fn with_commit(mut self, sha: String, message: String) -> Self {
        self.commit_sha = Some(sha);
        self.commit_message = Some(message);
        self
    }
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