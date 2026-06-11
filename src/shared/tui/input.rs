//! Keyboard event handling for the TUI.
//!
//! Mouse handling has been fully removed.
//! Returns an `Action` enum so `mod.rs` can execute side-effects
//! (leaving/entering the TUI, shell, VS Code) without `input.rs` knowing
//! about the terminal backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::time::sleep;

use crate::shared::core::types::{DbSchema, K8sService, Package};
use super::{
    background::{
        TuiMsg, spawn_container_fetch, spawn_db_container_fetch,
        spawn_db_log_stream, spawn_service_log_stream,
    },
    state::TuiState,
    types::{Focus, Popup, PopupAction, SidebarItem},
};

// ── Return type ───────────────────────────────────────────────────────────────

/// What the main loop should do after `handle_key` returns.
pub enum Action {
    /// Keep running.
    Continue,
    /// Tear down the TUI, run a shell in the given deployment, then restore.
    Shell(String),
    /// Tear down the TUI, open VS Code at the given URI, then restore.
    OpenVsCode(String),
    /// Tear down the TUI, run eject/uneject for the given service, then restore.
    Eject  { deployment: String, lang: String, meta: String, org: String },
    Uneject { deployment: String },
    /// Mount/unmount a package.
    Mount  { org: String, id: String, lang: String },
    Unmount { org: String, id: String },
    /// Exit the application.
    Quit,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Handle one keyboard event and mutate `state` in place.
/// Returns the `Action` the main loop must execute.
pub fn handle_key(
    key:       KeyEvent,
    state:     &mut TuiState,
    bg_tx:     &std::sync::mpsc::Sender<TuiMsg>,
    // Snapshots are passed in (already locked by the caller for drawing)
    services_snap:   &[K8sService],
    packages_snap:   &[Package],
    db_schemas_snap: &[DbSchema],
) -> Action {
    // ── Popup takes priority ──────────────────────────────────────────────────
    if state.popup.is_some() {
        return handle_popup_key(key, state, services_snap, packages_snap);
    }

    // ── Normal keys ───────────────────────────────────────────────────────────
    let can_shell = state.focus == Focus::Logs
        && matches!(state.sidebar_item, SidebarItem::Service(_))
        && is_deployed_non_ejected(state, services_snap);

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            state.popup = Some(Popup {
                service_name: String::new(),
                action:       PopupAction::Quit,
                selected:     1,
            });
            Action::Continue
        }

        // ── Focus movement ────────────────────────────────────────────────────
        KeyCode::Left if key.modifiers != KeyModifiers::SHIFT => {
            state.focus = Focus::Sidebar;
            Action::Continue
        }
        KeyCode::Right if key.modifiers != KeyModifiers::SHIFT => {
            if matches!(state.sidebar_item, SidebarItem::Service(_) | SidebarItem::DbSchema(_)) {
                state.focus = Focus::Logs;
            }
            Action::Continue
        }

        // ── Container tab: Shift+←/→ ──────────────────────────────────────────
        KeyCode::Left if key.modifiers == KeyModifiers::SHIFT => {
            shift_container(state, bg_tx, services_snap, db_schemas_snap, -1);
            Action::Continue
        }
        KeyCode::Right if key.modifiers == KeyModifiers::SHIFT => {
            shift_container(state, bg_tx, services_snap, db_schemas_snap, 1);
            Action::Continue
        }

        // ── Sidebar / scroll navigation ───────────────────────────────────────
        KeyCode::Up | KeyCode::Char('k') => {
            if state.focus == Focus::Logs {
                if state.auto_scroll { state.scroll_offset = state.db_last_max_scroll; }
                state.auto_scroll   = false;
                state.scroll_offset = state.scroll_offset.saturating_sub(1);
            } else {
                let prev = sidebar_prev(&state.sidebar_item, services_snap, packages_snap, db_schemas_snap);
                on_sidebar_move(&prev, state, bg_tx, services_snap, db_schemas_snap);
                state.sidebar_item = prev;
            }
            Action::Continue
        }

