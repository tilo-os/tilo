//! Scale and refresh rate of one display, planned as a candidate layout.
//!
//! Like [`crate::display_mode`], these are pure functions of the
//! compositor-published hardware snapshot and the committed layout. They
//! return a [`DisplayConfig`] that passes [`DisplayConfig::validate_for`], or
//! the reason the request cannot be met, and applying one goes through the
//! same [`crate::hardware::DisplayTransaction`] keep-or-restore handshake as
//! the Settings pane.

use crate::hardware::{DisplayConfig, DisplayOutputSnapshot, OutputConfig, OutputMode};

/// The scales the Settings pane offers: 100 % to 200 % in quarter steps
/// ([`MIN_SCALE`](crate::display_scale::MIN_SCALE), [`MAX_SCALE`](crate::display_scale::MAX_SCALE), [`SCALE_STEP`](crate::display_scale::SCALE_STEP)).
const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// The scales the Settings pane offers.
#[must_use]
pub fn scale_choices() -> Vec<f64> {
    SCALES.to_vec()
}

/// A rate as a person writes it: `60`, `59.94`.
#[must_use]
pub fn hz_label(refresh_mhz: u32) -> String {
    let hz = format!("{:.2}", f64::from(refresh_mhz) / 1000.0);
    hz.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The output `connector` names (by persisted key or by label), else the
/// primary one, else the built-in panel, else the first.
///
/// # Errors
///
/// No display is connected, or none is called `connector`.
pub fn target<'a>(
    outputs: &'a [DisplayOutputSnapshot],
    base: &DisplayConfig,
    connector: Option<&str>,
) -> Result<&'a DisplayOutputSnapshot, String> {
    if let Some(wanted) = connector.map(str::trim).filter(|wanted| !wanted.is_empty()) {
        return outputs
            .iter()
            .find(|output| {
                output.descriptor.connector == wanted || output.descriptor.label() == wanted
            })
            .ok_or_else(|| {
                let known: Vec<&str> = outputs.iter().map(|o| o.descriptor.label()).collect();
                format!("no display is called {wanted:?}: {}", known.join(", "))
            });
    }
    outputs
        .iter()
        .find(|output| {
            base.outputs
                .get(&output.descriptor.connector)
                .is_some_and(|config| config.enabled && config.primary)
        })
        .or_else(|| outputs.iter().find(|output| output.builtin))
        .or_else(|| outputs.first())
        .ok_or_else(|| "no display is connected".to_owned())
}

/// The refresh rates (milli-Hz) `output` offers at the resolution it runs.
#[must_use]
pub fn refresh_choices(output: &DisplayOutputSnapshot) -> Vec<u32> {
    let current = output.current_mode;
    let mut rates: Vec<u32> = output
        .descriptor
        .modes
        .iter()
        .filter(|mode| mode.width == current.width && mode.height == current.height)
        .map(|mode| mode.refresh_mhz)
        .collect();
    rates.sort_unstable();
    rates.dedup();
    rates
}

fn validated(
    outputs: &[DisplayOutputSnapshot],
    next: DisplayConfig,
) -> Result<DisplayConfig, String> {
    let descriptors: Vec<_> = outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    next.validate_for(&descriptors)
        .map_err(|error| format!("the display configuration is unsupported: {error:?}"))?;
    Ok(next)
}

fn entry<'a>(next: &'a mut DisplayConfig, output: &DisplayOutputSnapshot) -> &'a mut OutputConfig {
    next.outputs
        .entry(output.descriptor.connector.clone())
        .or_insert_with(|| output.unconfigured())
}

/// `base` with the display `connector` (the default one when `None`) at
/// `scale`, every other choice kept.
///
/// # Errors
///
/// `scale` is not one of [`scale_choices`], no such display, or the result
/// does not validate for this hardware.
pub fn plan_scale(
    outputs: &[DisplayOutputSnapshot],
    base: &DisplayConfig,
    connector: Option<&str>,
    scale: f64,
) -> Result<DisplayConfig, String> {
    if !scale_choices()
        .iter()
        .any(|choice| (choice - scale).abs() < 1e-6)
    {
        let offered: Vec<String> = scale_choices().iter().map(f64::to_string).collect();
        return Err(format!(
            "{scale} is not an offered scale: {}",
            offered.join(", ")
        ));
    }
    let output = target(outputs, base, connector)?;
    let mut next = base.clone();
    entry(&mut next, output).scale = scale;
    validated(outputs, next)
}

