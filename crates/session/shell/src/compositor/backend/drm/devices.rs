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

/// Order KMS nodes by the connectors they can actually drive.
///
/// `udev::all_gpus` is not a primary-display ordering. On hybrid systems it
/// commonly returns a connectorless discrete GPU before the integrated GPU
/// that owns the internal panel. Opening only that first node leaves the
/// compositor running headless while the display manager's last framebuffer
/// remains on screen.
///
/// Prefer the node with the greatest number of connected connectors, using a
/// connected internal panel (`eDP`, `LVDS`, or `DSI`) as the tie-breaker. That
/// keeps a dock with multiple live outputs ahead of a still-connected laptop
/// panel. Remaining ties keep udev's order, and an unreadable sysfs tree keeps
/// the complete input order as a safe fallback. This only orders scanout
/// candidates; render-node selection remains independent.
pub fn order_by_connected_connectors<I, P>(paths: I, drm_sysfs: &Path) -> Vec<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    let mut scored: Vec<(PathBuf, (usize, usize))> = paths
        .into_iter()
        .map(Into::into)
        .map(|path| {
            let score = connector_score(&path, drm_sysfs);
            (path, score)
        })
        .collect();
    // `sort_by_key` is stable, so equal scores preserve udev's deterministic
    // ordering. Reverse each component instead of reversing the whole vector,
    // which would also reverse ties.
    scored.sort_by_key(|(_, (connected, internal))| {
        (std::cmp::Reverse(*connected), std::cmp::Reverse(*internal))
    });
    scored.into_iter().map(|(path, _)| path).collect()
}

