// src/shared/cli/push_helper.rs

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;  // ← must be tokio::process, not std::process
use std::process::Stdio;

#[derive(Debug, Clone)]
pub struct TriggeredPipeline {
    pub pipeline_name: String,
    pub run_name: String,
    pub namespace: String,
}

#[derive(Debug)]
enum GitterEvent {
    Namespace(String),
    PipelineTriggering(String),
    RunCreated(String),
}

fn parse_gitter_line(line: &str) -> Option<GitterEvent> {
    let content = line.strip_prefix("remote: [ginger-gitter] ")?.trim();

    if let Some(ns) = content.strip_prefix("Target namespace: ") {
        return Some(GitterEvent::Namespace(ns.trim().to_string()));
    }
    if let Some(rest) = content.strip_prefix("PipelineRun/") {
        if let Some(run_name) = rest.strip_suffix(" created") {
            return Some(GitterEvent::RunCreated(run_name.trim().to_string()));
        }
    }
    if let Some(name) = content.strip_prefix("Triggering pipeline: ") {
        return Some(GitterEvent::PipelineTriggering(name.trim().to_string()));
    }

    None
}

pub async fn git_push(
    remote: &str,
    branch: &str,
) -> Result<Vec<TriggeredPipeline>, Box<dyn std::error::Error>> {
    let mut child = Command::new("git")
        .args(["push", remote, branch])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = child.stderr.take().expect("stderr not captured");
    let mut lines = BufReader::new(stderr).lines();

    let mut current_namespace: Option<String> = None;
    let mut current_pipeline: Option<String> = None;
    let mut triggered: Vec<TriggeredPipeline> = Vec::new();

    while let Some(line) = lines.next_line().await? {
        eprintln!("{line}");

        match parse_gitter_line(&line) {
            Some(GitterEvent::Namespace(ns)) => {
                current_namespace = Some(ns);
            }
            Some(GitterEvent::PipelineTriggering(name)) => {
                current_pipeline = Some(name);
            }
            Some(GitterEvent::RunCreated(run_name)) => {
                if let (Some(ns), Some(pipeline)) =
                    (current_namespace.clone(), current_pipeline.clone())
                {
                    triggered.push(TriggeredPipeline {
                        pipeline_name: pipeline,
                        run_name,
                        namespace: ns,
                    });
                    current_pipeline = None;
                }
            }
            None => {}
        }
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(format!("git push failed with exit code {:?}", status.code()).into());
    }

    Ok(triggered)
}

pub async fn resolve_remote(
    remote: &Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(r) = remote {
        return Ok(r.clone());
    }
    // tokio::process::Command — output() is async here
    let out = Command::new("git")
        .args(["remote"])
        .output()
        .await?;
    let remotes: Vec<&str> = std::str::from_utf8(&out.stdout)?
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    Ok(if remotes.len() == 1 {
        remotes[0].to_string()
    } else {
        "origin".to_string()
    })
}

pub async fn resolve_branch(
    branch: &Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(b) = branch {
        return Ok(b.clone());
    }
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await?;
    Ok(std::str::from_utf8(&out.stdout)?.trim().to_string())
}