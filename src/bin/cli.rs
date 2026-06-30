use clap::{Parser, Subcommand};
use ginger_shared_rs::utils::get_token_from_file_storage;
use IAMService::get_configuration as get_iam_configuration;
use MetadataService::get_configuration as get_metadata_configuration;

use ginger_code::shared::cli::{
    check_session_guard, handle_branch, logs_run, pipeline_run::run_pipeline_command, print_deployments, print_status, push_helpers::{PushResult, force_trigger, git_push, resolve_branch, resolve_remote}, send,
};

#[derive(Parser)]
#[command(
    name    = "ginger-code",
    about   = "Manage ephemeral dev environments",
    version,
    propagate_version = true,
)]
struct Cli {
    /// Set or switch the active branch (creates ephemeral env if needed)
    #[arg(short = 'b', long = "branch", value_name = "BRANCH")]
    branch: Option<String>,

    /// Base URL for the ephemeral env
    #[arg(short = 'u', long = "url", value_name = "URL", requires = "branch")]
    url: Option<String>,

    /// Force re-trigger the pipeline even if already on this branch
    #[arg(long = "rebuild-env", requires = "branch")]
    rebuild_env: bool,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check the daemon is alive
    Ping,

    /// Register a deployment into the active branch and start forwarding
    Register {
        #[arg(long)] deployment_name: String,
        #[arg(long)] deployment_port: u16,
        #[arg(long)] forwarding_port: u16,
    },

    /// Show active branch, env URL, and all registered deployments
    List,

    /// Remove a deployment and tear down its forward
    Remove {
        #[arg(long)] deployment_name: String,
    },

    /// Show the current active branch and env info
    Status,

    /// Stream logs for a Tekton PipelineRun (live or archived)
    LogsRun {
        /// namespace : {workspace}-{reponame}
        namespace: String,
        /// The PipelineRun's generated name
        run_name: String,

        /// Base URL of the tekton-sidekick service
        #[arg(long, env = "SIDEKICK_URL", default_value = "http://localhost:8000")]
        sidekick_url: String,

        /// Print newline-delimited JSON instead of the colored human-facing
        /// view — no ANSI color, no banner, one JSON object per line.
        #[arg(long, default_value_t = false)]
        raw: bool,
    },

    /// Push to git remote and watch triggered pipelines
    Push {
        /// Git remote (defaults to origin or sole remote)
        remote: Option<String>,

        /// Branch to push (defaults to current branch)
        branch: Option<String>,

        /// Base URL of the tekton-sidekick service
        #[arg(long, env = "SIDEKICK_URL", default_value = "https://tekton.gingersociety.org/sidekick")]
        sidekick_url: String,

        /// Don't open TUI after push, just print triggered pipelines
        #[arg(long, default_value_t = false)]
        no_watch: bool,

        /// Use raw NDJSON mode instead of TUI
        #[arg(long, default_value_t = false)]
        raw: bool,

        /// Skip the git push and manually trigger pipelines for the current
        /// branch — useful when code is already up to date on the remote.
        /// Derives repo name from the current directory and triggered_by from
        /// git config user.email. Opens the pipeline HEAD TUI on success
        /// (pass --no-watch to suppress).
        #[arg(long, default_value_t = false)]
        force_pipeline: bool,
    },

    Pipeline {
        /// Git ref to look up — HEAD, HEAD~1, a branch name, a partial
        /// SHA, etc. Defaults to HEAD (the last commit) when omitted.
        #[arg(default_value = "HEAD")]
        git_ref: String,

        /// Override the namespace (defaults to tasks-{repo-name}).
        #[arg(long)]
        namespace: Option<String>,

        /// Base URL of the tekton-sidekick service
        #[arg(long, env = "SIDEKICK_URL", default_value = "https://tekton.gingersociety.org/sidekick")]
        sidekick_url: String,

        /// Print newline-delimited JSON instead of the colored human-facing view.
        #[arg(long, default_value_t = false)]
        raw: bool,
    },

    #[command(hide = true)]
    Config,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Some(ref branch) = cli.branch {
        let url = cli.url.as_deref().map(|u| format!("{branch}.{u}"));
        handle_branch(branch, url.as_deref(), cli.rebuild_env).await;
        return;
    }

    let cmd = cli.command.unwrap_or(Cmd::Config);

    if matches!(cmd, Cmd::Config) {
        let token           = get_token_from_file_storage();
        let iam_config      = get_iam_configuration(Some(token.clone()));
        let metadata_config = get_metadata_configuration(Some(token.clone()));
        check_session_guard(&iam_config, &metadata_config).await;
        return;
    }

    if matches!(cmd, Cmd::Status) {
        print_status();
        return;
    }

