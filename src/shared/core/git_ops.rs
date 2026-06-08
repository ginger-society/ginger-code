// ── Git binary discovery ──────────────────────────────────────────────────────

pub async fn find_git_in_pod(pod_name: &str, container: &str) -> String {
    let probe = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            "command -v git 2>/dev/null || \
             find /usr/local/bin /usr/bin /nix/var/nix/profiles/default/bin \
                  /home/dev/.nix-profile/bin /root/.nix-profile/bin \
                  -name git -type f 2>/dev/null | head -1",
        ])
        .output()
        .await;

    match probe {
        Ok(out) => {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() { "git".to_string() } else { path }
        }
        Err(_) => "git".to_string(),
    }
}

// ── SSH config inside the pod (for git push) ──────────────────────────────────

/// Write a permanent `/home/dev/.ssh/config` that points `source` at
/// `source.gingersociety.org:3333`.  Idempotent — safe to call on every eject/mount.
pub async fn write_pod_ssh_config(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let setup = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            "mkdir -p /home/dev/.ssh && chmod 700 /home/dev/.ssh && chown dev:dev /home/dev/.ssh",
        ])
        .status()
        .await?;

    if !setup.success() {
        return Err("Failed to create /home/dev/.ssh in pod".into());
    }

    let pod_ssh_config = "Host source\n\
         User git\n\
         HostName source.gingersociety.org\n\
         Port 3333\n\
         StrictHostKeyChecking no\n\
         UserKnownHostsFile /dev/null\n";

    let write = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            &format!(
                "printf '%s' '{}' > /home/dev/.ssh/config && \
                 chmod 600 /home/dev/.ssh/config && \
                 chown dev:dev /home/dev/.ssh/config",
                pod_ssh_config
            ),
        ])
        .status()
        .await?;

    if write.success() {
        println!("  ✓ Permanent git SSH config written into pod (/home/dev/.ssh/config)");
    } else {
        eprintln!("  Warning: failed to write SSH config into pod");
    }
    Ok(())
}

// ── Temporary key copy / cleanup ──────────────────────────────────────────────

/// Copy `~/.ssh/id_ed25519{,.pub,-cert.pub}` into the pod for the initial clone.
pub async fn copy_ssh_keys_to_dev(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let home    = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir = home.join(".ssh");

    let mkdir = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            "mkdir -p /home/dev/.ssh && chmod 700 /home/dev/.ssh && chown dev:dev /home/dev/.ssh",
        ])
        .status()
        .await?;

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
            .args([
                "cp",
                local.to_str().unwrap(),
                &format!("{}:{}", pod_name, remote_path),
                "-c",
                container,
            ])
            .status()
            .await?;

        if !cp.success() {
            eprintln!("  Warning: failed to copy {} into pod", filename);
            continue;
        }

        tokio::process::Command::new("kubectl")
            .args([
                "exec",
                pod_name,
                "-c",
                container,
                "--",
                "sh",
                "-c",
                &format!("chmod {perms} {remote_path} && chown dev:dev {remote_path}"),
            ])
            .status()
            .await?;

        println!("  ✓ Copied {} → pod:{}", filename, remote_path);
    }
    Ok(())
}

/// Remove the temporary key files copied by [`copy_ssh_keys_to_dev`].
/// Leaves `/home/dev/.ssh/config` intact.
pub async fn delete_dev_ssh_keys(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let rm = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            "rm -f /home/dev/.ssh/id_ed25519 \
                    /home/dev/.ssh/id_ed25519.pub \
                    /home/dev/.ssh/id_ed25519-cert.pub",
        ])
        .status()
        .await?;

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

// ── Clone + branch checkout ───────────────────────────────────────────────────