        KeyCode::Down | KeyCode::Char('j') => {
            if state.focus == Focus::Logs {
                state.auto_scroll   = false;
                state.scroll_offset += 1;
            } else {
                let next = sidebar_next(&state.sidebar_item, services_snap, packages_snap, db_schemas_snap);
                on_sidebar_move(&next, state, bg_tx, services_snap, db_schemas_snap);
                state.sidebar_item = next;
            }
            Action::Continue
        }

        KeyCode::PageDown => {
            if state.focus == Focus::Logs { state.auto_scroll = true; state.scroll_offset = state.db_last_max_scroll; }
            Action::Continue
        }
        KeyCode::PageUp => {
            if state.focus == Focus::Logs { state.auto_scroll = false; state.scroll_offset = 0; }
            Action::Continue
        }
        KeyCode::Char('g') => {
            if state.focus == Focus::Logs { state.auto_scroll = false; state.scroll_offset = 0; }
            Action::Continue
        }
        KeyCode::Char('G') => {
            if state.focus == Focus::Logs { state.auto_scroll = true; state.scroll_offset = state.db_last_max_scroll; }
            Action::Continue
        }

        // ── Mount / unmount ───────────────────────────────────────────────────
        KeyCode::Char('m') => {
            if let SidebarItem::Package(pkg_i) = state.sidebar_item {
                if let Some(pkg) = packages_snap.get(pkg_i) {
                    state.popup = Some(Popup {
                        service_name: pkg.identifier.clone(),
                        action: if pkg.mounted { PopupAction::Unmount } else { PopupAction::Mount },
                        selected: 0,
                    });
                }
            }
            Action::Continue
        }

        // ── VS Code ───────────────────────────────────────────────────────────
        KeyCode::Char('c') => {
            if let Some(uri) = vscode_uri(&state.sidebar_item, services_snap, packages_snap) {
                Action::OpenVsCode(uri)
            } else {
                Action::Continue
            }
        }

        // ── Shell ─────────────────────────────────────────────────────────────
        KeyCode::Char('s') => {
            if can_shell {
                if let SidebarItem::Service(i) = state.sidebar_item {
                    if let Some(svc) = services_snap.get(i) {
                        if let Some(ref dep) = svc.deployment_name {
                            return Action::Shell(dep.clone());
                        }
                    }
                }
            }
            Action::Continue
        }

        // ── Eject / uneject ───────────────────────────────────────────────────
        KeyCode::Char('e') => {
            if let SidebarItem::Service(i) = state.sidebar_item {
                if let Some(svc) = services_snap.get(i) {
                    let has_dep  = svc.status != "Not deployed" && svc.status != "Unknown";
                    let has_lang = svc.lang.is_some();
                    if has_dep && has_lang {
                        state.popup = Some(Popup {
                            service_name: svc.meta_name.clone(),
                            action: if svc.ejected { PopupAction::Uneject } else { PopupAction::Eject },
                            selected: 0,
                        });
                    }
                }
            }
            Action::Continue
        }

        _ => Action::Continue,
    }
}

// ── Popup key handler ─────────────────────────────────────────────────────────

fn handle_popup_key(
    key:          KeyEvent,
    state:        &mut TuiState,
    services_snap: &[K8sService],
    packages_snap: &[Package],
) -> Action {
    let popup = match state.popup.as_mut() {
        Some(p) => p,
        None    => return Action::Continue,
    };

    if popup.action == PopupAction::ShellBlocked {
        state.popup = None;
        return Action::Continue;
    }

    match key.code {
        KeyCode::Left  | KeyCode::Char('h') => { popup.selected = 0; Action::Continue }
        KeyCode::Right | KeyCode::Char('l') => { popup.selected = 1; Action::Continue }
        KeyCode::Tab => {
            let sel = (popup.selected + 1) % 2;
            state.popup.as_mut().unwrap().selected = sel;
            Action::Continue
        }
        KeyCode::Esc => { state.popup = None; Action::Continue }
        KeyCode::Enter => {
            let selected = popup.selected;
            if selected == 1 {
                state.popup = None;
                return Action::Continue;
            }
            // selected == 0  →  confirm
            let action = match state.popup.take() {
                Some(p) => p.action,
                None    => return Action::Continue,
            };
            confirm_popup_action(action, state, services_snap, packages_snap)
        }
        _ => Action::Continue,
    }
}

