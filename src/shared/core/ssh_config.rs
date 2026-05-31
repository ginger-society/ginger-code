//! Helpers for reading and writing `~/.ssh/config` blocks.
//!
//! Each managed block is wrapped in:
//!
//! ```text
//! # BEGIN ginger-eject: <deployment_name>
//! Host <deployment_name>-local
//!     …
//! # END ginger-eject: <deployment_name>
//! ```
//!
//! The "source" host entry for git-over-SSH is guarded by its own pair of
//! markers so it is only written once regardless of how many deployments /
//! packages are ejected / mounted.

use std::fs;
use std::io::Write;

const CONFIG_BEGIN_MARKER: &str = "# BEGIN ginger-eject:";
const CONFIG_END_MARKER: &str = "# END ginger-eject:";
const SOURCE_HOST_MARKER: &str = "# BEGIN ginger-source";
const SOURCE_HOST_END: &str = "# END ginger-source";

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ssh_config_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Could not locate home directory")?;
    Ok(home.join(".ssh").join("config"))
}

fn ssh_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Could not locate home directory")?;
    Ok(home.join(".ssh"))
}

fn identity_paths() -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>>
{
    let home = dirs::home_dir().ok_or("Could not locate home directory")?;
    Ok((
        home.join(".ssh").join("id_ed25519"),
        home.join(".ssh").join("id_ed25519-cert.pub"),
    ))
}

fn ensure_ssh_config_file() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let dir  = ssh_dir()?;
    let path = dir.join("config");

    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    if !path.exists() {
        fs::File::create(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(path)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Add the static `source` host entry used for git-over-SSH (idempotent).
pub fn add_source_ssh_config() -> Result<(), Box<dyn std::error::Error>> {
    let path     = ensure_ssh_config_file()?;
    let existing = fs::read_to_string(&path).unwrap_or_default();

    if existing.contains(SOURCE_HOST_MARKER) {
        return Ok(());
    }

    let home          = dirs::home_dir().ok_or("Could not locate home directory")?;
    let identity_file = home.join(".ssh").join("id_ed25519");

    let block = format!(
        "\n{begin}\n\
         Host source\n\
             User git\n\
             HostName source.gingersociety.org\n\
             Port 3333\n\
             IdentityFile {identity}\n\
             StrictHostKeyChecking no\n\
             UserKnownHostsFile /dev/null\n\
         {end}\n",
        begin    = SOURCE_HOST_MARKER,
        end      = SOURCE_HOST_END,
        identity = identity_file.display(),
    );

    let mut file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(block.as_bytes())?;
    println!("✓ Added 'source' git SSH host to local ~/.ssh/config");
    Ok(())
}

/// Add a per-deployment / per-package SSH alias block (idempotent).
///
/// The `host_alias` is the name the user connects with: `ssh <host_alias>`.
/// Typically `<deployment_name>-local`.
pub fn add_ssh_config(
    deployment_name: &str,
    forwarding_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let path     = ensure_ssh_config_file()?;
    let existing = fs::read_to_string(&path).unwrap_or_default();

    let marker = format!("{} {}", CONFIG_BEGIN_MARKER, deployment_name);
    if existing.contains(&marker) {
        println!(
            "  ~/.ssh/config already contains block for '{}', skipping",
            deployment_name
        );
        return Ok(());
    }

    let (identity_file, cert_file) = identity_paths()?;
    let host_alias = format!("{}-local", deployment_name);

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

    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(block.as_bytes())?;
    println!("✓ Added SSH config block — connect with: ssh {}", host_alias);
    Ok(())
}

/// Remove the SSH alias block for `deployment_name` (idempotent).
pub fn remove_ssh_config(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = match ssh_config_path() {
        Ok(p) if p.exists() => p,
        _ => return Ok(()),
    };

    let content = fs::read_to_string(&path)?;
    let begin   = format!("{} {}", CONFIG_BEGIN_MARKER, deployment_name);
    let end     = format!("{} {}", CONFIG_END_MARKER, deployment_name);

    if !content.contains(&begin) {
        println!(
            "  No SSH config block found for '{}', nothing to remove",
            deployment_name
        );
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

    fs::write(&path, out)?;
    println!("✓ Removed SSH config block for '{}'", deployment_name);
    Ok(())
}