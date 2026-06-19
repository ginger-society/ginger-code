//! Bottom help-bar rendering and help text generation.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::shared::core::types::InfraAsCode;
use crate::shared::tui::types::{Focus, SidebarItem};

pub fn draw(
    f:               &mut Frame,
    area:            Rect,
    focus:           &Focus,
    sidebar_item:    &SidebarItem,
    has_deployment:  bool,
    has_lang:        bool,
    ejected:         bool,
    multi_container: bool,
    can_shell:       bool,
    iac:             &InfraAsCode,
) {
    f.render_widget(
        Paragraph::new(help_text(focus, sidebar_item, has_deployment, has_lang, ejected, multi_container, can_shell, iac))
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP)),
        area,
    );
}

pub fn help_text(
    focus:           &Focus,
    sidebar_item:    &SidebarItem,
    has_deployment:  bool,
    has_lang:        bool,
    ejected:         bool,
    multi_container: bool,
    can_shell:       bool,
    iac:             &InfraAsCode,
) -> String {
    match focus {
        Focus::Sidebar => match sidebar_item {
            SidebarItem::Service(_) => {
                let mut parts = vec!["↑/↓ navigate", "→ logs"];
                if multi_container { parts.push("⇧←/⇧→ container"); }
                if has_deployment && has_lang {
                    parts.push(if ejected { "e uneject" } else { "e eject" });
                }
                if ejected { parts.push("c VS Code"); }
                parts.push("q quit");
                parts.join("  |  ")
            }
            SidebarItem::Package(_) => {
                "↑/↓ navigate  |  m mount/unmount  |  c VS Code (if mounted)  |  q quit".to_string()
            }
            SidebarItem::DbSchema(_) => {
                "↑/↓ navigate  |  → logs  |  ⇧←/⇧→ container  |  q quit".to_string()
            }
            SidebarItem::InfraAsCode => {
                if iac.mounted {
                    "↑/↓ navigate  |  m unmount  |  c VS Code  |  q quit".to_string()
                } else {
                    "↑/↓ navigate  |  m mount  |  q quit".to_string()
                }
            }
        },
        Focus::Logs => {
            let mut parts = vec!["PgUp top", "PgDn follow", "↑/↓ scroll", "g/G jump", "⇧←/⇧→ container", "← sidebar"];
            if can_shell { parts.push("s shell"); }
            parts.push("q quit");
            parts.join("  |  ")
        }
    }
}