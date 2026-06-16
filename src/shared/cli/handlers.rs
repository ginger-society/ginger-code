use super::colour::{Colour, BOLD, CYAN, GREEN, RED, RESET, YELLOW};
use super::config::{branch_toml_path, CodeConfig};
use super::socket::daemon_running;
use ginger_gitter::apis::configuration::Configuration as GingerGitterConfiguration;
use ginger_gitter::apis::default_api::{HandleRunPipelineParams, HandleTriggerPipelineParams, handle_run_pipeline, handle_trigger_pipeline};
use ginger_gitter::get_configuration as get_ginger_gitter_configuration;
use ginger_gitter::models::{PipelineParam, RunPipelineRequest, TriggerPipelineRequest};
use ginger_shared_rs::utils::get_token_from_file_storage;


pub async fn handle_branch(branch: &str, url: Option<&str>, rebuild_env: bool) {
    let c = Colour::new();
    let token = get_token_from_file_storage();
    let gitter_config = get_ginger_gitter_configuration(Some(token));

    let mut cfg = CodeConfig::load();
    let prev = cfg.active_branch.clone();
    let switching = prev.as_deref() != Some(branch);
    // Trigger if switching to a new branch OR explicitly asked to rebuild
    let should_trigger = switching || rebuild_env;

    cfg.active_branch = Some(branch.to_string());
    cfg.active_url = url
        .map(|s| s.to_string())
        .or_else(|| cfg.active_url.clone()); // preserve existing url if not switching
    cfg.save();

    let branch_path = branch_toml_path(branch);
    if !branch_path.exists() {
        if let Some(p) = branch_path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        std::fs::write(&branch_path, "")
            .unwrap_or_else(|e| eprintln!("warn: could not init branch toml: {e}"));
    }

    // ── Print what we did ─────────────────────────────────────────────────
    if switching {
        if let Some(ref prev_branch) = prev {
            println!(
                "{}  Switched branch:  {} → {}{}",
                c.paint(CYAN, "⎇"),
                c.paint(YELLOW, prev_branch),
                c.paint(GREEN, branch),
                RESET,
            );
        } else {
            println!(
                "{}  Active branch set to: {}{}",
                c.paint(CYAN, "⎇"),
                c.paint(GREEN, branch),
                RESET,
            );
        }
    } else if rebuild_env {
        println!(
            "{}  Re-triggering pipeline for branch: {} {}(--rebuild-env){}",
            c.paint(CYAN, "⎇"),
            c.paint(GREEN, branch),
            YELLOW, RESET,
        );
    } else {
        println!(
            "{}  Already on branch: {}  (env/url updated)",
            c.paint(CYAN, "⎇"),
            c.paint(GREEN, branch),
        );
    }

    // ── Trigger pipeline if needed ────────────────────────────────────────
    if should_trigger {
        let vault_path = {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            std::path::PathBuf::from(home)
                .join(".ginger-society")
                .join("ephimeral_env_vault.json")
        };
        let vault_json = std::fs::read_to_string(&vault_path).unwrap_or_else(|e| {
            eprintln!("warn: could not read vault at {}: {e}", vault_path.display());
            "{}".to_string()
        });
        let vault_compact = serde_json::from_str::<serde_json::Value>(&vault_json)
            .map(|v| v.to_string())
            .unwrap_or(vault_json);

        let hosting_fqdn = cfg.active_url.clone().unwrap_or_default();

        match handle_run_pipeline(
            &gitter_config,
            HandleRunPipelineParams {
                run_pipeline_request: RunPipelineRequest {
                    branch: "main".to_string(),
                    pipeline_name: "debug.yml".to_string(),
                    repo: "ginger-society-iac".to_string(),
                    triggered_by: Some("ginger-code".to_string()),
                    params: Some(vec![
                        PipelineParam { key: "HOSTING_FQDN".to_string(), val: hosting_fqdn },
                        PipelineParam { key: "vault".to_string(),        val: vault_compact },
                    ]),
                },
            },
        )
        .await
        {
            Ok(resp) => println!("{:?}", resp),
            Err(e) => {
                eprintln!("{:?}", e);
                eprintln!("❌ Error triggering the pipeline");
            }
        }
    }

    if let Some(e) = &cfg.active_env {
        println!("   env : {}", c.paint(CYAN, e));
    }
    if let Some(u) = &cfg.active_url {
        println!("   url : {}", c.paint(CYAN, u));
    }

    if daemon_running() {
        println!("\n   Daemon detected — it will pick up the change within ~2s.");
        println!("   Reopen the dashboard to see the new branch.");
    } else {
        println!(
            "\n   {}Daemon not running{} — changes will take effect when it starts.",
            YELLOW, RESET
        );
        println!("   Launch it via the ginger-code tray app.");
    }
}

