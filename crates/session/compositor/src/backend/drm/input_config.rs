//! Pointer and touchpad device policy.
//!
//! This module is the decision half: it takes the person's input preferences
//! ([`InputConfig`]) plus what one libinput device can actually do, and
//! produces the exact `config_*` values to apply. [`super::libinput`] holds
//! the thin applier that pushes them onto a real libinput device.
//!
//! libinput defaults tapping to *off* because it can't tell a laptop touchpad
//! from a desk drawing tablet, and a spurious click from a resting palm is
//! worse than a missing one. On a laptop that reasoning inverts (a trackpad
//! requiring physical depression per click is the most-reported "this feels
//! broken" first impression), so the default here is on, with palm detection
//! and disable-while-typing turned on alongside it to buy back what libinput
//! was cautious about; the setting remains the person's to flip.

use std::collections::BTreeMap;
use std::time::Duration;

use super::tablet::TabletConfig;

/// Slowest auto-repeat the `key_repeat` slider can select, in keys per
/// second: below this, holding a key feels broken rather than slow.
pub const REPEAT_RATE_MIN: u32 = 10;
/// Fastest auto-repeat the slider can select, in keys per second.
pub const REPEAT_RATE_MAX: u32 = 50;
/// Longest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MAX_MS: u32 = 600;
/// Shortest delay before repeat starts, in milliseconds.
pub const REPEAT_DELAY_MIN_MS: u32 = 200;

/// Which button a clickpad press means.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TouchpadClickMethod {
    /// Whatever the device does untouched.
    #[default]
    Default,
    /// Software button areas along the bottom edge.
    Areas,
    /// The number of fingers on the pad picks the button.
    Clickfinger,
}

/// How a touchpad scrolls.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TouchpadScrollMethod {
    /// Two fingers moving together.
    #[default]
    TwoFinger,
    /// One finger along the pad's edge.
    Edge,
}

/// The person's input preferences, with the defaults applied. The defaults
/// match the Settings pane's starting values, so a person who never opened
/// Settings and one who opened it and changed nothing get the same session.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "input toggles: each is an independent, named setting mirroring the Settings pane, not a state machine"
)]
pub struct InputConfig {
    pub touchpad_natural_scroll: bool,
    /// Content-follows-finger scrolling for mice and trackballs.
    pub mouse_natural_scroll: bool,
    /// Tap the touchpad to click. **On** by default: see the module docs.
    pub tap_to_click: bool,
    /// Pointer acceleration, `0.0` (slowest) to `1.0` (fastest), `0.5`
    /// being libinput's neutral profile.
    pub pointer_speed: f64,
    pub touchpad_pointer_speed: Option<f64>,
    pub mouse_pointer_speed: Option<f64>,
    /// Keyboard auto-repeat, `0.0` (slow, long delay) to `1.0` (fast, short
    /// delay).
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
    /// Multiplier on every scroll distance, wheel and finger alike. Applied
    /// by the compositor rather than libinput, which has no such setting.
    pub scroll_speed: f64,
    /// Bounce keys: a key pressed again within this window of its release
    /// is ignored. `None` is off.
    pub bounce_keys: Option<Duration>,
    /// Mouse keys: the keypad moves and clicks the pointer.
    pub mouse_keys: bool,
    /// Each drawing tablet's settings, by [`device_key`].
    pub tablets: BTreeMap<String, TabletConfig>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            touchpad_natural_scroll: true,
            mouse_natural_scroll: false,
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
            bounce_keys: None,
            mouse_keys: false,
            tablets: BTreeMap::new(),
        }
    }
}

/// The identity per-device settings are keyed by: USB vendor and product
/// and the device's name, as Settings stores them.
pub fn device_key(vendor: u32, product: u32, name: &str) -> String {
    format!("{vendor:04x}:{product:04x}:{}", name.trim())
}

impl InputConfig {
    /// libinput's pointer-acceleration speed for this setting: the slider's
    /// `0.0 ..= 1.0` mapped onto libinput's `-1.0 ..= 1.0`, so the midpoint
    /// lands exactly on the untouched default of `0.0`.
    pub fn accel_speed(&self) -> f64 {
        (self.pointer_speed.clamp(0.0, 1.0) * 2.0) - 1.0
    }

