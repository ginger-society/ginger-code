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
        if used_ports.contains(&port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    Err("No free port available in the 2200–2299 range".into())
}

// ── Repo name from meta_name ──────────────────────────────────────────────────

/// "@ginger-society/dev-portal" → "ginger-society-dev-portal"
fn meta_to_repo_name(meta_name: &str) -> String {
    meta_name
        .trim_start_matches('@')
        .replace('/', "-")
}

// ── ~/.ssh/config helpers ─────────────────────────────────────────────────────

const CONFIG_BEGIN_MARKER: &str = "# BEGIN ginger-eject:";
const CONFIG_END_MARKER:   &str = "# END ginger-eject:";
const SOURCE_HOST_MARKER:  &str = "# BEGIN ginger-source";
const SOURCE_HOST_END:     &str = "# END ginger-source";

/// Append the `source` git Host block to local ~/.ssh/config (idempotent).
fn add_source_ssh_config() -> Result<(), Box<dyn std::error::Error>> {
    let home     = dirs::home_dir().ok_or("Could not locate home directory")?;
    let cfg_path = home.join(".ssh").join("config");

    let existing = fs::read_to_string(&cfg_path).unwrap_or_default();
    if existing.contains(SOURCE_HOST_MARKER) {
        return Ok(());
    }

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

/// Append a Host block for `deployment_name` to ~/.ssh/config.
/// ForwardAgent yes so git push inside the pod uses the MacBook's ssh-agent.
fn add_ssh_config(
    deployment_name: &str,
    forwarding_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let home     = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir  = home.join(".ssh");
    let cfg_path = ssh_dir.join("config");

    if !ssh_dir.exists() {
        fs::create_dir_all(&ssh_dir)?;
    }
    if !cfg_path.exists() {
        fs::File::create(&cfg_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o600))?;
        }
    }

    let identity_file = home.join(".ssh").join("id_ed25519");
    let cert_file     = home.join(".ssh").join("id_ed25519-cert.pub");
    let host_alias    = format!("{}-local", deployment_name);

    // ForwardAgent yes: the MacBook ssh-agent is forwarded into the pod so
    // `git push source:repo.git` works without any private key in the pod.
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

/// Remove the Host block for `deployment_name` from ~/.ssh/config.
fn remove_ssh_config(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ssh")
        .join("config");

    if !cfg_path.exists() {
        return Ok(());
    }

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
        if line.trim_start().starts_with(&begin) {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim_start().starts_with(&end) {
                skipping = false;
            }
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

async fn is_workspace_empty(pod_name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let out = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "--", "sh", "-c",
               "find /workspace -mindepth 1 -maxdepth 1 | head -1"])
        .output()
        .await?;

    Ok(String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

// ── SSH key helpers ───────────────────────────────────────────────────────────

/// Write a permanent /home/dev/.ssh/config into the pod for `git push`.
/// No private key is placed here — the MacBook ssh-agent is forwarded in
/// via ForwardAgent yes on the local ~/.ssh/config Host block.
async fn write_pod_ssh_config(pod_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Create /home/dev/.ssh owned by dev (kubectl exec runs as root)
    let setup_status = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "--", "sh", "-c",
               "mkdir -p /home/dev/.ssh && \
                chmod 700 /home/dev/.ssh && \
                chown dev:dev /home/dev/.ssh"])
        .status()
        .await?;

    if !setup_status.success() {
        return Err("Failed to create /home/dev/.ssh in pod".into());
    }

    // SSH alias `source` resolves via this config — no key path needed because
    // the forwarded agent supplies the identity.
    let pod_ssh_config =
        "Host source\n\
             User git\n\
             HostName source.gingersociety.org\n\
             Port 3333\n\
             StrictHostKeyChecking no\n\
             UserKnownHostsFile /dev/null\n";

    let write_status = tokio::process::Command::new("kubectl")
        .args([
            "exec", pod_name, "--", "sh", "-c",
            &format!(
                "printf '%s' '{}' > /home/dev/.ssh/config && \
                 chmod 600 /home/dev/.ssh/config && \
                 chown dev:dev /home/dev/.ssh/config",
                pod_ssh_config
            ),
        ])
        .status()
        .await?;

    if write_status.success() {
        println!("  ✓ Permanent git SSH config written into pod (/home/dev/.ssh/config)");
    } else {
        eprintln!("  Warning: failed to write SSH config into pod");
    }

    Ok(())
}

