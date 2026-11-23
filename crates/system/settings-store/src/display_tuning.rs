//! Scale and refresh rate of one display, planned as a candidate layout.
//!
//! Like [`crate::display_mode`], these are pure functions of the
//! compositor-published hardware snapshot and the committed layout. They
//! return a [`DisplayConfig`] that passes [`DisplayConfig::validate_for`], or
//! the reason the request cannot be met, and applying one goes through the
//! same [`crate::hardware::DisplayTransaction`] keep-or-restore handshake as
//! the Settings pane.

use crate::hardware::{DisplayConfig, DisplayOutputSnapshot, OutputConfig, OutputMode};

const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

#[must_use]
pub fn scale_choices() -> Vec<f64> {
    SCALES.to_vec()
}

#[must_use]
pub fn hz_label(refresh_mhz: u32) -> String {
    let hz = format!("{:.2}", f64::from(refresh_mhz) / 1000.0);
    hz.trim_end_matches('0').trim_end_matches('.').to_owned()
}

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
