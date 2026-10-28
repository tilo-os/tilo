//! `tilo-vmd display-apply`: give a headful slot's monitors the modes and
//! scales `--outputs` asked for, the way the Settings app does.
//!
//! It runs as the slot's user, reads the monitors the slot's compositor
//! published (`$XDG_RUNTIME_DIR/tilo/display-state.json`), stages the layout
//! as a display transaction in the user's settings, waits for the
//! compositor to apply it, and confirms it, after which the compositor
//! remembers it for that set of monitors. Nothing here talks to the
//! compositor directly, so the display path under test is the production
//! one.

use std::time::{Duration, Instant};

use settings_store::display_mode::attached_set;
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
    DisplayTransactionState, OutputConfig, OutputMode, OutputTransform, logical_extent,
};
use tilo_vm_protocol::OutputSpec;
use tilo_vm_protocol::display::format_outputs;

/// Refresh rate vkms modes run at; the closest one wins among equal sizes.
const PREFERRED_REFRESH_MHZ: u32 = 60_000;

/// The layout for `outputs` on `specs`: the n-th connector (in connector
/// order, as vkms numbers them) gets the n-th spec; they sit left to right,
/// top-aligned, edge to edge in logical pixels, the first one primary.
///
/// # Errors
/// A sentence saying which output cannot be arranged as asked.
pub fn plan(
    outputs: &[DisplayOutputSnapshot],
    specs: &[OutputSpec],
) -> Result<DisplayConfig, String> {
    if outputs.len() != specs.len() {
        return Err(format!(
            "the compositor shows {} output(s), --outputs {} asks for {}",
            outputs.len(),
            format_outputs(specs),
            specs.len()
        ));
    }
    let mut ordered: Vec<&DisplayOutputSnapshot> = outputs.iter().collect();
    ordered.sort_by_key(|o| connector_order(o.descriptor.label()));
    let mut config = DisplayConfig::default();
    let mut x: i32 = 0;
    for (index, (output, spec)) in ordered.iter().zip(specs).enumerate() {
        let mode = pick_mode(&output.descriptor.modes, spec).ok_or_else(|| {
            format!(
                "{} offers no {}x{} mode; it has {}",
                output.descriptor.label(),
                spec.width,
                spec.height,
                sizes(&output.descriptor.modes)
            )
        })?;
        config.outputs.insert(
            output.descriptor.connector.clone(),
            OutputConfig {
                enabled: true,
                primary: index == 0,
                x,
                y: 0,
                scale: spec.scale,
                transform: OutputTransform::Normal,
                mode: Some(mode),
                ..OutputConfig::default()
            },
        );
        let (width, _) = logical_extent(
            (mode.width, mode.height),
            spec.scale,
            OutputTransform::Normal,
        );
        x = x.saturating_add(i32::try_from(width).unwrap_or(i32::MAX));
    }
    let descriptors: Vec<_> = outputs.iter().map(|o| o.descriptor.clone()).collect();
    config
        .validate_for(&descriptors)
        .map_err(|e| format!("the display settings refuse this layout: {e:?}"))?;
    Ok(config)
}

/// The combined logical desktop `specs` make when laid out by [`plan`].
/// An absolute pointer spans exactly this, so `tilo-vm input move X Y`
/// lands on desktop pixel X, Y.
pub fn desktop_extent(specs: &[OutputSpec]) -> (u32, u32) {
    specs.iter().fold((0, 0), |(width, height), spec| {
        let (w, h) = logical_extent(
            (spec.width, spec.height),
            spec.scale,
            OutputTransform::Normal,
        );
        (width.saturating_add(w), height.max(h))
    })
}

/// `Virtual-2` sorts before `Virtual-10`.
fn connector_order(label: &str) -> (String, u64) {
    let name = label.trim_end_matches(|c: char| c.is_ascii_digit());
    let number = label.strip_prefix(name).unwrap_or_default();
    (name.to_owned(), number.parse().unwrap_or(0))
}

fn pick_mode(modes: &[OutputMode], spec: &OutputSpec) -> Option<OutputMode> {
    modes
        .iter()
        .filter(|m| m.width == spec.width && m.height == spec.height)
        .min_by_key(|m| m.refresh_mhz.abs_diff(PREFERRED_REFRESH_MHZ))
        .copied()
}

