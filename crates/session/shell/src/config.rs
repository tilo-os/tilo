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
    /// Software encoders clamp this to 30 fps; hardware encoders do not.
    pub target_fps: u32,
    pub encoder: EncoderChoice,
    /// HDR uses AV1 or HEVC Main10 when a hardware 10-bit encoder is
    /// available and every viewer can decode it.
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

/// Preferred video encoder, as a preference for the media stack's element
/// selection (see `collab::encoder::EncoderChoice::policy`).
///
/// `"vaapi"` / `"nvenc"` prefer that hardware family, falling back to
/// software.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// Hardware first, then royalty-free software, then Cisco's openh264.
    Auto,
    /// Software encoders only.
    Software,
    /// Prefer VA-API; other hardware only after software.
    Vaapi,
    /// Prefer NVENC; other hardware only after software.
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
