//! Which way up a convertible is held, from the kernel IIO accelerometer.
//!
//! The reader finds an `in_accel_*` device under `/sys/bus/iio/devices`,
//! applies its mount matrix and hands samples to [`OrientationTracker`],
//! which classifies them the way `iio-sensor-proxy` does (the dominant
//! tilt axis, nothing while the device lies flat) and adds the two things
//! that stop a screen from flapping in the hand: hysteresis, so a device
//! held near 45° stays where it is, and a debounce of consecutive samples.
//! [`RotationControl`] layers the user's rotation lock on top.

use std::fs;
use std::path::{Path, PathBuf};

const IIO_DEVICES: &str = "/sys/bus/iio/devices";

/// How the device is held, named by the edge of the built-in panel that
/// points up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceOrientation {
    /// The panel's top edge is up — the laptop/landscape default.
    #[default]
    Normal,
    /// Upside down.
    BottomUp,
    /// Turned clockwise: the panel's left edge is up.
    LeftUp,
    /// Turned counter-clockwise: the panel's right edge is up.
    RightUp,
}

/// One accelerometer reading in the panel's frame, any consistent unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccelSample {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// The first IIO device under `root` exposing an accelerometer.
pub fn accelerometer_in(root: &Path) -> Option<PathBuf> {
    let mut devices: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|device| {
            ["x", "y", "z"]
                .iter()
                .all(|axis| device.join(format!("in_accel_{axis}_raw")).exists())
        })
        .collect();
    devices.sort();
    devices.into_iter().next()
}

/// The machine's accelerometer, if it has one.
pub fn accelerometer() -> Option<PathBuf> {
    accelerometer_in(Path::new(IIO_DEVICES))
}

/// Whether this machine can tell which way up it is held. The rotation
/// lock only makes sense when this is true.
pub fn has_accelerometer() -> bool {
    accelerometer().is_some()
}

/// Read one sample from an accelerometer device: `raw × scale` per axis,
/// rotated into the panel's frame by the driver's mount matrix.
pub fn read_accel(device: &Path) -> Option<AccelSample> {
    let shared_scale = read_f64(&device.join("in_accel_scale")).unwrap_or(1.0);
    let axis = |name: &str| -> Option<f64> {
        let raw = read_f64(&device.join(format!("in_accel_{name}_raw")))?;
        let scale =
            read_f64(&device.join(format!("in_accel_{name}_scale"))).unwrap_or(shared_scale);
        Some(raw * scale)
    };
    let sample = AccelSample {
        x: axis("x")?,
        y: axis("y")?,
        z: axis("z")?,
    };
    let matrix = fs::read_to_string(device.join("in_accel_mount_matrix"))
        .or_else(|_| fs::read_to_string(device.join("mount_matrix")))
        .ok()
        .and_then(|text| parse_mount_matrix(&text));
    Some(matrix.map_or(sample, |m| apply_matrix(&m, sample)))
}

fn read_f64(path: &Path) -> Option<f64> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
}

/// Parse the IIO `"x1, x2, x3; y1, y2, y3; z1, z2, z3"` mount matrix.
fn parse_mount_matrix(text: &str) -> Option<[[f64; 3]; 3]> {
    let mut rows = text.trim().split(';');
    let mut matrix = [[0.0; 3]; 3];
    for row in &mut matrix {
        let mut cells = rows.next()?.split(',');
        for cell in row.iter_mut() {
            *cell = cells.next()?.trim().parse().ok()?;
        }
        if cells.next().is_some() {
            return None;
        }
    }
    rows.next().is_none().then_some(matrix)
}

fn apply_matrix(m: &[[f64; 3]; 3], s: AccelSample) -> AccelSample {
    let row = |r: &[f64; 3]| r[0] * s.x + r[1] * s.y + r[2] * s.z;
    AccelSample {
        x: row(&m[0]),
        y: row(&m[1]),
        z: row(&m[2]),
    }
}

/// Tilt, in degrees, below which the device counts as lying flat and its
/// orientation is left alone.
const FLAT_DEGREES: f64 = 35.0;
/// How much further a new orientation's axis must tilt than the current
/// one's before the screen turns.
const HYSTERESIS_DEGREES: f64 = 15.0;
/// Consecutive agreeing samples required before a turn.
const DEBOUNCE_SAMPLES: u8 = 3;

/// How far the device leans towards each orientation, in degrees. IIO
/// reports the reaction to gravity, so an upright panel reads negative `y`.
fn tilts(s: AccelSample) -> [(DeviceOrientation, f64); 4] {
    let bottom_up = s.y.atan2(s.x.hypot(s.z)).to_degrees();
    let left_up = s.x.atan2(s.y.hypot(s.z)).to_degrees();
    [
        (DeviceOrientation::Normal, -bottom_up),
        (DeviceOrientation::BottomUp, bottom_up),
        (DeviceOrientation::LeftUp, left_up),
        (DeviceOrientation::RightUp, -left_up),
    ]
}

