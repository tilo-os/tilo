use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DesktopConfig {
    pub shell: ShellConfig,
    pub collaboration: CollaborationConfig,
    #[serde(default)]
    pub apps: Vec<AppConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ShellConfig {
    pub top_bar_color: String,
    pub dock_opacity: f32,
    pub corner_radius: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CollaborationConfig {
    pub enabled: bool,
    pub auto_share_new_windows: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub name: String,
    pub icon: String,
    pub command: String,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig {
                top_bar_color: String::from("#1e1e1e"),
                dock_opacity: 0.8,
                corner_radius: 12.0,
            },
            collaboration: CollaborationConfig {
                enabled: true,
                auto_share_new_windows: false,
            },
            apps: vec![
                AppConfig { name: "Terminal".into(), icon: "󰆍".into(), command: "quick-terminal".into() },
                AppConfig { name: "Browser".into(), icon: "󰈹".into(), command: "firefox".into() },
                AppConfig { name: "Files".into(), icon: "󰉋".into(), command: "file-explorer".into() },
            ],
        }
    }
}

pub fn load_config() -> DesktopConfig {
    let paths = [
        dirs_next::config_dir()
            .map(|d| d.join("quick-desktop").join("desktop.toml"))
            .unwrap_or_default(),
        std::path::PathBuf::from("/etc/quick-desktop/desktop.toml"),
        std::path::PathBuf::from("desktop.toml"),
    ];

    for path in &paths {
        if let Ok(content) = fs::read_to_string(path) {
            tracing::info!("Loaded config from {}", path.display());
            return toml::from_str(&content).unwrap_or_default();
        }
    }

    tracing::info!("No config found, using defaults");
    DesktopConfig::default()
}