fn confirm_popup_action(
    action:        PopupAction,
    state:         &mut TuiState,
    services_snap: &[K8sService],
    packages_snap: &[Package],
) -> Action {
    match action {
        PopupAction::Quit => Action::Quit,
        PopupAction::ShellBlocked => Action::Continue,

        PopupAction::Eject | PopupAction::Uneject => {
            if let SidebarItem::Service(svc_i) = state.sidebar_item {
                if let Some(svc) = services_snap.get(svc_i) {
                    if let Some(ref dep) = svc.deployment_name {
                        let ejected = svc.ejected;
                        if ejected {
                            return Action::Uneject { deployment: dep.clone() };
                        } else {
                            return Action::Eject {
                                deployment: dep.clone(),
                                lang:       svc.lang.clone().unwrap_or_default(),
                                meta:       svc.meta_name.clone(),
                                org:        svc.organization_id.clone(),
                            };
                        }
                    }
                }
            }
            Action::Continue
        }

        PopupAction::Mount => {
            if let SidebarItem::Package(pkg_i) = state.sidebar_item {
                if let Some(pkg) = packages_snap.get(pkg_i) {
                    return Action::Mount {
                        org:  pkg.organization_id.clone(),
                        id:   pkg.identifier.clone(),
                        lang: pkg.lang.clone(),
                    };
                }
            }
            Action::Continue
        }

        PopupAction::Unmount => {
            if let SidebarItem::Package(pkg_i) = state.sidebar_item {
                if let Some(pkg) = packages_snap.get(pkg_i) {
                    return Action::Unmount {
                        org: pkg.organization_id.clone(),
                        id:  pkg.identifier.clone(),
                    };
                }
            }
            Action::Continue
        }
    }
}

// ── Sidebar navigation helpers ────────────────────────────────────────────────

fn on_sidebar_move(
    item:            &SidebarItem,
    state:           &mut TuiState,
    bg_tx:           &std::sync::mpsc::Sender<TuiMsg>,
    services_snap:   &[K8sService],
    db_schemas_snap: &[DbSchema],
) {
    match item {
        SidebarItem::Service(i) => {
            switch_service_logs(*i, services_snap, &mut state.svc_log_generation,
                                &state.container_selection, bg_tx, &state.logs);
            state.auto_scroll = true;
            state.scroll_offset = 0;
        }
        SidebarItem::DbSchema(i) => {
            maybe_start_db_stream(*i, db_schemas_snap, &mut state.db_log_schema,
                                  &mut state.db_log_generation, &state.db_logs, bg_tx,
                                  &mut state.db_containers, &mut state.db_selected_container);
            state.auto_scroll = true;
            state.scroll_offset = 0;
        }
        SidebarItem::Package(_) => {}
    }
}

fn shift_container(
    state:           &mut TuiState,
    bg_tx:           &std::sync::mpsc::Sender<TuiMsg>,
    services_snap:   &[K8sService],
    db_schemas_snap: &[DbSchema],
    direction:       i32,
) {
    match state.sidebar_item.clone() {
        SidebarItem::Service(svc_i) => {
            if let Some(svc) = services_snap.get(svc_i) {
                let n = svc.containers.len();
                if n > 1 {
                    let cur  = *state.container_selection.get(&svc_i).unwrap_or(&0);
                    let next = if direction < 0 {
                        if cur == 0 { n - 1 } else { cur - 1 }
                    } else {
                        (cur + 1) % n
                    };
                    select_container(svc_i, next, &[svc.clone()],
                                     &mut state.container_selection,
                                     &mut state.svc_log_generation, bg_tx, &state.logs);
                }
            }
        }
        SidebarItem::DbSchema(schema_i) => {
            let n = state.db_containers.len();
            if n > 1 {
                let cur = state.db_containers.iter()
                    .position(|c| Some(c.as_str()) == state.db_selected_container.as_deref())
                    .unwrap_or(0);
                let next = if direction < 0 {
                    if cur == 0 { n - 1 } else { cur - 1 }
                } else {
                    (cur + 1) % n
                };
                select_db_container(schema_i, next, &state.db_containers.clone(),
                                    db_schemas_snap, &mut state.db_selected_container,
                                    &mut state.db_log_generation, &mut state.db_log_schema,
                                    &state.db_logs, bg_tx);
                state.auto_scroll   = true;
                state.scroll_offset = 0;
            }
        }
        SidebarItem::Package(_) => {}
    }
}

