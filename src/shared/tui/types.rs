/// What the unified sidebar cursor is pointing at.
#[derive(PartialEq, Clone, Debug)]
pub enum SidebarItem {
    Service(usize),
    Package(usize),
    DbSchema(usize),
}

/// Which panel has keyboard focus — sidebar or the right-hand logs/detail pane.
#[derive(PartialEq, Clone, Debug)]
pub enum Focus {
    Sidebar,
    Logs,
}

#[derive(PartialEq)]
pub enum PopupAction {
    Eject,
    Uneject,
    Quit,
    /// Shown when the user presses 's' on an ejected service.
    ShellBlocked,
    /// Confirm mounting a dev container for a package.
    Mount,
    /// Confirm unmounting a dev container for a package.
    Unmount,
}

pub struct Popup {
    pub service_name: String,
    pub action:       PopupAction,
    /// 0 = Yes highlighted, 1 = No highlighted
    pub selected:     usize,
}