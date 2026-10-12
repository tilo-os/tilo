use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The image's `desktop.toml`, below the person's own.
const SYSTEM_CONFIG: &str = "/etc/tilo/desktop.toml";

/// The compositor's `desktop.toml`. Every table is optional; tables it does
/// not know are ignored.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct DesktopConfig {
    pub flatpak_share: FlatpakShareConfig,
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
/// `"vaapi"` / `"nvenc"` / `"v4l2"` prefer that hardware family, falling back
/// to software.
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
    /// Prefer the kernel's V4L2 codec blocks (ARM boards); other hardware only
    /// after software.
    V4l2,
}

/// Read `$XDG_CONFIG_HOME/tilo-desktop/desktop.toml`, else the image's
/// [`SYSTEM_CONFIG`], else the defaults. A file that does not parse is
/// reported and skipped rather than quietly read as the defaults.
pub fn load_config() -> DesktopConfig {
    let user = tilo_paths::config_dir().map(|dir| dir.join("tilo-desktop").join("desktop.toml"));
    load_first(
        user.iter()
            .map(PathBuf::as_path)
            .chain([Path::new(SYSTEM_CONFIG)]),
    )
}

fn load_first<'a>(paths: impl IntoIterator<Item = &'a Path>) -> DesktopConfig {
    for path in paths {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        match toml::from_str(&content) {
            Ok(config) => {
                tracing::info!(path = %path.display(), "loaded desktop config");
                return config;
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "desktop config does not parse; skipping it");
            }
        }
    }
    tracing::info!("no desktop config, using defaults");
    DesktopConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_parsing_config_wins_and_a_broken_one_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.toml");
        let system = dir.path().join("system.toml");
        std::fs::write(&broken, "[flatpak_share\n").unwrap();
        std::fs::write(
            &system,
            "[shell]\ncorner_radius = 12.0\n\n[flatpak_share]\ntarget_fps = 30\n",
        )
        .unwrap();
        let missing = dir.path().join("missing.toml");
        let config = load_first([missing.as_path(), broken.as_path(), system.as_path()]);
        assert_eq!(config.flatpak_share.target_fps, 30);
        assert_eq!(
            config.flatpak_share.max_bitrate_kbps,
            FlatpakShareConfig::default().max_bitrate_kbps
        );
        assert_eq!(
            load_first([missing.as_path()]).flatpak_share,
            FlatpakShareConfig::default()
        );
    }
}