// ── VS Code URI builder ───────────────────────────────────────────────────────

fn vscode_uri(
    sidebar_item:  &SidebarItem,
    services_snap: &[K8sService],
    packages_snap: &[Package],
) -> Option<String> {
    match sidebar_item {
        SidebarItem::Package(i) => {
            let pkg = packages_snap.get(*i)?;
            if !pkg.mounted { return None; }
            let alias = format!("{}-local", pkg.identifier);
            Some(format!("vscode-remote://ssh-remote+{}/workspace/{}-{}",
                alias, pkg.organization_id, pkg.identifier))
        }
        SidebarItem::Service(i) => {
            let svc = services_snap.get(*i)?;
            if !svc.ejected { return None; }
            let dep = svc.deployment_name.as_ref()?;
            Some(format!("vscode-remote://ssh-remote+{}-local/workspace/{}-{}",
                dep, svc.organization_id, dep))
        }
        SidebarItem::DbSchema(_) => None,
    }
}

// ── Predicate helpers ─────────────────────────────────────────────────────────

fn is_deployed_non_ejected(state: &TuiState, services_snap: &[K8sService]) -> bool {
    let SidebarItem::Service(i) = state.sidebar_item else { return false };
    let Some(svc) = services_snap.get(i) else { return false };
    let has_dep = svc.status != "Not deployed" && svc.status != "Unknown";
    if !has_dep { return false; }

    let idx      = *state.container_selection.get(&i).unwrap_or(&0);
    let container = svc.containers.get(idx).map(|s| s.as_str());
    let is_ejected_container = svc.ejected && matches!(
        (container, svc.ejected_container.as_deref()),
        (Some(ac), Some(ec)) if ac == ec
    ) || (svc.ejected && svc.containers.len() <= 1);

    !is_ejected_container
}

// ── Sidebar prev/next ─────────────────────────────────────────────────────────

fn sidebar_total(s: &[K8sService], p: &[Package], d: &[DbSchema]) -> usize {
    s.len() + p.len() + d.len()
}

fn sidebar_flat(item: &SidebarItem, sc: usize, pc: usize) -> usize {
    match item {
        SidebarItem::Service(i)  => *i,
        SidebarItem::Package(i)  => sc + i,
        SidebarItem::DbSchema(i) => sc + pc + i,
    }
}

fn sidebar_from_flat(flat: usize, sc: usize, pc: usize) -> SidebarItem {
    if flat < sc        { SidebarItem::Service(flat) }
    else if flat < sc + pc { SidebarItem::Package(flat - sc) }
    else               { SidebarItem::DbSchema(flat - sc - pc) }
}

pub fn sidebar_next(cur: &SidebarItem, s: &[K8sService], p: &[Package], d: &[DbSchema]) -> SidebarItem {
    let total = sidebar_total(s, p, d);
    if total == 0 { return cur.clone(); }
    let flat  = sidebar_flat(cur, s.len(), p.len());
    sidebar_from_flat((flat + 1).min(total - 1), s.len(), p.len())
}

pub fn sidebar_prev(cur: &SidebarItem, s: &[K8sService], p: &[Package], d: &[DbSchema]) -> SidebarItem {
    if sidebar_total(s, p, d) == 0 { return cur.clone(); }
    let flat = sidebar_flat(cur, s.len(), p.len());
    sidebar_from_flat(flat.saturating_sub(1), s.len(), p.len())
}

// ── Log / container switch helpers (also used by background message draining) ─

