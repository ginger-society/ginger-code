use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use tokio::io::AsyncWriteExt as _;

// ── Daemon socket helper ──────────────────────────────────────────────────────

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("ginger-code.sock")
}

fn send_to_daemon(payload: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("Cannot connect to ginger-code daemon at {}: {e}", path.display()))?;
    stream.write_all(format!("{payload}\n").as_bytes())?;
    let mut resp = String::new();
    BufReader::new(stream).read_line(&mut resp)?;
    Ok(serde_json::from_str(&resp)?)
}

// ── Active branch from code.toml ──────────────────────────────────────────────

fn active_branch() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".ginger-society").join("code.toml");
    let raw  = fs::read_to_string(&path).ok()?;
    let val: toml::Value = toml::from_str(&raw).ok()?;
    val.get("active_branch")?.as_str().map(|s| s.to_string())
}

// ── code.toml direct write ────────────────────────────────────────────────────

fn remove_from_branch_config(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Could not locate home directory")?;

    // Read active branch
    let cfg_path  = home.join(".ginger-society").join("code.toml");
    let raw       = fs::read_to_string(&cfg_path).unwrap_or_default();
    let cfg: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    let branch = match cfg.get("active_branch").and_then(|v| v.as_str()) {
        Some(b) => b.to_string(),
        None    => {
            println!("  No active branch in code.toml, nothing to remove from branch config");
            return Ok(());
        }
    };

    // Remove from branches/<slug>.toml
    let slug       = branch.replace('/', "-");
    let branch_path = home.join(".ginger-society").join("branches")
        .join(format!("{}.toml", slug));

    if !branch_path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&branch_path)?;
    let mut bc: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));

    if let Some(deps) = bc.get_mut("deployments").and_then(|d| d.as_array_mut()) {
        deps.retain(|d| {
            d.get("deployment_name")
                .and_then(|v| v.as_str())
                .map_or(true, |n| n != deployment_name)
        });
    }

    fs::write(&branch_path, toml::to_string_pretty(&bc)?)?;
    println!("✓ Removed '{}' from branch config ({})", deployment_name, branch);
    Ok(())
}

// ── Port discovery ────────────────────────────────────────────────────────────

