use clap::{Parser, Subcommand};
use ginger_shared_rs::utils::get_token_from_file_storage;
use IAMService::get_configuration as get_iam_configuration;
use MetadataService::get_configuration as get_metadata_configuration;

use ginger_code::shared::cli::{
    check_session_guard, handle_branch, logs_run, print_deployments, print_status, send,
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
        /// The PipelineRun's generated name
        run_name: String,
 
        /// Base URL of the tekton-sidekick service
        #[arg(long, env = "SIDEKICK_URL", default_value = "http://localhost:8000")]
        sidekick_url: String,

        /// Print newline-delimited JSON instead of the colored
        /// human-facing view -- no ANSI color, no banner, one JSON
        /// object per line. Intended for AI coding agents or other
        /// tooling consuming this stream programmatically.
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

    if let Cmd::LogsRun { ref run_name, ref sidekick_url, raw } = cmd {
        if let Err(e) = logs_run::stream_run_logs(sidekick_url, run_name, raw).await {
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

        Cmd::Config | Cmd::Status  | Cmd::LogsRun { .. }=> unreachable!(),
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