//! Whole-desktop display layouts: extend, mirror, only the built-in screen,
//! only external screens.
//!
//! These are the four answers to "where should my desktop appear?" that a
//! user expects from one keystroke. Each one is a pure function of the
//! compositor-published hardware snapshot and the committed layout, and
//! always yields a [`DisplayConfig`] that passes
//! [`DisplayConfig::validate_for`]. Applying one goes through the same
//! [`DisplayTransaction`] handshake as the Settings pane, so a layout that
//! blanks every screen restores itself when nobody confirms it.

use serde::{Deserialize, Serialize};

use crate::hardware::{
    DisplayConfig, DisplayConfigError, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplaySet,
    DisplayTransaction, OutputConfig, OutputMode, logical_extent,
};

/// Where the desktop appears across the connected displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayLayoutMode {
    /// Every display shows its own part of one larger desktop.
    Extend,
    /// Every display shows the same picture.
    Mirror,
    /// Only the built-in panel is on.
    BuiltinOnly,
    /// The built-in panel is off; external displays carry the desktop.
    ExternalOnly,
}

impl DisplayLayoutMode {
    /// Order in which the switcher lists and cycles the layouts.
    pub const ALL: [Self; 4] = [
        Self::Extend,
        Self::Mirror,
        Self::BuiltinOnly,
        Self::ExternalOnly,
    ];

    /// Stable identifier used in action ids and persisted state.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Extend => "extend",
            Self::Mirror => "mirror",
            Self::BuiltinOnly => "builtin-only",
            Self::ExternalOnly => "external-only",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == id)
    }

    /// Position in [`Self::ALL`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or_default()
    }
}

/// Why a layout cannot be planned for the attached hardware.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayLayoutError {
    /// Fewer than two displays are attached, so there is nothing to arrange.
    SingleDisplay,
    /// The layout needs a built-in panel and none is attached.
    NoBuiltin,
    /// No two displays share a resolution, so they cannot show one picture.
    NoCommonMirrorMode,
    /// The compositor has not published which displays are attached.
    NoHardwareSnapshot,
    /// The planned layout failed validation against the attached hardware.
    Invalid(DisplayConfigError),
}

impl std::fmt::Display for DisplayLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SingleDisplay => f.write_str("only one display is connected"),
            Self::NoBuiltin => f.write_str("this device has no built-in display"),
            Self::NoCommonMirrorMode => {
                f.write_str("the connected displays share no resolution to mirror")
            }
            Self::NoHardwareSnapshot => f.write_str("the compositor has not published displays"),
            Self::Invalid(error) => write!(f, "the layout is not supported: {error:?}"),
        }
    }
}

impl std::error::Error for DisplayLayoutError {}