fn sizes(modes: &[OutputMode]) -> String {
    let mut sizes: Vec<(u32, u32)> = modes.iter().map(|m| (m.width, m.height)).collect();
    sizes.sort_unstable();
    sizes.dedup();
    if sizes.is_empty() {
        return "no modes".into();
    }
    sizes
        .iter()
        .rev()
        .map(|(w, h)| format!("{w}x{h}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Stage, wait for and confirm the layout for `specs`. Runs as the slot's
/// user with its `HOME` and `XDG_RUNTIME_DIR`.
///
/// # Errors
/// A sentence for the step that failed or timed out.
pub fn apply(specs: &[OutputSpec], timeout: Duration) -> Result<String, String> {
    let started = Instant::now();
    let left = || timeout.saturating_sub(started.elapsed());
    let snapshot = wait_for(left(), || {
        DisplayHardwareSnapshot::load().filter(|s| s.outputs.len() >= specs.len())
    })
    .ok_or_else(|| {
        let seen = DisplayHardwareSnapshot::load().map_or_else(
            || "no display-state.json from a running compositor".to_owned(),
            |s| format!("{} output(s)", s.outputs.len()),
        );
        format!(
            "the compositor did not publish {} outputs within {}s ({seen})",
            specs.len(),
            timeout.as_secs()
        )
    })?;
    let candidate = plan(&snapshot.outputs, specs)?;
    let set = attached_set(&snapshot.outputs);
    let staged = DisplayTransaction::request(set, candidate)
        .map_err(|e| format!("could not stage the display change: {e}"))?;
    let mut transaction = wait_for(left(), || {
        DisplayTransaction::load().filter(|t| {
            t.id == staged.id
                && matches!(
                    t.state,
                    DisplayTransactionState::AwaitingConfirmation
                        | DisplayTransactionState::Confirmed
                        | DisplayTransactionState::Rejected
                        | DisplayTransactionState::Reverted
                )
        })
    })
    .ok_or("the compositor did not pick up the display change")?;
    // A change that cannot darken a screen (a scale alone, or the layout the
    // monitors already have) is kept at once, with nothing to confirm.
    if transaction.state == DisplayTransactionState::Confirmed {
        return Ok(format!("outputs set to {}", format_outputs(specs)));
    }
    if transaction.state != DisplayTransactionState::AwaitingConfirmation {
        return Err(format!(
            "the compositor rejected the layout: {}",
            transaction.message.as_deref().unwrap_or("no reason given")
        ));
    }
    transaction
        .request_confirmation()
        .map_err(|e| format!("could not confirm the display change: {e}"))?;
    wait_for(left(), || {
        DisplayTransaction::load()
            .filter(|t| t.id == staged.id && t.state == DisplayTransactionState::Confirmed)
    })
    .ok_or("the compositor did not confirm the display change")?;
    Ok(format!("outputs set to {}", format_outputs(specs)))
}

fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if started.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use settings_store::hardware::OutputDescriptor;
    use tilo_vm_protocol::display::parse_outputs;

    use super::*;

    fn mode(width: u32, height: u32, refresh_mhz: u32) -> OutputMode {
        OutputMode {
            width,
            height,
            refresh_mhz,
        }
    }

    fn vkms_output(label: &str) -> DisplayOutputSnapshot {
        let modes = vec![
            mode(1024, 768, 60_004),
            mode(2560, 1600, 59_972),
            mode(2560, 1600, 59_987),
            mode(1920, 1080, 60_000),
            mode(800, 600, 60_317),
        ];
        DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: label.into(),
                connector_label: label.into(),
                modes,
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            },
            current_mode: mode(1024, 768, 60_004),
            scale: 1.0,
            builtin: false,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        }
    }

    #[test]
    fn outputs_line_up_left_to_right_in_logical_pixels() {
        // Published out of order: connectors still map to specs by number.
        let outputs = [vkms_output("Virtual-2"), vkms_output("Virtual-1")];
        let specs = parse_outputs("2560x1600@2,1920x1080").unwrap();
        let config = plan(&outputs, &specs).unwrap();
        let first = &config.outputs["Virtual-1"];
        let second = &config.outputs["Virtual-2"];
        assert_eq!(first.mode, Some(mode(2560, 1600, 59_987)));
        assert!((first.scale - 2.0).abs() < f64::EPSILON);
        assert!(first.primary && !second.primary);
        assert_eq!((first.x, second.x), (0, 1280));
        assert_eq!(second.mode, Some(mode(1920, 1080, 60_000)));
        assert_eq!(desktop_extent(&specs), (1280 + 1920, 1080));
    }

    #[test]
    fn a_missing_mode_lists_the_ones_there_are() {
        let error = plan(
            &[vkms_output("Virtual-1")],
            &parse_outputs("1366x768").unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("Virtual-1 offers no 1366x768"), "{error}");
        assert!(error.contains("2560x1600, 1920x1080"), "{error}");
    }

    #[test]
    fn a_count_mismatch_is_refused() {
        let error = plan(
            &[vkms_output("Virtual-1")],
            &parse_outputs("800x600,800x600").unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("shows 1 output"), "{error}");
    }

    #[test]
    fn connectors_sort_by_number() {
        let mut labels = ["Virtual-10", "Virtual-2", "Virtual-1"];
        labels.sort_by_key(|l| connector_order(l));
        assert_eq!(labels, ["Virtual-1", "Virtual-2", "Virtual-10"]);
    }

    #[test]
    fn a_change_the_compositor_keeps_at_once_is_applied() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let (publisher_pid, publisher_start_ticks) =
            settings_store::hardware::publisher_identity().unwrap();
        DisplayHardwareSnapshot {
            publisher_pid,
            publisher_start_ticks,
            outputs: vec![vkms_output("Virtual-1")],
            ..DisplayHardwareSnapshot::default()
        }
        .save()
        .unwrap();
        // The compositor's answer to the mode it already shows: kept, with
        // nothing to confirm.
        let compositor = std::thread::spawn(|| {
            let mut transaction = tilo_test_env::wait_for("the staged display change", || {
                DisplayTransaction::load().filter(|t| t.state == DisplayTransactionState::Requested)
            });
            transaction.state = DisplayTransactionState::Confirmed;
            transaction.save().unwrap();
        });
        let applied = apply(
            &parse_outputs("1024x768").unwrap(),
            tilo_test_env::hang_bound(),
        );
        compositor.join().unwrap();
        assert_eq!(applied, Ok("outputs set to 1024x768@1".to_owned()));
    }
}
