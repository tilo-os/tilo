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
//! write access to that attribute — so Tilo ships that rule itself
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
            run(
                "brightnessctl",
                &["-c", "backlight", "set", &format!("{percent}%")],
            )
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
            // Without a class, brightnessctl falls back to any device, which on a
            // machine with no backlight is a keyboard LED.
            let out = run_capture("brightnessctl", &["-m", "-c", "backlight"])?;
            parse_brightness_machine(&out)
        }
    }
}

/// The panel backlight as the kernel holds it, for the flight record:
/// `device=<name> brightness=<n>/<max> actual=<n> bl_power=<n>`, or `None`
/// when there is no backlight. A blank or wake logs it, so a screen that
/// stays dark after a wake says whether its backlight came back.
pub fn backlight_state() -> Option<String> {
    backlight_state_in(Path::new(BACKLIGHT))
}

fn backlight_state_in(root: &Path) -> Option<String> {
    let dev = preferred_device(root)?;
    let field = |attr: &str| read_u32(&dev, attr).map_or_else(|| "?".to_owned(), |v| v.to_string());
    Some(format!(
        "device={} brightness={}/{} actual={} bl_power={}",
        dev.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        field("brightness"),
        field("max_brightness"),
        field("actual_brightness"),
        field("bl_power"),
    ))
}

/// Whether the panel backlight is powered, as the kernel holds it:
/// `Some(true)` for `bl_power` 0 (`FB_BLANK_UNBLANK`), `Some(false)` for any
/// blank level, `None` when there is no backlight or it says nothing. A wake
/// that leaves the backlight blanked has not lit the panel, whatever the CRTC
/// scans out.
pub fn backlight_powered() -> Option<bool> {
    backlight_powered_in(Path::new(BACKLIGHT))
}

fn backlight_powered_in(root: &Path) -> Option<bool> {
    read_u32(&preferred_device(root)?, "bl_power").map(|power| power == 0)
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
        if let Some(v) = read_f32(&device.join("in_illuminance_input")).filter(|v| v.is_finite()) {
            return Some(v.max(0.0));
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
    get_backlight(&preferred_device(root).ok_or("no backlight device")?)
}

/// Read one backlight device as a `0.0..=1.0` fraction.
pub(crate) fn get_backlight(dev: &Path) -> Result<f32, String> {
    let max = read_u32(dev, "max_brightness").ok_or("backlight max_brightness unreadable")?;
    if max == 0 {
        return Err("backlight max_brightness is 0".to_string());
    }
    let now = read_u32(dev, "brightness").ok_or("backlight brightness unreadable")?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "backlight raw values are far below 2^24, so the f32 division loses no \
                  precision in practice"
    )]
    let fraction = (now as f32 / max as f32).clamp(0.0, 1.0);
    Ok(fraction)
}

fn set_brightness_in(root: &Path, pct: f32) -> Result<(), String> {
    set_backlight(&preferred_device(root).ok_or("no backlight device")?, pct)
}

/// Write `pct` percent to one backlight device.
pub(crate) fn set_backlight(dev: &Path, pct: f32) -> Result<(), String> {
    set_backlight_with(dev, pct, logind_set_brightness)
}

fn set_backlight_with(
    dev: &Path,
    pct: f32,
    refused: impl FnOnce(&str, u32) -> Result<(), String>,
) -> Result<(), String> {
    let max = read_u32(dev, "max_brightness").ok_or("backlight max_brightness unreadable")?;
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
    let Err(write_err) = fs::write(&path, raw.to_string()) else {
        return Ok(());
    };
    let name = dev
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("backlight device has no name")?;
    refused(name, raw)
        .map_err(|logind_err| format!("write {}: {write_err}; {logind_err}", path.display()))
}

/// A host session (Silverblue, Bazzite) has no udev rule handing the attribute to
/// the user, and the user is usually not in `video`. logind's `SetBrightness`
/// lets the seat's active session set it; it is how GNOME and KDE dim there. The
/// connection is kept because brightness keys auto-repeat.
fn logind_set_brightness(name: &str, raw: u32) -> Result<(), String> {
    static SYSTEM_BUS: std::sync::Mutex<Option<zbus::Connection>> = std::sync::Mutex::new(None);
    let name = name.to_owned();
    crate::block_on(async move {
        let cached = SYSTEM_BUS.lock().ok().and_then(|bus| bus.clone());
        let bus = if let Some(bus) = cached {
            bus
        } else {
            let bus = zbus::Connection::system()
                .await
                .map_err(|e| format!("cannot reach the system bus: {e}"))?;
            if let Ok(mut slot) = SYSTEM_BUS.lock() {
                *slot = Some(bus.clone());
            }
            bus
        };
        bus.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.login1.Session"),
            "SetBrightness",
            &("backlight", name.as_str(), raw),
        )
        .await
        .map_err(|e| format!("logind SetBrightness: {e}"))?;
        Ok(())
    })
}

