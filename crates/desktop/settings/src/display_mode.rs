//! Whole-desktop display layouts: extend, mirror, only the built-in screen, only external screens.
//!
//! Each is a pure function of the compositor's hardware snapshot and the committed layout, and
//! always yields a [`DisplayConfig`] that passes [`DisplayConfig::validate_for`]. Applying one goes
//! through the same [`DisplayTransaction`] handshake as the Settings pane, so a layout that blanks
//! every screen restores itself when nobody confirms it.

use serde::{Deserialize, Serialize};

use crate::hardware::{
    DisplayConfig, DisplayConfigError, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplaySet,
    DisplayTransaction, OutputConfig, OutputMode, logical_extent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayLayoutMode {
    Extend,
    Mirror,
    BuiltinOnly,
    ExternalOnly,
}

impl DisplayLayoutMode {
    pub const ALL: [Self; 4] = [
        Self::Extend,
        Self::Mirror,
        Self::BuiltinOnly,
        Self::ExternalOnly,
    ];

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

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayLayoutError {
    SingleDisplay,
    NoBuiltin,
    NoCommonMirrorMode,
    NoHardwareSnapshot,
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
    let mirrored: Vec<String> = mirror_members(outputs, base);
    for output in outputs {
        let config = next
            .outputs
            .entry(output.descriptor.connector.clone())
            .or_insert_with(|| output.unconfigured());
        config.mirror_of = None;
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

pub fn stage_layout(mode: DisplayLayoutMode) -> std::io::Result<DisplayTransaction> {
    let snapshot = DisplayHardwareSnapshot::load()
        .ok_or_else(|| std::io::Error::other(DisplayLayoutError::NoHardwareSnapshot))?;
    let set = attached_set(&snapshot.outputs);
    let candidate = plan_layout(mode, &snapshot.outputs, &DisplayConfig::load_for(&set))
        .map_err(std::io::Error::other)?;
    DisplayTransaction::request(set, candidate)
}

pub fn attached_set(outputs: &[DisplayOutputSnapshot]) -> DisplaySet {
    let descriptors: Vec<_> = outputs.iter().map(|o| o.descriptor.clone()).collect();
    DisplaySet::of_descriptors(&descriptors)
}

fn desktop_extent(output: &DisplayOutputSnapshot, config: &OutputConfig) -> (i32, i32) {
    let mode = effective_mode(output, config);
    let (width, height) = logical_extent((mode.width, mode.height), config.scale, config.transform);
    (
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    )
}

fn mirror_members(outputs: &[DisplayOutputSnapshot], base: &DisplayConfig) -> Vec<String> {
    let mut members = Vec::new();
    for output in outputs {
        let connector = &output.descriptor.connector;
        if let Some(target) = base
            .outputs
            .get(connector)
            .and_then(|config| config.mirror_of.as_ref())
        {
            members.push(connector.clone());
            members.push(target.clone());
        }
    }
    members
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
    use crate::display_identify::{
        IDENTIFY_FRESH_MS, IDENTIFY_MS, IdentifyRequest, display_names, display_numbers,
    };
    use crate::display_scale::{
        MAX_SCALE, MIN_LOGICAL_WIDTH, MIN_SCALE, derive_scale, dpi_from_physical, resolve_scale,
        snap_scale,
    };
    use crate::display_tuning::{
        hz_label, plan_refresh, plan_scale, refresh_choices, scale_choices,
    };
    use crate::hardware::{
        AttachedOutput, DisplayConfigError, DisplayProfiles, DisplayTransaction,
        DisplayTransactionState, MonitorIdentity, MonitorLast, OutputDescriptor, OutputTransform,
    };

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

    fn descriptors(outputs: &[DisplayOutputSnapshot]) -> Vec<OutputDescriptor> {
        outputs.iter().map(|o| o.descriptor.clone()).collect()
    }

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

    fn attached<'a>(
        connector: &'a str,
        monitor: Option<&'a MonitorIdentity>,
    ) -> AttachedOutput<'a> {
        AttachedOutput {
            connector,
            connector_label: connector,
            monitor,
        }
    }

    fn at(x: i32) -> OutputConfig {
        OutputConfig {
            x,
            ..OutputConfig::default()
        }
    }

    fn monitor(make: &str, model: &str, serial: &str) -> MonitorIdentity {
        MonitorIdentity {
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            physical_size_mm: None,
        }
    }

    tilo_test_env::scenario! {
        #[allow(clippy::float_cmp, reason = "the expected scales are exact constants")]
        fn a_person_arranges_their_displays_and_the_arrangement_survives_a_replug;

        phase a_fresh_monitor_extends_beside_the_panel_and_a_valid_arrangement_is_kept;
        {
            let outputs = laptop_and_monitor();
            let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &DisplayConfig::default())
                .expect("two displays extend");
            assert!(plan.outputs["eDP-1"].enabled && plan.outputs["HDMI-A-1"].enabled);
            assert!(plan.outputs["eDP-1"].primary && !plan.outputs["HDMI-A-1"].primary);
            assert_eq!(
                (plan.outputs["eDP-1"].x, plan.outputs["eDP-1"].y),
                (0, 0)
            );
            assert_eq!(
                (
                    plan.outputs["HDMI-A-1"].x,
                    plan.outputs["HDMI-A-1"].y
                ),
                (2560, 0)
            );
            assert_eq!(
                active_layout(&outputs, &plan),
                Some(DisplayLayoutMode::Extend)
            );

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
            let kept = plan_layout(DisplayLayoutMode::Extend, &outputs, &base).unwrap();
            assert_eq!(
                (kept.outputs["eDP-1"].x, kept.outputs["eDP-1"].y),
                (640, 2160)
            );
            assert!(kept.outputs["HDMI-A-1"].primary, "the user's primary stays");
        }

        phase extend_accounts_for_rotation_dpi_and_the_scale_the_compositor_chose;
        {
            let outputs = laptop_and_monitor();
            let mut rotated = DisplayConfig::default();
            rotated.outputs.insert(
                "eDP-1".into(),
                OutputConfig {
                    transform: OutputTransform::Rotate90,
                    ..OutputConfig::default()
                },
            );
            let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &rotated).unwrap();
            assert_eq!(plan.outputs["HDMI-A-1"].x, 1600, "a rotated panel is 1600 wide");

            let mixed = vec![
                output("eDP-1", true, &[mode(2560, 1600, 60_000)]),
                output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
            ];
            let mut doubled = DisplayConfig::default();
            doubled
                .outputs
                .insert("eDP-1".into(), OutputConfig::at_scale(2.0));
            let plan = plan_layout(DisplayLayoutMode::Extend, &mixed, &doubled).unwrap();
            assert_eq!(
                (plan.outputs["HDMI-A-1"].x, plan.outputs["HDMI-A-1"].y),
                (1280, 0),
                "no gap beside the 2x panel"
            );
            assert_eq!(
                output_at(&plan, &mixed, 1279, 400).as_deref(),
                Some("eDP-1")
            );
            assert_eq!(
                output_at(&plan, &mixed, 1280, 400).as_deref(),
                Some("HDMI-A-1")
            );

            // Nothing saved yet: the compositor runs the HiDPI panel at 2x. The panel's first
            // entry keeps that scale and the monitor sits beside its 1280 logical pixels.
            let mut cold = output("eDP-1", true, &[mode(2560, 1600, 60_000)]);
            cold.scale = 2.0;
            let outputs = vec![
                cold,
                output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
            ];
            let plan = plan_layout(DisplayLayoutMode::Extend, &outputs, &DisplayConfig::default())
                .unwrap();
            assert!((plan.outputs["eDP-1"].scale - 2.0).abs() < f64::EPSILON);
            assert_eq!(
                (plan.outputs["HDMI-A-1"].x, plan.outputs["HDMI-A-1"].y),
                (1280, 0)
            );

        }

        phase mirror_shares_a_mode_both_displays_drive_and_refuses_when_none_is_shared;
        {
            let outputs = laptop_and_monitor();
            let plan = plan_layout(DisplayLayoutMode::Mirror, &outputs, &DisplayConfig::default())
                .unwrap();
            assert_eq!(plan.outputs["HDMI-A-1"].mirror_of.as_deref(), Some("eDP-1"));
            assert_eq!(plan.outputs["eDP-1"].mirror_of, None);
            assert_eq!(plan.outputs["eDP-1"].mode, Some(mode(1920, 1080, 60_000)));
            assert_eq!(plan.outputs["HDMI-A-1"].mode, Some(mode(1920, 1080, 59_940)));
            assert_eq!(
                active_layout(&outputs, &plan),
                Some(DisplayLayoutMode::Mirror)
            );

            let mut twins = vec![
                output("Virtual-1", true, &[mode(4096, 2160, 60_000), mode(1024, 768, 60_000)]),
                output("Virtual-2", false, &[mode(4096, 2160, 60_000), mode(1024, 768, 60_000)]),
            ];
            for output in &mut twins {
                output.current_mode = mode(1024, 768, 60_000);
            }
            let plan = plan_layout(DisplayLayoutMode::Mirror, &twins, &DisplayConfig::default())
                .unwrap();
            for connector in ["Virtual-1", "Virtual-2"] {
                assert_eq!(
                    plan.outputs[connector].mode,
                    Some(mode(1024, 768, 60_000)),
                    "{connector} mirrors in a mode it drives"
                );
            }

            let nothing_shared = vec![
                output("eDP-1", true, &[mode(2560, 1600, 60_000)]),
                output("DP-1", false, &[mode(3840, 2160, 60_000)]),
            ];
            assert_eq!(
                plan_layout(
                    DisplayLayoutMode::Mirror,
                    &nothing_shared,
                    &DisplayConfig::default()
                ),
                Err(DisplayLayoutError::NoCommonMirrorMode)
            );

            let extended = plan_layout(DisplayLayoutMode::Extend, &outputs, &plan).unwrap();
            assert_eq!(extended.outputs["eDP-1"].mode, None);
            assert_eq!(extended.outputs["HDMI-A-1"].mode, None);
            assert_eq!(extended.outputs["HDMI-A-1"].mirror_of, None);
            assert_eq!(extended.outputs["HDMI-A-1"].x, 2560);
        }

        phase single_screen_layouts_turn_the_other_side_off_and_need_two_displays;
        {
            let outputs = laptop_and_monitor();
            let builtin = plan_layout(
                DisplayLayoutMode::BuiltinOnly,
                &outputs,
                &DisplayConfig::default(),
            )
            .unwrap();
            assert!(builtin.outputs["eDP-1"].enabled && builtin.outputs["eDP-1"].primary);
            assert!(!builtin.outputs["HDMI-A-1"].enabled);
            let external =
                plan_layout(DisplayLayoutMode::ExternalOnly, &outputs, &builtin).unwrap();
            assert!(!external.outputs["eDP-1"].enabled);
            assert!(external.outputs["HDMI-A-1"].enabled && external.outputs["HDMI-A-1"].primary);

            let one = vec![output("eDP-1", true, &[mode(1920, 1080, 60_000)])];
            for plan in DisplayLayoutMode::ALL {
                assert_eq!(
                    plan_layout(plan, &one, &DisplayConfig::default()),
                    Err(DisplayLayoutError::SingleDisplay)
                );
            }
        }

        phase only_a_change_that_can_blank_a_screen_needs_confirming_and_unsupported_ones_are_refused;
        {
            let mut before = DisplayConfig::default();
            before.outputs.insert(
                "eDP-1".into(),
                OutputConfig {
                    mode: Some(mode(2560, 1600, 60_000)),
                    ..OutputConfig::default()
                },
            );
            let changed = |edit: &dyn Fn(&mut OutputConfig)| {
                let mut next = before.clone();
                if let Some(output) = next.outputs.get_mut("eDP-1") {
                    edit(output);
                }
                next.needs_confirmation_from(&before)
            };
            let check = |edit: &dyn Fn(&mut OutputConfig), blanking: bool, why: &str| {
                assert_eq!(changed(edit), blanking, "{why}");
            };
            check(&|o| o.scale = 1.5, false, "scale needs no confirmation");
            check(&|o| o.transform = OutputTransform::Rotate90, false, "rotation needs no confirmation");
            check(&|o| o.enabled = false, true, "turning a display off needs confirmation");
            check(&|o| o.mode = None, true, "another mode needs confirmation");
            check(&|o| o.hdr = true, true, "hdr needs confirmation");
            check(&|o| o.vrr = !o.vrr, true, "vrr needs confirmation");
            check(&|o| o.mirror_of = Some("DP-1".into()), true, "mirroring needs confirmation");
            let mut added = before.clone();
            added
                .outputs
                .insert("DP-1".into(), OutputConfig::default());
            assert!(added.needs_confirmation_from(&before), "a display turned on");
            assert!(before.needs_confirmation_from(&added), "a display gone");
            assert_eq!(DisplayTransaction::CONFIRM_TIMEOUT_MS, 15_000);

            let mut unsupported = DisplayConfig::default();
            unsupported.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mode: Some(mode(3840, 2160, 60_000)),
                    ..Default::default()
                },
            );
            let attached = [output("DP-1", false, &[mode(1920, 1080, 60_000)])];
            assert!(matches!(
                unsupported.validate_for(&descriptors(&attached)),
                Err(DisplayConfigError::UnsupportedMode { .. })
            ));

            let first = OutputDescriptor {
                connector: "pci:0000:01:00.0/HDMI-A-1".into(),
                connector_label: "HDMI-A-1".into(),
                modes: vec![],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            };
            let second = OutputDescriptor {
                connector: "pci:0000:03:00.0/HDMI-A-1".into(),
                ..first.clone()
            };
            let mut config = DisplayConfig::default();
            config.outputs.insert(
                "HDMI-A-1".into(),
                OutputConfig {
                    enabled: false,
                    ..Default::default()
                },
            );
            assert!(config.output_for(&first.connector).is_none());
            assert!(config.output_for(&second.connector).is_none());
            assert_eq!(first.label(), "HDMI-A-1");
            assert_eq!(first.card_qualifier(), Some("pci:0000:01:00.0"));

            let mut cycle = DisplayConfig::default();
            cycle.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mirror_of: Some("DP-2".into()),
                    ..Default::default()
                },
            );
            cycle.outputs.insert(
                "DP-2".into(),
                OutputConfig {
                    mirror_of: Some("DP-1".into()),
                    ..Default::default()
                },
            );
            let pair = [
                output("DP-1", false, &[mode(1920, 1080, 60_000)]),
                output("DP-2", false, &[mode(1920, 1080, 60_000)]),
            ];
            assert!(matches!(
                cycle.validate_for(&descriptors(&pair)),
                Err(DisplayConfigError::MirrorCycle { .. })
            ));

            let mut left = output("DP-1", false, &[mode(1920, 1080, 60_000)]);
            left.descriptor.modes.push(mode(1280, 720, 60_000));
            let right = output("DP-2", false, &[mode(1920, 1080, 60_000)]);
            let mut mismatched = DisplayConfig::default();
            mismatched.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mode: Some(left.descriptor.modes[1]),
                    mirror_of: Some("DP-2".into()),
                    ..Default::default()
                },
            );
            mismatched.outputs.insert(
                "DP-2".into(),
                OutputConfig {
                    mode: Some(right.descriptor.modes[0]),
                    ..Default::default()
                },
            );
            assert!(matches!(
                mismatched.validate_for(&descriptors(&[left.clone(), right])),
                Err(DisplayConfigError::IncompatibleMirrorModes { .. })
            ));

            let named = |make: &str, model: &str| {
                MonitorIdentity {
                    make: make.into(),
                    model: model.into(),
                    ..MonitorIdentity::default()
                }
                .display_name()
            };
            assert_eq!(named("Dell", "DELL U2720Q"), "Dell U2720Q");
            assert_eq!(named("LG", "27GL850"), "LG 27GL850");
            assert_eq!(named("", "Q27G2"), "Q27G2");
            assert_eq!(named("BOE", ""), "BOE");

            let mut transaction = DisplayTransaction {
                state: DisplayTransactionState::AwaitingConfirmation,
                deadline_unix_ms: Some(25_000),
                ..Default::default()
            };
            assert!(!transaction.expired(24_999));
            assert!(transaction.expired(25_000));
            transaction.state = DisplayTransactionState::Confirmed;
            assert!(!transaction.expired(u64::MAX));
            assert_eq!(
                logical_extent((2560, 1600), 2.0, OutputTransform::Normal),
                (1280, 800)
            );
            assert_eq!(
                logical_extent((1920, 1200), 1.25, OutputTransform::Rotate90),
                (960, 1536)
            );
            assert_eq!(
                logical_extent((2560, 1440), 1.5, OutputTransform::Rotate180),
                (1707, 960)
            );
            assert_eq!(
                logical_extent((1920, 1080), f64::NAN, OutputTransform::Normal),
                (1920, 1080)
            );
        }

        phase scale_and_refresh_are_chosen_per_display_and_refuse_what_is_not_offered;
        {
            let outputs = [
                output("HDMI-A-1", false, &[mode(2560, 1600, 60_000)]),
                output(
                    "eDP-1",
                    true,
                    &[mode(2560, 1600, 60_000), mode(2560, 1600, 120_000)],
                ),
            ];
            let base = DisplayConfig::default();
            assert_eq!(scale_choices().first().copied(), Some(MIN_SCALE));
            assert_eq!(scale_choices().last().copied(), Some(MAX_SCALE));
            assert_eq!(hz_label(59_940), "59.94");
            assert_eq!(hz_label(120_000), "120");
            assert_eq!(refresh_choices(&outputs[1]), vec![60_000, 120_000]);

            let planned = plan_scale(&outputs, &base, None, 2.0).unwrap();
            assert!((planned.outputs["eDP-1"].scale - 2.0).abs() < f64::EPSILON);
            assert!(!planned.outputs.contains_key("HDMI-A-1"));
            let named = plan_scale(&outputs, &base, Some("HDMI-A-1"), 1.25).unwrap();
            assert!((named.outputs["HDMI-A-1"].scale - 1.25).abs() < f64::EPSILON);
            assert!(plan_scale(&outputs, &base, None, 1.3).is_err());
            assert!(plan_scale(&outputs, &base, Some("DP-9"), 1.5).is_err());

            let fast = plan_refresh(&outputs, &base, None, 120.0).unwrap();
            assert_eq!(fast.outputs["eDP-1"].mode.unwrap().refresh_mhz, 120_000);
            assert!(plan_refresh(&outputs, &base, Some("HDMI-A-1"), 120.0).is_err());
            assert!(plan_refresh(&outputs, &base, None, 30.0).is_err());
        }

        phase the_panels_scale_follows_its_dpi_and_the_chosen_one_wins;
        {
            let laptop = ((1920, 1080), (294, 165));
            let four_k = ((3840, 2160), (597, 336));
            let laptop_4k = ((3840, 2160), (309, 174));
            assert_eq!(dpi_from_physical((1920, 1080), (0, 0)), None);
            assert_eq!(dpi_from_physical((0, 0), (300, 200)), None);
            assert_eq!(
                derive_scale(laptop.0, Some(laptop.1)),
                1.5,
                "a 13.3in 1080p panel needs 150%"
            );
            assert_eq!(
                derive_scale(laptop_4k.0, Some(laptop_4k.1)),
                2.0,
                "a 14in 4K panel is clamped to the 200% the control offers"
            );
            assert_eq!(derive_scale((1920, 1080), None), 1.0);
            assert_eq!(derive_scale((1920, 1080), Some((0, 0))), 1.0);
            let tiny_dense = ((1600, 1200), (120u32, 90u32));
            let scale = derive_scale(tiny_dense.0, Some(tiny_dense.1));
            assert!(
                (f64::from(tiny_dense.0.0) / scale) >= MIN_LOGICAL_WIDTH,
                "scale {scale} leaves too small a logical desktop"
            );
            assert_eq!(snap_scale(0.4), 1.0, "below 1.0 clamps up");
            assert_eq!(snap_scale(1.2), 1.25);
            assert_eq!(snap_scale(1.9), 2.0);
            assert_eq!(snap_scale(4.0), 2.0, "above the ceiling clamps down");
            assert_eq!(resolve_scale(four_k.0, Some(four_k.1), Some(1.25)), 1.25);
            assert_eq!(resolve_scale(four_k.0, Some(four_k.1), None), 1.5);
        }

        phase identify_numbers_names_and_lasts_three_seconds;
        {
            let outputs = [
                output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
                output("DP-2", false, &[mode(1920, 1080, 60_000)]),
                output("eDP-1", true, &[mode(1920, 1080, 60_000)]),
            ];
            assert_eq!(display_numbers(&outputs), vec![3, 2, 1]);

            let monitor_named = |connector: &str, model: &str| {
                let mut output = output(connector, false, &[mode(1920, 1080, 60_000)]);
                output.descriptor.monitor = Some(monitor("Dell", model, ""));
                output
            };
            let named = [
                output("eDP-1", true, &[mode(1920, 1080, 60_000)]),
                monitor_named("DP-1", "DELL U2724DE"),
                output("HDMI-A-1", false, &[mode(1920, 1080, 60_000)]),
            ];
            assert_eq!(
                display_names(&named),
                ["Built-in display", "Dell U2724DE", "External display"]
            );
            let request = IdentifyRequest {
                requested_unix_ms: 10_000,
            };
            assert_eq!(request.remaining_ms(9_999), None);
            assert_eq!(request.remaining_ms(10_000), Some(IDENTIFY_MS));
            assert_eq!(request.remaining_ms(12_500), Some(500));
            assert_eq!(request.remaining_ms(13_000), None);
            assert!(request.is_fresh(13_000), "a slow write is still answered");
            assert!(
                !request.is_fresh(10_001 + IDENTIFY_FRESH_MS),
                "nor replayed after a restart"
            );
        }

        phase a_kept_layout_restores_on_new_ports_and_remembers_what_each_monitor_showed;
        {
            let panel = monitor("BOE", "NE135FBM-N41", "");
            let left = monitor("Dell", "DELL U2720Q", "AAA");
            let right = monitor("Dell", "DELL U2720Q", "BBB");
            let saved = DisplaySet::of([
                attached("eDP-1", Some(&panel)),
                attached("DP-5", Some(&left)),
                attached("DP-6", Some(&right)),
            ]);
            let mut config = DisplayConfig::default();
            config.outputs.insert("eDP-1".into(), at(2560));
            config.outputs.insert("DP-5".into(), at(0));
            config.outputs.insert(
                "DP-6".into(),
                OutputConfig {
                    mirror_of: Some("DP-5".into()),
                    ..at(0)
                },
            );
            config.outputs.insert("HDMI-A-1".into(), at(9999));
            let mut profiles = DisplayProfiles::default();
            profiles.remember(&saved, &config);

            let replugged = DisplaySet::of([
                attached("eDP-1", Some(&panel)),
                attached("DP-7", Some(&right)),
                attached("DP-8", Some(&left)),
            ]);
            let restored = profiles.layout_for(&replugged).expect("office layout");
            assert_eq!(restored.outputs["eDP-1"].x, 2560);
            assert_eq!(restored.outputs["DP-8"].x, 0);
            assert_eq!(restored.outputs["DP-7"].mirror_of.as_deref(), Some("DP-8"));
            assert_eq!(restored.outputs.len(), 3, "a stray entry never follows");

            // The key names the monitors, not the ports they use; another monitor on the same
            // port is another set.
            let docked_on_dp2 = DisplaySet::of([
                attached("eDP-1", Some(&panel)),
                attached("DP-2", Some(&left)),
            ]);
            let docked_on_dp5 = DisplaySet::of([
                attached("DP-5", Some(&left)),
                attached("eDP-1", Some(&panel)),
            ]);
            assert_eq!(docked_on_dp2.key, docked_on_dp5.key);

            // A monitor keeps its scale in a set of monitors never arranged.
            let mut config = DisplayConfig::default();
            config.outputs.insert(
                "eDP-1".into(),
                OutputConfig {
                    scale: 1.25,
                    transform: OutputTransform::Rotate90,
                    ..OutputConfig::default()
                },
            );
            let mut profiles = DisplayProfiles::default();
            profiles.remember(&docked_on_dp2, &config);
            let panel_at_125 = MonitorLast {
                scale: 1.25,
                transform: OutputTransform::Rotate90,
            };
            let undocked = DisplaySet::of([attached("eDP-1", Some(&panel))]);
            assert_eq!(profiles.layout_for(&undocked), None, "never arranged");
            assert_eq!(
                profiles.last_shown_in(&undocked),
                BTreeMap::from([("eDP-1".to_owned(), panel_at_125)])
            );
        }
    }
}