    /// [`Self::accel_speed`] with the per-class override for touchpads or
    /// mice applied.
    pub fn accel_speed_for(&self, touchpad: bool) -> f64 {
        let speed = if touchpad {
            self.touchpad_pointer_speed.unwrap_or(self.pointer_speed)
        } else {
            self.mouse_pointer_speed.unwrap_or(self.pointer_speed)
        };
        (speed.clamp(0.0, 1.0) * 2.0) - 1.0
    }

    pub const fn natural_scroll_for(&self, touchpad: bool) -> bool {
        if touchpad {
            self.touchpad_natural_scroll
        } else {
            self.mouse_natural_scroll
        }
    }

    /// Keyboard auto-repeat as `(delay, rate in keys per second)`. The two
    /// move together in opposite directions ("faster" means both a shorter
    /// wait before it starts and a shorter gap between keys), so exposing
    /// them as one slider is only honest if the mapping does that.
    pub fn key_repeat_params(&self) -> (Duration, u32) {
        if let (Some(delay), Some(rate)) = (self.key_repeat_delay_ms, self.key_repeat_rate_hz) {
            return (Duration::from_millis(u64::from(delay)), rate);
        }
        let t = self.key_repeat.clamp(0.0, 1.0);
        let rate = f64::from(REPEAT_RATE_MIN)
            + t * f64::from(REPEAT_RATE_MAX.saturating_sub(REPEAT_RATE_MIN));
        let delay = f64::from(REPEAT_DELAY_MAX_MS)
            - t * f64::from(REPEAT_DELAY_MAX_MS.saturating_sub(REPEAT_DELAY_MIN_MS));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "t is clamped to [0, 1], so delay stays within 200..=600 and rate within 10..=50"
        )]
        let (delay_ms, rate) = (delay.round() as u64, rate.round() as u32);
        (Duration::from_millis(delay_ms), rate)
    }
}

/// What one libinput device is capable of. Every field is a question
/// libinput answers per device (`config_tap_finger_count`,
/// `config_scroll_has_natural_scroll`, `config_accel_is_available`, ...), so
/// the plan below never asks a device for something it doesn't have.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors libinput's per-capability query API one bool at a time, not a state machine"
)]
pub struct DeviceCaps {
    /// The device reports tap fingers, i.e. it is a touchpad.
    pub supports_tap: bool,
    pub supports_natural_scroll: bool,
    pub supports_accel: bool,
    /// Disable-while-typing, which only touchpads have.
    pub supports_dwt: bool,
    /// Emulate a middle click from a simultaneous left+right press.
    pub supports_middle_emulation: bool,
    /// The device moves the pointer.
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

/// The kind of device, as Settings groups devices.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Touchpad,
    Mouse,
    Keyboard,
    Touchscreen,
    Tablet,
    TabletPad,
    Other,
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
    /// A touchpad is the device class the laptop-specific defaults exist
    /// for: tapping, disable-while-typing, and two-finger scrolling.
    pub const fn is_touchpad(&self) -> bool {
        self.supports_tap
    }

    /// The kind this device is. A tablet's pad and pen are checked first:
    /// both can also report pointer capabilities.
    pub const fn kind(&self) -> DeviceKind {
        if self.is_tablet_pad {
            DeviceKind::TabletPad
        } else if self.is_tablet_tool {
            DeviceKind::Tablet
        } else if self.is_touchpad() {
            DeviceKind::Touchpad
        } else if self.is_touch {
            DeviceKind::Touchscreen
        } else if self.is_pointer {
            DeviceKind::Mouse
        } else if self.is_keyboard {
            DeviceKind::Keyboard
        } else {
            DeviceKind::Other
        }
    }
}

