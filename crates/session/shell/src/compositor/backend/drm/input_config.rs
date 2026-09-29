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

use settings_store::hardware::{InputDeviceKind, TouchpadClickMethod, TouchpadScrollMethod};

/// Settings section these values live in.
pub const SECTION: &str = "input";

pub const NATURAL_SCROLL_KEY: &str = "natural_scroll";
pub const TAP_TO_CLICK_KEY: &str = "tap_to_click";
pub const POINTER_SPEED_KEY: &str = "pointer_speed";
pub const KEY_REPEAT_KEY: &str = "key_repeat";
/// Exact host-imported delay; a Tilo slider choice clears this pair.
pub const KEY_REPEAT_DELAY_MS_KEY: &str = "key_repeat_delay_ms";
/// Exact host-imported rate. Zero means repeat is disabled.
pub const KEY_REPEAT_RATE_HZ_KEY: &str = "key_repeat_rate_hz";

/// Fully validated per-user XKB keymap request. The strings are ready for
/// libxkbcommon's rules API: one comma-separated variant slot per layout,
/// including empty slots for base layouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardSettings {
    pub source_ids: Vec<String>,
    pub layouts: String,
    pub variants: String,
    pub options: Option<String>,
    pub active_index: usize,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            source_ids: vec!["us".into()],
            layouts: "us".into(),
            variants: String::new(),
            options: None,
            active_index: 0,
        }
    }
}

impl KeyboardSettings {
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let config = settings_store::hardware::RichInputConfig::from_section(section);
        let sources = config.sources();
        Self {
            source_ids: sources.iter().map(|source| source.id.clone()).collect(),
            layouts: sources
                .iter()
                .map(|source| source.layout.as_str())
                .collect::<Vec<_>>()
                .join(","),
            variants: sources
                .iter()
                .map(|source| source.variant.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(","),
            options: {
                let options = config.xkb_options();
                (!options.is_empty()).then(|| options.join(","))
            },
            active_index: config.active_layout_index(),
        }
    }

    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(SECTION))
    }

    /// The legends printed on the keys of XKB group `index`, which the
    /// shell names keys and modifiers in.
    pub fn keycaps(&self, index: usize) -> ui::Keycaps {
        let layout = self.layouts.split(',').nth(index).unwrap_or_default();
        let variant = self.variants.split(',').nth(index).unwrap_or_default();
        ui::Keycaps::from_xkb(layout, variant)
    }

    /// The person-facing name of source `index` — XKB's own description
    /// ("English (US)", "German (no dead keys)") — falling back to the
    /// source id when the keymap cannot be compiled.
    #[cfg(not(target_os = "linux"))]
    pub fn source_label(&self, index: usize) -> String {
        self.source_ids.get(index).cloned().unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    pub fn source_label(&self, index: usize) -> String {
        use smithay::input::keyboard::xkb;
        let fallback = || self.source_ids.get(index).cloned().unwrap_or_default();
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(keymap) = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            &self.layouts,
            &self.variants,
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) else {
            return fallback();
        };
        let Ok(layout) = u32::try_from(index) else {
            return fallback();
        };
        if layout >= keymap.num_layouts() {
            return fallback();
        }
        let name = keymap.layout_get_name(layout);
        if name.is_empty() {
            fallback()
        } else {
            name.to_string()
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "accessibility toggles: each is an independent, named setting from a settings pane, not a state machine"
)]
pub struct AccessibilityInputSettings {
    pub sticky_keys: bool,
    pub slow_keys: bool,
    /// Ignore a second press of the same key shortly after its release.
    pub bounce_keys: bool,
    /// The bounce-keys window in milliseconds.
    pub bounce_keys_delay_ms: u32,
    /// Flash the visual alert whenever bounce keys swallows a press.
    pub bounce_keys_alert: bool,
    /// The numeric keypad drives the pointer.
    pub mouse_keys: bool,
    pub large_cursor: bool,
    pub cursor_size: u32,
}

impl Default for AccessibilityInputSettings {
    fn default() -> Self {
        Self {
            sticky_keys: false,
            slow_keys: false,
            bounce_keys: false,
            bounce_keys_delay_ms: settings_store::hardware::BOUNCE_KEYS_DEFAULT_DELAY_MS,
            bounce_keys_alert: false,
            mouse_keys: false,
            large_cursor: false,
            cursor_size: 24,
        }
    }
}

/// `accessibility` key the Shift+Alt+Num Lock chord flips.
pub const MOUSE_KEYS_KEY: &str = "mouse_keys";

impl AccessibilityInputSettings {
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section("accessibility"))
    }

    /// Parse the `accessibility` section; every field falls back to its
    /// default on its own.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let d = Self::default();
        let bool_at = |key: &str, fallback: bool| {
            section
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(fallback)
        };
        Self {
            sticky_keys: bool_at("sticky_keys", d.sticky_keys),
            slow_keys: bool_at("slow_keys", d.slow_keys),
            bounce_keys: bool_at("bounce_keys", d.bounce_keys),
            bounce_keys_delay_ms: section
                .get("bounce_keys_delay_ms")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.bounce_keys_delay_ms)
                .clamp(
                    settings_store::hardware::BOUNCE_KEYS_MIN_DELAY_MS,
                    settings_store::hardware::BOUNCE_KEYS_MAX_DELAY_MS,
                ),
            bounce_keys_alert: bool_at("bounce_keys_alert", d.bounce_keys_alert),
            mouse_keys: bool_at(MOUSE_KEYS_KEY, d.mouse_keys),
            large_cursor: bool_at("large_cursor", d.large_cursor),
            cursor_size: section
                .get("cursor_size")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.cursor_size)
                .clamp(24, 64),
        }
    }

    /// The bounce-keys window, or zero when bounce keys is off.
    pub fn bounce_window(self) -> Duration {
        if self.bounce_keys {
            Duration::from_millis(u64::from(self.bounce_keys_delay_ms))
        } else {
            Duration::ZERO
        }
    }

    pub fn effective_cursor_size(self) -> u32 {
        if self.large_cursor {
            self.cursor_size.clamp(24, 64)
        } else {
            24
        }
    }
}