    if let Cmd::LogsRun { ref namespace, ref run_name, ref sidekick_url, raw } = cmd {
        let targets = vec![logs_run::RunTarget {
            namespace: namespace.clone(),
            run_name: run_name.clone(),
        }];
        if let Err(e) = logs_run::stream_run_logs(sidekick_url, targets, raw).await {
            eprintln!("✗  {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Cmd::Pipeline { ref git_ref, ref namespace, ref sidekick_url, raw } = cmd {
        if let Err(e) = run_pipeline_command(git_ref, namespace.clone(), sidekick_url, raw).await {
            eprintln!("✗  {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Cmd::Push { ref remote, ref branch, ref sidekick_url, no_watch, raw, force_pipeline } = cmd {

        // ── --force-pipeline: skip git push, trigger directly, open TUI ──────
        if force_pipeline {
            let remote = match resolve_remote(remote).await {
                Ok(r) => r,
                Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
            };
            let branch = match resolve_branch(branch).await {
                Ok(b) => b,
                Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
            };

            println!("\n⚡ Pushing branch '{branch}' to '{remote}'...\n");

            let triggered = match git_push(&remote, &branch).await {
                Ok(PushResult::Triggered(runs)) => {
                    println!("\n✓ {} pipeline(s) triggered by push:\n", runs.len());
                    for p in &runs {
                        println!("  ● {}  →  {}", p.pipeline_name, p.run_name);
                        println!("    namespace: {}", p.namespace);
                    }
                    runs
                }
                Ok(PushResult::UpToDate) => {
                    println!("✓ Already up-to-date — force-triggering pipeline for branch '{branch}'...\n");

                    let runs = match force_trigger(&branch).await {
                        Ok(t) => t,
                        Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
                    };

                    if runs.is_empty() {
                        println!("✓ Pipeline triggered — no runs created (no .tekton files matched?)");
                        return;
                    }

                    println!("\n✓ {} pipeline(s) triggered:\n", runs.len());
                    for p in &runs {
                        println!("  ● {}  →  {}", p.pipeline_name, p.run_name);
                        println!("    namespace: {}", p.namespace);
                    }
                    runs
                }
                Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
            };

            if no_watch {
                return;
            }

            let targets: Vec<logs_run::RunTarget> = triggered.iter().map(|p| logs_run::RunTarget {
                namespace: p.namespace.clone(),
                run_name:  p.run_name.clone(),
            }).collect();

            if let Err(e) = logs_run::stream_run_logs(sidekick_url, targets, raw).await {
                eprintln!("✗  {e}");
                std::process::exit(1);
            }

            return;
        }

        // ── normal push path ──────────────────────────────────────────────────
        let remote = match resolve_remote(remote).await {
            Ok(r) => r,
            Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
        };
        let branch = match resolve_branch(branch).await {
            Ok(b) => b,
            Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
        };

        let triggered = match git_push(&remote, &branch).await {
            Ok(PushResult::Triggered(t)) => t,
            Ok(PushResult::UpToDate) => {
                println!("✓ Pushed — no pipelines triggered");
                return;
            }
            Err(e) => { eprintln!("✗  {e}"); std::process::exit(1); }
        };

        if triggered.is_empty() {
            println!("✓ Pushed — no pipelines triggered");
            return;
        }

        println!("\n✓ {} pipeline(s) triggered:\n", triggered.len());
        for p in &triggered {
            println!("  ● {}  →  {}", p.pipeline_name, p.run_name);
            println!("    namespace: {}", p.namespace);
        }

        if no_watch {
            return;
        }

        let targets: Vec<logs_run::RunTarget> = triggered.iter().map(|p| {
            logs_run::RunTarget {
                namespace: p.namespace.clone(),
                run_name: p.run_name.clone(),
            }
        }).collect();

        if let Err(e) = logs_run::stream_run_logs(sidekick_url, targets, raw).await {
            eprintln!("✗  {e}");
            std::process::exit(1);
        }

        return;
    }

    let val = match cmd {
        Cmd::Ping => send(r#"{"cmd":"ping"}"#),

        Cmd::List => send(r#"{"cmd":"list"}"#),

        Cmd::Register { deployment_name, deployment_port, forwarding_port } => {
            send(&serde_json::json!({
                "cmd":             "register",
                "deployment_name": deployment_name,
                "deployment_port": deployment_port,
                "forwarding_port": forwarding_port,
            }).to_string())
        }

        Cmd::Remove { deployment_name } => {
            send(&serde_json::json!({
                "cmd":             "remove",
                "deployment_name": deployment_name,
            }).to_string())
        }

        Cmd::Config | Cmd::Status | Cmd::LogsRun { .. } | Cmd::Push { .. } | Cmd::Pipeline { .. } => {
            unreachable!()
        }
    };

    match val["status"].as_str() {
        Some("ok")          => println!("✓  {}", val["message"].as_str().unwrap_or("ok")),
        Some("deployments") => print_deployments(&val),
        Some("error") => {
            eprintln!("✗  {}", val["message"].as_str().unwrap_or("unknown error"));
            std::process::exit(1);
        }
        _ => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
    }
}