/// Pick which backlight to drive when the kernel exposes several. `acpi_video*` is the
/// firmware's generic interface, usually a no-op or coarse duplicate of the native one, so
/// the native device (`intel_backlight`, `amdgpu_bl0`, `nv_backlight`, …) is preferred. Ties
/// break on name so the choice is stable rather than following readdir order.
pub(crate) fn preferred_device(root: &Path) -> Option<PathBuf> {
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
    if fields.get(1) != Some(&"backlight") {
        return Err(format!("brightnessctl -m found no backlight: {line}"));
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
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
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

    /// A blank or wake records what the kernel holds for the backlight, so
    /// a panel that stays dark says whether its backlight came back.
    #[test]
    fn the_backlight_state_reads_brightness_actual_and_power() {
        let dir = tree(&[("dp_aux_backlight", 0, 2047)]);
        let dev = dir.path().join("dp_aux_backlight");
        fs::write(dev.join("actual_brightness"), "0\n").unwrap();
        fs::write(dev.join("bl_power"), "4\n").unwrap();
        assert_eq!(
            backlight_state_in(dir.path()).as_deref(),
            Some("device=dp_aux_backlight brightness=0/2047 actual=0 bl_power=4")
        );
        let none = tempfile::tempdir().unwrap();
        assert_eq!(backlight_state_in(none.path()), None);
    }

    #[test]
    fn the_backlight_is_powered_only_while_bl_power_is_unblank() {
        let dir = tree(&[("dp_aux_backlight", 2047, 2047)]);
        let dev = dir.path().join("dp_aux_backlight");
        assert_eq!(
            backlight_powered_in(dir.path()),
            None,
            "no bl_power says nothing"
        );
        fs::write(dev.join("bl_power"), "4\n").unwrap();
        assert_eq!(backlight_powered_in(dir.path()), Some(false));
        fs::write(dev.join("bl_power"), "0\n").unwrap();
        assert_eq!(backlight_powered_in(dir.path()), Some(true));
        let none = tempfile::tempdir().unwrap();
        assert_eq!(backlight_powered_in(none.path()), None);
    }

    #[test]
    fn a_refused_sysfs_write_asks_logind_for_the_same_raw_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dev = dir.path().join("nvidia_wmi_ec_backlight");
        // A directory in place of the attribute refuses the write even as root.
        fs::create_dir_all(dev.join("brightness")).unwrap();
        fs::write(dev.join("max_brightness"), "200\n").unwrap();

        let mut asked = None;
        set_backlight_with(&dev, 40.0, |name, raw| {
            asked = Some((name.to_owned(), raw));
            Ok(())
        })
        .expect("logind accepted the level");
        assert_eq!(asked, Some(("nvidia_wmi_ec_backlight".to_owned(), 80)));

        let err = set_backlight_with(
            &dev,
            40.0,
            |_, _| Err("logind SetBrightness: denied".into()),
        )
        .expect_err("both paths refused");
        assert!(
            err.contains("write ") && err.contains("logind SetBrightness: denied"),
            "{err}"
        );
    }

    #[test]
    fn a_writable_attribute_never_reaches_logind() {
        let dir = tree(&[("intel_backlight", 10, 100)]);
        set_backlight_with(&dir.path().join("intel_backlight"), 55.0, |_, _| {
            panic!("logind asked although sysfs accepted the write")
        })
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("intel_backlight/brightness")).unwrap(),
            "55"
        );
    }

    #[test]
    fn backlight_devices_are_chosen_read_and_written_within_bounds() {
        {
            let t = tree(&[("intel_backlight", 4800, 24000)]);
            let v = get_brightness_in(t.path()).unwrap();
            assert!((v - 0.20).abs() < 1e-6, "4800/24000 = 20%, got {v}");
        }
        {
            let t = tree(&[("intel_backlight", 0, 24000)]);
            set_brightness_in(t.path(), 75.0).unwrap();
            let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
            assert_eq!(raw, "18000", "75% of 24000");
            let v = get_brightness_in(t.path()).unwrap();
            assert!((v - 0.75).abs() < 1e-4);
        }
        {
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
        {
            let t = tree(&[("acpi_video0", 5, 10)]);
            assert!((get_brightness_in(t.path()).unwrap() - 0.5).abs() < 1e-6);
        }
        {
            // 0.4% of 100 rounds to 0, which on some panels is "off".
            let t = tree(&[("intel_backlight", 50, 100)]);
            set_brightness_in(t.path(), 0.4).unwrap();
            let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
            assert_eq!(raw, "1", "a dim request must stay visible, not go dark");
        }
        {
            let t = tree(&[("intel_backlight", 50, 100)]);
            set_brightness_in(t.path(), 0.0).unwrap();
            let raw = fs::read_to_string(t.path().join("intel_backlight/brightness")).unwrap();
            assert_eq!(raw, "0");
        }
        {
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
        {
            let t = tree(&[]);
            assert!(!has_backlight_in(t.path()));
            assert!(get_brightness_in(t.path()).is_err());
            assert!(set_brightness_in(t.path(), 50.0).is_err());
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path().join("bogus");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("brightness"), "5\n").unwrap();
            assert!(preferred_device(dir.path()).is_none());
        }
    }

    fn has_backlight_in(root: &Path) -> bool {
        preferred_device(root).is_some()
    }

    #[test]
    fn brightnessctl_machine_output_parses_to_a_clamped_fraction() {
        {
            let s = "acpi_video0,backlight,4,40%,10";
            let v = parse_brightness_machine(s).unwrap();
            assert!((v - 0.40).abs() < 1e-6);
        }
        {
            let s = "acpi_video0,backlight,0,0%,10";
            assert!(parse_brightness_machine(s).unwrap().abs() < f32::EPSILON);
        }
        {
            // brightnessctl will never emit >100%, but a hypothetical 110% must not surface as a >1.0 fraction.
            let s = "x,backlight,11,110%,10";
            assert!((parse_brightness_machine(s).unwrap() - 1.0).abs() < f32::EPSILON);
        }
        {
            assert!(parse_brightness_machine("").is_err());
        }
        // A machine with no backlight: a keyboard LED is not a display.
        {
            assert!(parse_brightness_machine("input3::capslock,leds,0,0%,1").is_err());
        }
    }

    #[test]
    fn ambient_light_skips_nonfinite_sensor_readings() {
        let root = tempfile::tempdir().unwrap();
        let invalid = root.path().join("iio:device0");
        let valid = root.path().join("iio:device1");
        fs::create_dir_all(&invalid).unwrap();
        fs::create_dir_all(&valid).unwrap();
        fs::write(valid.join("in_illuminance_input"), "125\n").unwrap();
        for reading in ["NaN", "inf", "-inf"] {
            fs::write(invalid.join("in_illuminance_input"), reading).unwrap();
            assert_eq!(ambient_lux_from(root.path()), Some(125.0));
        }
        fs::remove_file(invalid.join("in_illuminance_input")).unwrap();
        for (raw, scale) in [("NaN", "1"), ("1", "NaN"), ("1e30", "1e30")] {
            fs::write(invalid.join("in_illuminance_raw"), raw).unwrap();
            fs::write(invalid.join("in_illuminance_scale"), scale).unwrap();
            assert_eq!(ambient_lux_from(root.path()), Some(125.0));
        }
        fs::remove_dir_all(valid).unwrap();
        assert_eq!(ambient_lux_from(root.path()), None);
    }

    #[test]
    fn ambient_light_is_read_from_iio_and_smoothed() {
        {
            let root = tempfile::tempdir().unwrap();
            let d = root.path().join("iio:device0");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("in_illuminance_raw"), "200\n").unwrap();
            fs::write(d.join("in_illuminance_scale"), "0.5\n").unwrap();
            assert_eq!(ambient_lux_from(root.path()), Some(100.0));
            fs::write(d.join("in_illuminance_input"), "42.5\n").unwrap();
            assert_eq!(ambient_lux_from(root.path()), Some(42.5));
        }
        {
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
}
