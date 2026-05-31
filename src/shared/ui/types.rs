/// Represents one service fetched from metadata, enriched with live k8s state.
#[derive(Clone, Debug)]
pub struct K8sService {
    /// e.g. "@ginger-society/dev-portal"
    pub meta_name:       String,
    /// e.g. "ginger-society"
    pub organization_id: String,
    /// k8s deployment name if matched, e.g. "dev-portal"
    pub deployment_name: Option<String>,
    /// "Running" / "Pending" / "Degraded" / "Not deployed" / "Unknown"
    pub status:          String,
    /// Number of ready pods, e.g. "1/1"
    pub ready:           String,
    /// Language from metadata e.g. "Rust" / "TS"
    pub lang:            Option<String>,
    /// Whether this deployment is currently ejected into builder mode
    pub ejected:         bool,
}

/// A package from the metadata service — not deployed on k8s.
#[derive(Clone, Debug)]
pub struct Package {
    pub identifier:      String,
    pub package_type:    String,
    pub lang:            String,
    pub description:     String,
    /// Used by mount/unmount ops — not displayed.
    pub organization_id: String,
    /// True when a dev container has been successfully mounted.
    pub mounted:         bool,
    /// Dependency identifiers shown in the detail panel.
    pub dependencies:    Vec<String>,
}

/// What the unified sidebar cursor is pointing at.
#[derive(PartialEq, Clone, Debug)]
pub enum SidebarItem {
    Service(usize),
    Package(usize),
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