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
/// Every node is opened and driven; the order only decides whose outputs
/// are adopted (and numbered) first and which card the backend reports as
/// its primary. `udev::all_gpus` is not a primary-display ordering: on hybrid
/// systems it commonly returns a connectorless discrete GPU before the
/// integrated GPU that owns the internal panel.
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
    /// (DRM flag `DRM_MODE_TYPE_PREFERRED`). Its size is the native
    /// resolution; its rate is not the fastest the panel offers.
    pub preferred: bool,
    /// DRM flag `DRM_MODE_FLAG_INTERLACE`: the rate counts fields.
    pub interlaced: bool,
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

/// The connector's default mode: [`select_mode`] with no configured mode.
/// `None` only when `modes` is empty.
#[cfg(test)]
fn preferred_mode(modes: &[ModeInfo]) -> Option<ModeInfo> {
    select_mode(modes, None).and_then(|index| modes.get(index).copied())
}

/// A connector flags a mode narrower than this PREFERRED only as a virtual
/// GPU's placeholder (virtio-gpu flags 640x480 while advertising up to
/// 5120x2160), never as a real panel's native resolution.
const MIN_TRUSTED_PREFERRED_WIDTH: u16 = 1024;

/// Largest mode taken when the PREFERRED flag sits on such a placeholder: a
/// window-sized virtual display, not the host's largest advertised mode.
const PLACEHOLDER_FALLBACK_MAX: (u16, u16) = (1920, 1080);

/// The index in `modes` to drive the connector at.
///
/// A configured mode (the Settings pane or the `display.refresh-rate`
/// action) is honoured when the connector still advertises it; a zero
/// `refresh_mhz` there names only the resolution. Otherwise the connector
/// runs at its native resolution (the PREFERRED mode's size) and the
/// fastest progressive rate the connector offers at that size. The kernel
/// flags one mode PREFERRED, usually the panel's 60 Hz timing, while the
/// same resolution at 90 or 120 Hz is listed beside it unflagged, so taking
/// the flagged mode itself would leave a fast panel at 60 Hz.
///
/// `None` only when `modes` is empty.
pub fn select_mode(
    modes: &[ModeInfo],
    requested: Option<settings_store::hardware::OutputMode>,
) -> Option<usize> {
    if let Some(wanted) = requested {
        let size = (wanted.width, wanted.height);
        let found = if wanted.refresh_mhz == 0 {
            fastest_at(modes, size)
        } else {
            modes
                .iter()
                .position(|m| size_of(m) == size && m.refresh_mhz == wanted.refresh_mhz)
        };
        if found.is_some() {
            return found;
        }
        tracing::warn!(
            ?wanted,
            "configured DRM mode disappeared; retaining a safe advertised mode"
        );
    }
    fastest_at(modes, native_size(modes)?)
}

fn size_of(mode: &ModeInfo) -> (u32, u32) {
    (u32::from(mode.width), u32::from(mode.height))
}

fn area(mode: &ModeInfo) -> u32 {
    let (width, height) = size_of(mode);
    width.saturating_mul(height)
}

/// The resolution the connector is built for: the largest PREFERRED mode's
/// size, or the largest size when nothing is flagged.
fn native_size(modes: &[ModeInfo]) -> Option<(u32, u32)> {
    let preferred = modes.iter().filter(|m| m.preferred).max_by_key(|m| area(m));
    let native = match preferred {
        Some(mode) if mode.width >= MIN_TRUSTED_PREFERRED_WIDTH => Some(mode),
        Some(placeholder) => modes
            .iter()
            .filter(|m| {
                m.width <= PLACEHOLDER_FALLBACK_MAX.0 && m.height <= PLACEHOLDER_FALLBACK_MAX.1
            })
            .max_by_key(|m| area(m))
            .or(Some(placeholder)),
        None => modes.iter().max_by_key(|m| area(m)),
    };
    native.map(size_of)
}

