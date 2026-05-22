/// Represents one service fetched from metadata, enriched with live k8s state.
#[derive(Clone, Debug)]
pub struct K8sService {
    /// e.g. "@ginger-society/dev-portal"
    pub meta_name: String,
    /// e.g. "ginger-society"
    pub organization_id: String,
    /// k8s deployment name if matched, e.g. "dev-portal"
    pub deployment_name: Option<String>,
    /// "Running" / "Pending" / "Degraded" / "Not deployed" / "Unknown"
    pub status: String,
    /// Number of ready pods, e.g. "1/1"
    pub ready: String,
    /// Language from metadata e.g. "Rust" / "TS"
    pub lang: Option<String>,
    /// Whether this deployment is currently ejected into builder mode
    pub ejected: bool,
}

#[derive(PartialEq, Clone)]
pub enum Focus {
    Services,
    Logs,
}

#[derive(PartialEq)]
pub enum PopupAction {
    Eject,
    Uneject,
    Quit,
    /// Shown when the user presses 's' on an ejected service
    ShellBlocked,
}

pub struct Popup {
    pub service_name: String,
    pub action: PopupAction,
    /// 0 = Yes highlighted, 1 = No highlighted
    pub selected: usize,
}