/// Copy SSH keys into /root/.ssh temporarily for the initial git clone.
/// /root is used because kubectl exec runs as root — avoids /home/dev permission issues.
async fn copy_ssh_keys_to_root(pod_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let home    = dirs::home_dir().ok_or("Could not locate home directory")?;
    let ssh_dir = home.join(".ssh");

    let mkdir_status = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "--", "sh", "-c",
               "mkdir -p /root/.ssh && chmod 700 /root/.ssh"])
        .status()
        .await?;

    if !mkdir_status.success() {
        return Err("Failed to create /root/.ssh in pod".into());
    }

    let files: &[(&str, &str, &str)] = &[
        ("id_ed25519",          "/root/.ssh/id_ed25519",          "600"),
        ("id_ed25519.pub",      "/root/.ssh/id_ed25519.pub",      "644"),
        ("id_ed25519-cert.pub", "/root/.ssh/id_ed25519-cert.pub", "644"),
    ];

    for (filename, remote_path, perms) in files {
        let local = ssh_dir.join(filename);
        if !local.exists() {
            eprintln!("  Warning: {} not found, skipping", local.display());
            continue;
        }

        let cp_status = tokio::process::Command::new("kubectl")
            .args(["cp", local.to_str().unwrap(), &format!("{}:{}", pod_name, remote_path)])
            .status()
            .await?;

        if !cp_status.success() {
            eprintln!("  Warning: failed to copy {} into pod", filename);
            continue;
        }

        tokio::process::Command::new("kubectl")
            .args(["exec", pod_name, "--", "chmod", perms, remote_path])
            .status()
            .await?;

        println!("  ✓ Copied {} → pod:{}", filename, remote_path);
    }

    Ok(())
}

/// Delete /root/.ssh entirely — the temporary clone keys live only there.
/// /home/dev/.ssh/config is intentionally left in place for git push.
async fn delete_root_ssh(pod_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rm_status = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "--", "rm", "-rf", "/root/.ssh"])
        .status()
        .await?;

    if rm_status.success() {
        println!("✓ Temporary SSH keys removed from pod (/root/.ssh wiped)");
    } else {
        eprintln!(
            "Warning: could not remove /root/.ssh from pod — remove manually:\n  \
             kubectl exec {} -- rm -rf /root/.ssh",
            pod_name
        );
    }

    Ok(())
}

/// Clone the repo into /workspace using GIT_SSH_COMMAND with the temp root key.
/// After clone, fix ownership to dev so the user can write to the directory.
async fn clone_repo(pod_name: &str, repo_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("  /workspace is empty — cloning {}...", repo_name);

    let clone_cmd = format!(
        "GIT_SSH_COMMAND='ssh \
             -i /root/.ssh/id_ed25519 \
             -o StrictHostKeyChecking=no \
             -o UserKnownHostsFile=/dev/null \
             -p 3333' \
         git clone -b main git@source.gingersociety.org:{repo}.git /workspace/{repo} && \
         chown -R dev:dev /workspace/{repo}",
        repo = repo_name,
    );

    let clone_status = tokio::process::Command::new("kubectl")
        .args(["exec", pod_name, "--", "sh", "-c", &clone_cmd])
        .status()
        .await?;

    if clone_status.success() {
        println!("✓ Cloned {} into /workspace/{}", repo_name, repo_name);
    } else {
        eprintln!(
            "Warning: git clone failed for '{}'. Clone manually inside the pod:\n  \
             GIT_SSH_COMMAND='ssh -i /root/.ssh/id_ed25519 \
             -o StrictHostKeyChecking=no \
             -o UserKnownHostsFile=/dev/null \
             -p 3333' \
             git clone git@source.gingersociety.org:{}.git /workspace/{}",
            repo_name, repo_name, repo_name
        );
    }

    Ok(())
}

// ── Builder image map ─────────────────────────────────────────────────────────

fn builder_image(lang: &str) -> &'static str {
    match lang {
        "TS"   => "gingersociety/dev-container-node:1",
        "Rust" => "gingersociety/rust-rocket-api-builder:latest",
        other  => todo!("builder image not yet defined for lang: {}", other),
    }
}

fn supports_ssh(lang: &str) -> bool {
    lang == "TS"
}

// ── Eject ─────────────────────────────────────────────────────────────────────