/// Slowest auto-repeat the `key_repeat` slider can select, in keys per second — below this, holding a key feels broken rather than slow.
pub const REPEAT_RATE_MIN: u32 = settings_store::hardware::KEY_REPEAT_SLIDER_MIN_HZ;
/// Fastest auto-repeat the slider can select, in keys per second.
pub const REPEAT_RATE_MAX: u32 = settings_store::hardware::KEY_REPEAT_SLIDER_MAX_HZ;
/// Longest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MAX_MS: u32 = 600;
/// Shortest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MIN_MS: u32 = 200;

/// The persisted input preferences, with the defaults applied. Mirrors the shape
/// `apps/settings` writes; the defaults match that pane's starting values, so a user who never opened Settings and one who opened it and changed nothing get the same session.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "persisted input toggles: each is an independent, named setting mirroring the Settings pane, not a state machine"
)]
pub struct InputSettings {
    /// Legacy scrolling preference used when the device-specific fields are
    /// absent in an older settings document.
    pub natural_scroll: bool,
    /// Content-follows-finger scrolling for touchpads.
    pub touchpad_natural_scroll: Option<bool>,
    /// Content-follows-finger scrolling for mice and trackballs.
    pub mouse_natural_scroll: Option<bool>,
    /// Tap the touchpad to click. **On** by default — see the module docs.
    pub tap_to_click: bool,
    /// Pointer acceleration, `0.0` (slowest) … `1.0` (fastest), `0.5` being libinput's neutral profile.
    pub pointer_speed: f64,
    pub touchpad_pointer_speed: Option<f64>,
    pub mouse_pointer_speed: Option<f64>,
    /// Keyboard auto-repeat, `0.0` (slow, long delay) … `1.0` (fast, short delay).
    pub key_repeat: f64,
    pub key_repeat_delay_ms: Option<u32>,
    pub key_repeat_rate_hz: Option<u32>,
    pub tap_drag: bool,
    /// A tap-and-drag survives lifting the finger until the next tap, so a
    /// long drag can be repositioned mid-way. Off by default, as in libinput.
    pub tap_drag_lock: bool,
    pub disable_while_typing: bool,
    pub middle_emulation: bool,
    pub left_handed: bool,
    pub flat_acceleration: bool,
    pub touchpad_gestures: bool,
    /// Whether touchpads send events at all; the touchpad-toggle key flips it.
    pub touchpad_enabled: bool,
    /// Switch touchpads off while an external mouse is plugged in.
    pub touchpad_disabled_on_external_mouse: bool,
    /// Which button a clickpad press means.
    pub touchpad_click_method: TouchpadClickMethod,
    /// Two-finger or edge scrolling on touchpads.
    pub touchpad_scroll_method: TouchpadScrollMethod,
    /// Multiplier on every scroll distance, wheel and finger alike. Applied by
    /// the compositor rather than libinput, which has no such setting.
    pub scroll_speed: f64,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            touchpad_natural_scroll: Some(
                settings_store::hardware::DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            ),
            mouse_natural_scroll: Some(settings_store::hardware::DEFAULT_MOUSE_NATURAL_SCROLL),
            tap_to_click: true,
            pointer_speed: 0.5,
            touchpad_pointer_speed: None,
            mouse_pointer_speed: None,
            key_repeat: 0.5,
            key_repeat_delay_ms: None,
            key_repeat_rate_hz: None,
            tap_drag: true,
            tap_drag_lock: false,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            flat_acceleration: false,
            touchpad_gestures: true,
            touchpad_enabled: true,
            touchpad_disabled_on_external_mouse: false,
            touchpad_click_method: TouchpadClickMethod::Default,
            touchpad_scroll_method: TouchpadScrollMethod::TwoFinger,
            scroll_speed: 1.0,
        }
    }
}