pub fn switch_service_logs(
    svc_i:               usize,
    services:            &[K8sService],
    svc_log_generation:  &mut u64,
    container_selection: &HashMap<usize, usize>,
    bg_tx:               &std::sync::mpsc::Sender<TuiMsg>,
    logs:                &Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    let Some(svc) = services.get(svc_i) else { return };
    let Some(dep) = svc.deployment_name.clone() else { return };

    let container_idx        = *container_selection.get(&svc_i).unwrap_or(&0);

    if svc.ejected && svc.containers.is_empty() {
        spawn_container_fetch(bg_tx.clone(), dep, svc_i);
        return;
    }

    let container            = svc.containers.get(container_idx).cloned();
    let is_ejected_container = svc.ejected
        && svc.ejected_container.as_deref() == container.as_deref();

    if is_ejected_container {
        logs.lock().unwrap().remove(&svc.meta_name);
        if svc.containers.is_empty() { spawn_container_fetch(bg_tx.clone(), dep, svc_i); }
        return;
    }

    *svc_log_generation += 1;
    spawn_service_log_stream(bg_tx.clone(), dep.clone(), container, *svc_log_generation);

    if svc.containers.is_empty() { spawn_container_fetch(bg_tx.clone(), dep, svc_i); }
}

pub fn select_container(
    svc_i:               usize,
    container_idx:       usize,
    services:            &[K8sService],
    container_selection: &mut HashMap<usize, usize>,
    svc_log_generation:  &mut u64,
    bg_tx:               &std::sync::mpsc::Sender<TuiMsg>,
    logs:                &Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    let Some(svc) = services.first() else { return };
    let Some(dep) = svc.deployment_name.clone() else { return };
    let container = svc.containers.get(container_idx).cloned();

    container_selection.insert(svc_i, container_idx);

    let is_ejected_container = svc.ejected
        && svc.ejected_container.as_deref() == container.as_deref();

    logs.lock().unwrap().remove(&svc.meta_name);

    if is_ejected_container { return; }

    *svc_log_generation += 1;
    spawn_service_log_stream(bg_tx.clone(), dep, container, *svc_log_generation);
}

pub fn maybe_start_db_stream(
    idx:                   usize,
    db_schemas:            &[DbSchema],
    db_log_schema:         &mut Option<usize>,
    db_log_generation:     &mut u64,
    db_logs:               &Arc<Mutex<Option<Vec<String>>>>,
    bg_tx:                 &std::sync::mpsc::Sender<TuiMsg>,
    db_containers:         &mut Vec<String>,
    db_selected_container: &mut Option<String>,
) {
    if *db_log_schema == Some(idx) { return; }

    *db_log_schema         = Some(idx);
    *db_log_generation    += 1;
    *db_logs.lock().unwrap() = None;
    *db_containers         = Vec::new();
    *db_selected_container = None;

    let slug = db_schemas.get(idx)
        .and_then(|s| s.k8s_name.clone())
        .unwrap_or_default();

    if slug.is_empty() {
        *db_logs.lock().unwrap() = Some(vec![]);
        return;
    }

    spawn_db_container_fetch(bg_tx.clone(), slug.clone(), idx);
    spawn_db_log_stream(bg_tx.clone(), slug, None, *db_log_generation);
}

pub fn select_db_container(
    schema_idx:            usize,
    container_idx:         usize,
    db_containers:         &[String],
    db_schemas:            &[DbSchema],
    db_selected_container: &mut Option<String>,
    db_log_generation:     &mut u64,
    db_log_schema:         &mut Option<usize>,
    db_logs:               &Arc<Mutex<Option<Vec<String>>>>,
    bg_tx:                 &std::sync::mpsc::Sender<TuiMsg>,
) {
    let Some(container) = db_containers.get(container_idx).cloned() else { return };

    *db_selected_container = Some(container.clone());
    *db_log_schema         = Some(schema_idx);
    *db_log_generation    += 1;
    *db_logs.lock().unwrap() = None;

    let slug = db_schemas.get(schema_idx)
        .and_then(|s| s.k8s_name.clone())
        .unwrap_or_default();

    if slug.is_empty() {
        *db_logs.lock().unwrap() = Some(vec![]);
        return;
    }

    spawn_db_log_stream(bg_tx.clone(), slug, Some(container), *db_log_generation);
}