fn connector_score(card_path: &Path, drm_sysfs: &Path) -> (usize, usize) {
    let Some(card_name) = card_path.file_name().and_then(|name| name.to_str()) else {
        return (0, 0);
    };
    let prefix = format!("{card_name}-");
    let Ok(entries) = std::fs::read_dir(drm_sysfs) else {
        return (0, 0);
    };
    let mut internal = 0usize;
    let mut connected = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(connector_name) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }
        connected = connected.saturating_add(1);
        let connector_name = connector_name.to_ascii_lowercase();
        if connector_name.starts_with("edp-")
            || connector_name.starts_with("lvds-")
            || connector_name.starts_with("dsi-")
        {
            internal = internal.saturating_add(1);
        }
    }
    (connected, internal)
}

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
    fn kms_node_detection_accepts_only_primary_cards() {
        // Phase `card0_is_kms_node`.
        assert!(is_kms_node(Path::new("/dev/dri/card0")));
        // Phase `renderd128_is_not_kms_node`.
        assert!(!is_kms_node(Path::new("/dev/dri/renderD128")));
        // Phase `random_path_is_not_kms_node`.
        assert!(!is_kms_node(Path::new("/tmp/cardN")));
        assert!(!is_kms_node(Path::new("/dev/dri/card")));
        assert!(!is_kms_node(Path::new("/dev/dri/cardX")));
        // Phase `filter_drops_render_nodes`.
        {
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
    }

    #[test]
    fn gpu_ranking_prefers_connected_outputs_then_the_internal_panel() {
        // Phase `connectorless_discrete_gpu_does_not_win_by_card_number`.
        {
            let sysfs = tempfile::tempdir().expect("temporary sysfs");
            std::fs::create_dir(sysfs.path().join("card0-DP-1")).expect("card0 connector");
            std::fs::write(sysfs.path().join("card0-DP-1/status"), "disconnected\n")
                .expect("card0 status");
            std::fs::create_dir(sysfs.path().join("card1-eDP-1")).expect("card1 connector");
            std::fs::write(sysfs.path().join("card1-eDP-1/status"), "connected\n")
                .expect("card1 status");

            let ordered = order_by_connected_connectors(
                [
                    PathBuf::from("/dev/dri/card0"),
                    PathBuf::from("/dev/dri/card1"),
                ],
                sysfs.path(),
            );
            assert_eq!(
                ordered,
                [
                    PathBuf::from("/dev/dri/card1"),
                    PathBuf::from("/dev/dri/card0")
                ]
            );
        }
        // Phase `more_connected_outputs_win_before_the_internal_panel_tiebreaker`.
        {
            let sysfs = tempfile::tempdir().expect("temporary sysfs");
            for connector in ["card0-DP-1", "card0-HDMI-A-1", "card1-eDP-1"] {
                std::fs::create_dir(sysfs.path().join(connector)).expect("connector");
                std::fs::write(sysfs.path().join(connector).join("status"), "connected\n")
                    .expect("status");
            }

            let ordered = order_by_connected_connectors(
                [
                    PathBuf::from("/dev/dri/card0"),
                    PathBuf::from("/dev/dri/card1"),
                ],
                sysfs.path(),
            );
            assert_eq!(ordered.first(), Some(&PathBuf::from("/dev/dri/card0")));
        }
        // Phase `internal_panel_breaks_a_connected_output_count_tie`.
        {
            let sysfs = tempfile::tempdir().expect("temporary sysfs");
            for connector in ["card0-DP-1", "card1-eDP-1"] {
                std::fs::create_dir(sysfs.path().join(connector)).expect("connector");
                std::fs::write(sysfs.path().join(connector).join("status"), "connected\n")
                    .expect("status");
            }

            let ordered = order_by_connected_connectors(
                [
                    PathBuf::from("/dev/dri/card0"),
                    PathBuf::from("/dev/dri/card1"),
                ],
                sysfs.path(),
            );
            assert_eq!(ordered.first(), Some(&PathBuf::from("/dev/dri/card1")));
        }
        // Phase `missing_sysfs_preserves_udev_order`.
        {
            let original = [
                PathBuf::from("/dev/dri/card2"),
                PathBuf::from("/dev/dri/card0"),
            ];
            let ordered =
                order_by_connected_connectors(&original, Path::new("/definitely/missing"));
            assert_eq!(ordered, original);
        }
    }

    #[test]
    fn preferred_mode_ranks_preferred_flag_then_resolution_then_refresh() {
        // Phase `preferred_mode_returns_none_for_empty_list`.
        assert!(preferred_mode(&[]).is_none());
        // Phase `preferred_mode_picks_only_mode`.
        {
            let m = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: false,
            };
            assert_eq!(preferred_mode(&[m]), Some(m));
        }
        // Phase `preferred_mode_prefers_higher_resolution`.
        {
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
        // Phase `preferred_mode_prefers_higher_refresh_at_same_resolution`.
        {
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
        // Phase `preferred_flag_wins_over_higher_resolution`.
        {
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
        // Phase `preferred_mode_among_preferred_picks_highest_resolution`.
        {
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
}

#[cfg(test)]
mod runtime_mode_policy_tests {
    use super::ModeInfo;
    #[test]
    fn preferred_mode_selection_picks_first_preferred_or_falls_back_to_highest() {
        // Phase `preferred_mode_selection_picks_first_preferred`.
        // Pure-logic mirror of the policy `enumerate_connectors_on_device`
        // implements when projecting DRM modes into `ModeInfo`s: the
        // `preferred` flag is honored. The full flow needs a real DRM device,
        // so we verify the *policy* end-to-end via `super::devices::preferred_mode`
        // instead — pinning the contract that a preferred mode in the list is the one picked.
        {
            use super::preferred_mode;
            let modes = [
                ModeInfo {
                    width: 3840,
                    height: 2160,
                    refresh_mhz: 60_000,
                    preferred: false,
                },
                ModeInfo {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                    preferred: true,
                },
                ModeInfo {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 144_000,
                    preferred: false,
                },
            ];
            let picked = preferred_mode(&modes).expect("non-empty modes");
            assert!(
                picked.preferred,
                "preferred flag must win over higher resolution"
            );
            assert_eq!(picked.width, 1920);
            assert_eq!(picked.height, 1080);
        }
        // Phase `preferred_mode_selection_falls_back_to_highest_when_no_preferred`.
        // And when none of the modes are flagged preferred, the policy falls back to the highest resolution × refresh, matching `enumerate_connectors_on_device`.
        {
            use super::preferred_mode;
            let modes = [
                ModeInfo {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                    preferred: false,
                },
                ModeInfo {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 60_000,
                    preferred: false,
                },
            ];
            let picked = preferred_mode(&modes).expect("non-empty");
            assert_eq!(picked.width, 2560);
        }
    }
}
