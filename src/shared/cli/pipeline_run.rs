// src/shared/cli/pipeline_run.rs
//
// Backing logic for `ginger-code pipeline <ref> [--namespace <ns>]`.
//
// Flow:
//   1. Resolve <ref> to an 8-char short SHA (git rev-parse --short=8).
//   2. Resolve the repo name (git toplevel basename) and, unless
//      --namespace was given explicitly, derive the namespace as
//      tasks-{repo-name}.
//   3. Query GET /<namespace>/runs-by-label?labels=ginger-gitter/repo=...,
//      ginger-gitter/sha=... via the generated tekton-sidekick client.
//   4. If that finds matches, feed every matched run into
//      logs_run::stream_run_logs as RunTargets (mirrors how `Push`
//      already does this for multiple pipelines triggered by one push).
//   5. If zero matches, the commit's pipeline may not have run yet, or
//      it ran as part of an earlier/different SHA on this branch — fall
//      back to a repo-only query (no sha filter) at limit=5 and print
//      those as candidates rather than guessing which one the user meant.

use ginger_shared_rs::utils::get_token_from_file_storage;
use tekton_sidekick::{apis::default_api::{RoutesRunsByLabelRunsByLabelParams, routes_runs_by_label_runs_by_label}, get_configuration};

use tekton_sidekick::apis::Error as SidekickApiError;

use super::logs_run::{self, RunTarget};
use super::pipeline_helper::{namespace_for_repo, resolve_repo_name, resolve_sha};

/// Maps the generated client's `Error<E>` into a plain message. Handled
/// explicitly (rather than relying on `?`/`From` to do this implicitly)
/// because openapi-generator's `Error<T>` carries useful structure
/// (status code + raw response body for `ResponseError`) that a bare
/// `Debug`-derived conversion would flatten into a much less readable
/// message — this pulls the status + body out specifically for that case.
fn describe_api_error<E: std::fmt::Debug>(e: SidekickApiError<E>) -> String {
    match e {
        SidekickApiError::ResponseError(resp) => {
            format!("sidekick returned {}: {}", resp.status, resp.content)
        }
        other => format!("{other:?}"),
    }
}

/// `git_ref` is the commit-ish to look up (`HEAD`, `HEAD~1`, a branch, a
/// partial SHA, etc — anything `git rev-parse` accepts). `namespace`, if
/// `Some`, overrides the `tasks-{repo-name}` default derived from the
/// git repo root's folder name.
pub async fn run_pipeline_command(
    git_ref: &str,
    namespace: Option<String>,
    sidekick_url: &str,
    raw: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo_name = resolve_repo_name().await?;
    let namespace = namespace.unwrap_or_else(|| namespace_for_repo(&repo_name));
    let sha = resolve_sha(git_ref).await?;

    println!("→ repo: {repo_name}   namespace: {namespace}   sha: {sha}   ref: {git_ref}");

    let token = get_token_from_file_storage();
    let config = get_configuration(Some(token));

    let labels = format!("ginger-gitter/repo={repo_name},ginger-gitter/sha={sha}");

    let response = routes_runs_by_label_runs_by_label(
        &config,
        RoutesRunsByLabelRunsByLabelParams {
            namespace: namespace.clone(),
            labels,
            limit: None, // default (7) is plenty for a single-sha lookup
        },
    )
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { describe_api_error(e).into() })?;

    if response.is_empty() {
        // No PipelineRun matched this exact repo+sha pair. Most likely
        // explanations: CI hasn't picked up this commit yet, or it
        // landed bundled into a push alongside other commits where only
        // the latest sha got labeled — either way, guessing which other
        // run the user meant would be worse than just showing them the
        // recent options and letting them re-run with the right ref.
        println!(
            "\n✗  No pipeline run found for sha {sha}.\n   It may have been pushed together with other commits — check the runs below and re-run with the matching ref if one applies.\n"
        );

        let fallback_labels = format!("ginger-gitter/repo={repo_name}");
        let fallback = routes_runs_by_label_runs_by_label(
            &config,
            RoutesRunsByLabelRunsByLabelParams {
                namespace,
                labels: fallback_labels,
                limit: Some(5),
            },
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { describe_api_error(e).into() })?;

        if fallback.is_empty() {
            println!("   (no pipeline runs found for this repo at all)");
        } else {
            println!("   Here are the last {} run(s) for {repo_name}:\n", fallback.len());
            for run in &fallback {
                println!("     {}   {}", run.created_time, run.name);
            }
            println!();
        }

        return Ok(());
    }

    println!("\n✓ found {} matching run(s):\n", response.len());
    for run in &response {
        println!("  ● {}", run.name);
    }
    println!();

    let targets: Vec<RunTarget> = response
        .iter()
        .map(|run| RunTarget {
            namespace: namespace.clone(),
            run_name: run.name.clone(),
        })
        .collect();

    logs_run::stream_run_logs(sidekick_url, targets, raw).await
}