use crate::shared::core::k8s_exec::{exec_in_pod, sh_exec, sh_output};

// ── Git binary discovery ──────────────────────────────────────────────────────

pub async fn find_git_in_pod(pod_name: &str, container: &str) -> String {
    let cmd = "command -v git 2>/dev/null || \
               find /usr/local/bin /usr/bin /nix/var/nix/profiles/default/bin \
                    /home/dev/.nix-profile/bin /root/.nix-profile/bin \
                    -name git -type f 2>/dev/null | head -1";

    sh_output(pod_name, container, cmd)
        .await
        .unwrap_or_else(|| "git".to_string())
}

// ── SSH config inside the pod ─────────────────────────────────────────────────

pub async fn write_pod_ssh_config(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let setup_cmd = "mkdir -p /home/dev/.ssh && \
                     chmod 700 /home/dev/.ssh && \
                     chown dev:dev /home/dev/.ssh";

    let ok = sh_exec(pod_name, container, setup_cmd)
        .await
        .map_err(|e| format!("Failed to create /home/dev/.ssh in pod: {e}"))?;

    if !ok {
        return Err("Failed to create /home/dev/.ssh in pod".into());
    }

    let pod_ssh_config = "Host source\n\
         User git\n\
         HostName source.gingersociety.org\n\
         Port 3333\n\
         StrictHostKeyChecking no\n\
         UserKnownHostsFile /dev/null\n";

    // Use printf to write the config — avoids heredoc quoting issues via exec.
    let write_cmd = format!(
        "printf '%s' '{}' > /home/dev/.ssh/config && \
         chmod 600 /home/dev/.ssh/config && \
         chown dev:dev /home/dev/.ssh/config",
        pod_ssh_config
    );

    let ok = sh_exec(pod_name, container, &write_cmd)
        .await
        .map_err(|e| format!("Failed to write SSH config into pod: {e}"))?;

    if ok {
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
    use k8s_openapi::api::core::v1::Pod;
    use kube::{Api, Client};
    use kube::api::AttachParams;
    use tokio::io::AsyncWriteExt;

    let client = Client::try_default().await.expect("kube client");
    let api: Api<Pod> = Api::default_namespaced(client);

    let home    = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir = home.join(".ssh");

    // Ensure target dir exists first
    let ok = sh_exec(
        pod_name, container,
        "mkdir -p /home/dev/.ssh && chmod 700 /home/dev/.ssh && chown dev:dev /home/dev/.ssh",
    )
    .await
    .map_err(|e| format!("Failed to create /home/dev/.ssh: {e}"))?;

    if !ok {
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

        let contents = std::fs::read(&local)?;

        // Stream file bytes into the pod via stdin of `tee`
        let ap = AttachParams {
            container: Some(container.to_string()),
            stdin:     true,
            stdout:    false,
            stderr:    false,
            tty:       false,
            ..Default::default()
        };

        let write_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("cat > {remote_path}"),
        ];

        let mut attached = api.exec(pod_name, write_cmd, &ap).await
            .map_err(|e| format!("exec failed for {filename}: {e}"))?;

        if let Some(mut stdin) = attached.stdin() {
            stdin.write_all(&contents).await?;
            // Drop stdin to signal EOF to the cat process
        }

        // Wait for completion
        if let Some(status) = attached.take_status() {
            status.await;
        }

        // Fix permissions and ownership
        sh_exec(
            pod_name, container,
            &format!("chmod {perms} {remote_path} && chown dev:dev {remote_path}"),
        )
        .await
        .ok();

        println!("  ✓ Copied {} → pod:{}", filename, remote_path);
    }

    Ok(())
}

/// Remove the temporary key files copied by [`copy_ssh_keys_to_dev`].
pub async fn delete_dev_ssh_keys(
    pod_name:  &str,
    container: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ok = sh_exec(
        pod_name, container,
        "rm -f /home/dev/.ssh/id_ed25519 \
                /home/dev/.ssh/id_ed25519.pub \
                /home/dev/.ssh/id_ed25519-cert.pub",
    )
    .await
    .map_err(|e| format!("Failed to remove SSH keys: {e}"))?;

    if ok {
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

pub async fn setup_repo_branch(
    pod_name:  &str,
    container: &str,
    git_repo:  &str,
    dir_name:  &str,
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

    // Write script into pod via stdin
    use k8s_openapi::api::core::v1::Pod;
    use kube::{Api, Client};
    use kube::api::AttachParams;
    use tokio::io::AsyncWriteExt;

    let client = Client::try_default().await.expect("kube client");
    let api: Api<Pod> = Api::default_namespaced(client);

    let write_ap = AttachParams {
        container: Some(container.to_string()),
        stdin:     true,
        stdout:    false,
        stderr:    false,
        tty:       false,
        ..Default::default()
    };

    let mut write_attached = api
        .exec(
            pod_name,
            vec!["sh", "-c", "cat > /tmp/ginger_setup.sh && chmod +x /tmp/ginger_setup.sh"],
            &write_ap,
        )
        .await
        .map_err(|e| format!("Failed to write setup script: {e}"))?;

    if let Some(mut stdin) = write_attached.stdin() {
        stdin.write_all(script.as_bytes()).await?;
    }
    if let Some(status) = write_attached.take_status() {
        status.await;
    }

    println!("  ✓ Setup script written to pod, executing...");

    // Execute the script as dev user
    let exec_ap = AttachParams {
        container: Some(container.to_string()),
        stdin:     false,
        stdout:    true,
        stderr:    true,
        tty:       false,
        ..Default::default()
    };

    let exec_fut = api.exec(
        pod_name,
        vec!["su", "dev", "-s", "/bin/sh", "/tmp/ginger_setup.sh"],
        &exec_ap,
    );

    let mut exec_attached = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        exec_fut,
    )
    .await
    .map_err(|_| "Setup script timed out after 120s")?
    .map_err(|e| format!("Failed to execute setup script: {e}"))?;

    use tokio::io::AsyncReadExt;

    let stdout = match exec_attached.stdout() {
        Some(mut r) => { let mut b = Vec::new(); r.read_to_end(&mut b).await?; String::from_utf8_lossy(&b).into_owned() }
        None => String::new(),
    };
    let stderr = match exec_attached.stderr() {
        Some(mut r) => { let mut b = Vec::new(); r.read_to_end(&mut b).await?; String::from_utf8_lossy(&b).into_owned() }
        None => String::new(),
    };

    let success = match exec_attached.take_status() {
        Some(s) => s.await.and_then(|s| s.status).map(|s| s == "Success").unwrap_or(false),
        None    => true,
    };

    for line in stdout.lines() { println!("  {}", line); }
    if !stderr.trim().is_empty() {
        for line in stderr.lines() { eprintln!("  [stderr] {}", line); }
    }

    if !success {
        return Err(format!("Setup script failed").into());
    }

    if stdout.contains("checked-out-remote") {
        println!("✓ Checked out existing remote branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("checked-out-local") {
        println!("✓ Checked out existing local branch '{}' in {}", branch, workspace_repo);
    } else if stdout.contains("created-new") {
        println!("✓ Created new branch '{}' in {} (push with: git push -u origin {})", branch, workspace_repo, branch);
    }

    Ok(())
}