/// `base` with the display `connector` (the default one when `None`) at the
/// resolution it runs now and `hertz`, every other choice kept.
///
/// # Errors
///
/// The display offers no such rate at its resolution, no such display, or
/// the result does not validate for this hardware.
pub fn plan_refresh(
    outputs: &[DisplayOutputSnapshot],
    base: &DisplayConfig,
    connector: Option<&str>,
    hertz: f64,
) -> Result<DisplayConfig, String> {
    let output = target(outputs, base, connector)?;
    let current = output.current_mode;
    let mode: OutputMode = output
        .descriptor
        .modes
        .iter()
        .copied()
        .filter(|mode| mode.width == current.width && mode.height == current.height)
        .find(|mode| (f64::from(mode.refresh_mhz) / 1000.0 - hertz).abs() < 0.005)
        .ok_or_else(|| {
            let offered: Vec<String> = refresh_choices(output).into_iter().map(hz_label).collect();
            format!(
                "{} offers no {hertz} Hz at {} x {}: {}",
                output.descriptor.label(),
                current.width,
                current.height,
                offered.join(", ")
            )
        })?;
    let mut next = base.clone();
    entry(&mut next, output).mode = Some(mode);
    validated(outputs, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_scale::{MAX_SCALE, MIN_SCALE, SCALE_STEP};
    use crate::hardware::{DisplayConfig, OutputDescriptor};

    fn output(connector: &str, builtin: bool, rates: &[u32]) -> DisplayOutputSnapshot {
        let modes: Vec<OutputMode> = rates
            .iter()
            .map(|&refresh_mhz| OutputMode {
                width: 2560,
                height: 1600,
                refresh_mhz,
            })
            .chain([OutputMode {
                width: 1920,
                height: 1200,
                refresh_mhz: 30_000,
            }])
            .collect();
        DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: String::new(),
                modes,
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: OutputMode {
                width: 2560,
                height: 1600,
                refresh_mhz: rates[0],
            },
            scale: 1.5,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            color_profile_labels: Default::default(),
        }
    }

    #[test]
    fn scale_and_refresh_are_planned_per_display_and_refuse_what_is_not_offered() {
        let outputs = [
            output("HDMI-A-1", false, &[60_000]),
            output("eDP-1", true, &[60_000, 120_000]),
        ];
        let base = DisplayConfig::default();
        assert_eq!(scale_choices().first().copied(), Some(MIN_SCALE));
        assert_eq!(scale_choices().last().copied(), Some(MAX_SCALE));
        assert!(
            scale_choices()
                .windows(2)
                .all(|pair| (pair[1] - pair[0] - SCALE_STEP).abs() < 1e-9)
        );
        assert_eq!(hz_label(59_940), "59.94");
        assert_eq!(hz_label(120_000), "120");

        // The built-in panel is the default target.
        let planned = plan_scale(&outputs, &base, None, 2.0).unwrap();
        assert_eq!(planned.outputs["eDP-1"].scale, 2.0);
        assert!(!planned.outputs.contains_key("HDMI-A-1"));
        let named = plan_scale(&outputs, &base, Some("HDMI-A-1"), 1.25).unwrap();
        assert_eq!(named.outputs["HDMI-A-1"].scale, 1.25);
        assert!(plan_scale(&outputs, &base, None, 1.3).is_err());
        assert!(plan_scale(&outputs, &base, Some("DP-9"), 1.5).is_err());

        let fast = plan_refresh(&outputs, &base, None, 120.0).unwrap();
        assert_eq!(fast.outputs["eDP-1"].mode.unwrap().refresh_mhz, 120_000);
        assert_eq!(fast.outputs["eDP-1"].mode.unwrap().width, 2560);
        assert!(
            plan_refresh(&outputs, &base, Some("HDMI-A-1"), 120.0).is_err(),
            "the external display offers only 60 Hz"
        );
        assert!(
            plan_refresh(&outputs, &base, None, 30.0).is_err(),
            "30 Hz exists only at another resolution"
        );
        assert_eq!(refresh_choices(&outputs[1]), vec![60_000, 120_000]);
    }
}
