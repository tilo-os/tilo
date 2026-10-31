//! The person's display settings, applied to the displays there are.
//!
//! The compositor publishes what it can drive ([`Compositor::display_snapshot`])
//! and applies a [`DisplayConfig`](tilo_compositor::DisplayConfig) without
//! reading any settings; Settings edits the person's arrangement, and a change
//! that could leave a screen dark waits for them in a
//! [`DisplayTransaction`]. This module is where the two meet: it keeps the
//! configuration for the monitors attached now in line with what the person
//! chose, applies it, and advances a staged change through its deadline,
//! keeping it only when the person says so.
//!
//! The stored halves are keyed by the monitors attached right now
//! ([`DisplaySet`]), so the office layout comes back when the dock does and a
//! stranger on a familiar port never inherits it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use os_actions::brightness::BrightnessTarget;
use settings_store::hardware::{
    DisplaySet, DisplayTransaction, DisplayTransactionState, MonitorLast, OutputConfig,
    OutputDescriptor, OutputTransform,
};
use tilo_compositor::DisplayConfig as CompositorDisplayConfig;
use tilo_compositor::backend::drm::devices::RequestedMode;
use tilo_compositor::{Backlight, Compositor, Hdr, Transform};

/// The displays as the person configured them, kept in step with the ones
/// attached.
#[derive(Default)]
pub(crate) struct Displays {
    /// The monitors the applied layout is for, and their descriptors.
    set: Option<DisplaySet>,
    descriptors: Vec<OutputDescriptor>,
    /// The snapshot generation `set` was taken from.
    generation: Option<u64>,
    /// The settings document last read, so an unchanged one is not read
    /// again on every pass.
    stamp: Option<settings_store::SettingsStamp>,
    seen: bool,
    /// When the change waiting for the person is put back.
    deadline: Option<u64>,
    /// What the compositor was last given, so it is not given the same
    /// arrangement on every pass.
    applied: Option<CompositorDisplayConfig>,
    /// The staged change last seen, to act on a change of its own rather
    /// than on every look.
    staged: Option<DisplayTransaction>,
    /// The trusted display tools the compositor was last given.
    trusted: Option<Vec<PathBuf>>,
    /// The built-in output last turned with the device, and the turn, so a
    /// device that stays put is not laid out again on every pass.
    turned: Option<(String, Transform)>,
}

impl Displays {
    /// Bring the displays in line with the person's settings and advance the
    /// change they were asked to confirm, `night_light` being the colour
    /// temperature the session runs the night light at while it is on, and
    /// `turned` the built-in output the device's orientation turns, with the
    /// turn it shows.
    ///
    /// # Errors
    /// The compositor could not set a mode, scale, arrangement or HDR.
    pub(crate) fn follow(
        &mut self,
        compositor: &mut Compositor,
        night_light: Option<u32>,
        turned: Option<(&str, Transform)>,
    ) -> Result<(), tilo_compositor::Error> {
        let now = unix_now_ms();
        let expired = self.deadline.is_some_and(|deadline| now >= deadline);
        let stamp = settings_store::stamp();
        let document = settings_store::document_changed(self.seen, self.stamp, stamp);
        let generation = compositor.displays_generation();
        let monitors_changed = self.generation != Some(generation);
        let turned_changed = self.turned_changed(turned);
        if !document && !expired && !monitors_changed && !turned_changed {
            return Ok(());
        }
        self.seen = true;
        self.stamp = stamp;
        self.generation = Some(generation);
        // The tools whose risky changes apply without asking: a trust given
        // or revoked in Settings reaches the compositor on this pass.
        let trusted = trusted_tools();
        if self.trusted.as_ref() != Some(&trusted) {
            compositor.set_trusted_display_tools(trusted.clone());
            self.trusted = Some(trusted);
        }
        if monitors_changed {
            let Some(snapshot) = compositor.display_snapshot().cloned() else {
                return Ok(());
            };
            self.descriptors = snapshot
                .outputs
                .iter()
                .map(|output| output.descriptor.clone())
                .collect();
            self.set = Some(DisplaySet::of_descriptors(&self.descriptors));
        }
        let Some(set) = self.set.clone() else {
            return Ok(());
        };
        let staged = DisplayTransaction::load();
        if staged != self.staged || expired {
            self.staged = staged.map(|transaction| settle(transaction, &self.descriptors, &set));
        }
        self.deadline = match &self.staged {
            Some(transaction)
                if transaction.state == DisplayTransactionState::AwaitingConfirmation =>
            {
                transaction.deadline_unix_ms
            }
            _ => None,
        };
        let (stored, last) =
            settings_store::hardware::DisplayConfig::load_effective_remembering(&set);
        if turned_changed {
            self.turned = turned.map(|(connector, turn)| (connector.to_owned(), turn));
        }
        let config = sent(
            &stored,
            &self.descriptors,
            &last,
            night_light,
            self.turned
                .as_ref()
                .map(|(connector, turn)| (connector.as_str(), *turn)),
        );
        if self.applied.as_ref() == Some(&config) {
            return Ok(());
        }
        compositor.set_display_config(&config)?;
        self.applied = Some(config);
        Ok(())
    }