/// Plan `mode` for the attached `outputs`, starting from `base` so every
/// per-display choice the layout does not decide (scale, rotation, HDR,
/// colour profile, the user's own arrangement) is kept.
///
/// # Errors
///
/// Returns why the layout is unavailable for this hardware; see
/// [`DisplayLayoutError`].
pub fn plan_layout(
    mode: DisplayLayoutMode,
    outputs: &[DisplayOutputSnapshot],
    base: &DisplayConfig,
) -> Result<DisplayConfig, DisplayLayoutError> {
    if outputs.len() < 2 {
        return Err(DisplayLayoutError::SingleDisplay);
    }
    let builtin = outputs.iter().find(|output| output.builtin);
    let mut next = base.clone();
    let mirrored: Vec<String> = mirror_participants(outputs, base);
    for output in outputs {
        let config = next
            .outputs
            .entry(output.descriptor.connector.clone())
            .or_insert_with(|| output.unconfigured());
        config.mirror_of = None;
        // A mirror pins every participant to a shared extent. Leaving the
        // mirror hands each display back to its own preferred timing.
        if mirrored.contains(&output.descriptor.connector) {
            config.mode = None;
        }
    }
    match mode {
        DisplayLayoutMode::Extend => {
            set_enabled(&mut next, outputs, |_| true);
            let primary = choose_primary(&next, outputs, |_| true);
            set_primary(&mut next, outputs, primary);
            arrange_if_overlapping(&mut next, outputs, primary);
        }
        DisplayLayoutMode::Mirror => {
            set_enabled(&mut next, outputs, |_| true);
            let target = builtin.map_or_else(
                || choose_primary(&next, outputs, |_| true),
                |builtin| builtin.descriptor.connector.as_str(),
            );
            let target = target.to_owned();
            set_primary(&mut next, outputs, &target);
            apply_mirror(&mut next, outputs, &target)?;
        }
        DisplayLayoutMode::BuiltinOnly => {
            let builtin = builtin.ok_or(DisplayLayoutError::NoBuiltin)?;
            let connector = builtin.descriptor.connector.as_str();
            set_enabled(&mut next, outputs, |output| {
                output.descriptor.connector == connector
            });
            set_primary(&mut next, outputs, connector);
        }
        DisplayLayoutMode::ExternalOnly => {
            if builtin.is_none() {
                return Err(DisplayLayoutError::NoBuiltin);
            }
            set_enabled(&mut next, outputs, |output| !output.builtin);
            let primary = choose_primary(&next, outputs, |output| !output.builtin);
            set_primary(&mut next, outputs, primary);
            arrange_if_overlapping(&mut next, outputs, primary);
        }
    }
    let descriptors: Vec<_> = outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    next.validate_for(&descriptors)
        .map_err(DisplayLayoutError::Invalid)?;
    Ok(next)
}

/// The layout `config` currently expresses for `outputs`, if it is one of
/// the four whole-desktop layouts.
pub fn active_layout(
    outputs: &[DisplayOutputSnapshot],
    config: &DisplayConfig,
) -> Option<DisplayLayoutMode> {
    if outputs.len() < 2 {
        return None;
    }
    let resolved = |output: &DisplayOutputSnapshot| {
        config
            .outputs
            .get(&output.descriptor.connector)
            .cloned()
            .unwrap_or_default()
    };
    let attached = |connector: &str| {
        outputs
            .iter()
            .any(|output| output.descriptor.connector == connector)
    };
    if outputs.iter().any(|output| {
        let config = resolved(output);
        config.enabled && config.mirror_of.as_deref().is_some_and(attached)
    }) {
        return Some(DisplayLayoutMode::Mirror);
    }
    let builtin_on = outputs
        .iter()
        .filter(|output| output.builtin)
        .map(|output| resolved(output).enabled)
        .next();
    let external_on = outputs
        .iter()
        .filter(|output| !output.builtin)
        .any(|output| resolved(output).enabled);
    let all_on = outputs.iter().all(|output| resolved(output).enabled);
    match (builtin_on, external_on) {
        _ if all_on => Some(DisplayLayoutMode::Extend),
        (Some(true), false) => Some(DisplayLayoutMode::BuiltinOnly),
        (Some(false), true) => Some(DisplayLayoutMode::ExternalOnly),
        _ => None,
    }
}

/// Whether `mode` can be planned for `outputs` right now.
pub fn layout_available(
    mode: DisplayLayoutMode,
    outputs: &[DisplayOutputSnapshot],
    base: &DisplayConfig,
) -> bool {
    plan_layout(mode, outputs, base).is_ok()
}

/// Plan `mode` against the live hardware snapshot and the committed layout,
/// then stage it as a [`DisplayTransaction`]. The compositor applies it at
/// once and restores the previous layout unless it is confirmed within
/// [`DisplayTransaction::CONFIRM_TIMEOUT_MS`].
///
/// # Errors
///
/// Returns an I/O error wrapping [`DisplayLayoutError`] when the layout is
/// unavailable, or the settings write error.
pub fn stage_layout(mode: DisplayLayoutMode) -> std::io::Result<DisplayTransaction> {
    let snapshot = DisplayHardwareSnapshot::load()
        .ok_or_else(|| std::io::Error::other(DisplayLayoutError::NoHardwareSnapshot))?;
    let set = attached_set(&snapshot.outputs);
    let candidate = plan_layout(mode, &snapshot.outputs, &DisplayConfig::load_for(&set))
        .map_err(std::io::Error::other)?;
    DisplayTransaction::request(set, candidate)
}

