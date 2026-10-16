//! The file explorer's per-device config directory (recent files, the
//! preview cache) and the item **zoom** steps the person's preferences
//! store ([`crate::resource::FilesPreferences`]).

use std::path::PathBuf;

/// Per-user config directory for the file explorer
/// (`$XDG_CONFIG_HOME/tilo-files`, i.e. `~/.config/tilo-files`). `None` when no
/// config dir can be resolved (headless/no-HOME environments) — callers then
/// fall back to in-memory defaults and simply don't persist.
pub fn config_dir() -> Option<PathBuf> {
    tilo_paths::AppDirs::new("tilo-files").config()
}

/// Smallest / largest zoom step. Level `0` is the default (scale `1.0`); each
/// step grows or shrinks item metrics by a fixed fraction.
pub const ZOOM_MIN: i32 = -2;
pub const ZOOM_MAX: i32 = 6;

/// Multiplier applied to row height / icon size / grid cell size for a given
/// zoom level. Level `0` → `1.0`; every step is +15 %.
pub fn zoom_scale(level: i32) -> f64 {
    let l = level.clamp(ZOOM_MIN, ZOOM_MAX);
    1.0 + 0.15 * f64::from(l)
}

/// Clamp a level into the supported range.
pub fn clamp_zoom(level: i32) -> i32 {
    level.clamp(ZOOM_MIN, ZOOM_MAX)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Zoom is 1x at level zero and grows with level.
    #[test]
    fn zoom_scale_grows_with_level() {
        // --- phase: zoom_scale_is_one_at_level_zero_and_grows_with_level ---
        {
            assert!((zoom_scale(0) - 1.0).abs() < 1e-9);
            assert!(zoom_scale(1) > zoom_scale(0));
            assert!(zoom_scale(-1) < zoom_scale(0));
            // Clamped at the extremes.
            assert_eq!(zoom_scale(999), zoom_scale(ZOOM_MAX));
            assert_eq!(zoom_scale(-999), zoom_scale(ZOOM_MIN));
        }
    }
}
