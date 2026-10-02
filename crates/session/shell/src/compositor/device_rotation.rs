//! Turn a convertible's built-in panel when the device is turned.
//!
//! A worker thread samples the IIO accelerometer, debounces the reading
//! through [`os_actions::RotationControl`] and applies the user's rotation
//! lock (`display.rotation_lock`, the quick-settings tile). Each change is
//! handed to the compositor loop, which asks the backend to transform the
//! built-in panel — never an external monitor. Machines without an
//! accelerometer start no thread at all.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use os_actions::DeviceOrientation;
use settings_store::hardware::OutputTransform;

/// Settings key (in the `display` section) holding the rotation lock.
pub const ROTATION_LOCK_KEY: &str = "rotation_lock";

const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
/// Re-read the lock every this many samples (once a second).
const LOCK_REFRESH_SAMPLES: u32 = 4;

/// The panel transform that keeps content upright for `orientation`.
pub fn output_transform(orientation: DeviceOrientation) -> OutputTransform {
    match orientation {
        DeviceOrientation::Normal => OutputTransform::Normal,
        DeviceOrientation::LeftUp => OutputTransform::Rotate90,
        DeviceOrientation::BottomUp => OutputTransform::Rotate180,
        DeviceOrientation::RightUp => OutputTransform::Rotate270,
    }
}

/// Whether the user locked rotation, from a loaded settings document.
pub fn rotation_locked(settings: &serde_json::Map<String, serde_json::Value>) -> bool {
    settings_store::bool_at(settings, "display", ROTATION_LOCK_KEY).unwrap_or(false)
}

/// Start following the accelerometer. Returns `None` when the machine has
/// none. `wake` is called after each change is queued so an idle
/// compositor loop picks it up promptly.
pub fn spawn(wake: impl Fn() + Send + 'static) -> Option<Receiver<OutputTransform>> {
    let device = os_actions::orientation::accelerometer()?;
    let (tx, rx) = mpsc::channel();
    let label = device.display().to_string();
    let spawned = std::thread::Builder::new()
        .name("tilo-rotation".into())
        .spawn(move || {
            let mut control =
                os_actions::RotationControl::new(rotation_locked(&settings_store::load()));
            let mut seen = true;
            let mut cached_stamp = settings_store::stamp();
            let mut locked = control.locked();
            let mut samples = 0_u32;
            loop {
                samples = samples.wrapping_add(1);
                let mut change = None;
                if samples.checked_rem(LOCK_REFRESH_SAMPLES) == Some(0) {
                    let current = settings_store::stamp();
                    if settings_store::document_changed(seen, cached_stamp, current) {
                        locked = rotation_locked(&settings_store::load());
                        seen = true;
                        cached_stamp = current;
                    }
                    if locked != control.locked() {
                        change = control.set_locked(locked);
                    }
                }
                if let Some(sample) = os_actions::orientation::read_accel(&device) {
                    change = control.sample(sample).or(change);
                }
                if let Some(orientation) = change {
                    tracing::info!(?orientation, "device rotation: turning the built-in panel");
                    if tx.send(output_transform(orientation)).is_err() {
                        return;
                    }
                    wake();
                }
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "device rotation: could not start the accelerometer worker");
        return None;
    }
    tracing::info!(device = %label, "device rotation: following the accelerometer");
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_stays_upright_whichever_way_the_device_is_held() {
        assert_eq!(
            output_transform(DeviceOrientation::Normal),
            OutputTransform::Normal
        );
        assert_eq!(
            output_transform(DeviceOrientation::LeftUp),
            OutputTransform::Rotate90
        );
        assert_eq!(
            output_transform(DeviceOrientation::BottomUp),
            OutputTransform::Rotate180
        );
        assert_eq!(
            output_transform(DeviceOrientation::RightUp),
            OutputTransform::Rotate270
        );
    }

    #[test]
    fn the_lock_is_read_from_the_display_section() {
        let locked = serde_json::json!({"display": {"rotation_lock": true}});
        let unset = serde_json::json!({"display": {}});
        assert!(rotation_locked(locked.as_object().expect("object")));
        assert!(!rotation_locked(unset.as_object().expect("object")));
    }
}
