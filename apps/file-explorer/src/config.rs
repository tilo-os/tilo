//! Small on-disk state the file explorer persists between runs: the current
//! icon/row **zoom level** and (via [`crate::favorites`]) the user's
//! favourites list. Both live under a single per-user config directory so
//! the app "remembers" how you left it.
//!
//! Everything here is a plain free function over an explicit directory so the
//! persistence is trivially testable (a test points it at a `tempdir`) and
//! never has to touch the real `~/.config` during `cargo test`.

use std::path::{Path, PathBuf};

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

/// Read the persisted zoom level from `dir/zoom`, defaulting to `0` when the
/// file is missing or unparseable.
pub fn load_zoom(dir: &Path) -> i32 {
    std::fs::read_to_string(dir.join("zoom"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map_or(0, clamp_zoom)
}

/// Write the zoom level to `dir/zoom` (creating `dir` if needed). Errors are
/// swallowed — a persistence failure must never break the UI.
pub fn save_zoom(dir: &Path, level: i32) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("zoom"), clamp_zoom(level).to_string());
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Zoom is 1x at level zero, grows with level and round-trips through disk.
    #[test]
    fn zoom_scale_grows_with_level_and_round_trips_through_disk() {
        // --- phase: zoom_scale_is_one_at_level_zero_and_grows_with_level ---
        {
            assert!((zoom_scale(0) - 1.0).abs() < 1e-9);
            assert!(zoom_scale(1) > zoom_scale(0));
            assert!(zoom_scale(-1) < zoom_scale(0));
            // Clamped at the extremes.
            assert_eq!(zoom_scale(999), zoom_scale(ZOOM_MAX));
            assert_eq!(zoom_scale(-999), zoom_scale(ZOOM_MIN));
        }

        // --- phase: zoom_round_trips_through_disk ---
        {
            let d = tempfile::tempdir().unwrap();
            // Missing file → default.
            assert_eq!(load_zoom(d.path()), 0);
            save_zoom(d.path(), 3);
            assert_eq!(load_zoom(d.path()), 3, "saved level is read back");
            // Out-of-range saves are clamped.
            save_zoom(d.path(), 99);
            assert_eq!(load_zoom(d.path()), ZOOM_MAX);
        }
    }
}