/// The fastest mode of `size`: progressive before interlaced (an interlaced
/// mode's rate counts fields, not frames), then the highest refresh, then
/// the kernel's order.
fn fastest_at(modes: &[ModeInfo], size: (u32, u32)) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| size_of(m) == size)
        .max_by_key(|&(index, m)| (!m.interlaced, m.refresh_mhz, std::cmp::Reverse(index)))
        .map(|(index, _)| index)
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
        assert!(!is_kms_node(Path::new("/tmp/cardN")));
        assert!(!is_kms_node(Path::new("/dev/dri/card")));
        assert!(!is_kms_node(Path::new("/dev/dri/cardX")));
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
        assert!(preferred_mode(&[]).is_none());
        {
            let m = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: false,
                interlaced: false,
            };
            assert_eq!(preferred_mode(&[m]), Some(m));
        }
        {
            let small = ModeInfo {
                width: 800,
                height: 600,
                refresh_mhz: 60_000,
                preferred: false,
                interlaced: false,
            };
            let big = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: false,
                interlaced: false,
            };
            assert_eq!(preferred_mode(&[small, big]), Some(big));
            // Order should not matter.
            assert_eq!(preferred_mode(&[big, small]), Some(big));
        }
        {
            let sixty = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: false,
                interlaced: false,
            };
            let one_twenty = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 120_000,
                preferred: false,
                interlaced: false,
            };
            assert_eq!(preferred_mode(&[sixty, one_twenty]), Some(one_twenty));
        }
        {
            // 1080p is preferred even though 4K exists — honor the connector's hint.
            let four_k = ModeInfo {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
                preferred: false,
                interlaced: false,
            };
            let preferred_1080p = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: true,
                interlaced: false,
            };
            assert_eq!(
                preferred_mode(&[four_k, preferred_1080p]),
                Some(preferred_1080p)
            );
        }
        {
            // Two preferred modes: pick the higher-resolution one (rare, but allowed by spec).
            let a = ModeInfo {
                width: 1280,
                height: 720,
                refresh_mhz: 60_000,
                preferred: true,
                interlaced: false,
            };
            let b = ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: true,
                interlaced: false,
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
        // The `preferred` flag is honored: a preferred mode in the list is
        // the one picked.
        {
            use super::preferred_mode;
            let modes = [
                ModeInfo {
                    width: 3840,
                    height: 2160,
                    refresh_mhz: 60_000,
                    preferred: false,
                    interlaced: false,
                },
                ModeInfo {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                    preferred: true,
                    interlaced: false,
                },
                ModeInfo {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 144_000,
                    preferred: false,
                    interlaced: false,
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
        // And when none of the modes are flagged preferred, the policy falls back to the highest resolution × refresh.
        {
            use super::preferred_mode;
            let modes = [
                ModeInfo {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                    preferred: false,
                    interlaced: false,
                },
                ModeInfo {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 60_000,
                    preferred: false,
                    interlaced: false,
                },
            ];
            let picked = preferred_mode(&modes).expect("non-empty");
            assert_eq!(picked.width, 2560);
        }
    }
}

#[cfg(test)]
mod refresh_selection_tests {
    use super::{ModeInfo, preferred_mode, select_mode};
    use settings_store::hardware::OutputMode;

    fn mode(width: u16, height: u16, hz_milli: u32, preferred: bool) -> ModeInfo {
        ModeInfo {
            width,
            height,
            refresh_mhz: hz_milli,
            preferred,
            interlaced: false,
        }
    }

    /// A 2880x1800 laptop panel as its kernel lists it: the PREFERRED mode
    /// is the 60 Hz timing, 90 Hz sits beside it unflagged, and a scaled
    /// lower resolution offers an even higher rate.
    fn fast_panel() -> Vec<ModeInfo> {
        vec![
            mode(2880, 1800, 60_000, true),
            mode(2880, 1800, 90_000, false),
            mode(2880, 1800, 48_000, false),
            mode(1920, 1200, 120_000, false),
            mode(1280, 800, 144_000, false),
        ]
    }

    #[test]
    fn a_ninety_hertz_panel_runs_at_ninety_hertz_at_its_native_resolution() {
        let modes = fast_panel();
        assert_eq!(select_mode(&modes, None), Some(1));
        let picked = preferred_mode(&modes).expect("modes");
        assert_eq!((picked.width, picked.height), (2880, 1800));
        assert_eq!(
            picked.refresh_mhz, 90_000,
            "a faster rate at another resolution must not win"
        );
    }

    #[test]
    fn interlaced_rates_count_fields_and_lose_to_progressive_modes() {
        let mut interlaced = mode(1920, 1080, 120_000, false);
        interlaced.interlaced = true;
        let modes = [mode(1920, 1080, 60_000, true), interlaced];
        assert_eq!(select_mode(&modes, None), Some(0));
        // A connector offering only interlaced modes still lights up.
        assert_eq!(select_mode(&[interlaced], None), Some(0));
    }

    #[test]
    fn exact_rates_break_near_ties_and_equal_modes_keep_kernel_order() {
        let modes = [
            mode(1920, 1080, 59_940, true),
            mode(1920, 1080, 60_000, false),
            mode(1920, 1080, 60_000, false),
        ];
        assert_eq!(select_mode(&modes, None), Some(1));
    }

    #[test]
    fn a_virtual_gpu_placeholder_preference_falls_back_to_a_window_sized_mode() {
        let modes = [
            mode(640, 480, 60_000, true),
            mode(5120, 2160, 60_000, false),
            mode(1920, 1080, 60_000, false),
            mode(1920, 1080, 75_000, false),
        ];
        assert_eq!(select_mode(&modes, None), Some(3));
        assert_eq!(select_mode(&[mode(640, 480, 60_000, true)], None), Some(0));
    }

    #[test]
    fn unflagged_lists_use_the_largest_resolution_at_its_fastest_rate() {
        let modes = [
            mode(1920, 1080, 144_000, false),
            mode(2560, 1440, 60_000, false),
            mode(2560, 1440, 120_000, false),
        ];
        assert_eq!(select_mode(&modes, None), Some(2));
        assert_eq!(select_mode(&[], None), None);
    }

    #[test]
    fn a_configured_refresh_rate_is_honoured_over_the_fastest_default() {
        let modes = fast_panel();
        let sixty = OutputMode {
            width: 2880,
            height: 1800,
            refresh_mhz: 60_000,
        };
        assert_eq!(select_mode(&modes, Some(sixty)), Some(0));
        let any_rate = OutputMode {
            width: 1920,
            height: 1200,
            refresh_mhz: 0,
        };
        assert_eq!(select_mode(&modes, Some(any_rate)), Some(3));
        // A configured mode the connector no longer offers falls back to the default.
        let gone = OutputMode {
            width: 2880,
            height: 1800,
            refresh_mhz: 165_000,
        };
        assert_eq!(select_mode(&modes, Some(gone)), Some(1));
    }
}
