//! Pure-logic helpers for the DRM backend: filtering DRM device paths and
//! picking a preferred mode out of a list of advertised modes.
//!
//! Hardware-touching code stays in `super::runtime`; this module exists so
//! the device-selection policy is testable on a dev machine with no seat /
//! TTY / GPU it can claim. Policy: pick the highest-resolution mode at the
//! highest refresh rate, breaking ties by preferring the connector's
//! preferred-flagged mode; `None` when no modes are available, so the
//! caller can log + skip the connector rather than panic.
//!
//! Render-only nodes (`/dev/dri/renderD*`) are excluded — they cannot do
//! KMS / page-flips, and the backend needs `cardN` nodes specifically.
//!
//! Tests below are pure-CPU; they don't touch /dev/dri.

use std::path::{Path, PathBuf};

/// Holds the subset of `drm::control::Mode` fields needed to compare two
/// modes. Mirrors the upstream layout but stays plain-data so tests can
/// build instances without a real DRM device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub width: u16,
    pub height: u16,
    /// Vertical refresh in millihertz (60Hz = 60000). DRM exposes this
    /// as `vrefresh` in plain Hz, but milli-Hz keeps tie-break logic
    /// honest for 59.94 vs 60.00.
    pub refresh_mhz: u32,
    /// Set when the connector marks this mode as its preferred one
    /// (DRM flag `DRM_MODE_TYPE_PREFERRED`). Used as a tie-breaker.
    pub preferred: bool,
}

/// Filter `/dev/dri/*` candidate paths down to KMS-capable `cardN` nodes.
/// `renderD*` nodes are render-only — using one as the primary GPU would
/// deadlock at the first `set_crtc`. Filtered by file-name prefix rather
/// than a second udev syscall; callers already get `card*`-only paths from
/// `all_gpus`, but this costs nothing as defence-in-depth.
pub fn filter_kms_nodes<I, P>(paths: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    paths
        .into_iter()
        .map(Into::into)
        .filter(|p| is_kms_node(p))
        .collect()
}

/// True iff `path`'s file name looks like a KMS card node
/// (`/dev/dri/cardN`, where `N` is a digit). Render nodes
/// (`/dev/dri/renderD128`) and arbitrary other entries return false.
pub fn is_kms_node(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // `strip_prefix` avoids a manual byte-index slice (which clippy can't
    // prove stays on a UTF-8 char boundary, even though "card" is ASCII).
    let Some(digits) = name.strip_prefix("card") else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Pick the "best" mode from a connector's mode list: prefer modes flagged
/// as the connector's preferred, then highest resolution, then highest
/// refresh rate. `None` only when `modes` is empty; ties are stable (first
/// candidate in iteration order wins).
pub fn preferred_mode(modes: &[ModeInfo]) -> Option<ModeInfo> {
    if modes.is_empty() {
        return None;
    }
    let preferred_set: Vec<ModeInfo> = modes.iter().copied().filter(|m| m.preferred).collect();
    let working: &[ModeInfo] = if preferred_set.is_empty() {
        modes
    } else {
        &preferred_set
    };
    working.iter().copied().max_by_key(|m| {
        // `width`/`height` are `u16`, so the widened product tops out around
        // 65535*65535 (~4.3e9), far under `u64::MAX` — this can never overflow.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "u16 * u16 widened to u64 cannot overflow"
        )]
        let area = u64::from(m.width) * u64::from(m.height);
        (area, m.refresh_mhz)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn card0_is_kms_node() {
        assert!(is_kms_node(Path::new("/dev/dri/card0")));
    }

    #[test]
    fn renderd128_is_not_kms_node() {
        assert!(!is_kms_node(Path::new("/dev/dri/renderD128")));
    }

    #[test]
    fn random_path_is_not_kms_node() {
        assert!(!is_kms_node(Path::new("/tmp/cardN")));
        assert!(!is_kms_node(Path::new("/dev/dri/card")));
        assert!(!is_kms_node(Path::new("/dev/dri/cardX")));
    }

    #[test]
    fn filter_drops_render_nodes() {
        let paths: Vec<PathBuf> = vec![
            "/dev/dri/card0".into(),
            "/dev/dri/renderD128".into(),
            "/dev/dri/card1".into(),
        ];
        let kept = filter_kms_nodes(paths);
        assert_eq!(
            kept,
            vec![
                PathBuf::from("/dev/dri/card0"),
                PathBuf::from("/dev/dri/card1")
            ]
        );
    }

    #[test]
    fn preferred_mode_returns_none_for_empty_list() {
        assert!(preferred_mode(&[]).is_none());
    }

    #[test]
    fn preferred_mode_picks_only_mode() {
        let m = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: false,
        };
        assert_eq!(preferred_mode(&[m]), Some(m));
    }

    #[test]
    fn preferred_mode_prefers_higher_resolution() {
        let small = ModeInfo {
            width: 800,
            height: 600,
            refresh_mhz: 60_000,
            preferred: false,
        };
        let big = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: false,
        };
        assert_eq!(preferred_mode(&[small, big]), Some(big));
        // Order should not matter.
        assert_eq!(preferred_mode(&[big, small]), Some(big));
    }

    #[test]
    fn preferred_mode_prefers_higher_refresh_at_same_resolution() {
        let sixty = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: false,
        };
        let one_twenty = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 120_000,
            preferred: false,
        };
        assert_eq!(preferred_mode(&[sixty, one_twenty]), Some(one_twenty));
    }

    #[test]
    fn preferred_flag_wins_over_higher_resolution() {
        // 1080p is preferred even though 4K exists — honor the connector's hint.
        let four_k = ModeInfo {
            width: 3840,
            height: 2160,
            refresh_mhz: 60_000,
            preferred: false,
        };
        let preferred_1080p = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: true,
        };
        assert_eq!(
            preferred_mode(&[four_k, preferred_1080p]),
            Some(preferred_1080p)
        );
    }

    #[test]
    fn preferred_mode_among_preferred_picks_highest_resolution() {
        // Two preferred modes: pick the higher-resolution one (rare, but allowed by spec).
        let a = ModeInfo {
            width: 1280,
            height: 720,
            refresh_mhz: 60_000,
            preferred: true,
        };
        let b = ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: true,
        };
        assert_eq!(preferred_mode(&[a, b]), Some(b));
    }
}