    /// The change waiting for the person's answer now: its id, and when the
    /// previous arrangement comes back without one.
    pub(crate) fn awaiting_confirmation(&self) -> Option<(u64, Option<u64>)> {
        match &self.staged {
            Some(transaction)
                if transaction.state == DisplayTransactionState::AwaitingConfirmation =>
            {
                Some((transaction.id, transaction.deadline_unix_ms))
            }
            _ => None,
        }
    }

    /// Record the person's answer to the change waiting now: keeping it
    /// commits it as the layout for these displays, reverting puts the
    /// previous one back. False when change `id` no longer waits, having
    /// been answered elsewhere or restored by its deadline.
    ///
    /// # Errors
    /// The answered transaction could not be written.
    pub(crate) fn answer(&self, id: u64, keep: bool) -> std::io::Result<bool> {
        let Some(mut transaction) = self.staged.clone().filter(|transaction| {
            transaction.id == id
                && transaction.state == DisplayTransactionState::AwaitingConfirmation
        }) else {
            return Ok(false);
        };
        if keep {
            transaction.request_confirmation()?;
        } else {
            transaction.request_revert()?;
        }
        Ok(true)
    }

    /// Whether `turned` is a different output or a different turn from the
    /// one last laid out.
    fn turned_changed(&self, turned: Option<(&str, Transform)>) -> bool {
        match (&self.turned, turned) {
            (None, None) => false,
            (Some((connector, turn)), Some((now, shown))) => connector != now || *turn != shown,
            _ => true,
        }
    }
}

/// The executables of the display tools the person trusts.
pub(crate) fn trusted_tools() -> Vec<PathBuf> {
    settings_store::display_tools::TrustedDisplayTools::load()
        .tools
        .into_iter()
        .map(|tool| PathBuf::from(tool.executable))
        .collect()
}

/// Advance `transaction` as far as the attached monitors and the person's
/// answer take it, one step at a time: an expired or reverted change puts
/// the previous arrangement back before it counts as done.
fn settle(
    mut transaction: DisplayTransaction,
    descriptors: &[OutputDescriptor],
    set: &DisplaySet,
) -> DisplayTransaction {
    for _ in 0..4 {
        let before = (transaction.state, transaction.deadline_unix_ms);
        transaction = reconcile(transaction, descriptors, set, unix_now_ms());
        match transaction.state {
            DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::Confirmed
            | DisplayTransactionState::Reverted
            | DisplayTransactionState::Rejected => break,
            DisplayTransactionState::Requested
            | DisplayTransactionState::ConfirmRequested
            | DisplayTransactionState::RevertRequested => {}
        }
        if before == (transaction.state, transaction.deadline_unix_ms) {
            break;
        }
    }
    transaction
}