/// The orientation `sample` suggests given the one currently shown, or
/// `None` when it suggests keeping it (flat, ambiguous, or not tilted far
/// enough past the hysteresis band).
pub fn classify(sample: AccelSample, current: DeviceOrientation) -> Option<DeviceOrientation> {
    if !(sample.x.is_finite() && sample.y.is_finite() && sample.z.is_finite()) {
        return None;
    }
    let tilts = tilts(sample);
    let (best, best_tilt) = tilts.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    if best_tilt < FLAT_DEGREES || best == current {
        return None;
    }
    let current_tilt = tilts
        .iter()
        .find(|(orientation, _)| *orientation == current)
        .map_or(f64::NEG_INFINITY, |(_, tilt)| *tilt);
    (best_tilt - current_tilt >= HYSTERESIS_DEGREES).then_some(best)
}

/// Debounced orientation from a stream of samples.
#[derive(Clone, Debug, Default)]
pub struct OrientationTracker {
    current: DeviceOrientation,
    candidate: Option<(DeviceOrientation, u8)>,
}

impl OrientationTracker {
    pub fn new(current: DeviceOrientation) -> Self {
        Self {
            current,
            candidate: None,
        }
    }

    pub fn current(&self) -> DeviceOrientation {
        self.current
    }

    /// Feed one sample; returns the new orientation when it changes.
    pub fn update(&mut self, sample: AccelSample) -> Option<DeviceOrientation> {
        let Some(suggested) = classify(sample, self.current) else {
            self.candidate = None;
            return None;
        };
        let count = match self.candidate {
            Some((candidate, count)) if candidate == suggested => count.saturating_add(1),
            _ => 1,
        };
        if count >= DEBOUNCE_SAMPLES {
            self.current = suggested;
            self.candidate = None;
            Some(suggested)
        } else {
            self.candidate = Some((suggested, count));
            None
        }
    }
}

/// Device orientation with the user's rotation lock applied: the panel
/// follows the device unless locked, and catches up when unlocked.
#[derive(Clone, Debug, Default)]
pub struct RotationControl {
    tracker: OrientationTracker,
    locked: bool,
    shown: DeviceOrientation,
}

impl RotationControl {
    pub fn new(locked: bool) -> Self {
        Self {
            locked,
            ..Self::default()
        }
    }

