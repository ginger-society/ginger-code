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
    pub ssh_host:        Option<String>,
    // Container selector state
    /// All containers in the running pod. Empty until first poll completes.
    pub containers:           Vec<String>,
    /// None = no override
    /// Some(name) = explicit --container flag passed to kubectl logs / exec.
    pub selected_container:   Option<String>,
    pub ejected_container:    Option<String>,
    pub transitioning: bool,
}

impl K8sService {
    /// The name of the container considered "ejected" (dev-mode) for this
    /// service.
    ///
    /// `ejected_container` is currently never populated from k8s (eject
    /// stores `ginger-main-container` as a Deployment annotation, which we
    /// don't read back into this struct), so we fall back to
    /// `deployment_name` — the same fallback `eject::resolve_main_container`
    /// uses when the `x-ginger-code-ejectable` annotation is absent.
    pub fn ejected_container_name(&self) -> Option<&str> {
        self.ejected_container
            .as_deref()
            .or(self.deployment_name.as_deref())
    }
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


#[derive(Clone, Debug)]
pub struct DbSchema {
    pub id:              i64,
    pub name:            String,
    pub identifier:      Option<String>,
    pub db_type:         Option<String>,
    pub organization_id: String,
    pub tables:          Vec<String>,
    pub description:     Option<String>,
    pub version:         Option<String>,
    pub pipeline_status: Option<String>,
    pub updated_at:      String,
    // k8s enrichment
    pub k8s_name:        Option<String>,
    pub k8s_status:      String,
    pub k8s_ready:       String,
}