/// The monitor set `outputs` describe; layouts are remembered per set.
pub fn attached_set(outputs: &[DisplayOutputSnapshot]) -> DisplaySet {
    let descriptors: Vec<_> = outputs.iter().map(|o| o.descriptor.clone()).collect();
    DisplaySet::of_descriptors(&descriptors)
}

/// Extent of an output in the shared desktop plane, where
/// [`OutputConfig::x`]/[`OutputConfig::y`] live: the mode it will run over
/// its scale, turned by its rotation. Placing displays by mode size instead
/// leaves a gap beside a `HiDPI` panel that the pointer cannot cross.
fn desktop_extent(output: &DisplayOutputSnapshot, config: &OutputConfig) -> (i32, i32) {
    let mode = effective_mode(output, config);
    let (width, height) = logical_extent((mode.width, mode.height), config.scale, config.transform);
    (
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    )
}

fn mirror_participants(outputs: &[DisplayOutputSnapshot], base: &DisplayConfig) -> Vec<String> {
    let mut participants = Vec::new();
    for output in outputs {
        let connector = &output.descriptor.connector;
        if let Some(target) = base
            .outputs
            .get(connector)
            .and_then(|config| config.mirror_of.as_ref())
        {
            participants.push(connector.clone());
            participants.push(target.clone());
        }
    }
    participants
}

fn set_enabled(
    config: &mut DisplayConfig,
    outputs: &[DisplayOutputSnapshot],
    enabled: impl Fn(&DisplayOutputSnapshot) -> bool,
) {
    for output in outputs {
        if let Some(entry) = config.outputs.get_mut(&output.descriptor.connector) {
            entry.enabled = enabled(output);
        }
    }
}

/// Keep the user's primary when it stays on; otherwise prefer the built-in
/// panel, then the first eligible display.
fn choose_primary<'a>(
    config: &DisplayConfig,
    outputs: &'a [DisplayOutputSnapshot],
    eligible: impl Fn(&DisplayOutputSnapshot) -> bool,
) -> &'a str {
    let is_primary = |output: &&DisplayOutputSnapshot| {
        config
            .outputs
            .get(&output.descriptor.connector)
            .is_some_and(|entry| entry.primary)
    };
    outputs
        .iter()
        .filter(|output| eligible(output))
        .find(is_primary)
        .or_else(|| {
            outputs
                .iter()
                .filter(|output| eligible(output))
                .find(|output| output.builtin)
        })
        .or_else(|| outputs.iter().find(|output| eligible(output)))
        .map_or("", |output| output.descriptor.connector.as_str())
}

fn set_primary(config: &mut DisplayConfig, outputs: &[DisplayOutputSnapshot], primary: &str) {
    for output in outputs {
        if let Some(entry) = config.outputs.get_mut(&output.descriptor.connector) {
            entry.primary = output.descriptor.connector == primary;
        }
    }
}

/// The mode an output will run: its configured mode, else the native mode
/// the compositor picks when none is configured (the largest advertised),
/// else what it runs now.
fn effective_mode(output: &DisplayOutputSnapshot, config: &OutputConfig) -> OutputMode {
    config.mode.unwrap_or_else(|| {
        output
            .descriptor
            .modes
            .iter()
            .copied()
            .max_by_key(|mode| {
                (
                    u64::from(mode.width).saturating_mul(u64::from(mode.height)),
                    mode.refresh_mhz,
                )
            })
            .unwrap_or(output.current_mode)
    })
}

