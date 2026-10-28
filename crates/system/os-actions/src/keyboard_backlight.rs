//! Keyboard backlight, written straight to the kernel's LED class.
//!
//! Every laptop keyboard light the kernel drives appears as
//! `/sys/class/leds/<vendor>::kbd_backlight/`, with the same `brightness` /
//! `max_brightness` pair the display backlight uses. The same reasoning as
//! [`crate::display`] applies — no helper binary, no fork per keypress — and
//! the same udev rule (`os/usr/lib/udev/rules.d/90-tilo-backlight.rules`)
//! hands `brightness` to the `video` group.
//!
//! Keyboard lights have very few steps: most expose two or three levels, a
//! few expose 0–255. [`KeyboardBacklight::step`] moves one hardware level on
//! the coarse ones and a tenth of the range on the fine ones, so one press
//! is always a visible change and never a jump from dim to blinding.

use std::fs;
use std::path::{Path, PathBuf};

/// The kernel's LED class. A parameter on the inner functions so the tests can point at a
/// fixture tree.
const LEDS: &str = "/sys/class/leds";

/// The suffix the kernel's LED naming convention reserves for keyboard lights.
const KBD_SUFFIX: &str = "::kbd_backlight";

/// Ranges at or below this many levels step one hardware level per press.
const COARSE_LEVELS: u32 = 10;

/// One reading of the keyboard light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardBacklight {
    pub level: u32,
    pub max: u32,
}

impl KeyboardBacklight {
    /// The level as a `0.0..=1.0` fraction of the device's range.
    #[allow(
        clippy::cast_precision_loss,
        reason = "LED raw values are far below 2^24, so the f32 division loses no precision"
    )]
    pub fn fraction(self) -> f32 {
        if self.max == 0 {
            return 0.0;
        }
        (self.level as f32 / self.max as f32).clamp(0.0, 1.0)
    }

    /// The level one press of a brightness key moves to, saturating at both ends.
    pub fn step(self, up: bool) -> u32 {
        let delta = if self.max <= COARSE_LEVELS {
            1
        } else {
            (self.max / 10).max(1)
        };
        if up {
            self.level.saturating_add(delta).min(self.max)
        } else {
            self.level.saturating_sub(delta)
        }
    }

    /// The level the on/off key moves to: off when lit, full when dark.
    pub fn toggled(self) -> u32 {
        if self.level > 0 { 0 } else { self.max }
    }
}

/// Read the keyboard light.
///
/// # Errors
///
/// Returns an error message when this machine has no keyboard backlight or its attributes
/// cannot be read.
pub fn get_keyboard_backlight() -> Result<KeyboardBacklight, String> {
    get_in(Path::new(LEDS))
}

/// Set the keyboard light to the raw hardware `level`, clamped to the device's range.
pub fn set_keyboard_backlight(level: u32) -> Result<(), String> {
    set_in(Path::new(LEDS), level)
}

/// Whether this machine has a keyboard light at all.
pub fn has_keyboard_backlight() -> bool {
    device(Path::new(LEDS)).is_some()
}

fn get_in(root: &Path) -> Result<KeyboardBacklight, String> {
    let dev = device(root).ok_or("no keyboard backlight")?;
    let max = read_u32(&dev, "max_brightness").ok_or("keyboard max_brightness unreadable")?;
    if max == 0 {
        return Err("keyboard max_brightness is 0".to_string());
    }
    let level = read_u32(&dev, "brightness").ok_or("keyboard brightness unreadable")?;
    Ok(KeyboardBacklight {
        level: level.min(max),
        max,
    })
}

fn set_in(root: &Path, level: u32) -> Result<(), String> {
    let dev = device(root).ok_or("no keyboard backlight")?;
    let max = read_u32(&dev, "max_brightness").ok_or("keyboard max_brightness unreadable")?;
    let path = dev.join("brightness");
    fs::write(&path, level.min(max).to_string())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// The first keyboard light by name, so the choice is stable rather than following readdir
/// order on the rare machine with two (a docked keyboard beside the built-in one).
fn device(root: &Path) -> Option<PathBuf> {
    let mut devices: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(KBD_SUFFIX))
                && p.join("brightness").exists()
                && p.join("max_brightness").exists()
        })
        .collect();
    devices.sort();
    devices.into_iter().next()
}

fn read_u32(dir: &Path, attr: &str) -> Option<u32> {
    fs::read_to_string(dir.join(attr)).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture LED class tree: `(name, current, max)`.
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
    fn the_keyboard_light_is_found_among_other_leds() {
        let t = tree(&[
            ("input3::capslock", 0, 1),
            ("tpacpi::kbd_backlight", 1, 2),
            ("platform::mute", 1, 1),
        ]);
        assert_eq!(
            get_in(t.path()).unwrap(),
            KeyboardBacklight { level: 1, max: 2 },
            "caps-lock and mute LEDs are not the keyboard light"
        );
    }

    #[test]
    fn a_write_lands_on_the_keyboard_light_and_round_trips() {
        let t = tree(&[("asus::kbd_backlight", 0, 3)]);
        set_in(t.path(), 2).unwrap();
        assert_eq!(
            fs::read_to_string(t.path().join("asus::kbd_backlight/brightness")).unwrap(),
            "2"
        );
        assert_eq!(get_in(t.path()).unwrap().level, 2);
    }

    #[test]
    fn a_write_past_the_maximum_saturates() {
        let t = tree(&[("dell::kbd_backlight", 0, 2)]);
        set_in(t.path(), 9).unwrap();
        assert_eq!(
            fs::read_to_string(t.path().join("dell::kbd_backlight/brightness")).unwrap(),
            "2",
            "the kernel rejects a value above max_brightness"
        );
    }

    #[test]
    fn a_machine_without_a_keyboard_light_says_so() {
        let t = tree(&[("input3::capslock", 0, 1)]);
        assert!(device(t.path()).is_none());
        assert!(get_in(t.path()).is_err());
        assert!(set_in(t.path(), 1).is_err());
    }

    #[test]
    fn coarse_lights_step_one_level_and_fine_ones_a_tenth() {
        let coarse = KeyboardBacklight { level: 1, max: 2 };
        assert_eq!(coarse.step(true), 2);
        assert_eq!(coarse.step(false), 0);
        assert_eq!(KeyboardBacklight { level: 2, max: 2 }.step(true), 2);
        assert_eq!(KeyboardBacklight { level: 0, max: 2 }.step(false), 0);

        let fine = KeyboardBacklight {
            level: 100,
            max: 255,
        };
        assert_eq!(fine.step(true), 125);
        assert_eq!(fine.step(false), 75);
        assert_eq!(
            KeyboardBacklight {
                level: 250,
                max: 255
            }
            .step(true),
            255
        );
    }

    #[test]
    fn the_toggle_turns_a_lit_keyboard_off_and_a_dark_one_fully_on() {
        assert_eq!(KeyboardBacklight { level: 1, max: 3 }.toggled(), 0);
        assert_eq!(KeyboardBacklight { level: 0, max: 3 }.toggled(), 3);
    }

    #[test]
    fn the_fraction_is_level_over_max() {
        let half = KeyboardBacklight { level: 1, max: 2 }.fraction();
        assert!((half - 0.5).abs() < f32::EPSILON);
    }
}