/// One step of `transaction`: apply it, wait for the person, keep it, or
/// put the previous arrangement back.
fn reconcile(
    mut transaction: DisplayTransaction,
    descriptors: &[OutputDescriptor],
    set: &DisplaySet,
    now_unix_ms: u64,
) -> DisplayTransaction {
    if transaction.expired(now_unix_ms) {
        transaction.state = DisplayTransactionState::RevertRequested;
        transaction.message = Some("Display change timed out and was restored".into());
    }
    match transaction.state {
        DisplayTransactionState::Requested => {
            if descriptors.is_empty() {
                transaction.state = DisplayTransactionState::Rejected;
                transaction.message = Some("No display describes itself yet".into());
            } else if transaction.set.key != set.key {
                transaction.state = DisplayTransactionState::Rejected;
                transaction.message =
                    Some("The attached displays changed before the arrangement was applied".into());
            } else if let Err(error) = transaction.candidate.validate_for(descriptors) {
                transaction.state = DisplayTransactionState::Rejected;
                transaction.message = Some(format!("Rejected unsupported display plan: {error:?}"));
            } else if !transaction
                .candidate
                .needs_confirmation_from(&transaction.previous)
            {
                // Nothing here can leave a screen dark (a scale, a position,
                // a colour profile): it stays at once rather than counting
                // down to a restore nobody needs.
                if let Err(error) = transaction.candidate.save_for(&transaction.set) {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message = Some(format!("Could not commit display plan: {error}"));
                } else {
                    transaction.state = DisplayTransactionState::Confirmed;
                    transaction.deadline_unix_ms = None;
                    transaction.message = Some("Display configuration applied".into());
                }
            } else {
                transaction.state = DisplayTransactionState::AwaitingConfirmation;
                transaction.deadline_unix_ms =
                    Some(now_unix_ms.saturating_add(DisplayTransaction::CONFIRM_TIMEOUT_MS));
                transaction.message = Some(
                    "Applied temporarily; confirm within 15 seconds or it will be restored".into(),
                );
            }
        }
        DisplayTransactionState::ConfirmRequested => {
            if let Err(error) = transaction.candidate.save_for(&transaction.set) {
                transaction.state = DisplayTransactionState::Rejected;
                transaction.message = Some(format!("Could not commit display plan: {error}"));
            } else {
                transaction.state = DisplayTransactionState::Confirmed;
                transaction.deadline_unix_ms = None;
                transaction.message = Some("Display configuration kept".into());
            }
        }
        DisplayTransactionState::RevertRequested => {
            if let Err(error) = transaction.previous.save_for(&transaction.set) {
                transaction.state = DisplayTransactionState::Rejected;
                transaction.message = Some(format!("Could not restore display plan: {error}"));
            } else {
                transaction.state = DisplayTransactionState::Reverted;
                transaction.deadline_unix_ms = None;
                transaction
                    .message
                    .get_or_insert_with(|| "Previous display configuration restored".into());
            }
        }
        DisplayTransactionState::AwaitingConfirmation
        | DisplayTransactionState::Confirmed
        | DisplayTransactionState::Reverted
        | DisplayTransactionState::Rejected => {}
    }
    if let Err(error) = transaction.save() {
        tracing::warn!(%error, "the display change's state was not kept");
    }
    transaction
}

/// The stored arrangement as the compositor takes it, at the night light
/// temperature the session runs while it is on, with `turned`'s output
/// turned the way the device is held.
///
/// A monitor the person never arranged for keeps the scale and rotation it
/// was last shown at, so a familiar laptop panel is not thrown back to 1x
/// the first time it is docked.
///
/// The turn goes over the stored one for that panel: a screen whose machine
/// is held on its side is not readable at the angle its arrangement was
/// arranged at, and the rotation lock is what pins it instead.
fn sent(
    stored: &settings_store::hardware::DisplayConfig,
    descriptors: &[OutputDescriptor],
    last: &BTreeMap<String, MonitorLast>,
    night_light: Option<u32>,
    turned: Option<(&str, Transform)>,
) -> CompositorDisplayConfig {
    let mut config = CompositorDisplayConfig {
        night_light,
        ..CompositorDisplayConfig::default()
    };
    let mut placed = BTreeMap::<&str, &OutputConfig>::new();
    for (connector, output) in &stored.outputs {
        placed.insert(connector.as_str(), output);
        place(&mut config, connector, output);
    }
    for descriptor in descriptors {
        if placed.contains_key(descriptor.connector.as_str()) {
            continue;
        }
        let Some(shown) = last.get(&descriptor.connector) else {
            continue;
        };
        let output = OutputConfig {
            scale: shown.scale,
            transform: shown.transform,
            ..OutputConfig::default()
        };
        place(&mut config, &descriptor.connector, &output);
    }
    if let Some((connector, transform)) = turned {
        config.transforms.insert(connector.to_owned(), transform);
    }
    config
}