fn find_free_22xx_port() -> Result<u16, Box<dyn std::error::Error>> {
    let used_ports: std::collections::HashSet<u16> =
        match send_to_daemon(r#"{"cmd":"list"}"#) {
            Ok(val) => val["deployments"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|d| d["forwarding_port"].as_u64().map(|p| p as u16))
                .collect(),
            Err(_) => std::collections::HashSet::new(),
        };

    for port in 2200u16..=2299 {
        if used_ports.contains(&port) { continue; }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    Err("No free port available in the 2200–2299 range".into())
}

// ── Repo name from meta_name ──────────────────────────────────────────────────

/// "@ginger-society/dev-portal" → "ginger-society-dev-portal"
fn meta_to_repo_name(meta_name: &str) -> String {
    meta_name.trim_start_matches('@').replace('/', "-")
}

// ── ~/.ssh/config helpers ─────────────────────────────────────────────────────

const CONFIG_BEGIN_MARKER: &str = "# BEGIN ginger-eject:";
const CONFIG_END_MARKER:   &str = "# END ginger-eject:";
const SOURCE_HOST_MARKER:  &str = "# BEGIN ginger-source";
const SOURCE_HOST_END:     &str = "# END ginger-source";

fn add_source_ssh_config() -> Result<(), Box<dyn std::error::Error>> {
    let home     = dirs::home_dir().ok_or("Could not locate home directory")?;
    let cfg_path = home.join(".ssh").join("config");

    let existing = fs::read_to_string(&cfg_path).unwrap_or_default();
    if existing.contains(SOURCE_HOST_MARKER) { return Ok(()); }

    let identity_file = home.join(".ssh").join("id_ed25519");
    let block = format!(
        "\n{begin}\nHost source\n    User git\n    HostName source.gingersociety.org\n    Port 3333\n    IdentityFile {identity}\n    StrictHostKeyChecking no\n    UserKnownHostsFile /dev/null\n{end}\n",
        begin    = SOURCE_HOST_MARKER,
        end      = SOURCE_HOST_END,
        identity = identity_file.display(),
    );

    let mut file = fs::OpenOptions::new().create(true).append(true).open(&cfg_path)?;
    file.write_all(block.as_bytes())?;
    println!("✓ Added 'source' git SSH host to local ~/.ssh/config");
    Ok(())
}

fn add_ssh_config(
    deployment_name: &str,
    forwarding_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let home     = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir  = home.join(".ssh");
    let cfg_path = ssh_dir.join("config");

    if !ssh_dir.exists() { fs::create_dir_all(&ssh_dir)?; }
    if !cfg_path.exists() {
        fs::File::create(&cfg_path)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o600))?;
        }
    }

    let identity_file = home.join(".ssh").join("id_ed25519");
    let cert_file     = home.join(".ssh").join("id_ed25519-cert.pub");
    let host_alias    = format!("{}-local", deployment_name);

    let block = format!(
        "\n{begin} {name}\n\
         Host {alias}\n\
             HostName localhost\n\
             Port {port}\n\
             User dev\n\
             IdentityFile {identity}\n\
             CertificateFile {cert}\n\
             IdentitiesOnly yes\n\
             StrictHostKeyChecking no\n\
             UserKnownHostsFile /dev/null\n\
             ForwardAgent yes\n\
         {end} {name}\n",
        begin    = CONFIG_BEGIN_MARKER,
        end      = CONFIG_END_MARKER,
        name     = deployment_name,
        alias    = host_alias,
        port     = forwarding_port,
        identity = identity_file.display(),
        cert     = cert_file.display(),
    );

    let existing = fs::read_to_string(&cfg_path).unwrap_or_default();
    let marker   = format!("{} {}", CONFIG_BEGIN_MARKER, deployment_name);
    if existing.contains(&marker) {
        println!("  ~/.ssh/config already contains block for '{}', skipping", deployment_name);
        return Ok(());
    }

    let mut file = fs::OpenOptions::new().append(true).open(&cfg_path)?;
    file.write_all(block.as_bytes())?;
    println!("✓ Added SSH config block — connect with: ssh {}", host_alias);
    Ok(())
}

fn remove_ssh_config(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ssh").join("config");

    if !cfg_path.exists() { return Ok(()); }

    let content = fs::read_to_string(&cfg_path)?;
    let begin   = format!("{} {}", CONFIG_BEGIN_MARKER, deployment_name);
    let end     = format!("{} {}", CONFIG_END_MARKER,   deployment_name);

    if !content.contains(&begin) {
        println!("  No SSH config block found for '{}', nothing to remove", deployment_name);
        return Ok(());
    }

    let mut out      = String::with_capacity(content.len());
    let mut skipping = false;

    for line in content.lines() {
        if line.trim_start().starts_with(&begin) { skipping = true; continue; }
        if skipping {
            if line.trim_start().starts_with(&end) { skipping = false; }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    fs::write(&cfg_path, out)?;
    println!("✓ Removed SSH config block for '{}'", deployment_name);
    Ok(())
}

// ── Workspace helpers ─────────────────────────────────────────────────────────

async fn is_workspace_empty(
    pod_name:  &str,
    container: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let out = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            "find /workspace -mindepth 1 -maxdepth 1 | head -1",
        ])
        .output().await?;

    Ok(String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

// ── SSH key helpers ───────────────────────────────────────────────────────────

async fn write_pod_ssh_config(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let setup = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            "mkdir -p /home/dev/.ssh && chmod 700 /home/dev/.ssh && chown dev:dev /home/dev/.ssh",
        ])
        .status().await?;

    if !setup.success() {
        return Err("Failed to create /home/dev/.ssh in pod".into());
    }

    let pod_ssh_config =
        "Host source\n\
             User git\n\
             HostName source.gingersociety.org\n\
             Port 3333\n\
             StrictHostKeyChecking no\n\
             UserKnownHostsFile /dev/null\n";

    let write = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            &format!(
                "printf '%s' '{}' > /home/dev/.ssh/config && \
                 chmod 600 /home/dev/.ssh/config && \
                 chown dev:dev /home/dev/.ssh/config",
                pod_ssh_config
            ),
        ])
        .status().await?;

    if write.success() {
        println!("  ✓ Permanent git SSH config written into pod (/home/dev/.ssh/config)");
    } else {
        eprintln!("  Warning: failed to write SSH config into pod");
    }
    Ok(())
}
async fn copy_ssh_keys_to_dev(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let home    = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir = home.join(".ssh");

    let mkdir = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            "mkdir -p /home/dev/.ssh && chmod 700 /home/dev/.ssh && chown dev:dev /home/dev/.ssh",
        ])
        .status().await?;

    if !mkdir.success() {
        return Err("Failed to create /home/dev/.ssh in pod".into());
    }

    let files: &[(&str, &str, &str)] = &[
        ("id_ed25519",          "/home/dev/.ssh/id_ed25519",          "600"),
        ("id_ed25519.pub",      "/home/dev/.ssh/id_ed25519.pub",      "644"),
        ("id_ed25519-cert.pub", "/home/dev/.ssh/id_ed25519-cert.pub", "644"),
    ];

    for (filename, remote_path, perms) in files {
        let local = ssh_dir.join(filename);
        if !local.exists() {
            eprintln!("  Warning: {} not found, skipping", local.display());
            continue;
        }

        let cp = tokio::process::Command::new("kubectl")
            .args(["cp", local.to_str().unwrap(), &format!("{}:{}", pod_name, remote_path)])
            .status().await?;

        if !cp.success() {
            eprintln!("  Warning: failed to copy {} into pod", filename);
            continue;
        }

        // Fix ownership and permissions
        tokio::process::Command::new("kubectl")
            .args([
                "exec", pod_name, "-c", container,
                "--", "sh", "-c",
                &format!("chmod {perms} {remote_path} && chown dev:dev {remote_path}"),
            ])
            .status().await?;

        println!("  ✓ Copied {} → pod:{}", filename, remote_path);
    }
    Ok(())
}

