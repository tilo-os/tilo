use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DesktopConfig {
    pub shell: ShellConfig,
    pub collaboration: CollaborationConfig,
    /// Tunable knobs for the Flatpak-app share pipeline (video capture
    /// / encode / transport). Defaults hit the 720p60 ~8 Mbps sweet
    /// spot; heavier presenters can raise `max_bitrate_kbps` in the
    /// per-workspace collab profile.
    ///
    /// Absent from a legacy config file → falls through to
    /// [`FlatpakShareConfig::default`] via serde's `default` attribute.
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
                AppConfig { name: "Terminal".into(), icon: "󰆍".into(), command: "quick-terminal".into() },
                AppConfig { name: "Browser".into(), icon: "󰈹".into(), command: "browser".into() },
                AppConfig { name: "Files".into(), icon: "󰉋".into(), command: "file-explorer".into() },
            ],
        }
    }
}

/// Tunable knobs for the Flatpak-app share pipeline. Lives on
/// [`DesktopConfig`] under `[flatpak_share]`; per-workspace overrides
/// would layer on top (v1 has no override mechanism — every workspace
/// uses the same profile).
///
/// The encoder choice is declared here rather than probed at runtime so
/// a user with both VA-API and NVENC available can pin one — helpful
/// on hybrid laptops where `Auto` might guess the wrong device.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct FlatpakShareConfig {
    /// Cap on the video-stream bitrate (kilobits / sec). At 720p60 the
    /// v1 default lands ~5–8 Mbps of visually clean encode; users on
    /// slow uplinks should drop this to 3000 to keep motion-to-photon
    /// under 100 ms on WAN.
    pub max_bitrate_kbps: u32,
    /// Target frame rate for the owner-side capture. The
    /// [`crate::network::streaming::StreamingThrottle`] enforces this;
    /// v1 caps at 30 fps because the openh264 software encoder can't
    /// sustain 60 fps at 720p on a typical laptop. Values above the
    /// software cap silently clamp; hardware paths lift the ceiling.
    pub target_fps: u32,
    /// Which encoder to select. `Auto` runtime-detects — v1's detect is
    /// "always pick openh264" because hardware bindings are not in-tree.
    pub encoder: EncoderChoice,
    /// HDR pipeline configuration. `None` → SDR H.264 (default,
    /// widest compatibility). `Some(HdrConfig)` → HEVC Main10 with the
    /// configured transfer (PQ or HLG) + static metadata. HDR requires
    /// a hardware encoder that supports HEVC Main10 (`hevc_vaapi` on
    /// modern Intel/AMD; `hevc_nvenc` on NVIDIA); on hosts without one
    /// the session constructor returns
    /// [`crate::collab::flatpak_share::ShareError::EncoderUnavailable`]
    /// rather than silently downgrading — the UI must surface an HDR-
    /// unsupported error.
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

/// Which H.264 encoder implementation to build.
///
/// The `Vaapi` / `Nvenc` variants are *declared* in v1 so config files
/// can pin them without breaking on future upgrades, but they currently
/// resolve to the same openh264 software path — the bindings are not
/// yet in the workspace. Selecting one today returns
/// [`crate::collab::flatpak_share::ShareError::EncoderUnavailable`] from
/// the session constructor, letting UI surface a "hardware encoder not
/// available on this build" message rather than silently falling back.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// Runtime-detect. v1 → always openh264 (hardware bindings pending).
    Auto,
    /// Force openh264 software encode. Portable, no GPU required, 30 fps
    /// at 720p on a modern laptop.
    Openh264,
    /// VA-API hardware encode (Intel iGPU, AMD). Not linked in v1.
    Vaapi,
    /// NVENC hardware encode. Not linked in v1.
    Nvenc,
}

pub fn load_config() -> DesktopConfig {
    let paths = [
        dirs::config_dir()
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
