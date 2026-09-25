use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DesktopConfig {
    pub shell: ShellConfig,
    pub collaboration: CollaborationConfig,
    #[serde(default)]
    pub flatpak_share: FlatpakShareConfig,
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
            flatpak_share: FlatpakShareConfig::default(),
            apps: vec![
                AppConfig {
                    name: "Terminal".into(),
                    icon: "󰆍".into(),
                    command: "quick-terminal".into(),
                },
                AppConfig {
                    name: "Browser".into(),
                    icon: "󰈹".into(),
                    command: "firefox".into(),
                },
                AppConfig {
                    name: "Files".into(),
                    icon: "󰉋".into(),
                    command: "file-explorer".into(),
                },
            ],
        }
    }
}

/// Video settings for shared application windows.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct FlatpakShareConfig {
    /// Video bitrate cap in kbps.
    pub max_bitrate_kbps: u32,
    /// `OpenH264` sessions clamp this to 30 fps; hardware encoders do not.
    pub target_fps: u32,
    pub encoder: EncoderChoice,
    /// HDR uses HEVC Main10 when a compatible hardware encoder is available.
    #[serde(default)]
    pub hdr: Option<crate::collab::hdr_metadata::HdrConfig>,
}

impl Default for FlatpakShareConfig {
    fn default() -> Self {
        Self {
            max_bitrate_kbps: 8000,
            target_fps: 60,
            encoder: EncoderChoice::Auto,
            hdr: None,
        }
    }
}

/// Preferred video encoder backend.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// Select the best available backend at runtime.
    Auto,
    /// Force `OpenH264` software encoding.
    Openh264,
    /// Prefer VA-API, falling back to `OpenH264` for SDR if unavailable.
    Vaapi,
    /// Prefer NVENC, falling back to `OpenH264` for SDR if unavailable.
    Nvenc,
}

pub fn load_config() -> DesktopConfig {
    let paths = [
        xdg_paths::config_dir()
            .map(|d| d.join("tilo-desktop").join("desktop.toml"))
            .unwrap_or_default(),
        std::path::PathBuf::from("/etc/tilo-desktop/desktop.toml"),
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