/// `meta_name` is e.g. "@ginger-society/dev-portal" — used to derive the repo name.
pub async fn eject(
    deployment_name: &str,
    lang: &str,
    meta_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {

    // ── Read session user ─────────────────────────────────────────────────────
    let user_file = dirs::home_dir()
        .ok_or("Could not locate home directory")?
        .join(".ginger-society")
        .join("user.json");

    let user_json = fs::read_to_string(&user_file)
        .map_err(|e| format!("Could not read {}: {}", user_file.display(), e))?;

    let user_details: serde_json::Value = serde_json::from_str(&user_json)
        .map_err(|e| format!("Could not parse user.json: {}", e))?;

    let session_user = user_details["sub"]
        .as_str()
        .ok_or("sub missing or not a string in user.json")?
        .to_string();

    let image     = builder_image(lang);
    let ssh       = supports_ssh(lang);
    let repo_name = meta_to_repo_name(meta_name);

    // ── Read current image ────────────────────────────────────────────────────
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.spec.template.spec.containers[0].image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
    if original_image.is_empty() {
        return Err("Could not read current image from deployment".into());
    }

    // ── Create PVC ────────────────────────────────────────────────────────────
    let pvc_name = format!("{}-eject-pvc", deployment_name);
    let pvc_yaml = format!(
        r#"apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc_name}
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 4Gi
"#
    );

    let mut apply = tokio::process::Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = apply.stdin.take() {
        stdin.write_all(pvc_yaml.as_bytes()).await?;
    }
    apply.wait().await?;

    // ── Patch deployment ──────────────────────────────────────────────────────
    let command = if ssh {
        serde_json::json!(["/usr/sbin/sshd", "-D", "-e"])
    } else {
        serde_json::json!(["sleep", "infinity"])
    };

    let mut container = serde_json::json!({
        "name":         deployment_name,
        "image":        image,
        "command":      command,
        "volumeMounts": [{ "name": "workspace", "mountPath": "/workspace" }]
    });

    if ssh {
        container["ports"] = serde_json::json!([{ "containerPort": 22 }]);
    }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        "true",
                "ginger-original-image": original_image,
            }
        },
        "spec": {
            "template": {
                "spec": {
                    "containers": [container],
                    "volumes": [{
                        "name": "workspace",
                        "persistentVolumeClaim": { "claimName": pvc_name }
                    }]
                }
            }
        }
    });

    let status = tokio::process::Command::new("kubectl")
        .args([
            "patch", "deployment", deployment_name,
            "--type", "strategic",
            "-p", &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }

    println!("✓ Patched {} → {}", deployment_name, image);

    if ssh {
        // ── Wait for pod ──────────────────────────────────────────────────────
        println!("⏳ Waiting for pod to be scheduled...");
        let pod_name = wait_for_pod_scheduled(deployment_name).await?;
        println!("  pod scheduled: {}", pod_name);

        println!("⏳ Waiting for pod container to be ready...");
        let wait_status = tokio::process::Command::new("kubectl")
            .args([
                "wait",
                &format!("pod/{}", pod_name),
                "--for=condition=Ready",
                "--timeout=120s",
            ])
            .status()
            .await?;

        if !wait_status.success() {
            return Err(format!(
                "Timed out waiting for pod '{}' to become ready", pod_name
            ).into());
        }

        println!("✓ Pod ready: {}", pod_name);

        // ── Write SSH principal ───────────────────────────────────────────────
        let principal_cmd = format!(
            "echo '{}' > /etc/ssh/auth_principals/dev",
            session_user
        );

        let exec_status = tokio::process::Command::new("kubectl")
            .args(["exec", &pod_name, "--", "sh", "-c", &principal_cmd])
            .status()
            .await?;

        if !exec_status.success() {
            return Err(format!(
                "Failed to write SSH principal '{}' into pod {}", session_user, pod_name
            ).into());
        }

        println!("✓ SSH principal '{}' written", session_user);

        // ── Write permanent /home/dev/.ssh/config (agent-forwarded push) ──────
        println!("⏳ Writing pod SSH config for git push...");
        if let Err(e) = write_pod_ssh_config(&pod_name).await {
            eprintln!("Warning: {e}");
        }

        // ── Clone if workspace is empty ───────────────────────────────────────
        // Keys only touch the pod for the duration of the clone, then wiped.
        println!("⏳ Checking workspace...");
        match is_workspace_empty(&pod_name).await {
            Ok(true) => {
                println!("⏳ Copying SSH keys into pod for initial clone...");
                match copy_ssh_keys_to_root(&pod_name).await {
                    Err(e) => eprintln!("Warning: could not copy SSH keys into pod: {e}"),
                    Ok(()) => {
                        if let Err(e) = clone_repo(&pod_name, &repo_name).await {
                            eprintln!("Warning: {e}");
                        }
                        // Always wipe regardless of clone success
                        if let Err(e) = delete_root_ssh(&pod_name).await {
                            eprintln!("Warning: {e}");
                        }
                    }
                }
            }
            Ok(false) => {
                println!("  /workspace is not empty — skipping key copy and clone");
            }
            Err(e) => {
                eprintln!("Warning: could not check workspace: {e}");
            }
        }

        // ── Add `source` host to local ~/.ssh/config ──────────────────────────
        if let Err(e) = add_source_ssh_config() {
            eprintln!("Warning: could not add source host to local ~/.ssh/config: {e}");
        }

        // ── Pick a free port and register with daemon ─────────────────────────
        let forwarding_port = find_free_22xx_port()?;

        let register_payload = serde_json::json!({
            "cmd":             "register",
            "deployment_name": deployment_name,
            "deployment_port": 22,
            "forwarding_port": forwarding_port,
        });

        match send_to_daemon(&register_payload.to_string()) {
            Ok(resp) if resp["status"] == "ok" => {
                println!(
                    "✓ Registered '{}' with daemon — port-forward on localhost:{}",
                    deployment_name, forwarding_port
                );
            }
            Ok(resp) => eprintln!("Warning: daemon responded unexpectedly: {}", resp),
            Err(e) => {
                eprintln!(
                    "Warning: could not register with daemon (is ginger-code running?): {e}\n\
                     You can register manually with:\n  \
                     ginger-code register --deployment-name {} --deployment-port 22 --forwarding-port {}",
                    deployment_name, forwarding_port
                );
            }
        }

        // ── Add local SSH config Host block (with ForwardAgent yes) ───────────
        if let Err(e) = add_ssh_config(deployment_name, forwarding_port) {
            eprintln!("Warning: could not update local ~/.ssh/config: {e}");
        }

        println!("\nConnect with:  ssh {}-local", deployment_name);
        println!("Then push with: git push  (agent forwarding carries your key)");
    }

    Ok(())
}

