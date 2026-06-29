// src/shared/cli/push_helper.rs

use ginger_gitter::apis::default_api::HandleTriggerPipelineParams;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
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

/// Manually trigger a pipeline without pushing — used by `--force-pipeline`.
///
/// Derives the repo name from the current directory's parent folder name
/// (same convention as ginger-gitter) and the triggered_by from git config
/// user.email. Returns the triggered pipeline info so the caller can open
/// the TUI.
pub async fn force_trigger(
    branch: &str,
) -> Result<Vec<TriggeredPipeline>, Box<dyn std::error::Error>> {
    use ginger_gitter::{apis::default_api::handle_trigger_pipeline, get_configuration, models::TriggerPipelineRequest};
    use ginger_shared_rs::utils::get_token_from_file_storage;

    let repo = resolve_repo_name().await?;
    let triggered_by = Some(resolve_git_email().await?);

    println!("  repo        : {repo}");
    println!("  branch      : {branch}");
    println!("  triggered_by: {:?}", triggered_by);

    let token = get_token_from_file_storage();
    let gitter_config = get_configuration(Some(token));

    let response = handle_trigger_pipeline(
        &gitter_config,
        HandleTriggerPipelineParams {
            trigger_pipeline_request: TriggerPipelineRequest {
                branch: branch.to_string(),
                repo,
                triggered_by,
            },
        },
    )
    .await
    .map_err(|e| format!("trigger_pipeline API call failed: {e:?}"))?;

    let triggered = response
        .pipeline_runs
        .unwrap_or_default()
        .into_iter()
        .map(|pr| TriggeredPipeline {
            pipeline_name: pr.pipeline_name,
            run_name:      pr.run_name,
            namespace:     pr.namespace,
        })
        .collect();

    Ok(triggered)
}

/// Returns the current directory's folder name — ginger-gitter's convention
/// for the repo name used in namespace and pipeline lookups.
pub async fn resolve_repo_name() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    cwd.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "could not determine repo name from current directory".into())
}

/// Returns the git config user.email for the current repo.
pub async fn resolve_git_email() -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .await?;
    let email = std::str::from_utf8(&out.stdout)?.trim().to_string();
    if email.is_empty() {
        return Err("git config user.email is not set".into());
    }
    Ok(email)
}

pub async fn resolve_remote(
    remote: &Option<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(r) = remote {
        return Ok(r.clone());
    }
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