/// Ensure `/workspace/<dir_name>` exists on `branch`, cloning from
/// `source:<git_repo>.git` if needed.
///
/// Two names are required because they differ:
/// * `git_repo` — the gitolite remote name, org-prefixed
///               e.g. `"ginger-society-ginger-db"`
/// * `dir_name` — the local workspace directory (slug only, no org prefix)
///               e.g. `"ginger-db"`
///
/// Three cases handled transparently:
/// 1. `/workspace/<dir_name>` absent  → clone into it, then checkout branch.
/// 2. Directory present, branch missing → checkout or create branch.
/// 3. Directory present, branch already active → no-op.
pub async fn setup_repo_branch(
    pod_name:  &str,
    container: &str,
    git_repo:  &str,   // gitolite remote:  "ginger-society-ginger-db"
    dir_name:  &str,   // local directory:  "ginger-db"
    branch:    &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let git = find_git_in_pod(pod_name, container).await;
    println!("  Using git binary: {}", git);
    println!("  Remote repo: source:{}.git", git_repo);
    println!("  Local dir:   /workspace/{}", dir_name);

    let workspace_repo = format!("/workspace/{}", dir_name);

    let script = format!(
        r#"#!/bin/sh
set -e
export HOME=/home/dev
export GIT="{git}"

echo "[clone] checking SSH access..."
ssh source 2>&1 || true

if [ ! -d "{ws}/{dir}" ]; then
    echo "[clone] cloning {git_repo} into {dir}..."
    "$GIT" clone -b main source:{git_repo}.git {ws}/{dir} \
        || "$GIT" clone -b master source:{git_repo}.git {ws}/{dir}
    chown -R dev:dev {ws}/{dir}
    echo "[clone] done"
    DEFAULT_BRANCH=$("$GIT" -C {ws}/{dir} rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
    echo "[clone] on branch: $DEFAULT_BRANCH"
else
    echo "[clone] repo already exists at {ws}/{dir}"
fi

echo "[branch] setting up branch {branch}..."
"$GIT" -C {ws}/{dir} fetch origin {branch} 2>/dev/null || true

if "$GIT" -C {ws}/{dir} show-ref --verify --quiet refs/remotes/origin/{branch}; then
    "$GIT" -C {ws}/{dir} checkout -B {branch} origin/{branch}
    echo "checked-out-remote"
elif "$GIT" -C {ws}/{dir} show-ref --verify --quiet refs/heads/{branch}; then
    "$GIT" -C {ws}/{dir} checkout {branch}
    echo "checked-out-local"
else
    echo "[branch] {branch} not on remote — creating fresh"
    "$GIT" -C {ws}/{dir} checkout -b {branch}
    echo "created-new"
fi
"#,
        git      = git,
        ws       = "/workspace",
        git_repo = git_repo,
        dir      = dir_name,
        branch   = branch,
    );

    // Write the script into the pod
    let write_cmd = format!(
        "cat > /tmp/ginger_setup.sh << 'GINGER_EOF'\n{}\nGINGER_EOF\nchmod +x /tmp/ginger_setup.sh",
        script
    );

    let write_out = tokio::process::Command::new("kubectl")
        .args([
            "exec",
            pod_name,
            "-c",
            container,
            "--",
            "sh",
            "-c",
            &write_cmd,
        ])
        .output()
        .await?;

    if !write_out.status.success() {
        return Err(format!(
            "Failed to write setup script into pod: {}",
            String::from_utf8_lossy(&write_out.stderr).trim()
        )
        .into());
    }

    println!("  ✓ Setup script written to pod, executing...");

    let exec_out = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("kubectl")
            .args([
                "exec",
                pod_name,
                "-c",
                container,
                "--",
                "su",
                "dev",
                "-s",
                "/bin/sh",
                "/tmp/ginger_setup.sh",
            ])
            .output(),
    )
    .await
    .map_err(|_| "Setup script timed out after 120s")?
    .map_err(|e| format!("Failed to execute setup script: {e}"))?;

    let stdout = String::from_utf8_lossy(&exec_out.stdout);
    let stderr = String::from_utf8_lossy(&exec_out.stderr);

    for line in stdout.lines() {
        println!("  {}", line);
    }
    if !stderr.trim().is_empty() {
        for line in stderr.lines() {
            eprintln!("  [stderr] {}", line);
        }
    }

    if !exec_out.status.success() {
        return Err(format!(
            "Setup script failed (exit {:?})",
            exec_out.status.code()
        )
        .into());
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