/// Rectangles of the enabled outputs in the shared plane.
fn enabled_rects(
    config: &DisplayConfig,
    outputs: &[DisplayOutputSnapshot],
) -> Vec<(String, (i32, i32, i32, i32))> {
    outputs
        .iter()
        .filter_map(|output| {
            let entry = config.outputs.get(&output.descriptor.connector)?;
            entry.enabled.then(|| {
                let (w, h) = desktop_extent(output, entry);
                (
                    output.descriptor.connector.clone(),
                    (
                        entry.x,
                        entry.y,
                        entry.x.saturating_add(w),
                        entry.y.saturating_add(h),
                    ),
                )
            })
        })
        .collect()
}

/// Keep the user's arrangement unless two enabled displays would cover the
/// same part of the desktop (every fresh config places all of them at the
/// origin). Then lay them out in a row, primary first.
fn arrange_if_overlapping(
    config: &mut DisplayConfig,
    outputs: &[DisplayOutputSnapshot],
    primary: &str,
) {
    let rects = enabled_rects(config, outputs);
    let overlapping = rects.iter().enumerate().any(|(index, (_, a))| {
        rects
            .iter()
            .skip(index.saturating_add(1))
            .any(|(_, b)| a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3)
    });
    if !overlapping {
        return;
    }
    let mut order: Vec<&DisplayOutputSnapshot> = outputs
        .iter()
        .filter(|output| {
            config
                .outputs
                .get(&output.descriptor.connector)
                .is_some_and(|entry| entry.enabled)
        })
        .collect();
    order.sort_by_key(|output| output.descriptor.connector != primary);
    let mut x = 0_i32;
    for output in order {
        let Some(entry) = config.outputs.get_mut(&output.descriptor.connector) else {
            continue;
        };
        let (width, _) = desktop_extent(output, entry);
        entry.x = x;
        entry.y = 0;
        x = x.saturating_add(width);
    }
}