/// The compositor's part of one output's stored configuration.
fn place(config: &mut CompositorDisplayConfig, connector: &str, output: &OutputConfig) {
    if !output.enabled {
        config.disabled.insert(connector.to_owned());
    }
    // The display the person made primary is the one their desk's bar, new
    // windows and sheets go to. A stored primary the person turned off
    // stays named here and the compositor names no primary while nothing
    // is lit on it, so the choice comes back with the display.
    if output.primary {
        config.primary = Some(connector.to_owned());
    }
    if let Some(mode) = output.mode {
        config.modes.insert(
            connector.to_owned(),
            RequestedMode {
                width: mode.width,
                height: mode.height,
                refresh_mhz: mode.refresh_mhz,
            },
        );
    }
    if output.scale.is_finite() && output.scale > 0.0 {
        config.scales.insert(connector.to_owned(), output.scale);
    }
    config
        .positions
        .insert(connector.to_owned(), (output.x, output.y));
    config
        .transforms
        .insert(connector.to_owned(), transform(output.transform));
    if output.vrr {
        config.variable_refresh.insert(connector.to_owned());
    }
    if let Some(profile) = &output.color_profile {
        config
            .color_profiles
            .insert(connector.to_owned(), PathBuf::from(profile));
    }
    if output.hdr {
        config.hdr.insert(
            connector.to_owned(),
            Hdr {
                sdr_white_nits: f32::from(output.sdr_brightness_nits),
                ..Hdr::default()
            },
        );
    }
}

/// The quarter turn `transform` is, in the compositor's terms. A mirrored
/// transform is the turn it mirrors.
fn transform(transform: OutputTransform) -> Transform {
    match transform {
        OutputTransform::Normal => Transform::Normal,
        OutputTransform::Rotate90 => Transform::_90,
        OutputTransform::Rotate180 => Transform::_180,
        OutputTransform::Rotate270 => Transform::_270,
    }
}

/// Milliseconds since the Unix epoch: the clock staged changes count down
/// on.
pub(crate) fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The built-in panel's backlight, which the idle dim lowers and puts back:
/// on a laptop the panel itself dims, and on a desktop, a VM or an external
/// monitor, where there is none, the compositor darkens the scene instead.
pub(crate) struct PanelBacklight {
    /// The panel's backlight device, when the machine has one.
    target: Option<BrightnessTarget>,
}

impl PanelBacklight {
    /// The backlight of the built-in panel attached now, if any.
    pub(crate) fn find() -> Self {
        Self::of(&os_actions::brightness::monitors())
    }

    /// The built-in panel's own backlight among `monitors`: never one an
    /// external monitor answers over DDC/CI, and never a software dim.
    fn of(monitors: &[os_actions::brightness::MonitorBrightness]) -> Self {
        let target = monitors
            .iter()
            .find(|monitor| {
                os_actions::brightness::is_builtin_connector(&monitor.connector)
                    && matches!(&monitor.target, BrightnessTarget::Backlight(_))
            })
            .map(|monitor| monitor.target.clone());
        Self { target }
    }
}

impl Backlight for PanelBacklight {
    fn brightness(&self) -> Option<f32> {
        self.target.as_ref()?.get().ok()
    }

    fn set_brightness(&self, fraction: f32) -> bool {
        self.target
            .as_ref()
            .is_some_and(|target| target.set(fraction * 100.0).is_ok())
    }

    fn powered(&self) -> Option<bool> {
        os_actions::display::backlight_powered()
    }

