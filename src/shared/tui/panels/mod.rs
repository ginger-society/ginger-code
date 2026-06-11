//! Rendering sub-modules for each TUI panel.
//! The only public surface is `draw()` and `DrawnAreas`.

pub mod sidebar;
pub mod service;
pub mod db_schema;
pub mod package;
pub mod help_bar;

use std::collections::HashMap;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::shared::core::types::{DbSchema, K8sService, Package};
use crate::shared::tui::{
    popup::render_popup,
    types::{Focus, Popup, SidebarItem},
};

pub use sidebar::DrawnAreas;

// ── Color / icon helpers (shared across sub-modules) ─────────────────────────

pub fn status_color(status: &str) -> ratatui::style::Color {
    use ratatui::style::Color;
    match status {
        "Running"      => Color::Green,
        "Degraded"     => Color::Yellow,
        "Pending"      => Color::Yellow,
        "Not deployed" => Color::DarkGray,
        _              => Color::Red,
    }
}

pub fn status_icon(status: &str) -> &'static str {
    match status {
        "Running"      => "●",
        "Degraded"     => "◐",
        "Pending"      => "○",
        "Not deployed" => "·",
        _              => "✗",
    }
}

// ── Top-level draw ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw(
    f:                     &mut Frame,
    services:              &[K8sService],
    packages:              &[Package],
    db_schemas:            &[DbSchema],
    sidebar_item:          &SidebarItem,
    logs:                  &HashMap<String, Vec<String>>,
    db_logs:               Option<&[String]>,
    focus:                 &Focus,
    auto_scroll:           bool,
    scroll_offset:         usize,
    has_deployment:        bool,
    has_lang:              bool,
    is_ejected_now:        bool,
    popup:                 Option<&Popup>,
    active_container_idx:  usize,
    active_container:      Option<&str>,
    db_containers:         &[String],
    db_selected_container: Option<&str>,
    can_shell:             bool,
    db_last_max_scroll:    &mut usize,
    scroll_offset_out:     &mut usize,
    sidebar_scroll_out:    &mut usize,
    log_max_scroll_out:    &mut usize,  // ← always written; covers both service and db panels
) {
    let area = f.size();

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(root[0]);

    // ── Sidebar ───────────────────────────────────────────────────────────────
    let sidebar_scroll = sidebar::draw(f, chunks[0], services, packages, db_schemas, sidebar_item, focus);
    *sidebar_scroll_out = sidebar_scroll;

    // ── Right panel ───────────────────────────────────────────────────────────
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(0)])
        .split(chunks[1]);

    let multi_container = if let SidebarItem::Service(i) = sidebar_item {
        services.get(*i).map(|s| s.containers.len() > 1).unwrap_or(false)
    } else { false };

    // Clear the entire right panel before rendering so no characters from a
    // previously selected service bleed through (e.g. after switching items
    // while scrolled, or when the new content is shorter than the old).
    f.render_widget(ratatui::widgets::Clear, chunks[1]);

    match sidebar_item {
        SidebarItem::Package(i) => {
            package::draw(f, chunks[1], packages.get(*i), focus);
        }

        SidebarItem::DbSchema(i) => {
            let max_scroll = db_schema::draw(
                f, chunks[1], db_schemas.get(*i), db_logs,
                focus, scroll_offset, auto_scroll,
                db_containers, db_selected_container,
            );
            *db_last_max_scroll  = max_scroll;
            *log_max_scroll_out  = max_scroll;
            if auto_scroll { *scroll_offset_out = max_scroll; }
        }

        SidebarItem::Service(svc_idx) => {
            let selected = services.get(*svc_idx);

            // Calculate scroll before drawing
            if let Some(svc) = selected {
                let log_text  = logs.get(&svc.meta_name).map(|l| l.join("\n")).unwrap_or_default();
                let max_scroll = log_text.lines().count()
                    .saturating_sub(f.size().height.saturating_sub(10) as usize);
                if auto_scroll {
                    *scroll_offset_out = max_scroll;
                } else {
                    *scroll_offset_out = (*scroll_offset_out).min(max_scroll);
                    if *scroll_offset_out >= max_scroll { *scroll_offset_out = max_scroll; }
                }
            }

            let svc_max = if multi_container {
                let info_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(4), Constraint::Length(2), Constraint::Min(0)])
                    .split(chunks[1]);

                service::draw_info(f, info_chunks[0], selected, has_deployment, has_lang, is_ejected_now);
                service::draw_container_tabs(f, info_chunks[1], selected, active_container_idx, active_container);
                service::draw_logs(f, info_chunks[2], selected, logs, focus, auto_scroll, *scroll_offset_out, active_container)
            } else {
                service::draw_info(f, right_chunks[0], selected, has_deployment, has_lang, is_ejected_now);
                service::draw_logs(f, right_chunks[1], selected, logs, focus, auto_scroll, *scroll_offset_out, active_container)
            };

            *log_max_scroll_out = svc_max;
        }
    }

    // ── Help bar ──────────────────────────────────────────────────────────────
    let help_multi = multi_container
        || (matches!(sidebar_item, SidebarItem::DbSchema(_)) && db_containers.len() > 1);

    help_bar::draw(f, root[1], focus, sidebar_item, has_deployment, has_lang,
                   is_ejected_now, help_multi, can_shell);

    // ── Popup overlay ─────────────────────────────────────────────────────────
    if let Some(p) = popup { render_popup(f, p, area); }
}