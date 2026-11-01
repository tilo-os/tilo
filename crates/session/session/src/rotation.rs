//! Which way up the device is held, and whether the screen follows it.
//!
//! A convertible's built-in panel turns with the machine: the kernel's
//! accelerometer says which edge is up, and the panel's picture is drawn so
//! that it reads upright in the person's hands. The rotation lock pins it
//! instead, for a screen read while the machine lies on its side.
//!
//! The lock is the person's own setting
//! ([`settings_store::hardware::ROTATION_LOCK`]), so this follows the settings
//! document as the displays do, and hands the turn it found to
//! [`Displays`](crate::display_settings::Displays) rather than turning an
//! output itself: one place decides where each output's pixels go.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use os_actions::orientation::{self, DeviceOrientation, RotationControl};
use tilo_compositor::{OutputInfo, Transform};

/// How often the accelerometer is read. The kernel sends no event for a
/// device being turned, and the orientation is debounced over consecutive
/// samples: a quarter of a second is fine enough for a screen to follow a
/// hand without reading sensors on every frame.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Where the session reads which way up the device is held.
#[derive(Clone, Debug, Default)]
pub enum Accelerometer {
    /// This machine's, from the kernel's sensors.
    #[default]
    Machine,
    /// The sensor under this root (`/sys/bus/iio/devices` on a machine): a
    /// stand-in, for a session started over a fake one.
    At(PathBuf),
    /// None: this session's screens never turn with the device.
    Off,
}

/// The built-in panel's turn, as the device is held now and the lock allows.
#[derive(Default)]
pub(crate) struct Rotation {
    /// The accelerometer's device, once found: a machine's sensor does not
    /// move, so looking for it again on every pass would only cost.
    device: Option<PathBuf>,
    /// The lock as the person's settings have it, and the orientation the
    /// panel shows.
    control: RotationControl,
    /// The settings document last read, so an unchanged one is not read
    /// again on every pass.
    seen: bool,
    stamp: Option<settings_store::SettingsStamp>,
    /// When the sensor is read next.
    due: Option<Instant>,
    /// The built-in output and the turn it shows, when the last pass found
    /// both, so a device that stays put hands back the same turn and the
    /// displays leave the output alone.
    turned: Option<(String, Transform)>,
}

impl Rotation {
    /// Mount the reader for `accelerometer`.
    pub(crate) fn mount(accelerometer: Accelerometer) -> Self {
        let device = match accelerometer {
            Accelerometer::Machine => orientation::accelerometer(),
            Accelerometer::At(root) => orientation::accelerometer_in(&root),
            Accelerometer::Off => None,
        };
        Self {
            device,
            ..Self::default()
        }
    }

    /// The built-in output and the turn the panel shows, `None` when no
    /// output of this session turns with the device.
    ///
    /// Only a built-in panel turns: a monitor on the desk stands still while
    /// the machine in the person's hands is turned around.
    pub(crate) fn follow(&mut self, outputs: &[OutputInfo]) -> Option<(&str, Transform)> {
        let device = self.device.as_deref()?;
        let Some(panel) = panel(outputs) else {
            self.turned = None;
            return None;
        };
        let stamp = settings_store::stamp();
        if settings_store::document_changed(self.seen, self.stamp, stamp) {
            self.seen = true;
            self.stamp = stamp;
            self.control
                .set_locked(settings_store::hardware::ROTATION_LOCK.get());
        }
        let now = Instant::now();
        if self.due.is_none_or(|due| now >= due) {
            self.due = now.checked_add(SAMPLE_INTERVAL);
            if let Some(sample) = orientation::read_accel(device) {
                self.control.sample(sample);
            }
        }
        let shown = turn(self.control.shown());
        if self
            .turned
            .as_ref()
            .is_none_or(|(connector, turn)| connector != &panel.name || *turn != shown)
        {
            self.turned = Some((panel.name.clone(), shown));
        }
        self.turned
            .as_ref()
            .map(|(connector, transform)| (connector.as_str(), *transform))
    }
}

/// The output of `outputs` that turns with the device: the built-in panel
/// and nothing else, because a monitor on the desk stands still while the
/// machine in the person's hands is turned around.
fn panel(outputs: &[OutputInfo]) -> Option<&OutputInfo> {
    outputs
        .iter()
        .find(|output| output.enabled && os_actions::brightness::is_builtin_connector(&output.name))
}

/// The turn the panel shows when the device is held `orientation`.
fn turn(orientation: DeviceOrientation) -> Transform {
    match orientation {
        DeviceOrientation::Normal => Transform::Normal,
        DeviceOrientation::LeftUp => Transform::_90,
        DeviceOrientation::BottomUp => Transform::_180,
        DeviceOrientation::RightUp => Transform::_270,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            monitor: name.into(),
            label: name.into(),
            enabled: true,
            rect: Some((0, 0, 800, 600)),
            description: None,
        }
    }

    #[test]
    fn the_turn_follows_the_edge_that_points_up() {
        assert_eq!(turn(DeviceOrientation::Normal), Transform::Normal);
        assert_eq!(turn(DeviceOrientation::LeftUp), Transform::_90);
        assert_eq!(turn(DeviceOrientation::BottomUp), Transform::_180);
        assert_eq!(turn(DeviceOrientation::RightUp), Transform::_270);
    }

    #[test]
    fn only_a_built_in_panel_turns() {
        assert_eq!(panel(&[output("DP-1"), output("HDMI-A-1")]), None);
        let outputs = [output("DP-1"), output("eDP-1")];
        assert_eq!(
            panel(&outputs).map(|output| output.name.as_str()),
            Some("eDP-1")
        );
    }
}
