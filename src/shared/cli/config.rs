use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("code.toml")
}

pub fn branches_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ginger-society").join("branches")
}

pub fn branch_slug(branch: &str) -> String {
    branch.replace('/', "-")
}

pub fn branch_toml_path(branch: &str) -> PathBuf {
    branches_dir().join(format!("{}.toml", branch_slug(branch)))
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct CodeConfig {
    pub active_branch: Option<String>,
    pub active_env:    Option<String>,
    pub active_url:    Option<String>,
}

impl CodeConfig {
    pub fn load() -> Self {
        let path = config_path();
        if !path.exists() {
            return Self::default();
        }
        toml::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        std::fs::write(&path, toml::to_string_pretty(self).expect("toml"))
            .expect("write code.toml");
    }
}