use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tilo_window_share::config::FlatpakShareConfig;

/// The image's `desktop.toml`, below the person's own.
const SYSTEM_CONFIG: &str = "/etc/tilo/desktop.toml";

/// The compositor's `desktop.toml`. Every table is optional; tables it does
/// not know are ignored.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct DesktopConfig {
    pub flatpak_share: FlatpakShareConfig,
}

/// Read `$XDG_CONFIG_HOME/tilo-desktop/desktop.toml`, else the image's
/// `SYSTEM_CONFIG`, else the defaults. A file that does not parse is
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
