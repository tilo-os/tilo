//! Display brightness, written straight to the kernel's backlight class.
//!
//! # Why not `brightnessctl`
//!
//! 1. **Not installed.** `os/Containerfile` doesn't pull it in, so on the shipped image
//!    every brightness call would fail silently.
//! 2. **A fork per keypress.** Brightness keys auto-repeat; shelling out at the typematic
//!    rate means spawning, exec'ing and reaping a process tens of times a second from the
//!    compositor, which must not stall.
//! 3. **One more thing to keep in the image.** A bootc image is rebuilt and re-derived; a
//!    package that exists only to `open()`/`write()`/`close()` a file we can open ourselves
//!    is a dependency with no payload.
//!
//! Writing `/sys/class/backlight/<dev>/brightness` is what `brightnessctl` does anyway. The
//! one thing it brings that a bare write doesn't is the udev rule granting the `video` group
//! write access to that attribute — so qOS ships that rule itself
//! (`os/usr/lib/udev/rules.d/`), smaller and under our control.
//!
//! `brightnessctl` is still tried as a fallback when the sysfs write is refused, since a dev
//! host may have it installed with its own rule, and it costs nothing when absent.

use std::fs;
use std::path::{Path, PathBuf};

use crate::process::{run, run_capture, which};

/// The kernel's backlight class. A parameter on the inner functions so the tests can point
/// at a fixture tree.
const BACKLIGHT: &str = "/sys/class/backlight";
const IIO_DEVICES: &str = "/sys/bus/iio/devices";

/// Set the backlight brightness to `pct` percent, clamped to `[0, 100]`.
///
/// # Errors
///
/// Returns an error message if the sysfs write fails and the `brightnessctl` fallback is
/// either not installed or also fails.
pub fn set_brightness(pct: f32) -> Result<(), String> {
    let clamped = pct.clamp(0.0, 100.0);
    match set_brightness_in(Path::new(BACKLIGHT), clamped) {
        Ok(()) => Ok(()),
        Err(sysfs_err) => {
            // Only worth a second attempt if the tool is actually there; otherwise report the real
            // reason rather than "not installed", which would point at the wrong problem.
            if !which("brightnessctl") {
                return Err(sysfs_err);
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0.0..=100.0 above, so the rounded value always fits in u32 \
                          with no sign loss"
            )]
            let percent = clamped.round() as u32;
            run("brightnessctl", &["set", &format!("{percent}%")])
                .map_err(|tool_err| format!("{sysfs_err}; brightnessctl also failed: {tool_err}"))
        }
    }
}

/// Read the current brightness as a `0.0..=1.0` fraction.
///
/// # Errors
///
/// Returns an error message if no backlight device is found and the `brightnessctl` fallback is
/// either not installed, fails, or its output cannot be parsed.
pub fn get_brightness() -> Result<f32, String> {
    match get_brightness_in(Path::new(BACKLIGHT)) {
        Ok(v) => Ok(v),
        Err(sysfs_err) => {
            if !which("brightnessctl") {
                return Err(sysfs_err);
            }
            let out = run_capture("brightnessctl", &["-m"])?;
            parse_brightness_machine(&out)
        }
    }
}

/// Whether this machine has a controllable backlight at all. Desktops and most VMs do not,
/// and the quick-settings panel uses this to leave the slider out rather than show a dead one.
pub fn has_backlight() -> bool {
    preferred_device(Path::new(BACKLIGHT)).is_some()
}

/// Read ambient illuminance in lux from the kernel IIO ABI.  Drivers expose
/// either a processed `in_illuminance_input`, or `raw × scale`; both are
/// standard IIO forms.  No sensor means `None`, which Settings renders as an
/// unavailable capability rather than a working-looking toggle.
pub fn ambient_lux() -> Option<f32> {
    ambient_lux_from(Path::new(IIO_DEVICES))
}

pub fn has_ambient_light_sensor() -> bool {
    ambient_lux().is_some()
}

fn ambient_lux_from(root: &Path) -> Option<f32> {
    let mut devices: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    devices.sort();
    for device in devices {
        if let Some(v) = read_f32(&device.join("in_illuminance_input")) {
            return v.is_finite().then_some(v.max(0.0));
        }
        if let Some(raw) = read_f32(&device.join("in_illuminance_raw")) {
            let scale = read_f32(&device.join("in_illuminance_scale")).unwrap_or(1.0);
            let value = raw * scale;
            if value.is_finite() {
                return Some(value.max(0.0));
            }
        }
    }
    None
}

fn read_f32(path: &Path) -> Option<f32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Stateful lux→brightness policy.  Logarithmic mapping matches human light
/// perception; the EMA and deadband prevent a desk shadow from making the
/// panel visibly pulse.  The controller emits only meaningful changes.
#[derive(Clone, Debug, Default)]
pub struct AmbientBrightnessController {
    filtered_lux: Option<f32>,
    last_output: Option<f32>,
}

