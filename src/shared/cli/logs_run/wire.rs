// src/bin/logs_run/wire.rs
//
// Shared wire types that mirror tekton-sidekick's models::run_stream
// payloads. Both raw mode and TUI mode deserialize into these.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSource {
    Tekton,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StepMeta {
    pub name: String,
    #[allow(dead_code)]
    pub container: String,
    pub status: RunStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TaskMeta {
    pub name: String,
    #[allow(dead_code)]
    pub task_ref: Option<String>,
    #[allow(dead_code)]
    pub taskrun_name: String,
    #[allow(dead_code)]
    pub pod_name: Option<String>,
    pub status: RunStatus,
    #[allow(dead_code)]
    pub reason: Option<String>,
    pub steps: Vec<StepMeta>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RunMeta {
    pub run_name: String,
    pub source: RunSource,
    pub pipeline_name: Option<String>,
    pub status: RunStatus,
    #[allow(dead_code)]
    pub reason: Option<String>,
    pub tasks: Vec<TaskMeta>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LogLine {
    pub task: String,
    pub step: String,
    pub line: String,
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StepStatusUpdate {
    pub task: String,
    pub step: String,
    pub status: RunStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TaskStatusUpdate {
    pub task: String,
    pub status: RunStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RunDone {
    pub run_name: String,
    pub status: RunStatus,
    pub reason: Option<String>,
    pub duration_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct StreamError {
    pub message: String,
}

/// Minimal path-segment escaping for the run name.
pub fn urlencode_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}