    fn describe(&self) -> Option<String> {
        let BrightnessTarget::Backlight(device) = self.target.as_ref()? else {
            return None;
        };
        Some(device.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use os_actions::brightness::{BrightnessTarget, MonitorBrightness};

    use super::*;

    fn panel(connector: &str, target: BrightnessTarget) -> MonitorBrightness {
        MonitorBrightness {
            connector: connector.into(),
            target,
            edid: None,
        }
    }

    #[test]
    fn every_choice_a_monitor_has_becomes_the_compositors_own_knob() {
        let mut stored = settings_store::hardware::DisplayConfig::default();
        stored.outputs.insert(
            "DP-1".into(),
            OutputConfig {
                enabled: false,
                primary: true,
                x: 1920,
                y: 240,
                scale: 2.0,
                transform: OutputTransform::Rotate270,
                mode: Some(settings_store::hardware::OutputMode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 143_981,
                }),
                hdr: true,
                sdr_brightness_nits: 312,
                vrr: true,
                color_profile: Some("/usr/share/color/icc/panel.icc".into()),
                ..OutputConfig::default()
            },
        );
        let config = super::sent(
            &stored,
            &[],
            &std::collections::BTreeMap::new(),
            Some(3400),
            None,
        );
        assert_eq!(config.night_light, Some(3400));
        assert!(config.disabled.contains("DP-1"));
        assert_eq!(
            config.primary.as_deref(),
            Some("DP-1"),
            "the display the person chose is the primary one"
        );
        assert_eq!(config.positions.get("DP-1"), Some(&(1920, 240)));
        assert_eq!(config.scales.get("DP-1"), Some(&2.0));
        assert_eq!(
            config.transforms.get("DP-1"),
            Some(&tilo_compositor::Transform::_270)
        );
        assert_eq!(
            config
                .modes
                .get("DP-1")
                .map(|mode| (mode.width, mode.height, mode.refresh_mhz)),
            Some((2560, 1440, 143_981))
        );
        assert!(config.variable_refresh.contains("DP-1"));
        assert_eq!(
            config.color_profiles.get("DP-1"),
            Some(&std::path::PathBuf::from("/usr/share/color/icc/panel.icc"))
        );
        assert_eq!(
            config.hdr.get("DP-1").map(|hdr| hdr.sdr_white_nits),
            Some(312.0)
        );
    }

    #[test]
    fn a_monitor_never_arranged_for_keeps_the_scale_and_turn_it_was_last_shown_at() {
        let mut last = std::collections::BTreeMap::new();
        last.insert(
            "eDP-1".to_owned(),
            settings_store::hardware::MonitorLast {
                scale: 1.5,
                transform: OutputTransform::Rotate90,
            },
        );
        let descriptor = settings_store::hardware::OutputDescriptor {
            connector: "eDP-1".into(),
            connector_label: String::new(),
            modes: Vec::new(),
            hdr: false,
            vrr: false,
            color_profiles: Vec::new(),
            monitor: None,
        };
        let config = super::sent(
            &settings_store::hardware::DisplayConfig::default(),
            std::slice::from_ref(&descriptor),
            &last,
            None,
            None,
        );
        assert_eq!(config.scales.get("eDP-1"), Some(&1.5));
        assert_eq!(
            config.transforms.get("eDP-1"),
            Some(&tilo_compositor::Transform::_90)
        );
    }

    #[test]
    fn the_panel_follows_the_device_over_the_turn_it_was_shown_at() {
        let mut stored = settings_store::hardware::DisplayConfig::default();
        stored.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                transform: OutputTransform::Rotate180,
                ..OutputConfig::default()
            },
        );
        let turned = Some(("eDP-1", tilo_compositor::Transform::_90));
        let config = super::sent(
            &stored,
            &[],
            &std::collections::BTreeMap::new(),
            None,
            turned,
        );
        assert_eq!(
            config.transforms.get("eDP-1"),
            Some(&tilo_compositor::Transform::_90),
            "the panel turns with the machine, whatever its arrangement says"
        );
    }

    #[test]
    fn the_panel_is_the_built_in_display_and_never_an_external_one() {
        let directory = tempfile::tempdir().expect("a directory");
        let built_in = BrightnessTarget::Backlight(directory.path().join("intel_backlight"));
        let chosen = PanelBacklight::of(&[
            panel("DP-1", BrightnessTarget::Ddc("/dev/i2c-4".into())),
            panel("eDP-1", built_in.clone()),
        ]);
        assert_eq!(chosen.target, Some(built_in));

        let without_panel = PanelBacklight::of(&[
            panel("DP-1", BrightnessTarget::Ddc("/dev/i2c-4".into())),
            panel("HDMI-A-1", BrightnessTarget::Software),
        ]);
        assert_eq!(without_panel.target, None);
    }

    #[test]
    fn the_dim_lowers_the_panels_backlight_and_puts_it_back() {
        let device = tempfile::tempdir().expect("a device");
        std::fs::write(device.path().join("max_brightness"), "200").expect("a maximum");
        std::fs::write(device.path().join("brightness"), "200").expect("a brightness");
        let panel = PanelBacklight::of(&[panel(
            "eDP-1",
            BrightnessTarget::Backlight(device.path().to_owned()),
        )]);

        assert_eq!(panel.brightness(), Some(1.0));
        assert!(panel.set_brightness(0.3));
        assert!((panel.brightness().expect("a brightness") - 0.3).abs() < 0.01);
        assert_eq!(
            panel.describe().as_deref(),
            Some(device.path().to_str().expect("a UTF-8 path")),
        );
    }
}