impl AmbientBrightnessController {
    pub fn update(&mut self, lux: f32) -> Option<f32> {
        if !lux.is_finite() || lux < 0.0 {
            return None;
        }
        let filtered = self.filtered_lux.map_or(lux, |old| old * 0.78 + lux * 0.22);
        self.filtered_lux = Some(filtered);
        // 0 lux → 12%; ~10 lux → 26%; ~100 lux → 49%; ~1000 lux → 73%;
        // 10k lux → 96%.  Clamp leaves headroom at both ends.
        let target = (12.0 + 21.0 * (filtered + 1.0).log10()).clamp(12.0, 100.0);
        if self
            .last_output
            .is_some_and(|old| (old - target).abs() < 4.0)
        {
            return None;
        }
        self.last_output = Some(target);
        Some(target)
    }

    pub fn reset(&mut self) {
        self.filtered_lux = None;
        self.last_output = None;
    }
}

fn get_brightness_in(root: &Path) -> Result<f32, String> {
    let dev = preferred_device(root).ok_or("no backlight device")?;
    let max = read_u32(&dev, "max_brightness").ok_or("backlight max_brightness unreadable")?;
    if max == 0 {
        return Err("backlight max_brightness is 0".to_string());
    }
    let now = read_u32(&dev, "brightness").ok_or("backlight brightness unreadable")?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "backlight raw values are far below 2^24, so the f32 division loses no \
                  precision in practice"
    )]
    let fraction = (now as f32 / max as f32).clamp(0.0, 1.0);
    Ok(fraction)
}

fn set_brightness_in(root: &Path, pct: f32) -> Result<(), String> {
    let dev = preferred_device(root).ok_or("no backlight device")?;
    let max = read_u32(&dev, "max_brightness").ok_or("backlight max_brightness unreadable")?;
    if max == 0 {
        return Err("backlight max_brightness is 0".to_string());
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "max_brightness is far below 2^24, and the result is clamped into 0.0..=max \
                  before the final cast, so no precision is lost and the value never goes negative"
    )]
    let raw = ((pct / 100.0) * max as f32).round().clamp(0.0, max as f32) as u32;
    // Never write a hard 0 from a percentage above zero: some panels treat 0 as "backlight off"
    // with no way for the user to see the screen well enough to bring it back.
    let raw = if pct > 0.0 { raw.max(1) } else { raw };
    let path = dev.join("brightness");
    fs::write(&path, raw.to_string()).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Pick which backlight to drive when the kernel exposes several. `acpi_video*` is the
/// firmware's generic interface, usually a no-op or coarse duplicate of the native one, so
/// the native device (`intel_backlight`, `amdgpu_bl0`, `nv_backlight`, …) is preferred. Ties
/// break on name so the choice is stable rather than following readdir order.
fn preferred_device(root: &Path) -> Option<PathBuf> {
    let mut devices: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("brightness").exists() && p.join("max_brightness").exists())
        .collect();
    devices.sort();
    devices
        .iter()
        .find(|p| !is_firmware_device(p))
        .or_else(|| devices.first())
        .cloned()
}

fn is_firmware_device(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("acpi_video"))
}

fn read_u32(dir: &Path, attr: &str) -> Option<u32> {
    fs::read_to_string(dir.join(attr)).ok()?.trim().parse().ok()
}