/// Point every other display at `target` and pick the largest resolution
/// they all advertise, each at its own best refresh rate: the compositor
/// samples one viewport per sink, so timings may differ but extents may not.
/// A resolution larger than one a display is driving now is taken only when
/// nothing smaller is shared: sinks (virtual ones especially) advertise
/// modes they cannot scan out, and a mirror that picks one goes black.
fn apply_mirror(
    config: &mut DisplayConfig,
    outputs: &[DisplayOutputSnapshot],
    target: &str,
) -> Result<(), DisplayLayoutError> {
    let target_output = outputs
        .iter()
        .find(|output| output.descriptor.connector == target)
        .ok_or(DisplayLayoutError::NoCommonMirrorMode)?;
    let shared: Vec<(u32, u32)> = target_output
        .descriptor
        .modes
        .iter()
        .map(|mode| (mode.width, mode.height))
        .filter(|&(width, height)| {
            outputs.iter().all(|output| {
                output
                    .descriptor
                    .modes
                    .iter()
                    .any(|mode| mode.width == width && mode.height == height)
            })
        })
        .collect();
    let driven = |&(width, height): &(u32, u32)| {
        outputs.iter().all(|output| {
            width <= output.current_mode.width && height <= output.current_mode.height
        })
    };
    let largest = |extents: &mut dyn Iterator<Item = (u32, u32)>| {
        extents.max_by_key(|&(width, height)| {
            (u64::from(width).saturating_mul(u64::from(height)), width)
        })
    };
    let extent = largest(&mut shared.iter().copied().filter(driven))
        .or_else(|| largest(&mut shared.iter().copied()))
        .ok_or(DisplayLayoutError::NoCommonMirrorMode)?;
    for output in outputs {
        let best = output
            .descriptor
            .modes
            .iter()
            .filter(|mode| (mode.width, mode.height) == extent)
            .copied()
            .max_by_key(|mode| mode.refresh_mhz);
        if let Some(entry) = config.outputs.get_mut(&output.descriptor.connector) {
            entry.mode = best;
            if output.descriptor.connector != target {
                entry.mirror_of = Some(target.to_owned());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::hardware::{OutputDescriptor, OutputTransform};

    fn mode(width: u32, height: u32, refresh_mhz: u32) -> OutputMode {
        OutputMode {
            width,
            height,
            refresh_mhz,
        }
    }

    fn output(connector: &str, builtin: bool, modes: &[OutputMode]) -> DisplayOutputSnapshot {
        DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: modes.to_vec(),
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: modes[0],
            scale: 1.0,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        }
    }

    fn laptop_and_monitor() -> Vec<DisplayOutputSnapshot> {
        vec![
            output(
                "eDP-1",
                true,
                &[mode(2560, 1600, 60_000), mode(1920, 1080, 60_000)],
            ),
            output(
                "HDMI-A-1",
                false,
                &[mode(3840, 2160, 60_000), mode(1920, 1080, 59_940)],
            ),
        ]
    }

    #[test]
    fn extend_places_a_fresh_monitor_beside_the_builtin_panel() {
        let outputs = laptop_and_monitor();
        let plan = plan_layout(
            DisplayLayoutMode::Extend,
            &outputs,
            &DisplayConfig::default(),
        )
        .expect("two displays extend");
        let panel = &plan.outputs["eDP-1"];
        let monitor = &plan.outputs["HDMI-A-1"];
        assert!(panel.enabled && monitor.enabled);
        assert!(panel.primary && !monitor.primary);
        assert_eq!((panel.x, panel.y), (0, 0));
        assert_eq!((monitor.x, monitor.y), (2560, 0));
        assert_eq!(
            active_layout(&outputs, &plan),
            Some(DisplayLayoutMode::Extend)
        );
    }

    #[test]
    fn extend_keeps_a_non_overlapping_user_arrangement() {
        let outputs = laptop_and_monitor();
        let mut base = DisplayConfig::default();
        base.outputs.insert(
            "HDMI-A-1".into(),
            OutputConfig {
                x: 0,
                y: 0,
                primary: true,
                ..OutputConfig::default()
            },
        );
        base.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                x: 640,
                y: 2160,
                ..OutputConfig::default()
            },
        );
        let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &base).unwrap();
        assert_eq!(
            (plan.outputs["eDP-1"].x, plan.outputs["eDP-1"].y),
            (640, 2160)
        );
        assert!(plan.outputs["HDMI-A-1"].primary, "the user's primary stays");
    }

    #[test]
    fn extend_accounts_for_rotation() {
        let outputs = laptop_and_monitor();
        let mut base = DisplayConfig::default();
        base.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                transform: OutputTransform::Rotate90,
                ..OutputConfig::default()
            },
        );
        let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &base).unwrap();
        assert_eq!(plan.outputs["HDMI-A-1"].x, 1600);
    }

    /// The output whose logical rectangle holds the desktop point `(x, y)`.
    fn output_at(
        plan: &DisplayConfig,
        outputs: &[DisplayOutputSnapshot],
        x: i32,
        y: i32,
    ) -> Option<String> {
        enabled_rects(plan, outputs)
            .into_iter()
            .find(|(_, (left, top, right, bottom))| {
                (*left..*right).contains(&x) && (*top..*bottom).contains(&y)
            })
            .map(|(connector, _)| connector)
    }

    #[test]
    fn extend_places_mixed_dpi_displays_edge_to_edge_in_logical_size() {
        // A 2x laptop panel beside a 1x monitor: the panel is 1280 logical
        // pixels wide, so the monitor starts there, not at its 2560 pixels.
        let outputs = vec![
            output("eDP-1", true, &[mode(2560, 1600, 60_000)]),
            output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
        ];
        let mut base = DisplayConfig::default();
        base.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                scale: 2.0,
                ..OutputConfig::default()
            },
        );
        let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &base).unwrap();
        assert_eq!((plan.outputs["eDP-1"].x, plan.outputs["eDP-1"].y), (0, 0));
        assert_eq!(
            (plan.outputs["HDMI-A-1"].x, plan.outputs["HDMI-A-1"].y),
            (1280, 0),
            "no gap beside the 2x panel"
        );
        // The pointer crosses the shared edge: the last column is the
        // panel's, the next one the monitor's.
        assert_eq!(
            output_at(&plan, &outputs, 1279, 400).as_deref(),
            Some("eDP-1")
        );
        assert_eq!(
            output_at(&plan, &outputs, 1280, 400).as_deref(),
            Some("HDMI-A-1")
        );
    }

    #[test]
    fn extend_keeps_the_scale_the_compositor_chose_for_an_unconfigured_display() {
        // Nothing saved yet: the compositor runs the HiDPI panel at the 2x
        // it chose itself. Writing the panel's first entry must keep that
        // scale, not drop it to 1x, and place the monitor beside it.
        let mut panel = output("eDP-1", true, &[mode(2560, 1600, 60_000)]);
        panel.scale = 2.0;
        let outputs = vec![
            panel,
            output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
        ];
        let plan = plan_layout(
            DisplayLayoutMode::Extend,
            &outputs,
            &DisplayConfig::default(),
        )
        .unwrap();
        assert!((plan.outputs["eDP-1"].scale - 2.0).abs() < f64::EPSILON);
        assert!((plan.outputs["HDMI-A-1"].scale - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            (plan.outputs["HDMI-A-1"].x, plan.outputs["HDMI-A-1"].y),
            (1280, 0),
            "beside the panel's 1280 logical pixels"
        );
    }

    #[test]
    fn extend_keeps_an_arrangement_that_is_edge_to_edge_in_logical_size() {
        // A 1.5x 4K monitor is 2560 logical pixels wide, so at x = -2560 it
        // ends exactly where the 1x panel begins. Measured in mode pixels it
        // would seem to overlap the panel and be pushed aside.
        let outputs = vec![
            output("eDP-1", true, &[mode(1920, 1080, 60_000)]),
            output("DP-1", false, &[mode(3840, 2160, 60_000)]),
        ];
        let mut base = DisplayConfig::default();
        base.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                primary: true,
                ..OutputConfig::default()
            },
        );
        base.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                x: -2560,
                scale: 1.5,
                ..OutputConfig::default()
            },
        );
        let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &base).unwrap();
        assert_eq!(
            plan.outputs["DP-1"].x, -2560,
            "logically edge to edge, kept"
        );
        assert_eq!(output_at(&plan, &outputs, -1, 500).as_deref(), Some("DP-1"));
        assert_eq!(output_at(&plan, &outputs, 0, 500).as_deref(), Some("eDP-1"));
    }

    #[test]
    fn mirror_targets_the_builtin_panel_at_the_largest_shared_extent() {
        let outputs = laptop_and_monitor();
        let plan = plan_layout(
            DisplayLayoutMode::Mirror,
            &outputs,
            &DisplayConfig::default(),
        )
        .unwrap();
        let panel = &plan.outputs["eDP-1"];
        let monitor = &plan.outputs["HDMI-A-1"];
        assert_eq!(monitor.mirror_of.as_deref(), Some("eDP-1"));
        assert_eq!(panel.mirror_of, None);
        assert_eq!(panel.mode, Some(mode(1920, 1080, 60_000)));
        assert_eq!(monitor.mode, Some(mode(1920, 1080, 59_940)));
        assert_eq!(
            active_layout(&outputs, &plan),
            Some(DisplayLayoutMode::Mirror)
        );
    }

    #[test]
    fn mirror_never_picks_a_resolution_neither_display_is_driving() {
        // Two virtual outputs running 1024x768 that both advertise 4K.
        let mut outputs = vec![
            output(
                "Virtual-1",
                true,
                &[mode(4096, 2160, 60_000), mode(1024, 768, 60_000)],
            ),
            output(
                "Virtual-2",
                false,
                &[mode(4096, 2160, 60_000), mode(1024, 768, 60_000)],
            ),
        ];
        for output in &mut outputs {
            output.current_mode = mode(1024, 768, 60_000);
        }
        let plan = plan_layout(
            DisplayLayoutMode::Mirror,
            &outputs,
            &DisplayConfig::default(),
        )
        .unwrap();
        for connector in ["Virtual-1", "Virtual-2"] {
            assert_eq!(
                plan.outputs[connector].mode,
                Some(mode(1024, 768, 60_000)),
                "{connector}"
            );
        }
    }

    #[test]
    fn mirror_without_a_shared_resolution_is_unavailable() {
        let outputs = vec![
            output("eDP-1", true, &[mode(2560, 1600, 60_000)]),
            output("DP-1", false, &[mode(3840, 2160, 60_000)]),
        ];
        assert_eq!(
            plan_layout(
                DisplayLayoutMode::Mirror,
                &outputs,
                &DisplayConfig::default()
            ),
            Err(DisplayLayoutError::NoCommonMirrorMode)
        );
    }

    #[test]
    fn leaving_a_mirror_releases_the_pinned_modes() {
        let outputs = laptop_and_monitor();
        let mirrored = plan_layout(
            DisplayLayoutMode::Mirror,
            &outputs,
            &DisplayConfig::default(),
        )
        .unwrap();
        let extended = plan_layout(DisplayLayoutMode::Extend, &outputs, &mirrored).unwrap();
        assert_eq!(extended.outputs["eDP-1"].mode, None);
        assert_eq!(extended.outputs["HDMI-A-1"].mode, None);
        assert_eq!(extended.outputs["HDMI-A-1"].mirror_of, None);
        assert_eq!(extended.outputs["HDMI-A-1"].x, 2560);
    }

    #[test]
    fn single_screen_layouts_turn_the_other_side_off() {
        let outputs = laptop_and_monitor();
        let builtin = plan_layout(
            DisplayLayoutMode::BuiltinOnly,
            &outputs,
            &DisplayConfig::default(),
        )
        .unwrap();
        assert!(builtin.outputs["eDP-1"].enabled && builtin.outputs["eDP-1"].primary);
        assert!(!builtin.outputs["HDMI-A-1"].enabled);
        assert_eq!(
            active_layout(&outputs, &builtin),
            Some(DisplayLayoutMode::BuiltinOnly)
        );

        let external = plan_layout(DisplayLayoutMode::ExternalOnly, &outputs, &builtin).unwrap();
        assert!(!external.outputs["eDP-1"].enabled);
        assert!(external.outputs["HDMI-A-1"].enabled && external.outputs["HDMI-A-1"].primary);
        assert_eq!(
            active_layout(&outputs, &external),
            Some(DisplayLayoutMode::ExternalOnly)
        );
    }

    #[test]
    fn layouts_need_two_displays_and_single_screen_layouts_need_a_panel() {
        let one = vec![output("eDP-1", true, &[mode(1920, 1080, 60_000)])];
        for mode in DisplayLayoutMode::ALL {
            assert_eq!(
                plan_layout(mode, &one, &DisplayConfig::default()),
                Err(DisplayLayoutError::SingleDisplay)
            );
        }
        let desk = vec![
            output("DP-1", false, &[mode(1920, 1080, 60_000)]),
            output("DP-2", false, &[mode(1920, 1080, 60_000)]),
        ];
        assert!(layout_available(
            DisplayLayoutMode::Extend,
            &desk,
            &DisplayConfig::default()
        ));
        assert!(layout_available(
            DisplayLayoutMode::Mirror,
            &desk,
            &DisplayConfig::default()
        ));
        assert!(!layout_available(
            DisplayLayoutMode::BuiltinOnly,
            &desk,
            &DisplayConfig::default()
        ));
        assert!(!layout_available(
            DisplayLayoutMode::ExternalOnly,
            &desk,
            &DisplayConfig::default()
        ));
    }

    #[test]
    fn mode_ids_round_trip() {
        for mode in DisplayLayoutMode::ALL {
            assert_eq!(DisplayLayoutMode::from_id(mode.id()), Some(mode));
        }
    }
}