    /// The orientation the panel should show.
    pub fn shown(&self) -> DeviceOrientation {
        self.shown
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Feed one sample; returns the orientation to show when it changes.
    pub fn sample(&mut self, sample: AccelSample) -> Option<DeviceOrientation> {
        self.tracker.update(sample);
        self.follow()
    }

    /// Lock or unlock rotation; unlocking returns the device's orientation
    /// when it differs from what the panel shows.
    pub fn set_locked(&mut self, locked: bool) -> Option<DeviceOrientation> {
        self.locked = locked;
        self.follow()
    }

    fn follow(&mut self) -> Option<DeviceOrientation> {
        let device = self.tracker.current();
        if self.locked || device == self.shown {
            return None;
        }
        self.shown = device;
        Some(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: f64 = 9.81;

    fn held(orientation: DeviceOrientation) -> AccelSample {
        match orientation {
            DeviceOrientation::Normal => AccelSample {
                x: 0.0,
                y: -G,
                z: 0.0,
            },
            DeviceOrientation::BottomUp => AccelSample {
                x: 0.0,
                y: G,
                z: 0.0,
            },
            DeviceOrientation::LeftUp => AccelSample {
                x: G,
                y: 0.0,
                z: 0.0,
            },
            DeviceOrientation::RightUp => AccelSample {
                x: -G,
                y: 0.0,
                z: 0.0,
            },
        }
    }

    /// A reading `degrees` of the way from upright towards left-up.
    fn tilted_left(degrees: f64) -> AccelSample {
        let r = degrees.to_radians();
        AccelSample {
            x: G * r.sin(),
            y: -G * r.cos(),
            z: 0.0,
        }
    }

    fn fake_iio(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("tempdir");
        let light = root.path().join("iio:device0");
        fs::create_dir(&light).expect("light sensor");
        fs::write(light.join("in_illuminance_raw"), "12\n").expect("lux");
        let accel = root.path().join("iio:device1");
        fs::create_dir(&accel).expect("accelerometer");
        for (name, value) in files {
            fs::write(accel.join(name), value).expect("attribute");
        }
        root
    }

    #[test]
    fn each_way_up_is_recognised_from_the_upright_default() {
        for orientation in [
            DeviceOrientation::BottomUp,
            DeviceOrientation::LeftUp,
            DeviceOrientation::RightUp,
        ] {
            assert_eq!(
                classify(held(orientation), DeviceOrientation::Normal),
                Some(orientation)
            );
        }
        assert_eq!(
            classify(held(DeviceOrientation::Normal), DeviceOrientation::Normal),
            None
        );
    }

    #[test]
    fn a_device_lying_flat_keeps_its_orientation() {
        let flat = AccelSample {
            x: 1.0,
            y: 1.5,
            z: -G,
        };
        assert_eq!(classify(flat, DeviceOrientation::LeftUp), None);
    }

    #[test]
    fn hysteresis_holds_the_orientation_near_forty_five_degrees() {
        // Just past the diagonal: tilted more left than up, but not by the
        // hysteresis margin — the screen must not turn.
        assert_eq!(classify(tilted_left(50.0), DeviceOrientation::Normal), None);
        // And coming back, the same reading keeps left-up.
        assert_eq!(classify(tilted_left(40.0), DeviceOrientation::LeftUp), None);
        // Clearly turned: rotate.
        assert_eq!(
            classify(tilted_left(60.0), DeviceOrientation::Normal),
            Some(DeviceOrientation::LeftUp)
        );
    }

    #[test]
    fn a_turn_needs_consecutive_agreeing_samples() {
        let mut tracker = OrientationTracker::default();
        let left = held(DeviceOrientation::LeftUp);
        assert_eq!(tracker.update(left), None);
        assert_eq!(tracker.update(left), None);
        // A jolt back to upright restarts the count.
        assert_eq!(tracker.update(held(DeviceOrientation::Normal)), None);
        assert_eq!(tracker.update(left), None);
        assert_eq!(tracker.update(left), None);
        assert_eq!(tracker.update(left), Some(DeviceOrientation::LeftUp));
        assert_eq!(tracker.update(left), None, "no repeat once turned");
    }

    #[test]
    fn the_lock_holds_the_panel_and_unlocking_catches_up() {
        let mut control = RotationControl::new(true);
        let right = held(DeviceOrientation::RightUp);
        for _ in 0..DEBOUNCE_SAMPLES {
            assert_eq!(control.sample(right), None, "locked panels never turn");
        }
        assert_eq!(control.shown(), DeviceOrientation::Normal);
        assert_eq!(control.set_locked(false), Some(DeviceOrientation::RightUp));
        assert_eq!(control.shown(), DeviceOrientation::RightUp);
        // Locking again pins the current orientation.
        assert_eq!(control.set_locked(true), None);
        for _ in 0..DEBOUNCE_SAMPLES {
            assert_eq!(control.sample(held(DeviceOrientation::Normal)), None);
        }
        assert_eq!(control.shown(), DeviceOrientation::RightUp);
    }

    #[test]
    fn an_unlocked_panel_follows_the_device() {
        let mut control = RotationControl::new(false);
        let bottom = held(DeviceOrientation::BottomUp);
        let turns: Vec<_> = (0..DEBOUNCE_SAMPLES)
            .filter_map(|_| control.sample(bottom))
            .collect();
        assert_eq!(turns, vec![DeviceOrientation::BottomUp]);
    }

    #[test]
    fn the_reader_finds_the_accelerometer_and_applies_scale_and_mount_matrix() {
        let root = fake_iio(&[
            ("in_accel_x_raw", "0\n"),
            ("in_accel_y_raw", "-1000\n"),
            ("in_accel_z_raw", "12\n"),
            ("in_accel_scale", "0.00981\n"),
            // Mounted rotated a quarter turn: sensor y is panel x.
            ("in_accel_mount_matrix", "0, 1, 0; -1, 0, 0; 0, 0, 1\n"),
        ]);
        let device = accelerometer_in(root.path()).expect("accelerometer found");
        assert!(device.ends_with("iio:device1"), "light sensor skipped");
        let sample = read_accel(&device).expect("sample");
        assert!((sample.x - -9.81).abs() < 1e-9);
        assert!(sample.y.abs() < 1e-9);
        assert_eq!(
            classify(sample, DeviceOrientation::Normal),
            Some(DeviceOrientation::RightUp)
        );
    }

    #[test]
    fn no_accelerometer_means_none() {
        let root = tempfile::tempdir().expect("tempdir");
        assert_eq!(accelerometer_in(root.path()), None);
        assert_eq!(accelerometer_in(&root.path().join("missing")), None);
    }

    #[test]
    fn malformed_mount_matrices_are_ignored() {
        assert_eq!(parse_mount_matrix("1, 0; 0, 1"), None);
        assert_eq!(
            parse_mount_matrix("1, 0, 0; 0, 1, 0; 0, 0, 1; 1, 1, 1"),
            None
        );
        assert!(parse_mount_matrix("1, 0, 0; 0, 1, 0; 0, 0, 1").is_some());
    }
}
