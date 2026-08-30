//! Pointer and keyboard device policy, and the settings behind it.
//!
//! This module is the decision half: it takes persisted settings plus what one libinput device
//! can actually do, and produces the exact `config_*` values to apply. [`super::runtime`] holds
//! the thin applier that pushes them onto a real `libinput::Device`.
//!
//! libinput defaults tapping to *off* because it can't tell a laptop touchpad from a desk
//! drawing tablet, and a spurious click from a resting palm is worse than a missing one. On a
//! laptop that reasoning inverts — a trackpad requiring physical depression per click is the
//! most-reported "this feels broken" first impression — so the default here is on, with palm
//! detection and disable-while-typing turned on alongside it to buy back what libinput was cautious about; the setting remains the user's to flip.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Settings section these values live in.
pub const SECTION: &str = "input";

pub const NATURAL_SCROLL_KEY: &str = "natural_scroll";
pub const TAP_TO_CLICK_KEY: &str = "tap_to_click";
pub const POINTER_SPEED_KEY: &str = "pointer_speed";
pub const KEY_REPEAT_KEY: &str = "key_repeat";

/// Slowest auto-repeat the `key_repeat` slider can select, in keys per second — below this, holding a key feels broken rather than slow.
pub const REPEAT_RATE_MIN: u32 = 10;
/// Fastest auto-repeat the slider can select, in keys per second.
pub const REPEAT_RATE_MAX: u32 = 50;
/// Longest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MAX_MS: u32 = 600;
/// Shortest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MIN_MS: u32 = 200;

/// The persisted input preferences, with the defaults applied. Mirrors the shape
/// `apps/settings` writes; the defaults match that pane's starting values, so a user who never opened Settings and one who opened it and changed nothing get the same session.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct InputSettings {
    /// Content-follows-finger scrolling. Off by default, matching the Settings pane.
    pub natural_scroll: bool,
    /// Tap the touchpad to click. **On** by default — see the module docs.
    pub tap_to_click: bool,
    /// Pointer acceleration, `0.0` (slowest) … `1.0` (fastest), `0.5` being libinput's neutral profile.
    pub pointer_speed: f64,
    /// Keyboard auto-repeat, `0.0` (slow, long delay) … `1.0` (fast, short delay).
    pub key_repeat: f64,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            tap_to_click: true,
            pointer_speed: 0.5,
            key_repeat: 0.5,
        }
    }
}

impl InputSettings {
    /// Parse the `input` section of a settings document. Every field falls back to its default
    /// independently, so a document that only sets `tap_to_click` leaves the rest alone, and a wrong-typed value is treated as absent rather than discarding the whole section.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let d = Self::default();
        let bool_at = |k: &str, fallback: bool| {
            section
                .get(k)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(fallback)
        };
        let unit_at = |k: &str, fallback: f64| {
            section
                .get(k)
                .and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite())
                .map_or(fallback, |v| v.clamp(0.0, 1.0))
        };
        Self {
            natural_scroll: bool_at(NATURAL_SCROLL_KEY, d.natural_scroll),
            tap_to_click: bool_at(TAP_TO_CLICK_KEY, d.tap_to_click),
            pointer_speed: unit_at(POINTER_SPEED_KEY, d.pointer_speed),
            key_repeat: unit_at(KEY_REPEAT_KEY, d.key_repeat),
        }
    }

    /// Read the live settings file. Any failure yields the defaults — a corrupt config must never leave the session without a working touchpad.
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(SECTION))
    }

    /// libinput's pointer-acceleration speed for this setting: the slider's `0.0 … 1.0` mapped onto libinput's `-1.0 … 1.0`, so the midpoint lands exactly on the untouched default of `0.0`.
    pub fn accel_speed(&self) -> f64 {
        (self.pointer_speed.clamp(0.0, 1.0) * 2.0) - 1.0
    }

    /// Keyboard auto-repeat as `(delay, rate in keys per second)`. The two move together in
    /// opposite directions — "faster" means both a shorter wait before it starts and a shorter gap between keys — so exposing them as one slider is only honest if the mapping does that.
    pub fn key_repeat_params(&self) -> (Duration, u32) {
        let t = self.key_repeat.clamp(0.0, 1.0);
        let rate = f64::from(REPEAT_RATE_MIN) + t * f64::from(REPEAT_RATE_MAX - REPEAT_RATE_MIN);
        let delay = f64::from(REPEAT_DELAY_MAX_MS)
            - t * f64::from(REPEAT_DELAY_MAX_MS - REPEAT_DELAY_MIN_MS);
        (
            Duration::from_millis(delay.round() as u64),
            rate.round() as u32,
        )
    }
}