async fn delete_dev_ssh_keys(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Only remove the key files, not the entire .ssh dir —
    // /home/dev/.ssh/config was written by write_pod_ssh_config and should stay
    let rm = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            "rm -f /home/dev/.ssh/id_ed25519 \
                    /home/dev/.ssh/id_ed25519.pub \
                    /home/dev/.ssh/id_ed25519-cert.pub",
        ])
        .status().await?;

    if rm.success() {
        println!("✓ Temporary SSH keys removed from pod (/home/dev/.ssh keys wiped)");
    } else {
        eprintln!(
            "Warning: could not remove SSH keys from pod — remove manually:\n  \
             kubectl exec {} -c {} -- rm -f /home/dev/.ssh/id_ed25519 \
             /home/dev/.ssh/id_ed25519.pub /home/dev/.ssh/id_ed25519-cert.pub",
            pod_name, container
        );
    }
    Ok(())
}


async fn find_git_in_pod(
    pod_name:  &str,
    container: &str,
) -> String {
    // Try common locations used by nix, asdf, brew, or plain distros
    let probe = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c",
            "command -v git 2>/dev/null || \
             find /usr/local/bin /usr/bin /nix/var/nix/profiles/default/bin \
                  /home/dev/.nix-profile/bin /root/.nix-profile/bin \
                  -name git -type f 2>/dev/null | head -1",
        ])
        .output().await;

    match probe {
        Ok(out) => {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() { "git".to_string() } else { path }
        }
        Err(_) => "git".to_string(),
    }
}