pub fn print_deployments(val: &serde_json::Value) {
    let c = Colour::new();

    if let Some(branch) = val["active_branch"].as_str() {
        println!("{}branch:{} {}", BOLD, RESET, c.paint(GREEN, branch));
    }
    if let Some(url) = val["active_url"].as_str() {
        println!("{}url:   {}{}", BOLD, RESET, c.paint(CYAN, url));
    }
    println!();

    let deps = match val["deployments"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => {
            println!("(no deployments registered for this branch)");
            return;
        }
    };

    let name_w = deps
        .iter()
        .map(|d| d["deployment_name"].as_str().unwrap_or("").len())
        .max()
        .unwrap_or(16)
        .max(16);

    println!(
        "{}{:<name_w$}  {:>9}  {:>8}  {:>8}  {:<13}  {}{}",
        BOLD,
        "DEPLOYMENT", "DEP PORT", "FWD PORT", "RESTARTS", "STATUS", "PID",
        RESET,
        name_w = name_w,
    );
    println!("{}", "─".repeat(name_w + 60));

    for d in deps {
        let name     = d["deployment_name"].as_str().unwrap_or("?");
        let dport    = d["deployment_port"].as_u64().unwrap_or(0);
        let fport    = d["forwarding_port"].as_u64().unwrap_or(0);
        let restarts = d["restarts"].as_u64().unwrap_or(0);
        let pid_str  = d["pid"]
            .as_u64()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        let fst = &d["forward_status"];
        let tag = fst["status"].as_str().unwrap_or("retrying");
        let (icon, label) = match tag {
            "connected" => (c.paint(GREEN, "●"), c.paint(GREEN, "CONNECTED")),
            "offline"   => (c.paint(RED,   "○"), c.paint(RED,   "OFFLINE")),
            _ => {
                let attempt = fst["attempt"].as_u64().unwrap_or(0);
                (
                    c.paint(YELLOW, "↻"),
                    c.paint(YELLOW, &format!("RETRYING (#{attempt})")),
                )
            }
        };

        println!(
            "{:<name_w$}  {:>9}  {:>8}  {:>8}  {} {:<12}  {}",
            name, dport, fport, restarts,
            icon, label, pid_str,
            name_w = name_w,
        );
    }
}

pub fn print_status() {
    let c   = Colour::new();
    let cfg = CodeConfig::load();

    println!("{}ginger-code status{}", BOLD, RESET);
    println!();

    match cfg.active_branch {
        Some(ref b) => println!("  branch : {}", c.paint(GREEN, b)),
        None        => println!("  branch : {}", c.paint(YELLOW, "(none — run `ginger-code -b <branch>`)")),
    }
    match cfg.active_env {
        Some(ref e) => println!("  env    : {}", c.paint(CYAN, e)),
        None        => println!("  env    : -"),
    }
    match cfg.active_url {
        Some(ref u) => println!("  url    : {}", c.paint(CYAN, u)),
        None        => println!("  url    : -"),
    }

    println!();
    if daemon_running() {
        println!("  daemon : {}", c.paint(GREEN, "running"));
    } else {
        println!("  daemon : {}", c.paint(RED, "not running"));
    }
}