impl InputSettings {
    /// Parse the `input` section of a settings document. Every field falls back to its default
    /// independently, so a document that only sets `tap_to_click` leaves the rest alone, and a wrong-typed value is treated as absent rather than discarding the whole section.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let d = Self::default();
        let legacy_natural_scroll = section
            .get(NATURAL_SCROLL_KEY)
            .and_then(serde_json::Value::as_bool);
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
            natural_scroll: legacy_natural_scroll.unwrap_or(d.natural_scroll),
            touchpad_natural_scroll: section
                .get("touchpad_natural_scroll")
                .and_then(serde_json::Value::as_bool)
                .or(legacy_natural_scroll)
                .or(d.touchpad_natural_scroll),
            mouse_natural_scroll: section
                .get("mouse_natural_scroll")
                .and_then(serde_json::Value::as_bool)
                .or(legacy_natural_scroll)
                .or(d.mouse_natural_scroll),
            tap_to_click: bool_at(TAP_TO_CLICK_KEY, d.tap_to_click),
            pointer_speed: unit_at(POINTER_SPEED_KEY, d.pointer_speed),
            touchpad_pointer_speed: section
                .get("touchpad_pointer_speed")
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0)),
            mouse_pointer_speed: section
                .get("mouse_pointer_speed")
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0)),
            key_repeat: unit_at(KEY_REPEAT_KEY, d.key_repeat),
            key_repeat_delay_ms: section
                .get(KEY_REPEAT_DELAY_MS_KEY)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| (1..=5_000).contains(value)),
            key_repeat_rate_hz: section
                .get(KEY_REPEAT_RATE_HZ_KEY)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value <= 1_000),
            tap_drag: bool_at("tap_drag", d.tap_drag),
            tap_drag_lock: bool_at("tap_drag_lock", d.tap_drag_lock),
            disable_while_typing: bool_at("disable_while_typing", d.disable_while_typing),
            middle_emulation: bool_at("middle_emulation", d.middle_emulation),
            left_handed: bool_at("left_handed", d.left_handed),
            flat_acceleration: section
                .get("accel_profile")
                .and_then(serde_json::Value::as_str)
                == Some("flat"),
            touchpad_gestures: bool_at("touchpad_gestures", d.touchpad_gestures),
            touchpad_enabled: bool_at(
                settings_store::hardware::TOUCHPAD_ENABLED_KEY,
                d.touchpad_enabled,
            ),
            touchpad_disabled_on_external_mouse: bool_at(
                settings_store::hardware::TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY,
                d.touchpad_disabled_on_external_mouse,
            ),
            touchpad_click_method: section
                .get("touchpad_click_method")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or(d.touchpad_click_method),
            touchpad_scroll_method: section
                .get("touchpad_scroll_method")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or(d.touchpad_scroll_method),
            scroll_speed: section
                .get(settings_store::hardware::SCROLL_SPEED_KEY)
                .and_then(serde_json::Value::as_f64)
                .map_or(d.scroll_speed, settings_store::hardware::clamp_scroll_speed),
        }
    }

    /// Read the live settings file. Any failure yields the defaults — a corrupt config must never leave the session without a working touchpad.
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(SECTION))
    }

    /// Read the settings file at `path` with the same fallbacks as [`Self::load`].
    pub fn load_at(path: &std::path::Path) -> Self {
        Self::from_section(&settings_store::load_section_at(path, SECTION))
    }

    /// libinput's pointer-acceleration speed for this setting: the slider's `0.0 … 1.0` mapped onto libinput's `-1.0 … 1.0`, so the midpoint lands exactly on the untouched default of `0.0`.
    pub fn accel_speed(&self) -> f64 {
        (self.pointer_speed.clamp(0.0, 1.0) * 2.0) - 1.0
    }

    pub fn accel_speed_for(&self, touchpad: bool) -> f64 {
        let speed = if touchpad {
            self.touchpad_pointer_speed.unwrap_or(self.pointer_speed)
        } else {
            self.mouse_pointer_speed.unwrap_or(self.pointer_speed)
        };
        (speed.clamp(0.0, 1.0) * 2.0) - 1.0
    }

    /// Resolve the per-device preference, falling back to the legacy global
    /// toggle for settings documents written before the split.
    pub fn natural_scroll_for(&self, touchpad: bool) -> bool {
        if touchpad {
            self.touchpad_natural_scroll.unwrap_or(self.natural_scroll)
        } else {
            self.mouse_natural_scroll.unwrap_or(self.natural_scroll)
        }
    }

    /// Keyboard auto-repeat as `(delay, rate in keys per second)`. The two move together in
    /// opposite directions — "faster" means both a shorter wait before it starts and a shorter gap between keys — so exposing them as one slider is only honest if the mapping does that.
    pub fn key_repeat_params(&self) -> (Duration, u32) {
        if let (Some(delay), Some(rate)) = (self.key_repeat_delay_ms, self.key_repeat_rate_hz) {
            return (Duration::from_millis(u64::from(delay)), rate);
        }
        let t = self.key_repeat.clamp(0.0, 1.0);
        let rate = f64::from(REPEAT_RATE_MIN) + t * f64::from(REPEAT_RATE_MAX - REPEAT_RATE_MIN);
        let delay = f64::from(REPEAT_DELAY_MAX_MS)
            - t * f64::from(REPEAT_DELAY_MAX_MS - REPEAT_DELAY_MIN_MS);
        // `t` is clamped to `[0.0, 1.0]`, so `rate` stays within
        // `[REPEAT_RATE_MIN, REPEAT_RATE_MAX]` (10..=50) and `delay` within
        // `[REPEAT_DELAY_MIN_MS, REPEAT_DELAY_MAX_MS]` (200..=600) — both
        // comfortably non-negative and far under the target types' ranges.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "delay/rate are bounded to small known-positive ranges by the clamp above"
        )]
        let delay_ms = delay.round() as u64;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "delay/rate are bounded to small known-positive ranges by the clamp above"
        )]
        let rate = rate.round() as u32;
        (Duration::from_millis(delay_ms), rate)
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
    #[must_use]
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
        // `checked_add` sidesteps a theoretical `Instant` overflow (would
        // need centuries of uptime); falling back to "now" just makes the
        // next `changed()` call re-check immediately instead of panicking.
        self.next_check = Some(now.checked_add(self.interval).unwrap_or(now));
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

    /// Let the next [`Self::changed`] look regardless of the rate limit — for when a file-change notification says there is something to see.
    pub fn check_now(&mut self) {
        self.next_check = None;
    }
}

/// What one libinput device is capable of. Every field is a question libinput answers per
/// device (`config_tap_finger_count`, `config_scroll_has_natural_scroll`, `config_accel_is_available`, …), so the plan below never asks a device for something it doesn't have.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors libinput's per-capability query API one bool at a time, not a state machine"
)]
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
    pub supports_left_handed: bool,
    pub supports_flat_acceleration: bool,
    /// libinput lists software button areas among the click methods.
    pub supports_click_areas: bool,
    /// libinput lists clickfinger among the click methods.
    pub supports_clickfinger: bool,
    /// The click method libinput would use untouched, if the device has any.
    pub default_click_method: Option<ClickMethod>,
    pub supports_two_finger_scroll: bool,
    pub supports_edge_scroll: bool,
    /// libinput can switch the device off while an external mouse is present.
    pub supports_disable_on_external_mouse: bool,
    pub is_keyboard: bool,
    /// A touchscreen: absolute contacts, not a touchpad's relative ones.
    pub is_touch: bool,
    /// A drawing tablet's pen digitiser.
    pub is_tablet_tool: bool,
    /// The buttons, rings and strips beside a drawing tablet's surface.
    pub is_tablet_pad: bool,
}

/// A libinput send-events mode, independent of the libinput bindings so the
/// plan stays a pure value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SendEvents {
    Enabled,
    Disabled,
    DisabledOnExternalMouse,
}

/// A libinput click method, independent of the libinput bindings so the plan
/// stays a pure value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClickMethod {
    ButtonAreas,
    Clickfinger,
}

/// A libinput touchpad scroll method this module can choose.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScrollMethod {
    TwoFinger,
    Edge,
}

