//! The file explorer's per-device config directory and its zoom levels.

use std::path::PathBuf;

pub fn config_dir() -> Option<PathBuf> {
    tilo_paths::AppDirs::new("tilo-files").config()
}

pub const ZOOM_MIN: i32 = -2;
pub const ZOOM_MAX: i32 = 6;

pub fn zoom_scale(level: i32) -> f64 {
    let l = level.clamp(ZOOM_MIN, ZOOM_MAX);
    1.0 + 0.15 * f64::from(l)
}

pub fn clamp_zoom(level: i32) -> i32 {
    level.clamp(ZOOM_MIN, ZOOM_MAX)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    #[test]
    fn zoom_scale_grows_with_level() {
        {
            assert!((zoom_scale(0) - 1.0).abs() < 1e-9);
            assert!(zoom_scale(1) > zoom_scale(0));
            assert!(zoom_scale(-1) < zoom_scale(0));
            assert_eq!(zoom_scale(999), zoom_scale(ZOOM_MAX));
            assert_eq!(zoom_scale(-999), zoom_scale(ZOOM_MIN));
        }
    }
}