// ── Uneject ───────────────────────────────────────────────────────────────────

pub async fn uneject(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let img_out = tokio::process::Command::new("kubectl")
        .args([
            "get", "deployment", deployment_name,
            "-o", "jsonpath={.metadata.annotations.ginger-original-image}",
        ])
        .output()
        .await?;

    let original_image = String::from_utf8_lossy(&img_out.stdout).trim().to_string();
    if original_image.is_empty() {
        return Err(
            "No ginger-original-image annotation — was this deployment ejected?".into()
        );
    }

    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "ginger-ejected":        null,
                "ginger-original-image": null,
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
        .args([
            "patch", "deployment", deployment_name,
            "--type", "strategic",
            "-p", &patch.to_string(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(format!("kubectl patch failed for {}", deployment_name).into());
    }

    println!("✓ Unejected {} → restored {}", deployment_name, original_image);

    // ── Deregister from daemon ────────────────────────────────────────────────
    let remove_payload = serde_json::json!({
        "cmd":             "remove",
        "deployment_name": deployment_name,
    });

    match send_to_daemon(&remove_payload.to_string()) {
        Ok(resp) if resp["status"] == "ok" => {
            println!("✓ Deregistered '{}' from daemon — port-forward stopped", deployment_name);
        }
        Ok(resp) => eprintln!("Warning: daemon responded unexpectedly: {}", resp),
        Err(e) => {
            eprintln!(
                "Warning: could not deregister from daemon (is ginger-code running?): {e}\n\
                 You can remove manually with:\n  \
                 ginger-code remove --deployment-name {}",
                deployment_name
            );
        }
    }

    // ── Remove local SSH config Host block ────────────────────────────────────
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
                "get", "pods",
                "-l", &label,
                "--no-headers",
                "-o", "custom-columns=NAME:.metadata.name",
            ])
            .output()
            .await?;

        if let Some(name) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| !l.is_empty())
            .map(|l| l.trim().to_string())
        {
            return Ok(name);
        }

        println!("  … pod not scheduled yet (attempt {}), retrying in 3s", attempt);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    Err(format!("Timed out waiting for a pod to be scheduled for '{}'", deployment_name).into())
}