/// The configuration to push onto one device. `None` means "this device
/// cannot be told that, so don't ask".
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DevicePlan {
    pub tap: Option<bool>,
    /// Tap-and-drag rides with tapping: a person who taps to click expects
    /// tap-hold-move to drag. Turning tapping on without it produces a
    /// touchpad that clicks but cannot drag a window.
    pub tap_drag: Option<bool>,
    /// Only ever on while tap-and-drag is: a lock on a drag that cannot
    /// start would do nothing but confuse the next tap.
    pub tap_drag_lock: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub disable_while_typing: Option<bool>,
    pub middle_emulation: Option<bool>,
    /// Present as milli-units (`accel_speed * 1000`, rounded) so the plan
    /// stays `Eq` and comparable in tests; the applier divides it back.
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
    /// libinput's acceleration speed, back in its own `-1.0 ..= 1.0` units.
    pub fn accel_speed(&self) -> Option<f64> {
        self.accel_speed_milli.map(|m| f64::from(m) / 1000.0)
    }
}

/// Decide what to configure on a device with `caps`, given `config`: a pure
/// function of the two, since it is applied on device add and re-applied on
/// every change (a plan depending on prior state would drift on re-plug).
pub fn plan_for(config: &InputConfig, caps: &DeviceCaps) -> DevicePlan {
    DevicePlan {
        tap: caps.supports_tap.then_some(config.tap_to_click),
        tap_drag: caps
            .supports_tap
            .then_some(config.tap_to_click && config.tap_drag),
        tap_drag_lock: caps
            .supports_tap
            .then_some(config.tap_to_click && config.tap_drag && config.tap_drag_lock),
        natural_scroll: caps
            .supports_natural_scroll
            .then_some(config.natural_scroll_for(caps.is_touchpad())),
        // Disable-while-typing is what makes tap-to-click safe to default on.
        disable_while_typing: caps.supports_dwt.then_some(config.disable_while_typing),
        // A touchpad with no physical middle button is the norm; without
        // emulation there's no way to middle-click at all.
        middle_emulation: caps
            .supports_middle_emulation
            .then_some(config.middle_emulation),
        accel_speed_milli: caps.supports_accel.then(|| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "accel_speed_for() is bounded to [-1.0, 1.0]; milli-units stay within [-1000, 1000]"
            )]
            let milli = (config.accel_speed_for(caps.is_touchpad()) * 1000.0).round() as i32;
            milli
        }),
        // A pen's orientation is its tablet's own setting, applied when its
        // coordinates are mapped; the mouse preference must not also turn it.
        left_handed: caps
            .supports_left_handed
            .then_some(config.left_handed && !caps.is_tablet_tool),
        flat_acceleration: caps
            .supports_flat_acceleration
            .then_some(config.flat_acceleration),
        send_events: caps.is_touchpad().then_some(if !config.touchpad_enabled {
            SendEvents::Disabled
        } else if config.touchpad_disabled_on_external_mouse
            && caps.supports_disable_on_external_mouse
        {
            SendEvents::DisabledOnExternalMouse
        } else {
            SendEvents::Enabled
        }),
        click_method: match config.touchpad_click_method {
            TouchpadClickMethod::Default => caps.default_click_method,
            TouchpadClickMethod::Areas => caps
                .supports_click_areas
                .then_some(ClickMethod::ButtonAreas),
            TouchpadClickMethod::Clickfinger => caps
                .supports_clickfinger
                .then_some(ClickMethod::Clickfinger),
        },
        scroll_method: match config.touchpad_scroll_method {
            TouchpadScrollMethod::TwoFinger => caps
                .supports_two_finger_scroll
                .then_some(ScrollMethod::TwoFinger),
            TouchpadScrollMethod::Edge => caps.supports_edge_scroll.then_some(ScrollMethod::Edge),
        },
    }
}

/// A device the configuration can be pushed onto. libinput devices
/// implement it in [`super::libinput`]; a test double proves the same path.
pub trait ConfigurableDevice {
    /// What the device can be told.
    fn caps(&self) -> DeviceCaps;
    /// Push `plan` onto the device. Every `None` field is left untouched.
    fn apply(&mut self, plan: &DevicePlan);
}