/// How often the settings file is stat'ed for changes.
///
/// A poll rather than an inotify watch: the compositor already loops at the display's cadence, and one `stat` a second on a page-cached file is far less machinery than a filesystem watcher, for a change a human makes at most a few times a session.
pub const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Notices when the settings file changes, so a preference just flipped in Settings takes effect without restarting the session.
pub struct SettingsWatch {
    path: Option<PathBuf>,
    /// Modification time as of the last check. `None` before the first check, and while the file doesn't exist.
    last: Option<SystemTime>,
    /// Whether a check has happened at all — distinguishes "no file yet" from "not looked yet", so the file appearing for the first time counts as a change rather than the baseline.
    primed: bool,
    next_check: Option<Instant>,
    interval: Duration,
}

impl Default for SettingsWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsWatch {
    /// Watch the real settings file.
    pub fn new() -> Self {
        Self::at(settings_store::settings_path())
    }

    /// Watch an explicit path — how the tests drive it without touching the user's real configuration.
    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            last: None,
            primed: false,
            next_check: None,
            interval: SETTINGS_POLL_INTERVAL,
        }
    }

    /// Use a different poll interval; tests set it to zero so they don't wait out a real second.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Whether the settings file has changed since the last time this returned `true`. Rate-limited to [`Self::with_interval`]; calls in between are free.
    pub fn changed(&mut self, now: Instant) -> bool {
        if let Some(next) = self.next_check
            && now < next
        {
            return false;
        }
        self.next_check = Some(now + self.interval);
        let Some(path) = self.path.as_ref() else {
            return false;
        };
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let first_look = !self.primed;
        self.primed = true;
        let moved = mtime != self.last;
        self.last = mtime;
        // The first look establishes the baseline rather than reporting a change — otherwise every session start would re-apply settings already applied at construction.
        moved && !first_look
    }
}

/// What one libinput device is capable of. Every field is a question libinput answers per
/// device (`config_tap_finger_count`, `config_scroll_has_natural_scroll`, `config_accel_is_available`, …), so the plan below never asks a device for something it doesn't have.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceCaps {
    /// The device reports tap fingers — i.e. it is a touchpad.
    pub supports_tap: bool,
    pub supports_natural_scroll: bool,
    pub supports_accel: bool,
    /// Disable-while-typing, which only touchpads have.
    pub supports_dwt: bool,
    /// Emulate a middle click from a simultaneous left+right press.
    pub supports_middle_emulation: bool,
    /// The device has left-handed configuration, i.e. it is a pointer.
    pub is_pointer: bool,
}

impl DeviceCaps {
    /// A touchpad is the device class the laptop-specific defaults exist for: tapping, disable-while-typing, and two-finger scrolling.
    pub fn is_touchpad(&self) -> bool {
        self.supports_tap
    }
}

/// The configuration to push onto one device. `None` means "this device cannot be told that, so don't ask".
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DevicePlan {
    pub tap: Option<bool>,
    /// Tap-and-drag rides with tapping: a user who taps to click expects tap-hold-move to drag. Turning tapping on without it produces a touchpad that clicks but cannot drag a window.
    pub tap_drag: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub disable_while_typing: Option<bool>,
    pub middle_emulation: Option<bool>,
    /// Present as milli-units (`accel_speed * 1000`, rounded) so the plan stays `Eq` and comparable in tests; the applier divides it back.
    pub accel_speed_milli: Option<i32>,
}