fn parse_brightness_machine(stdout: &str) -> Result<f32, String> {
    // Format example: `acpi_video0,backlight,4,40%,10`
    let line = stdout.lines().next().ok_or("brightnessctl -m empty")?;
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 4 {
        return Err(format!("brightnessctl -m unexpected: {line}"));
    }
    // Field 3 is the percentage with a trailing `%`.
    let pct_str = fields
        .get(3)
        .ok_or_else(|| format!("brightnessctl -m unexpected: {line}"))?
        .trim_end_matches('%');
    let pct: f32 = pct_str
        .parse()
        .map_err(|e| format!("brightness pct parse: {e} ({pct_str:?})"))?;
    Ok((pct / 100.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture backlight class tree: `(name, current, max)`.
    fn tree(devices: &[(&str, u32, u32)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, now, max) in devices {
            let d = dir.path().join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("brightness"), format!("{now}\n")).unwrap();
            fs::write(d.join("max_brightness"), format!("{max}\n")).unwrap();
        }
        dir
    }

    #[test]
    fn brightness_is_the_raw_value_over_max() {
        let t = tree(&[("intel_backlight", 4800, 24000)]);
        let v = get_brightness_in(t.path()).unwrap();
        assert!((v - 0.20).abs() < 1e-6, "4800/24000 = 20%, got {v}");
    }

    #[test]
    fn setting_a_percentage_writes_the_scaled_raw_value() {
        let t = tree(&[("intel_backlight", 0, 24000)]);
        set_brightness_in(t.path(), 75.0).unwrap();
        let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
        assert_eq!(raw, "18000", "75% of 24000");
        // And it round-trips through the getter.
        let v = get_brightness_in(t.path()).unwrap();
        assert!((v - 0.75).abs() < 1e-4);
    }

    #[test]
    fn a_native_device_is_preferred_over_the_firmware_one() {
        let t = tree(&[("acpi_video0", 5, 10), ("intel_backlight", 100, 1000)]);
        let dev = preferred_device(t.path()).unwrap();
        assert_eq!(
            dev.file_name().unwrap(),
            "intel_backlight",
            "acpi_video* is the coarse firmware shim, not the panel"
        );
        let v = get_brightness_in(t.path()).unwrap();
        assert!((v - 0.10).abs() < 1e-6, "read from the native device");
    }

    #[test]
    fn a_firmware_only_machine_still_works() {
        let t = tree(&[("acpi_video0", 5, 10)]);
        assert!((get_brightness_in(t.path()).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_nonzero_percentage_never_writes_a_hard_zero() {
        // 0.4% of 100 rounds to 0, which on some panels is "off".
        let t = tree(&[("intel_backlight", 50, 100)]);
        set_brightness_in(t.path(), 0.4).unwrap();
        let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
        assert_eq!(raw, "1", "a dim request must stay visible, not go dark");
    }

    #[test]
    fn an_explicit_zero_is_honoured() {
        let t = tree(&[("intel_backlight", 50, 100)]);
        set_brightness_in(t.path(), 0.0).unwrap();
        let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
        assert_eq!(raw, "0");
    }

    #[test]
    fn a_nonsense_percentage_never_writes_past_the_device_maximum() {
        // The kernel rejects a value above max_brightness with EINVAL, so an out-of-range request
        // must saturate rather than fail the whole write and leave the panel where it was.
        let t = tree(&[("intel_backlight", 0, 100)]);
        set_brightness_in(t.path(), 500.0).unwrap();
        assert_eq!(
            fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap(),
            "100",
            "saturates at max_brightness"
        );
    }

    #[test]
    fn a_machine_with_no_backlight_reports_so_rather_than_guessing() {
        let t = tree(&[]);
        assert!(!has_backlight_in(t.path()));
        assert!(get_brightness_in(t.path()).is_err());
        assert!(set_brightness_in(t.path(), 50.0).is_err());
    }

    fn has_backlight_in(root: &Path) -> bool {
        preferred_device(root).is_some()
    }

    #[test]
    fn a_directory_missing_max_brightness_is_not_a_backlight() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("bogus");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("brightness"), "5\n").unwrap();
        assert!(preferred_device(dir.path()).is_none());
    }

    #[test]
    fn parse_brightness_machine_returns_fraction() {
        let s = "acpi_video0,backlight,4,40%,10";
        let v = parse_brightness_machine(s).unwrap();
        assert!((v - 0.40).abs() < 1e-6);
    }

    #[test]
    fn parse_brightness_machine_handles_zero() {
        let s = "acpi_video0,backlight,0,0%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 0.0);
    }

    #[test]
    fn parse_brightness_machine_clamps_above_one() {
        // brightnessctl will never emit >100%, but a hypothetical 110% must not surface as a >1.0 fraction.
        let s = "x,b,11,110%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 1.0);
    }

    #[test]
    fn parse_brightness_machine_empty_is_err() {
        assert!(parse_brightness_machine("").is_err());
    }

    /// The public setter must never panic, whatever the host looks like — it runs on machines
    /// with a backlight, without one, and with a root-owned one this process cannot write.
    #[test]
    fn set_brightness_never_panics_on_this_host() {
        let result = set_brightness(50.0);
        if let Err(e) = result {
            assert!(!e.is_empty(), "a failure must say why");
        }
    }

    #[test]
    fn ambient_sensor_accepts_processed_and_scaled_iio_forms() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("iio:device0");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("in_illuminance_raw"), "200\n").unwrap();
        fs::write(d.join("in_illuminance_scale"), "0.5\n").unwrap();
        assert_eq!(ambient_lux_from(root.path()), Some(100.0));
        fs::write(d.join("in_illuminance_input"), "42.5\n").unwrap();
        assert_eq!(ambient_lux_from(root.path()), Some(42.5));
    }

    #[test]
    fn ambient_controller_smooths_and_ignores_small_changes() {
        let mut c = AmbientBrightnessController::default();
        let first = c.update(100.0).unwrap();
        assert!((40.0..60.0).contains(&first));
        assert_eq!(
            c.update(102.0),
            None,
            "sensor noise stays inside the deadband"
        );
        let mut changed = None;
        for _ in 0..12 {
            changed = c.update(10_000.0).or(changed);
        }
        assert!(changed.is_some(), "a sustained daylight change is applied");
    }
}