impl DeviceCaps {
    /// A touchpad is the device class the laptop-specific defaults exist for: tapping, disable-while-typing, and two-finger scrolling.
    pub fn is_touchpad(&self) -> bool {
        self.supports_tap
    }

    /// The Settings section this device belongs to. A tablet's pad and pen
    /// are checked first: both can also report pointer capabilities.
    pub fn kind(&self) -> InputDeviceKind {
        if self.is_tablet_pad {
            InputDeviceKind::TabletPad
        } else if self.is_tablet_tool {
            InputDeviceKind::Tablet
        } else if self.is_touchpad() {
            InputDeviceKind::Touchpad
        } else if self.is_touch {
            InputDeviceKind::Touchscreen
        } else if self.is_pointer {
            InputDeviceKind::Mouse
        } else if self.is_keyboard {
            InputDeviceKind::Keyboard
        } else {
            InputDeviceKind::Other
        }
    }
}

/// The configuration to push onto one device. `None` means "this device cannot be told that, so don't ask".
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DevicePlan {
    pub tap: Option<bool>,
    /// Tap-and-drag rides with tapping: a user who taps to click expects tap-hold-move to drag. Turning tapping on without it produces a touchpad that clicks but cannot drag a window.
    pub tap_drag: Option<bool>,
    /// Only ever on while tap-and-drag is: a lock on a drag that cannot
    /// start would do nothing but confuse the next tap.
    pub tap_drag_lock: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub disable_while_typing: Option<bool>,
    pub middle_emulation: Option<bool>,
    /// Present as milli-units (`accel_speed * 1000`, rounded) so the plan stays `Eq` and comparable in tests; the applier divides it back.
    pub accel_speed_milli: Option<i32>,
    pub left_handed: Option<bool>,
    pub flat_acceleration: Option<bool>,
    /// Whether the device sends events. Asked only of touchpads: a mouse or
    /// keyboard is never switched off by the touchpad key.
    pub send_events: Option<SendEvents>,
    /// Always a method the device listed; `Default` resolves to the device's
    /// own default so switching back really undoes an earlier choice.
    pub click_method: Option<ClickMethod>,
    pub scroll_method: Option<ScrollMethod>,
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
        tap_drag: caps
            .supports_tap
            .then_some(settings.tap_to_click && settings.tap_drag),
        tap_drag_lock: caps
            .supports_tap
            .then_some(settings.tap_to_click && settings.tap_drag && settings.tap_drag_lock),
        natural_scroll: caps
            .supports_natural_scroll
            .then_some(settings.natural_scroll_for(caps.is_touchpad())),
        // Only touchpads get disable-while-typing, and unconditionally: it's what makes tap-to-click safe to default on, so it's not a separate preference to forget to set.
        disable_while_typing: caps.supports_dwt.then_some(settings.disable_while_typing),
        // A touchpad with no physical middle button is the norm; without emulation there's no way to middle-click at all.
        middle_emulation: caps
            .supports_middle_emulation
            .then_some(settings.middle_emulation),
        accel_speed_milli: caps.supports_accel.then(|| {
            // `accel_speed()` is `pointer_speed.clamp(0.0, 1.0) * 2.0 - 1.0`,
            // i.e. always within `[-1.0, 1.0]`; milli-units keep it within
            // `[-1000, 1000]`, far under `i32`'s range.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "accel_speed() is bounded to [-1.0, 1.0]; milli-units stay within [-1000, 1000]"
            )]
            let milli = (settings.accel_speed_for(caps.is_touchpad()) * 1000.0).round() as i32;
            milli
        }),
        // A pen's orientation is its tablet's own setting, applied when its
        // coordinates are mapped; the mouse preference must not also turn it.
        left_handed: caps.supports_left_handed.then_some(settings.left_handed && !caps.is_tablet_tool),
        flat_acceleration: caps
            .supports_flat_acceleration
            .then_some(settings.flat_acceleration),
        send_events: caps.is_touchpad().then_some(if !settings.touchpad_enabled {
            SendEvents::Disabled
        } else if settings.touchpad_disabled_on_external_mouse
            && caps.supports_disable_on_external_mouse
        {
            SendEvents::DisabledOnExternalMouse
        } else {
            SendEvents::Enabled
        }),
        click_method: match settings.touchpad_click_method {
            TouchpadClickMethod::Default => caps.default_click_method,
            TouchpadClickMethod::Areas => caps
                .supports_click_areas
                .then_some(ClickMethod::ButtonAreas),
            TouchpadClickMethod::Clickfinger => caps
                .supports_clickfinger
                .then_some(ClickMethod::Clickfinger),
        },
        scroll_method: match settings.touchpad_scroll_method {
            TouchpadScrollMethod::TwoFinger => caps
                .supports_two_finger_scroll
                .then_some(ScrollMethod::TwoFinger),
            TouchpadScrollMethod::Edge => caps.supports_edge_scroll.then_some(ScrollMethod::Edge),
        },
    }
}

/// A device the settings can be pushed onto. libinput devices implement it
/// in the DRM runtime; anything else (a test double) proves the same path.
pub trait ConfigurableDevice {
    /// What the device can be told.
    fn caps(&self) -> DeviceCaps;
    /// Push `plan` onto the device. Every `None` field is left untouched.
    fn apply(&mut self, plan: &DevicePlan);
}

/// Configure one device from `settings`: when it is added, and again for
/// each settings change. Returns the plan that was applied.
pub fn configure<D: ConfigurableDevice + ?Sized>(
    settings: &InputSettings,
    device: &mut D,
) -> DevicePlan {
    let plan = plan_for(settings, &device.caps());
    device.apply(&plan);
    plan
}

/// Re-apply `settings` to every attached device, so a change is felt on the
/// touchpad under the user's fingers rather than on the next one plugged in.
pub fn reconfigure_all<D: ConfigurableDevice>(settings: &InputSettings, devices: &mut [D]) {
    for device in devices {
        configure(settings, device);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact normalized input fixtures are the contract under test"
)]
mod tests {
    use super::*;
    // Only the tests build a `Path`; the module itself deals in `PathBuf`.
    use std::path::Path;