impl DevicePlan {
    /// libinput's acceleration speed, back in its own `-1.0 … 1.0` units.
    pub fn accel_speed(&self) -> Option<f64> {
        self.accel_speed_milli.map(|m| f64::from(m) / 1000.0)
    }
}

/// Decide what to configure on a device with `caps`, given `settings` — a pure function of the two inputs, since it's applied on device *add* and re-applied on every settings change (a plan depending on prior state would drift on re-plug).
pub fn plan_for(settings: &InputSettings, caps: &DeviceCaps) -> DevicePlan {
    DevicePlan {
        tap: caps.supports_tap.then_some(settings.tap_to_click),
        tap_drag: caps.supports_tap.then_some(settings.tap_to_click),
        natural_scroll: caps
            .supports_natural_scroll
            .then_some(settings.natural_scroll),
        // Only touchpads get disable-while-typing, and unconditionally: it's what makes tap-to-click safe to default on, so it's not a separate preference to forget to set.
        disable_while_typing: caps.supports_dwt.then_some(true),
        // A touchpad with no physical middle button is the norm; without emulation there's no way to middle-click at all.
        middle_emulation: caps.supports_middle_emulation.then_some(true),
        accel_speed_milli: caps
            .supports_accel
            .then(|| (settings.accel_speed() * 1000.0).round() as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn section(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match v {
            serde_json::Value::Object(m) => m,
            _ => panic!("test fixture must be an object"),
        }
    }

    fn touchpad() -> DeviceCaps {
        DeviceCaps {
            supports_tap: true,
            supports_natural_scroll: true,
            supports_accel: true,
            supports_dwt: true,
            supports_middle_emulation: true,
            is_pointer: true,
        }
    }

    fn mouse() -> DeviceCaps {
        DeviceCaps {
            supports_tap: false,
            supports_natural_scroll: true,
            supports_accel: true,
            supports_dwt: false,
            supports_middle_emulation: false,
            is_pointer: true,
        }
    }

    fn keyboard() -> DeviceCaps {
        DeviceCaps::default()
    }

    // settings parsing

    #[test]
    fn tap_to_click_defaults_on_for_a_laptop() {
        // The headline default: libinput says off, but that's the wrong answer on a laptop, and an empty settings file is exactly the state a fresh install is in.
        assert!(InputSettings::default().tap_to_click);
        assert!(InputSettings::from_section(&section(json!({}))).tap_to_click);
    }

    #[test]
    fn the_other_defaults_match_what_the_settings_pane_starts_from() {
        let d = InputSettings::default();
        assert!(!d.natural_scroll);
        assert_eq!(d.pointer_speed, 0.5);
        assert_eq!(d.key_repeat, 0.5);
    }

    #[test]
    fn persisted_values_are_read_back() {
        let s = InputSettings::from_section(&section(json!({
            "natural_scroll": true,
            "tap_to_click": false,
            "pointer_speed": 0.9,
            "key_repeat": 0.25,
        })));
        assert!(s.natural_scroll);
        assert!(!s.tap_to_click, "an explicit off must beat the default on");
        assert_eq!(s.pointer_speed, 0.9);
        assert_eq!(s.key_repeat, 0.25);
    }

    #[test]
    fn each_field_falls_back_independently() {
        // A document that only mentions one key must not reset the others.
        let s = InputSettings::from_section(&section(json!({ "pointer_speed": 0.1 })));
        assert_eq!(s.pointer_speed, 0.1);
        assert!(s.tap_to_click);
        assert!(!s.natural_scroll);
        assert_eq!(s.key_repeat, 0.5);
    }

    #[test]
    fn wrong_types_and_out_of_range_values_are_survivable() {
        let s = InputSettings::from_section(&section(json!({
            "tap_to_click": "yes",
            "pointer_speed": 42.0,
            "key_repeat": -3.0,
        })));
        assert!(s.tap_to_click, "a string is not a bool; use the default");
        assert_eq!(s.pointer_speed, 1.0, "out of range clamps into the slider");
        assert_eq!(s.key_repeat, 0.0);
    }

    // derived values

    #[test]
    fn the_midpoint_pointer_speed_is_libinputs_neutral() {
        // 0.5 must map to exactly 0.0, or every user who never touched the slider gets a different acceleration than libinput's default.
        assert_eq!(InputSettings::default().accel_speed(), 0.0);
    }

    #[test]
    fn pointer_speed_spans_the_full_libinput_range() {
        let slow = InputSettings {
            pointer_speed: 0.0,
            ..Default::default()
        };
        let fast = InputSettings {
            pointer_speed: 1.0,
            ..Default::default()
        };
        assert_eq!(slow.accel_speed(), -1.0);
        assert_eq!(fast.accel_speed(), 1.0);
    }

    #[test]
    fn key_repeat_moves_rate_up_and_delay_down_together() {
        let slow = InputSettings {
            key_repeat: 0.0,
            ..Default::default()
        };
        let fast = InputSettings {
            key_repeat: 1.0,
            ..Default::default()
        };
        let (slow_delay, slow_rate) = slow.key_repeat_params();
        let (fast_delay, fast_rate) = fast.key_repeat_params();
        assert_eq!(slow_rate, REPEAT_RATE_MIN);
        assert_eq!(fast_rate, REPEAT_RATE_MAX);
        assert_eq!(
            slow_delay,
            Duration::from_millis(u64::from(REPEAT_DELAY_MAX_MS))
        );
        assert_eq!(
            fast_delay,
            Duration::from_millis(u64::from(REPEAT_DELAY_MIN_MS))
        );
        assert!(
            fast_rate > slow_rate && fast_delay < slow_delay,
            "one slider, both directions"
        );
    }

    #[test]
    fn the_midpoint_key_repeat_is_in_the_middle_of_both_ranges() {
        let (delay, rate) = InputSettings::default().key_repeat_params();
        assert_eq!(rate, 30);
        assert_eq!(delay, Duration::from_millis(400));
    }

    // per-device plans

    #[test]
    fn a_touchpad_gets_tapping_dragging_and_palm_protection() {
        let plan = plan_for(&InputSettings::default(), &touchpad());
        assert_eq!(plan.tap, Some(true));
        assert_eq!(
            plan.tap_drag,
            Some(true),
            "tapping without tap-and-drag is a touchpad that cannot drag"
        );
        assert_eq!(
            plan.disable_while_typing,
            Some(true),
            "disable-while-typing is what makes defaulting tap on safe"
        );
        assert_eq!(plan.middle_emulation, Some(true));
    }

    #[test]
    fn turning_tap_off_turns_tap_drag_off_with_it() {
        let s = InputSettings {
            tap_to_click: false,
            ..Default::default()
        };
        let plan = plan_for(&s, &touchpad());
        assert_eq!(plan.tap, Some(false));
        assert_eq!(plan.tap_drag, Some(false));
    }

    #[test]
    fn a_mouse_is_never_asked_about_tapping() {
        // `config_tap_set_enabled` on a device with no tap fingers errors every call; not asking is the whole point of the capability gate.
        let plan = plan_for(&InputSettings::default(), &mouse());
        assert_eq!(plan.tap, None);
        assert_eq!(plan.tap_drag, None);
        assert_eq!(plan.disable_while_typing, None);
        assert_eq!(plan.middle_emulation, None);
    }

    #[test]
    fn natural_scroll_reaches_every_device_that_has_it() {
        let s = InputSettings {
            natural_scroll: true,
            ..Default::default()
        };
        assert_eq!(plan_for(&s, &touchpad()).natural_scroll, Some(true));
        assert_eq!(plan_for(&s, &mouse()).natural_scroll, Some(true));
        // …and none that doesn't.
        assert_eq!(plan_for(&s, &keyboard()).natural_scroll, None);
    }

    #[test]
    fn pointer_speed_reaches_devices_with_acceleration() {
        let s = InputSettings {
            pointer_speed: 1.0,
            ..Default::default()
        };
        assert_eq!(plan_for(&s, &mouse()).accel_speed(), Some(1.0));
        let s = InputSettings {
            pointer_speed: 0.0,
            ..Default::default()
        };
        assert_eq!(plan_for(&s, &touchpad()).accel_speed(), Some(-1.0));
        assert_eq!(plan_for(&s, &keyboard()).accel_speed(), None);
    }

    #[test]
    fn a_keyboard_gets_an_empty_plan() {
        // Nothing in the pointer half applies; the keyboard half rides on `key_repeat_params` in the seat, not a device config.
        assert_eq!(
            plan_for(&InputSettings::default(), &keyboard()),
            DevicePlan::default()
        );
    }

    #[test]
    fn the_plan_is_a_pure_function_of_settings_and_caps() {
        // Re-applying after a settings change must produce the same plan a freshly-plugged device would get — otherwise a re-plug silently changes behaviour.
        let s = InputSettings {
            tap_to_click: false,
            natural_scroll: true,
            pointer_speed: 0.75,
            key_repeat: 0.2,
        };
        assert_eq!(plan_for(&s, &touchpad()), plan_for(&s, &touchpad()));
    }

    #[test]
    fn is_touchpad_is_the_tap_capability() {
        assert!(touchpad().is_touchpad());
        assert!(!mouse().is_touchpad());
        assert!(!keyboard().is_touchpad());
    }

    // settings watch

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("qos-input-watch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("settings.json")
    }

    fn watch(path: &PathBuf) -> SettingsWatch {
        SettingsWatch::at(Some(path.clone())).with_interval(Duration::ZERO)
    }

    #[test]
    fn the_first_look_is_a_baseline_not_a_change() {
        // Otherwise every session start re-applies settings already applied while constructing the backend.
        let path = scratch("baseline");
        std::fs::write(&path, "{}").expect("write");
        let mut w = watch(&path);
        assert!(!w.changed(Instant::now()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_write_is_noticed_once() {
        let path = scratch("write");
        std::fs::write(&path, r#"{"input":{"tap_to_click":true}}"#).expect("write");
        let mut w = watch(&path);
        assert!(!w.changed(Instant::now()), "baseline");

        // A same-second rewrite can land on the same mtime on filesystems with coarse timestamps, so move it explicitly rather than racing the clock — the watch's contract is "the mtime moved".
        std::fs::write(&path, r#"{"input":{"tap_to_click":false}}"#).expect("rewrite");
        let moved = std::time::SystemTime::now() + Duration::from_secs(2);
        let f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open for times");
        f.set_modified(moved).expect("set mtime");
        drop(f);

        assert!(w.changed(Instant::now()), "the rewrite must be noticed");
        assert!(
            !w.changed(Instant::now()),
            "and only once — re-applying every tick would fight the user"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_file_appearing_for_the_first_time_counts_as_a_change() {
        // A fresh install has no settings.json until the user opens Settings; the first write is exactly when the compositor has to start honouring it.
        let path = scratch("appear");
        let _ = std::fs::remove_file(&path);
        let mut w = watch(&path);
        assert!(!w.changed(Instant::now()), "baseline: no file");
        std::fs::write(&path, "{}").expect("write");
        assert!(w.changed(Instant::now()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn checks_are_rate_limited() {
        // The loop runs at the display's cadence; stat'ing 144 times a second for a rarely-edited file is waste.
        let path = scratch("ratelimit");
        std::fs::write(&path, "{}").expect("write");
        let mut w = SettingsWatch::at(Some(path.clone())).with_interval(Duration::from_hours(1));
        let now = Instant::now();
        assert!(!w.changed(now), "baseline");
        std::fs::remove_file(&path).expect("remove");
        assert!(
            !w.changed(now),
            "a second call inside the interval must not even look"
        );
    }

    #[test]
    fn a_watch_with_no_settings_path_never_reports_a_change() {
        // No config directory at all — a container with no HOME. The compositor must not spin on it.
        let mut w = SettingsWatch::at(None).with_interval(Duration::ZERO);
        assert!(!w.changed(Instant::now()));
        assert!(!w.changed(Instant::now()));
    }
}
