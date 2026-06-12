# Ginger Code

Ginger Code is a developer tool for managing **ephemeral Kubernetes development environments**. It runs as a background daemon with a system tray icon, and gives you a CLI, a GUI dashboard, and a TUI to inspect services, stream logs, shell into pods, port-forward deployments to your local machine, and switch entire deployments into a "dev container" (ejected) mode for live coding directly against the cluster.

It's built around the idea of **branches**: each git branch you're working on can have its own set of port-forwarded deployments, its own ephemeral environment URL, and its own ejected/mounted dev containers — and switching branches automatically reconfigures everything.

## What it does

- **Branch-aware port forwarding** — register a Kubernetes deployment and Ginger Code keeps a local TCP port forwarded to it, automatically reconnecting if the pod restarts or the network drops.
- **Eject / Uneject** — swap a running deployment's container image for a "builder" dev image (with SSH, your repo cloned, and your branch checked out), so you can edit code live in the cluster via VS Code Remote-SSH or a terminal. Uneject restores the original image.
- **Mount / Unmount dev containers** — spin up a standalone dev container (with PVC-backed workspace) for a package/library that isn't itself a deployed service, useful for working on shared libraries.
- **System tray app** — shows connection status (green/amber/red) for all forwards in the active branch, with menu items to open the dashboard, open VS Code for any deployment, or open a CLI.
- **GUI dashboard (egui)** — a desktop app showing services, packages, and DB schemas from your organization, with live status, log streaming, an embedded terminal (PTY) for shelling into pods, and one-click eject/uneject/mount/unmount.
- **TUI (terminal dashboard)** — a `ratatui`-based equivalent of the GUI for terminal-only environments, with the same sidebar/log/eject/mount workflows and keyboard navigation.
- **CLI** — lightweight commands for switching branches, registering/removing port forwards, and checking daemon/forward status, designed to be scripted (e.g. from CI or other dev tooling).

## How it's organized

- **Daemon** (`main.rs`) — runs in the background, watches `~/.ginger-society/code.toml` for branch changes, manages all active port-forwards via `kube-rs`, monitors network connectivity, and exposes a Unix socket API (`ping`, `register`, `list`, `remove`).
- **Tray** (`tray.rs`) — a `tray-icon`/`winit` app showing daemon/forward status and providing quick actions.
- **CLI** (`bin/cli.rs`) — talks to the daemon over the Unix socket, plus handles the `-b/--branch` flag and `config`/`status` subcommands directly.
- **GUI** (`bin/gui.rs`, `shared/gui/`) — an `egui`/`eframe` desktop dashboard.
- **TUI** (`shared/tui/`) — a `ratatui`/`crossterm` terminal dashboard, launched via the session-guarded `config` command.
- **Shared core** (`shared/core/`) — all the Kubernetes, git, SSH, and metadata-service logic shared by the GUI, TUI, and CLI: eject/uneject, mount/unmount, port discovery, SSH config management, pod exec/attach, and data fetching from the Ginger Society metadata API.

## Prerequisites

- A working `kubectl`/`KUBECONFIG` pointing at the target cluster (the daemon builds a `kube-rs` client from your default kube config).
- A logged-in Ginger Society session (token stored via `ginger_shared_rs::utils::get_token_from_file_storage`), used to talk to the IAM and Metadata services.
- `~/.ginger-society/user.json` containing your user details (used as the SSH principal when ejecting).
- For ejected/mounted dev containers: an SSH key pair at `~/.ssh/id_ed25519` (and optional `id_ed25519-cert.pub`) for connecting to the gitolite "source" host and to the dev container itself.
- (Optional) VS Code with the Remote-SSH extension, if you want the "Open Editor" actions to work.

## Getting started

1. **Start the daemon and tray**

   Run the binary with no arguments to start the daemon, tray icon, network monitor, and Unix socket listener:

   ```sh
   ginger-code
   ```

   Or run it headless (no tray, e.g. on a server/CI):

   ```sh
   ginger-code --daemon
   ```

2. **Set your active branch**

   This is the central concept — most other commands operate against the "active branch":

   ```sh
   ginger-code -b feature/my-branch -e feat01 -u feat01.ginger-society.test-env.acme.com
   ```

   - `-b/--branch` — the branch name (creates `~/.ginger-society/branches/<slug>.toml` if needed).
   - `-e/--env` — an environment name to associate with this branch.
   - `-u/--url` — the public URL of the ephemeral environment for this branch.

   This just writes `~/.ginger-society/code.toml`; the running daemon picks up the change within ~2 seconds, tears down old forwards, and starts new ones for the new branch.

3. **Open the dashboard**

   - From the tray menu: **Open Dashboard**.
   - Or run the GUI directly:

     ```sh
     ginger-code --gui
     ```

   The dashboard lists your organization's **Services**, **Packages & Executables**, and **DB Schemas**. Selecting a service shows live status, container tabs, and streamed logs; you can open a terminal into any container, or eject the service into dev mode.

4. **Open the TUI**

   Running `ginger-code` (or `ginger-code config`, its hidden default) after authenticating launches the terminal dashboard. Navigate with `↑/↓`, switch focus with `←/→`, switch containers with `Shift+←/→`, and use `s` to shell, `e` to eject/uneject, `c` to open VS Code, `m` to mount/unmount packages, and `q` to quit.

## CLI reference

All daemon-dependent commands require the daemon to be running (`ginger-code` or `ginger-code --daemon`).

```sh
# Check the daemon is alive
ginger-code ping

# Register a deployment for port-forwarding in the active branch
ginger-code register --deployment-name my-service --deployment-port 8080 --forwarding-port 8080

# List all deployments and their forward status for the active branch
ginger-code list

# Remove a deployment's registration and tear down its forward
ginger-code remove --deployment-name my-service

# Show the active branch, env, url, and daemon status (no daemon required)
ginger-code status
```

## Eject / Uneject workflow

**Eject** swaps a deployment's main container image for a builder image matching its language (currently TypeScript and Rust), mounts a workspace PVC, clones your repo over SSH, and checks out the active branch:

- From the GUI/TUI: select the service and press **Eject** / `e`.
- Once ejected, an SSH config entry (`<deployment>-local`) and a local port-forward (in the 2200–2299 range) are created automatically.
- Connect with: `ssh <deployment>-local`, then `git push` (your SSH agent forwarding carries your key).
- Open the workspace in VS Code via the **Open Editor** action — uses `vscode-remote://ssh-remote+<deployment>-local/...`.

**Uneject** restores the original container image, removes the branch-config entry, tears down the port-forward, and cleans up the local SSH config.

## Mount / Unmount workflow

For packages/libraries that aren't deployed services, **Mount** creates a dedicated dev-container Deployment + PVCs, clones the repo, and (for SSH-capable languages) sets up the same SSH access pattern as eject. **Unmount** deletes the deployment and PVCs and cleans up SSH config.

## Notes

- Branch configuration lives in `~/.ginger-society/code.toml` (active branch/env/url) and `~/.ginger-society/branches/<branch-slug>.toml` (per-branch deployment registrations).
- The daemon automatically detects network loss/restoration and marks forwards as offline/retrying/connected accordingly — the tray icon reflects this with green (all connected), amber (partial), or red (offline/no branch).
- Supported eject/mount languages are currently `TS` and `Rust`; other languages will report an error when attempting to eject or mount.