    use serde_json::json;

    fn section(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match v {
            serde_json::Value::Object(m) => m,
            _ => panic!("test fixture must be an object"),
        }
    }

    #[test]
    fn input_settings_defaults_persistence_and_fallbacks() {
        // Phase `accessibility_cursor_size_is_capability_safe`.
        {
            let normal = AccessibilityInputSettings::default();
            assert_eq!(normal.effective_cursor_size(), 24);
            let large = AccessibilityInputSettings {
                large_cursor: true,
                cursor_size: 48,
                ..normal
            };
            assert_eq!(large.effective_cursor_size(), 48);
        }
        // Phase `tap_to_click_defaults_on_for_a_laptop`.
        // The headline default: libinput says off, but that's the wrong answer on a laptop, and an empty settings file is exactly the state a fresh install is in.
        assert!(InputSettings::default().tap_to_click);
        assert!(InputSettings::from_section(&section(json!({}))).tap_to_click);
        // Phase `the_other_defaults_match_what_the_settings_pane_starts_from`.
        {
            let d = InputSettings::default();
            assert!(!d.natural_scroll);
            assert_eq!(d.touchpad_natural_scroll, Some(true));
            assert_eq!(d.mouse_natural_scroll, Some(false));
            assert_eq!(d.pointer_speed, 0.5);
            assert_eq!(d.key_repeat, 0.5);
        }
        // Phase `unset_scroll_preferences_keep_touchpad_natural_and_wheel_conventional`.
        {
            let settings = InputSettings::from_section(&section(json!({})));
            assert!(settings.natural_scroll_for(true));
            assert!(!settings.natural_scroll_for(false));
            assert_eq!(plan_for(&settings, &touchpad()).natural_scroll, Some(true));
            assert_eq!(plan_for(&settings, &mouse()).natural_scroll, Some(false));
        }
        // Phase `legacy_global_scroll_preference_still_applies_to_both_device_classes`.
        {
            let settings = InputSettings::from_section(&section(json!({
                "natural_scroll": false,
            })));
            assert!(!settings.natural_scroll_for(true));
            assert!(!settings.natural_scroll_for(false));
        }
        // Phase `persisted_values_are_read_back`.
        {
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
        // Phase `each_field_falls_back_independently`.
        {
            // A document that only mentions one key must not reset the others.
            let s = InputSettings::from_section(&section(json!({ "pointer_speed": 0.1 })));
            assert_eq!(s.pointer_speed, 0.1);
            assert!(s.tap_to_click);
            assert!(!s.natural_scroll);
            assert_eq!(s.key_repeat, 0.5);
        }
        // Phase `wrong_types_and_out_of_range_values_are_survivable`.
        {
            let s = InputSettings::from_section(&section(json!({
                "tap_to_click": "yes",
                "pointer_speed": 42.0,
                "key_repeat": -3.0,
            })));
            assert!(s.tap_to_click, "a string is not a bool; use the default");
            assert_eq!(s.pointer_speed, 1.0, "out of range clamps into the slider");
            assert_eq!(s.key_repeat, 0.0);
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
            supports_left_handed: true,
            supports_flat_acceleration: true,
            supports_click_areas: true,
            supports_clickfinger: true,
            default_click_method: Some(ClickMethod::ButtonAreas),
            supports_two_finger_scroll: true,
            supports_edge_scroll: true,
            supports_disable_on_external_mouse: true,
            ..DeviceCaps::default()
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
            supports_left_handed: true,
            supports_flat_acceleration: true,
            ..DeviceCaps::default()
        }
    }

    fn keyboard() -> DeviceCaps {
        DeviceCaps::default()
    }

    // settings parsing

    // derived values

    #[test]
    fn pointer_speed_and_key_repeat_map_onto_libinput_ranges() {
        // Phase `the_midpoint_pointer_speed_is_libinputs_neutral`.
        // 0.5 must map to exactly 0.0, or every user who never touched the slider gets a different acceleration than libinput's default.
        assert_eq!(InputSettings::default().accel_speed(), 0.0);
        // Phase `pointer_speed_spans_the_full_libinput_range`.
        {
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
        // Phase `key_repeat_moves_rate_up_and_delay_down_together`.
        {
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
        // Phase `the_midpoint_key_repeat_is_in_the_middle_of_both_ranges`.
        {
            let (delay, rate) = InputSettings::default().key_repeat_params();
            assert_eq!(rate, 30);
            assert_eq!(delay, Duration::from_millis(400));
        }
        // Phase `host_repeat_values_preserve_delay_rate_and_disable_semantics`.
        {
            let enabled = InputSettings {
                key_repeat_delay_ms: Some(275),
                key_repeat_rate_hz: Some(37),
                ..Default::default()
            };
            assert_eq!(
                enabled.key_repeat_params(),
                (Duration::from_millis(275), 37)
            );

            let disabled = InputSettings {
                key_repeat_delay_ms: Some(500),
                key_repeat_rate_hz: Some(0),
                ..Default::default()
            };
            assert_eq!(
                disabled.key_repeat_params(),
                (Duration::from_millis(500), 0)
            );
        }
    }

    // per-device plans

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn device_plan_follows_settings_and_capabilities() {
        // Phase `a_touchpad_gets_tapping_dragging_and_palm_protection`.
        {
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
        // Phase `the_touchpad_toggle_switches_off_touchpads_and_nothing_else`.
        {
            assert_eq!(
                plan_for(&InputSettings::default(), &touchpad()).send_events,
                Some(SendEvents::Enabled),
                "an absent preference leaves the touchpad on"
            );
            let off = InputSettings::from_section(&section(json!({"touchpad_enabled": false})));
            assert!(!off.touchpad_enabled);
            assert_eq!(
                plan_for(&off, &touchpad()).send_events,
                Some(SendEvents::Disabled)
            );
            assert_eq!(
                plan_for(&off, &mouse()).send_events,
                None,
                "a mouse is never switched off by the touchpad key"
            );
            assert_eq!(plan_for(&off, &keyboard()).send_events, None);
        }
        // Phase `a_touchpad_can_step_aside_while_a_mouse_is_connected`.
        {
            let with_mouse = InputSettings::from_section(&section(json!({
                "touchpad_disabled_on_external_mouse": true
            })));
            assert!(with_mouse.touchpad_disabled_on_external_mouse);
            assert!(!InputSettings::default().touchpad_disabled_on_external_mouse);
            assert_eq!(
                plan_for(&with_mouse, &touchpad()).send_events,
                Some(SendEvents::DisabledOnExternalMouse)
            );
            assert_eq!(
                plan_for(&with_mouse, &mouse()).send_events,
                None,
                "the mouse itself is never switched off"
            );
            let unsupported = DeviceCaps {
                supports_disable_on_external_mouse: false,
                ..touchpad()
            };
            assert_eq!(
                plan_for(&with_mouse, &unsupported).send_events,
                Some(SendEvents::Enabled),
                "a touchpad libinput cannot hand off keeps working"
            );
            let off = InputSettings::from_section(&section(json!({
                "touchpad_enabled": false,
                "touchpad_disabled_on_external_mouse": true
            })));
            assert_eq!(
                plan_for(&off, &touchpad()).send_events,
                Some(SendEvents::Disabled),
                "switched off means off, mouse or not"
            );
        }
        // Phase `turning_tap_off_turns_tap_drag_off_with_it`.
        {
            let s = InputSettings {
                tap_to_click: false,
                ..Default::default()
            };
            let plan = plan_for(&s, &touchpad());
            assert_eq!(plan.tap, Some(false));
            assert_eq!(plan.tap_drag, Some(false));
        }
        // Phase `a_mouse_is_never_asked_about_tapping`.
        {
            // `config_tap_set_enabled` on a device with no tap fingers errors every call; not asking is the whole point of the capability gate.
            let plan = plan_for(&InputSettings::default(), &mouse());
            assert_eq!(plan.tap, None);
            assert_eq!(plan.tap_drag, None);
            assert_eq!(plan.disable_while_typing, None);
            assert_eq!(plan.middle_emulation, None);
        }
        // Phase `legacy_global_natural_scroll_reaches_every_device_when_device_overrides_are_absent`.
        {
            let s = InputSettings {
                natural_scroll: true,
                touchpad_natural_scroll: None,
                mouse_natural_scroll: None,
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &touchpad()).natural_scroll, Some(true));
            assert_eq!(plan_for(&s, &mouse()).natural_scroll, Some(true));
            // …and none that doesn't.
            assert_eq!(plan_for(&s, &keyboard()).natural_scroll, None);
        }
        // Phase `mouse_and_touchpad_can_keep_independent_scroll_directions`.
        {
            let s = InputSettings {
                natural_scroll: false,
                touchpad_natural_scroll: Some(true),
                mouse_natural_scroll: Some(false),
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &touchpad()).natural_scroll, Some(true));
            assert_eq!(plan_for(&s, &mouse()).natural_scroll, Some(false));
        }
        // Phase `pointer_speed_reaches_devices_with_acceleration`.
        {
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
        // Phase `a_keyboard_gets_an_empty_plan`.
        // Nothing in the pointer half applies; the keyboard half rides on `key_repeat_params` in the seat, not a device config.
        assert_eq!(
            plan_for(&InputSettings::default(), &keyboard()),
            DevicePlan::default()
        );
        // Phase `the_plan_is_a_pure_function_of_settings_and_caps`.
        {
            // Re-applying after a settings change must produce the same plan a freshly-plugged device would get — otherwise a re-plug silently changes behaviour.
            let s = InputSettings {
                tap_to_click: false,
                natural_scroll: true,
                pointer_speed: 0.75,
                key_repeat: 0.2,
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &touchpad()), plan_for(&s, &touchpad()));
        }
        // Phase `is_touchpad_is_the_tap_capability`.
        assert!(touchpad().is_touchpad());
        assert!(!mouse().is_touchpad());
        assert!(!keyboard().is_touchpad());
    }

    #[test]
    fn keyboard_settings_compile_layouts_and_migrate_the_legacy_one() {
        // Phase `keyboard_settings_compile_order_variants_active_and_compose`.
        {
            let section = serde_json::json!({
                "keyboard_layout": "de:nodeadkeys",
                "keyboard_layouts": ["us", "de:nodeadkeys", "fr"],
                "compose_key": "ralt"
            })
            .as_object()
            .unwrap()
            .clone();
            let settings = KeyboardSettings::from_section(&section);
            assert_eq!(settings.layouts, "us,de,fr");
            assert_eq!(settings.variants, ",nodeadkeys,");
            assert_eq!(settings.active_index, 1);
            assert_eq!(settings.options.as_deref(), Some("compose:ralt"));
        }
        // Phase `a_source_is_named_as_xkb_describes_it`.
        {
            let settings = KeyboardSettings::default();
            let label = settings.source_label(0);
            assert!(
                label == "English (US)" || label == "us",
                "XKB's description, or the id when no XKB data is installed: {label}"
            );
            assert_eq!(settings.source_label(3), "", "no fourth source");
        }
        // Phase `keyboard_settings_migrate_the_single_legacy_layout`.
        {
            let section = serde_json::json!({ "keyboard_layout": "gb" })
                .as_object()
                .unwrap()
                .clone();
            let settings = KeyboardSettings::from_section(&section);
            assert_eq!(settings.source_ids, ["gb"]);
            assert_eq!(settings.layouts, "gb");
            assert_eq!(settings.variants, "");
            assert_eq!(settings.active_index, 0);
        }
    }

    // touchpad click and scroll methods

    #[test]
    fn click_method_choices_map_onto_libinput_methods() {
        for (choice, expected) in [
            (TouchpadClickMethod::Areas, Some(ClickMethod::ButtonAreas)),
            (
                TouchpadClickMethod::Clickfinger,
                Some(ClickMethod::Clickfinger),
            ),
            (TouchpadClickMethod::Default, Some(ClickMethod::ButtonAreas)),
        ] {
            let settings = InputSettings {
                touchpad_click_method: choice,
                ..Default::default()
            };
            assert_eq!(plan_for(&settings, &touchpad()).click_method, expected);
        }
    }

    #[test]
    fn default_click_method_follows_the_device_not_a_fixed_value() {
        // An Apple-style clickpad defaults to clickfinger; "Default" must
        // restore that rather than forcing button areas on it.
        let apple = DeviceCaps {
            default_click_method: Some(ClickMethod::Clickfinger),
            ..touchpad()
        };
        assert_eq!(
            plan_for(&InputSettings::default(), &apple).click_method,
            Some(ClickMethod::Clickfinger)
        );
    }

    #[test]
    fn unsupported_click_methods_are_never_requested() {
        let areas_only = DeviceCaps {
            supports_clickfinger: false,
            ..touchpad()
        };
        let settings = InputSettings {
            touchpad_click_method: TouchpadClickMethod::Clickfinger,
            ..Default::default()
        };
        assert_eq!(plan_for(&settings, &areas_only).click_method, None);
        assert_eq!(plan_for(&settings, &mouse()).click_method, None);
        assert_eq!(
            plan_for(&InputSettings::default(), &mouse()).click_method,
            None,
            "a mouse has no default click method to restore"
        );
    }

    #[test]
    fn scroll_method_is_capability_checked() {
        let edge = InputSettings {
            touchpad_scroll_method: TouchpadScrollMethod::Edge,
            ..Default::default()
        };
        assert_eq!(
            plan_for(&InputSettings::default(), &touchpad()).scroll_method,
            Some(ScrollMethod::TwoFinger)
        );
        assert_eq!(
            plan_for(&edge, &touchpad()).scroll_method,
            Some(ScrollMethod::Edge)
        );
        let single_touch = DeviceCaps {
            supports_two_finger_scroll: false,
            ..touchpad()
        };
        assert_eq!(
            plan_for(&InputSettings::default(), &single_touch).scroll_method,
            None
        );
        assert_eq!(plan_for(&edge, &mouse()).scroll_method, None);
        assert_eq!(plan_for(&edge, &keyboard()).scroll_method, None);
    }

    #[test]
    fn touchpad_choices_and_scroll_speed_are_read_from_the_section() {
        let s = InputSettings::from_section(&section(json!({
            "touchpad_click_method": "clickfinger",
            "touchpad_scroll_method": "edge",
            "scroll_speed": 2.5,
        })));
        assert_eq!(s.touchpad_click_method, TouchpadClickMethod::Clickfinger);
        assert_eq!(s.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(s.scroll_speed, 2.5);

        let odd = InputSettings::from_section(&section(json!({
            "touchpad_click_method": 3,
            "scroll_speed": -1.0,
        })));
        assert_eq!(odd.touchpad_click_method, TouchpadClickMethod::Default);
        assert_eq!(
            odd.scroll_speed,
            settings_store::hardware::SCROLL_SPEED_MIN,
            "out-of-range speeds clamp into the slider"
        );
        assert_eq!(InputSettings::default().scroll_speed, 1.0);
    }

    #[test]
    fn drag_lock_rides_on_tapping_and_tap_and_drag() {
        assert!(!InputSettings::default().tap_drag_lock);
        assert_eq!(
            plan_for(&InputSettings::default(), &touchpad()).tap_drag_lock,
            Some(false),
            "off by default, and said so explicitly so switching back undoes it"
        );
        let locked = InputSettings::from_section(&section(json!({"tap_drag_lock": true})));
        assert!(locked.tap_drag_lock);
        assert_eq!(plan_for(&locked, &touchpad()).tap_drag_lock, Some(true));
        for (tap_to_click, tap_drag) in [(false, true), (true, false)] {
            let without_drag = InputSettings {
                tap_to_click,
                tap_drag,
                ..locked
            };
            assert_eq!(
                plan_for(&without_drag, &touchpad()).tap_drag_lock,
                Some(false),
                "a lock needs a tap-and-drag to hold"
            );
        }
        assert_eq!(plan_for(&locked, &mouse()).tap_drag_lock, None);
    }

    #[test]
    fn every_touchpad_choice_reaches_its_libinput_setting() {
        let s = InputSettings::from_section(&section(json!({
            "tap_to_click": true,
            "tap_drag": true,
            "tap_drag_lock": true,
            "disable_while_typing": false,
            "middle_emulation": false,
            "touchpad_natural_scroll": false,
            "touchpad_click_method": "clickfinger",
            "touchpad_scroll_method": "edge",
        })));
        let plan = plan_for(&s, &touchpad());
        assert_eq!(plan.tap, Some(true));
        assert_eq!(plan.tap_drag, Some(true));
        assert_eq!(plan.tap_drag_lock, Some(true));
        assert_eq!(plan.disable_while_typing, Some(false));
        assert_eq!(plan.middle_emulation, Some(false));
        assert_eq!(plan.natural_scroll, Some(false));
        assert_eq!(plan.click_method, Some(ClickMethod::Clickfinger));
        assert_eq!(plan.scroll_method, Some(ScrollMethod::Edge));
    }

    #[test]
    fn device_kind_puts_each_device_in_one_settings_section() {
        assert_eq!(touchpad().kind(), InputDeviceKind::Touchpad);
        assert_eq!(mouse().kind(), InputDeviceKind::Mouse);
        assert_eq!(keyboard().kind(), InputDeviceKind::Other);
        let typing = DeviceCaps {
            is_keyboard: true,
            ..DeviceCaps::default()
        };
        assert_eq!(typing.kind(), InputDeviceKind::Keyboard);
        let screen = DeviceCaps {
            is_touch: true,
            ..DeviceCaps::default()
        };
        assert_eq!(screen.kind(), InputDeviceKind::Touchscreen);
        let pen = DeviceCaps {
            is_tablet_tool: true,
            is_pointer: true,
            ..DeviceCaps::default()
        };
        assert_eq!(pen.kind(), InputDeviceKind::Tablet, "a pen is not a mouse");
        let pad = DeviceCaps {
            is_tablet_pad: true,
            is_keyboard: true,
            ..DeviceCaps::default()
        };
        assert_eq!(pad.kind(), InputDeviceKind::TabletPad);
    }

    #[test]
    fn a_pen_is_never_turned_by_the_mouse_handedness() {
        let pen = DeviceCaps {
            is_tablet_tool: true,
            supports_left_handed: true,
            ..DeviceCaps::default()
        };
        let left = InputSettings {
            left_handed: true,
            ..Default::default()
        };
        assert_eq!(plan_for(&left, &pen).left_handed, Some(false));
        assert_eq!(plan_for(&left, &mouse()).left_handed, Some(true));
    }

    #[derive(Default)]
    struct Recorder {
        caps: DeviceCaps,
        applied: Vec<DevicePlan>,
    }

    impl ConfigurableDevice for Recorder {
        fn caps(&self) -> DeviceCaps {
            self.caps
        }
        fn apply(&mut self, plan: &DevicePlan) {
            self.applied.push(*plan);
        }
    }

    #[test]
    fn reconfigure_all_reaches_every_device_with_its_own_plan() {
        let mut devices = [
            Recorder {
                caps: touchpad(),
                ..Recorder::default()
            },
            Recorder {
                caps: mouse(),
                ..Recorder::default()
            },
        ];
        let s = InputSettings {
            tap_to_click: false,
            ..Default::default()
        };
        reconfigure_all(&s, &mut devices);
        assert_eq!(devices[0].applied, [plan_for(&s, &touchpad())]);
        assert_eq!(devices[1].applied, [plan_for(&s, &mouse())]);
    }

    // accessibility keyboard filters

    #[test]
    fn bounce_keys_window_defaults_to_300ms_and_is_off_until_enabled() {
        let d = AccessibilityInputSettings::default();
        assert_eq!(d.bounce_window(), Duration::ZERO);
        let on = AccessibilityInputSettings::from_section(&section(json!({
            "bounce_keys": true,
        })));
        assert_eq!(on.bounce_window(), Duration::from_millis(300));
        let clamped = AccessibilityInputSettings::from_section(&section(json!({
            "bounce_keys": true,
            "bounce_keys_delay_ms": 60_000,
            "mouse_keys": true,
        })));
        assert_eq!(clamped.bounce_window(), Duration::from_millis(1_000));
        assert!(clamped.mouse_keys);
        assert!(!clamped.sticky_keys);
    }
    // settings watch

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tilo-input-watch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("settings.json")
    }

    fn watch(path: &Path) -> SettingsWatch {
        SettingsWatch::at(Some(path.to_path_buf())).with_interval(Duration::ZERO)
    }

    #[test]
    fn settings_watch_reports_each_real_change_once_and_rate_limits() {
        // Phase `the_first_look_is_a_baseline_not_a_change`.
        {
            // Otherwise every session start re-applies settings already applied while constructing the backend.
            let path = scratch("baseline");
            std::fs::write(&path, "{}").expect("write");
            let mut w = watch(&path);
            assert!(!w.changed(Instant::now()));
            let _ = std::fs::remove_file(&path);
        }
        // Phase `a_write_is_noticed_once`.
        {
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
        // Phase `the_file_appearing_for_the_first_time_counts_as_a_change`.
        {
            // A fresh install has no settings.json until the user opens Settings; the first write is exactly when the compositor has to start honouring it.
            let path = scratch("appear");
            let _ = std::fs::remove_file(&path);
            let mut w = watch(&path);
            assert!(!w.changed(Instant::now()), "baseline: no file");
            std::fs::write(&path, "{}").expect("write");
            assert!(w.changed(Instant::now()));
            let _ = std::fs::remove_file(&path);
        }
        // Phase `checks_are_rate_limited`.
        {
            // The loop runs at the display's cadence; stat'ing 144 times a second for a rarely-edited file is waste.
            let path = scratch("ratelimit");
            std::fs::write(&path, "{}").expect("write");
            let mut w =
                SettingsWatch::at(Some(path.clone())).with_interval(Duration::from_secs(3600));
            let now = Instant::now();
            assert!(!w.changed(now), "baseline");
            std::fs::remove_file(&path).expect("remove");
            assert!(
                !w.changed(now),
                "a second call inside the interval must not even look"
            );
            // Phase `a_notified_write_is_seen_inside_the_interval`.
            // An idle compositor wakes only for the notification; waiting out the interval would drop the write until unrelated input arrived.
            w.check_now();
            assert!(w.changed(now), "the removal must be seen on demand");
            assert!(
                !w.changed(now),
                "and the rate limit resumes after the forced look"
            );
        }
        // Phase `a_watch_with_no_settings_path_never_reports_a_change`.
        {
            // No config directory at all — a container with no HOME. The compositor must not spin on it.
            let mut w = SettingsWatch::at(None).with_interval(Duration::ZERO);
            assert!(!w.changed(Instant::now()));
            assert!(!w.changed(Instant::now()));
        }
    }

    #[test]
    fn input_method_sources_compile_no_group_and_select_the_primary_layout() {
        let section = serde_json::json!({
            "keyboard_layout": "ime/rime",
            "keyboard_layouts": ["de", "ime/rime", "fr"]
        })
        .as_object()
        .unwrap()
        .clone();
        let settings = KeyboardSettings::from_section(&section);
        assert_eq!(settings.source_ids, ["de", "fr"]);
        assert_eq!(settings.layouts, "de,fr");
        assert_eq!(
            settings.active_index, 0,
            "the engine types on the primary layout"
        );

        let section = serde_json::json!({
            "keyboard_layout": "fr",
            "keyboard_layouts": ["de", "ime/rime", "fr"]
        })
        .as_object()
        .unwrap()
        .clone();
        assert_eq!(KeyboardSettings::from_section(&section).active_index, 1);
    }
}