// ── Clone + branch checkout ───────────────────────────────────────────────────
//
// Three cases:
//   1. /workspace/<repo> does not exist  → clone main, then checkout branch
//   2. /workspace/<repo> exists, branch not checked out → checkout branch
//   3. /workspace/<repo> exists, branch already checked out → nothing to do
async fn setup_repo_branch(
    pod_name:  &str,
    container: &str,
    repo_name: &str,
    branch:    &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let git = find_git_in_pod(pod_name, container).await;
    println!("  Using git binary: {}", git);

    let workspace_repo = format!("/workspace/{}", repo_name);

    // ── Write a script into the pod and execute it ────────────────────────────
    // This avoids all stdin/tty/su plumbing issues with kubectl exec.
    // The script runs as dev user via the entrypoint, with /home/dev/.ssh/config
    // already set up by write_pod_ssh_config.

    let script = format!(
        r#"#!/bin/sh
set -e
export HOME=/home/dev
export GIT="{git}"

echo "[clone] checking SSH access..."
ssh source 2>&1 || true

if [ ! -d "{ws}/{repo}" ]; then
    echo "[clone] cloning {repo}..."
    "$GIT" clone -b main source:{repo}.git {ws}/{repo} \
        || "$GIT" clone -b master source:{repo}.git {ws}/{repo}
    chown -R dev:dev {ws}/{repo}
    echo "[clone] done"
    DEFAULT_BRANCH=$("$GIT" -C {ws}/{repo} rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
    echo "[clone] on branch: $DEFAULT_BRANCH"
else
    echo "[clone] repo already exists"
fi

echo "[branch] setting up branch {branch}..."
"$GIT" -C {ws}/{repo} fetch origin {branch} 2>/dev/null || true

if "$GIT" -C {ws}/{repo} show-ref --verify --quiet refs/remotes/origin/{branch}; then
    "$GIT" -C {ws}/{repo} checkout -B {branch} origin/{branch}
    echo "checked-out-remote"
elif "$GIT" -C {ws}/{repo} show-ref --verify --quiet refs/heads/{branch}; then
    "$GIT" -C {ws}/{repo} checkout {branch}
    echo "checked-out-local"
else
    echo "[branch] {branch} not on remote — creating fresh"
    "$GIT" -C {ws}/{repo} checkout -b {branch}
    echo "created-new"
fi
"#,
        git    = git,
        ws     = "/workspace",
        repo   = repo_name,
        branch = branch,
    );

    // Write script into pod
    let write_cmd = format!(
        "cat > /tmp/ginger_setup.sh << 'GINGER_EOF'\n{}\nGINGER_EOF\nchmod +x /tmp/ginger_setup.sh",
        script
    );

    let write_out = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "sh", "-c", &write_cmd,
        ])
        .output().await?;

    if !write_out.status.success() {
        return Err(format!(
            "Failed to write setup script into pod: {}",
            String::from_utf8_lossy(&write_out.stderr).trim()
        ).into());
    }

    println!("  ✓ Setup script written to pod, executing...");

    // Execute as dev user
    let exec_out = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("kubectl")
            .args([
                "exec", pod_name, "-c", container,
                "--", "su", "dev", "-s", "/bin/sh", "/tmp/ginger_setup.sh",
            ])
            .output(),
    ).await
    .map_err(|_| "Setup script timed out after 120s")?
    .map_err(|e| format!("Failed to execute setup script: {e}"))?;

    let stdout = String::from_utf8_lossy(&exec_out.stdout);
    let stderr = String::from_utf8_lossy(&exec_out.stderr);

    // Always print output for visibility
    for line in stdout.lines() { println!("  {}", line); }
    if !stderr.trim().is_empty() {
        for line in stderr.lines() { eprintln!("  [stderr] {}", line); }
    }

    if !exec_out.status.success() {
        return Err(format!(
            "Setup script failed (exit {:?})", exec_out.status.code()
        ).into());
    }

    if stdout.contains("checked-out-remote") {
        println!("✓ Checked out existing remote branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("checked-out-local") {
        println!("✓ Checked out existing local branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("created-new") {
        println!(
            "✓ Created new branch '{}' in {} (push with: git push -u origin {})",
            branch, workspace_repo, branch
        );
    }

    Ok(())
}

async fn checkout_or_create_branch(
    pod_name:       &str,
    container:      &str,
    workspace_repo: &str,
    branch:         &str,
    git:            &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let fetch_cmd = format!(
        "{git} -C {repo} fetch origin {branch} 2>/dev/null || true; \
         if {git} -C {repo} show-ref --verify --quiet refs/remotes/origin/{branch}; then \
             {git} -C {repo} checkout -B {branch} origin/{branch} && \
             echo 'checked-out-remote'; \
         elif {git} -C {repo} show-ref --verify --quiet refs/heads/{branch}; then \
             {git} -C {repo} checkout {branch} && \
             echo 'checked-out-local'; \
         else \
             echo 'Branch {branch} does not exist on remote or locally — creating fresh off current HEAD'; \
             {git} -C {repo} checkout -b {branch} && \
             echo 'created-new'; \
         fi",
        git    = git,
        repo   = workspace_repo,
        branch = branch,
    );

    let out = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "-c", container,
            "--", "su", "-", "dev", "-s", "/bin/sh", "-c",
            &fetch_cmd,
        ])
        .output().await?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    if stdout.contains("checked-out-remote") {
        println!("✓ Checked out existing remote branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("checked-out-local") {
        println!("✓ Checked out existing local branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("created-new") {
        println!(
            "✓ Created new branch '{}' in {} (branch did not exist on remote — \
             push with: git push -u origin {})",
            branch, workspace_repo, branch
        );
    } else {
        return Err(format!(
            "Branch checkout failed for '{}' in {}\n  stdout: {}\n  stderr: {}",
            branch, workspace_repo, stdout.trim(), stderr.trim()
        ).into());
    }

    Ok(())
}

// ── Builder image map ─────────────────────────────────────────────────────────

fn builder_image(lang: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    match lang {
        "TS"   => Ok("gingersociety/dev-container-node:5"),
        "Rust" => Ok("gingersociety/dev-container-rust:7"),
        other  => Err(format!("builder image not yet defined for lang: {}", other).into()),
    }
}

fn supports_ssh(lang: &str) -> bool {
    matches!(lang, "TS" | "Rust")
}

fn assert_daemon_reachable() -> Result<(), Box<dyn std::error::Error>> {
    match send_to_daemon(r#"{"cmd":"ping"}"#) {
        Ok(val) if val["status"] == "ok" => Ok(()),
        Ok(val) => Err(format!(
            "Daemon responded unexpectedly to ping: {}\n\
             Make sure ginger-code is running.",
            val
        ).into()),
        Err(e) => Err(format!(
            "Cannot reach ginger-code daemon: {e}\n\
             Start it with: ginger-code\n\
             Or check that it is running in the system tray."
        ).into()),
    }
}

// ── Eject ─────────────────────────────────────────────────────────────────────

pub async fn eject(
    deployment_name: &str,
    lang:            &str,
    meta_name:       &str,
) -> Result<(), Box<dyn std::error::Error>> {

    assert_daemon_reachable()?;

    // ── Read active branch ────────────────────────────────────────────────────
    let branch = active_branch().ok_or(
        "No active branch set — run `ginger-code -b <branch>` first"
    )?;
    println!("  Active branch: {}", branch);

    // ── Read session user ─────────────────────────────────────────────────────
    let user_file = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ginger-society").join("user.json");

    let user_json    = fs::read_to_string(&user_file)
        .map_err(|e| format!("Could not read {}: {}", user_file.display(), e))?;
    let user_details: serde_json::Value = serde_json::from_str(&user_json)
        .map_err(|e| format!("Could not parse user.json: {}", e))?;
    let session_user = user_details["sub"]
        .as_str()
        .ok_or("sub missing or not a string in user.json")?
        .to_string();

    let image     = builder_image(lang)?;
    let ssh       = supports_ssh(lang);
    let repo_name = meta_to_repo_name(meta_name).to_lowercase();

    // ── Read current image ────────────────────────────────────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.spec.template.spec.containers[0].image}",
        ])
        .output().await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
    if original_image.is_empty() {
        return Err("Could not read current image from deployment".into());
    }

    // ── Create workspace PVC ──────────────────────────────────────────────────
    let pvc_name            = format!("{}-eject-pvc", deployment_name);
    let principals_pvc_name = format!("{}-ssh-principals-pvc", deployment_name);

    let pvc_yaml = format!(
        "apiVersion: v1\nkind: PersistentVolumeClaim\nmetadata:\n  name: {pvc_name}\nspec:\n  accessModes: [ReadWriteOnce]\n  resources:\n    requests:\n      storage: 4Gi\n"
    );
    let mut apply = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = apply.stdin.take() { stdin.write_all(pvc_yaml.as_bytes()).await?; }
    apply.wait().await?;

    let principals_pvc_yaml = format!(
        "apiVersion: v1\nkind: PersistentVolumeClaim\nmetadata:\n  name: {principals_pvc_name}\nspec:\n  accessModes: [ReadWriteOnce]\n  resources:\n    requests:\n      storage: 16Mi\n"
    );
    let mut apply2 = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = apply2.stdin.take() { stdin.write_all(principals_pvc_yaml.as_bytes()).await?; }
    apply2.wait().await?;
    println!("✓ SSH principals PVC ready: {}", principals_pvc_name);

    // ── Patch deployment ──────────────────────────────────────────────────────
    let command = if ssh { serde_json::json!(["/entrypoint.sh"]) }
                  else   { serde_json::json!(["sleep", "infinity"]) };

    let mut container = serde_json::json!({
        "name":    deployment_name,
        "image":   image,
        "command": command,
        "volumeMounts": [
            { "name": "workspace",      "mountPath": "/workspace" },
            { "name": "ssh-principals", "mountPath": "/etc/ssh/auth_principals" },
        ]
    });
    if ssh { container["ports"] = serde_json::json!([{ "containerPort": 22 }]); }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        "true",
                "ginger-original-image": original_image,
                "ginger-branch":         branch,
            }
        },
        "spec": {
            "template": {
                "metadata": { "annotations": { "kubectl.kubernetes.io/restartedAt": null } },
                "spec": {
                    "containers": [container],
                    "volumes": [
                        { "name": "workspace",      "persistentVolumeClaim": { "claimName": pvc_name } },
                        { "name": "ssh-principals", "persistentVolumeClaim": { "claimName": principals_pvc_name } },
                    ]
                }
            }
        }
    });

    let status = tokio::process::Command::new("kubectl")
        .args(["patch", "deployment", deployment_name, "--type", "strategic", "-p", &patch.to_string()])
        .status().await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }
    println!("✓ Patched {} → {} (branch: {})", deployment_name, image, branch);

    if ssh {
        // ── Wait for pod ──────────────────────────────────────────────────────
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("\n⏸  Sleeping 5s to allow rollout to stabilize...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let final_pod = wait_for_pod_scheduled(deployment_name).await?;

        tokio::process::Command::new("kubectl")
            .args(["wait", &format!("pod/{}", final_pod), "--for=condition=Ready", "--timeout=300s"])
            .status().await?;

        println!("✓ Pod ready: {}", final_pod);

        // ── Write SSH principal ───────────────────────────────────────────────
        let principal_cmd = format!(
            "mkdir -p /etc/ssh/auth_principals && \
             chown root:root /etc/ssh/auth_principals && \
             chmod 755 /etc/ssh/auth_principals && \
             echo '{session_user}' > /etc/ssh/auth_principals/dev && \
             chown root:root /etc/ssh/auth_principals/dev && \
             chmod 644 /etc/ssh/auth_principals/dev",
        );

        let exec_status = tokio::process::Command::new("kubectl")
            .args(["exec", &final_pod, "-c", deployment_name, "--", "sh", "-c", &principal_cmd])
            .status().await?;

        if !exec_status.success() {
            return Err(format!(
                "Failed to write SSH principal '{}' into pod {}",
                session_user, final_pod
            ).into());
        }
        println!("✓ SSH principal '{}' written", session_user);

        // ── Write pod SSH config ──────────────────────────────────────────────
        println!("⏳ Writing pod SSH config for git push...");
        if let Err(e) = write_pod_ssh_config(&final_pod, deployment_name).await {
            eprintln!("Warning: {e}");
        }

        // ── Clone / checkout branch ───────────────────────────────────────────
        println!("⏳ Checking workspace and branch...");

        let workspace_empty = is_workspace_empty(&final_pod, deployment_name).await
            .unwrap_or(true);

        if workspace_empty {
            // Copy keys for the initial clone, then clean them up
            println!("⏳ Copying SSH keys into pod for initial clone...");
            match copy_ssh_keys_to_dev(&final_pod, deployment_name).await {
                Err(e) => eprintln!("Warning: could not copy SSH keys into pod: {e}"),
                Ok(()) => {
                    setup_repo_branch(&final_pod, deployment_name, &repo_name, &branch).await?;
                    // was: delete_root_ssh
                    if let Err(e) = delete_dev_ssh_keys(&final_pod, deployment_name).await {
                        eprintln!("Warning: {e}");
                    }
                }
            }
        } else {
            // Workspace has content — repo exists, just ensure correct branch
            // Keys not needed for checkout (no clone required)
            println!("  /workspace is not empty — checking branch...");
            setup_repo_branch(&final_pod, deployment_name, &repo_name, &branch).await?;
        }

        // ── Local SSH config ──────────────────────────────────────────────────
        if let Err(e) = add_source_ssh_config() {
            eprintln!("Warning: could not add source host to local ~/.ssh/config: {e}");
        }

        let forwarding_port = find_free_22xx_port()?;

        let register_payload = serde_json::json!({
            "cmd":             "register",
            "deployment_name": deployment_name,
            "deployment_port": 22,
            "forwarding_port": forwarding_port,
        });

        match send_to_daemon(&register_payload.to_string()) {
            Ok(resp) if resp["status"] == "ok" => {
                println!("✓ Registered '{}' with daemon — port-forward on localhost:{}", deployment_name, forwarding_port);
            }
            Ok(resp) => eprintln!("Warning: daemon responded unexpectedly: {}", resp),
            Err(e) => {
                eprintln!(
                    "Warning: could not register with daemon: {e}\n\
                     Register manually:\n  \
                     ginger-code register --deployment-name {} --deployment-port 22 --forwarding-port {}",
                    deployment_name, forwarding_port
                );
            }
        }

        if let Err(e) = add_ssh_config(deployment_name, forwarding_port) {
            eprintln!("Warning: could not update local ~/.ssh/config: {e}");
        }

        println!("\nConnect with:  ssh {}-local", deployment_name);
        println!("Then push with: git push  (agent forwarding carries your key)");
    }

    println!("\n⏸  Sleeping 2s...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    Ok(())
}

// ── Uneject ───────────────────────────────────────────────────────────────────

pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    assert_daemon_reachable()?;

    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.metadata.annotations.ginger-original-image}",
        ])
        .output().await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
    if original_image.is_empty() {
        return Err("No ginger-original-image annotation — was this deployment ejected?".into());
    }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        null,
                "ginger-original-image": null,
                "ginger-branch":         null,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name":         deployment_name,
                        "image":        original_image,
                        "command":      null,
                        "volumeMounts": []
                    }],
                    "volumes": []
                }
            }
        }
    });

    let status = tokio::process::Command::new("kubectl")
        .args(["patch", "deployment", deployment_name, "--type", "strategic", "-p", &patch.to_string()])
        .status().await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }
    println!("✓ Unejected {} → restored {}", deployment_name, original_image);

    // Remove from branch config directly (works even if daemon is down)
    if let Err(e) = remove_from_branch_config(deployment_name) {
        eprintln!("Warning: could not update branch config: {e}");
    }

    // Notify daemon to stop the forward immediately
    let remove_payload = serde_json::json!({
        "cmd":             "remove",
        "deployment_name": deployment_name,
    });

    match send_to_daemon(&remove_payload.to_string()) {
        Ok(resp) if resp["status"] == "ok" => {
            println!("✓ Notified daemon — port-forward stopped immediately");
        }
        Ok(resp) => eprintln!("Warning: daemon responded unexpectedly: {}", resp),
        Err(_)   => println!("  (daemon unreachable — forward will stop on next watcher tick)"),
    }

    if let Err(e) = remove_ssh_config(deployment_name) {
        eprintln!("Warning: could not clean up local ~/.ssh/config: {e}");
    }

    Ok(())
}

// ── Pod scheduling wait ───────────────────────────────────────────────────────

async fn wait_for_pod_scheduled(
    deployment_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let label = format!("app={}", deployment_name);
    for attempt in 1..=40 {
        let out = tokio::process::Command::new("kubectl")
            .args([
                "get", "pods", "-l", &label,
                "--field-selector=status.phase!=Failed",
                "--no-headers",
                "-o", "custom-columns=NAME:.metadata.name,DELETED:.metadata.deletionTimestamp",
            ])
            .output().await?;

        if let Some(name) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty() && l.contains("<none>"))
            .filter_map(|l| l.split_whitespace().next().map(|s| s.trim().to_string()))
            .next()
        {
            return Ok(name);
        }

        println!("  … pod not scheduled yet (attempt {}), retrying in 3s", attempt);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod for '{}'", deployment_name).into())
}