/// Configure one device from `config`: when it is added, and again for each
/// change. Returns the plan that was applied.
pub fn configure<D: ConfigurableDevice + ?Sized>(
    config: &InputConfig,
    device: &mut D,
) -> DevicePlan {
    let plan = plan_for(config, &device.caps());
    device.apply(&plan);
    plan
}

/// Re-apply `config` to every attached device, so a change is felt on the
/// touchpad under the person's fingers rather than on the next one plugged
/// in.
pub fn reconfigure_all<D: ConfigurableDevice>(config: &InputConfig, devices: &mut [D]) {
    for device in devices {
        configure(config, device);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "exact normalized input fixtures are the contract under test"
)]
mod tests {
    use super::*;

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
            supports_natural_scroll: true,
            supports_accel: true,
            is_pointer: true,
            supports_left_handed: true,
            supports_flat_acceleration: true,
            ..DeviceCaps::default()
        }
    }

    fn keyboard() -> DeviceCaps {
        DeviceCaps::default()
    }

    #[test]
    fn defaults_tap_on_a_laptop_and_keep_wheels_conventional() {
        // libinput says off, but that's the wrong answer on a laptop.
        let d = InputConfig::default();
        assert!(d.tap_to_click);
        assert!(d.touchpad_natural_scroll);
        assert!(!d.mouse_natural_scroll);
        assert_eq!(d.pointer_speed, 0.5);
        assert_eq!(d.key_repeat, 0.5);
        assert_eq!(plan_for(&d, &touchpad()).natural_scroll, Some(true));
        assert_eq!(plan_for(&d, &mouse()).natural_scroll, Some(false));
        assert_eq!(d.bounce_keys, None);
        assert!(!d.mouse_keys);
    }

    #[test]
    fn pointer_speed_and_key_repeat_map_onto_libinput_ranges() {
        // 0.5 must map to exactly 0.0, or everyone who never touched the
        // slider gets a different acceleration than libinput's default.
        assert_eq!(InputConfig::default().accel_speed(), 0.0);
        let slow = InputConfig {
            pointer_speed: 0.0,
            key_repeat: 0.0,
            ..Default::default()
        };
        let fast = InputConfig {
            pointer_speed: 1.0,
            key_repeat: 1.0,
            ..Default::default()
        };
        assert_eq!(slow.accel_speed(), -1.0);
        assert_eq!(fast.accel_speed(), 1.0);
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
        let (delay, rate) = InputConfig::default().key_repeat_params();
        assert_eq!(rate, 30);
        assert_eq!(delay, Duration::from_millis(400));
        let exact = InputConfig {
            key_repeat_delay_ms: Some(275),
            key_repeat_rate_hz: Some(37),
            ..Default::default()
        };
        assert_eq!(exact.key_repeat_params(), (Duration::from_millis(275), 37));
        let disabled = InputConfig {
            key_repeat_delay_ms: Some(500),
            key_repeat_rate_hz: Some(0),
            ..Default::default()
        };
        assert_eq!(
            disabled.key_repeat_params(),
            (Duration::from_millis(500), 0)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario, one block per case")]
    fn device_plan_follows_config_and_capabilities() {
        // A touchpad gets tapping, dragging and palm protection.
        {
            let plan = plan_for(&InputConfig::default(), &touchpad());
            assert_eq!(plan.tap, Some(true));
            assert_eq!(
                plan.tap_drag,
                Some(true),
                "tapping without tap-and-drag is a touchpad that cannot drag"
            );
            assert_eq!(plan.disable_while_typing, Some(true));
            assert_eq!(plan.middle_emulation, Some(true));
        }
        // The touchpad toggle switches off touchpads and nothing else.
        {
            assert_eq!(
                plan_for(&InputConfig::default(), &touchpad()).send_events,
                Some(SendEvents::Enabled)
            );
            let off = InputConfig {
                touchpad_enabled: false,
                ..Default::default()
            };
            assert_eq!(
                plan_for(&off, &touchpad()).send_events,
                Some(SendEvents::Disabled)
            );
            assert_eq!(plan_for(&off, &mouse()).send_events, None);
            assert_eq!(plan_for(&off, &keyboard()).send_events, None);
        }
        // A touchpad can step aside while a mouse is connected.
        {
            let with_mouse = InputConfig {
                touchpad_disabled_on_external_mouse: true,
                ..Default::default()
            };
            assert_eq!(
                plan_for(&with_mouse, &touchpad()).send_events,
                Some(SendEvents::DisabledOnExternalMouse)
            );
            assert_eq!(plan_for(&with_mouse, &mouse()).send_events, None);
            let unsupported = DeviceCaps {
                supports_disable_on_external_mouse: false,
                ..touchpad()
            };
            assert_eq!(
                plan_for(&with_mouse, &unsupported).send_events,
                Some(SendEvents::Enabled),
                "a touchpad libinput cannot hand off keeps working"
            );
            let off = InputConfig {
                touchpad_enabled: false,
                ..with_mouse
            };
            assert_eq!(
                plan_for(&off, &touchpad()).send_events,
                Some(SendEvents::Disabled),
                "switched off means off, mouse or not"
            );
        }
        // Turning tap off turns tap-drag off with it.
        {
            let s = InputConfig {
                tap_to_click: false,
                ..Default::default()
            };
            let plan = plan_for(&s, &touchpad());
            assert_eq!(plan.tap, Some(false));
            assert_eq!(plan.tap_drag, Some(false));
        }
        // A mouse is never asked about tapping: libinput errors every call.
        {
            let plan = plan_for(&InputConfig::default(), &mouse());
            assert_eq!(plan.tap, None);
            assert_eq!(plan.tap_drag, None);
            assert_eq!(plan.disable_while_typing, None);
            assert_eq!(plan.middle_emulation, None);
        }
        // Mouse and touchpad keep independent scroll directions.
        {
            let s = InputConfig {
                touchpad_natural_scroll: false,
                mouse_natural_scroll: true,
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &touchpad()).natural_scroll, Some(false));
            assert_eq!(plan_for(&s, &mouse()).natural_scroll, Some(true));
            assert_eq!(plan_for(&s, &keyboard()).natural_scroll, None);
        }
        // Pointer speed reaches devices with acceleration, per class.
        {
            let s = InputConfig {
                pointer_speed: 1.0,
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &mouse()).accel_speed(), Some(1.0));
            let s = InputConfig {
                pointer_speed: 0.0,
                ..Default::default()
            };
            assert_eq!(plan_for(&s, &touchpad()).accel_speed(), Some(-1.0));
            assert_eq!(plan_for(&s, &keyboard()).accel_speed(), None);
            let split = InputConfig {
                touchpad_pointer_speed: Some(1.0),
                mouse_pointer_speed: Some(0.0),
                ..Default::default()
            };
            assert_eq!(plan_for(&split, &touchpad()).accel_speed(), Some(1.0));
            assert_eq!(plan_for(&split, &mouse()).accel_speed(), Some(-1.0));
        }
        // A keyboard gets an empty plan.
        assert_eq!(
            plan_for(&InputConfig::default(), &keyboard()),
            DevicePlan::default()
        );
        assert!(touchpad().is_touchpad());
        assert!(!mouse().is_touchpad());
        assert!(!keyboard().is_touchpad());
    }

    #[test]
    fn click_methods_follow_the_choice_and_the_device() {
        for (choice, expected) in [
            (TouchpadClickMethod::Areas, Some(ClickMethod::ButtonAreas)),
            (
                TouchpadClickMethod::Clickfinger,
                Some(ClickMethod::Clickfinger),
            ),
            (TouchpadClickMethod::Default, Some(ClickMethod::ButtonAreas)),
        ] {
            let config = InputConfig {
                touchpad_click_method: choice,
                ..Default::default()
            };
            assert_eq!(plan_for(&config, &touchpad()).click_method, expected);
        }
        // An Apple-style clickpad defaults to clickfinger; "Default" must
        // restore that rather than forcing button areas on it.
        let apple = DeviceCaps {
            default_click_method: Some(ClickMethod::Clickfinger),
            ..touchpad()
        };
        assert_eq!(
            plan_for(&InputConfig::default(), &apple).click_method,
            Some(ClickMethod::Clickfinger)
        );
        // Unsupported methods are never requested.
        let areas_only = DeviceCaps {
            supports_clickfinger: false,
            ..touchpad()
        };
        let clickfinger = InputConfig {
            touchpad_click_method: TouchpadClickMethod::Clickfinger,
            ..Default::default()
        };
        assert_eq!(plan_for(&clickfinger, &areas_only).click_method, None);
        assert_eq!(plan_for(&clickfinger, &mouse()).click_method, None);
        assert_eq!(
            plan_for(&InputConfig::default(), &mouse()).click_method,
            None
        );
    }

    #[test]
    fn scroll_method_is_capability_checked() {
        let edge = InputConfig {
            touchpad_scroll_method: TouchpadScrollMethod::Edge,
            ..Default::default()
        };
        assert_eq!(
            plan_for(&InputConfig::default(), &touchpad()).scroll_method,
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
            plan_for(&InputConfig::default(), &single_touch).scroll_method,
            None
        );
        assert_eq!(plan_for(&edge, &mouse()).scroll_method, None);
        assert_eq!(plan_for(&edge, &keyboard()).scroll_method, None);
    }

    #[test]
    fn drag_lock_rides_on_tapping_and_tap_and_drag() {
        assert_eq!(
            plan_for(&InputConfig::default(), &touchpad()).tap_drag_lock,
            Some(false),
            "off by default, and said so explicitly so switching back undoes it"
        );
        let locked = InputConfig {
            tap_drag_lock: true,
            ..Default::default()
        };
        assert_eq!(plan_for(&locked, &touchpad()).tap_drag_lock, Some(true));
        for (tap_to_click, tap_drag) in [(false, true), (true, false)] {
            let without_drag = InputConfig {
                tap_to_click,
                tap_drag,
                ..locked.clone()
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
    fn device_kind_puts_each_device_in_one_group() {
        assert_eq!(touchpad().kind(), DeviceKind::Touchpad);
        assert_eq!(mouse().kind(), DeviceKind::Mouse);
        assert_eq!(keyboard().kind(), DeviceKind::Other);
        let typing = DeviceCaps {
            is_keyboard: true,
            ..DeviceCaps::default()
        };
        assert_eq!(typing.kind(), DeviceKind::Keyboard);
        let screen = DeviceCaps {
            is_touch: true,
            ..DeviceCaps::default()
        };
        assert_eq!(screen.kind(), DeviceKind::Touchscreen);
        let pen = DeviceCaps {
            is_tablet_tool: true,
            is_pointer: true,
            ..DeviceCaps::default()
        };
        assert_eq!(pen.kind(), DeviceKind::Tablet, "a pen is not a mouse");
        let pad = DeviceCaps {
            is_tablet_pad: true,
            is_keyboard: true,
            ..DeviceCaps::default()
        };
        assert_eq!(pad.kind(), DeviceKind::TabletPad);
    }

    #[test]
    fn a_pen_is_never_turned_by_the_mouse_handedness() {
        let pen = DeviceCaps {
            is_tablet_tool: true,
            supports_left_handed: true,
            ..DeviceCaps::default()
        };
        let left = InputConfig {
            left_handed: true,
            ..Default::default()
        };
        assert_eq!(plan_for(&left, &pen).left_handed, Some(false));
        assert_eq!(plan_for(&left, &mouse()).left_handed, Some(true));
    }

    #[test]
    fn device_keys_match_what_settings_stores() {
        assert_eq!(
            device_key(0x056a, 0x0374, " Wacom Intuos "),
            "056a:0374:Wacom Intuos"
        );
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
        let s = InputConfig {
            tap_to_click: false,
            ..Default::default()
        };
        reconfigure_all(&s, &mut devices);
        assert_eq!(devices[0].applied, [plan_for(&s, &touchpad())]);
        assert_eq!(devices[1].applied, [plan_for(&s, &mouse())]);
    }
}
