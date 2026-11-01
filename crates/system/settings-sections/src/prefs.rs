//! The shared **system-preferences store** — durable per-user device and
//! session preferences the Settings panes own.
//!
//! Preferences the rest of the system reads (system sounds, night light,
//! pointer behaviour, update policy), one typed section each.
//!
//! # The document (a format contract, not a crate dependency)
//!
//! Preferences live in the **same** settings document the region store uses
//! (`settings_store::Store`), namespaced by domain so each section grows
//! independently and unrelated writers never collide:
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale":  { … },                       // owned by region.rs
//!   "audio":   { "system_sounds": { "enabled": true, "volume": 0.7, … } },
//!   "display": { "auto_brightness": false, "night_light": false },
//!   "input":   { "touchpad_natural_scroll": true, "tap_to_click": true,
//!                "pointer_speed": 0.5, "key_repeat": 0.5,
//!                "keyboard_layout": "us" },
//!   "updates": { "auto": true, "last_checked": "Never" },
//!   "appearance": { "theme": "Dark", "high_contrast": false,
//!                   "reduce_motion": false, "text_scale": 1.0, "ui_scale": 1.0 },
//!   "notifications": { "do_not_disturb": false, "show_banners": true,
//!                      "show_previews": true, "play_sounds": true,
//!                      "show_on_lock_screen": false },
//!   "accessibility": { "large_cursor": false, "cursor_size": 24,
//!                      "screen_reader": false, "on_screen_keyboard": "auto",
//!                      "sticky_keys": false, "slow_keys": false,
//!                      "bounce_keys": false, "bounce_keys_delay_ms": 300,
//!                      "bounce_keys_alert": false, "mouse_keys": false,
//!                      "visual_alerts": false }
//! }
//! ```
//!
//! # Non-destructive writes
//!
//! Each section is stored on its own and keeps every key it does not name,
//! so a pane that changes one preference never clobbers another pane's — or
//! the region store's, or the compositor's — keys.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The person's persistent audio transforms and which system events make a
/// sound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct AudioPrefs {
    /// "System sounds": master switch, volume, theme and per-event choices.
    /// The compositor plays them; see [`settings_store::system_sounds`].
    #[serde(default)]
    pub system_sounds: settings_store::system_sounds::SystemSoundPrefs,
    /// Persistent left/right balance in UI space (`0` left, `0.5` centre,
    /// `1` right). The compositor session, not this pane, owns actuation.
    #[serde(default = "default_audio_balance")]
    #[model(range = 0.0..=1.0)]
    pub balance: f32,
    /// Persistent accessibility mono transform. A compositor-session owner
    /// keeps the bounded virtual sink alive across Settings navigation.
    #[serde(default)]
    #[model(readonly)]
    pub mono_output: bool,
    /// Give calls an echo-cancelled microphone. The compositor session owns
    /// the `PipeWire` client that provides it.
    #[serde(default = "default_true")]
    #[model(readonly)]
    pub echo_cancellation: bool,
}

const fn default_audio_balance() -> f32 {
    0.5
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            system_sounds: settings_store::system_sounds::SystemSoundPrefs::default(),
            balance: default_audio_balance(),
            mono_output: false,
            echo_cancellation: true,
        }
    }
}

/// This device's remembered default audio devices.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
pub struct AudioDevicePrefs {
    /// Label of the last chosen default sink (output device), if any.
    #[model(readonly)]
    pub output_device: Option<String>,
    /// Label of the last chosen default source (input device), if any.
    #[model(readonly)]
    pub input_device: Option<String>,
}

/// The night light: a comfort of every display the person uses, applied by
/// the compositor's output layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct NightLightPrefs {
    /// Warm the whole screen to [`Self::night_light_temperature`].
    #[serde(default)]
    pub night_light: bool,
    /// Target correlated colour temperature used by the compositor's
    /// whole-output Night Light transform.
    #[serde(default = "default_night_light_temperature")]
    #[model(range = 1_000..=10_000)]
    pub night_light_temperature: u32,
    /// Follow the Appearance day/night schedule instead of the manual switch:
    /// on from sunset, off from sunrise.
    #[serde(default)]
    pub night_light_schedule: bool,
}

impl Default for NightLightPrefs {
    fn default() -> Self {
        Self {
            night_light: false,
            night_light_schedule: false,
            night_light_temperature: default_night_light_temperature(),
        }
    }
}

/// This device's display preferences, applied by the compositor's output
/// layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct DisplayPrefs {
    #[model(readonly)]
    pub auto_brightness: bool,
    /// `HiDPI` display scale factor: `1.0` = 100 %, `1.25`, `1.5`, `1.75`,
    /// `2.0`. The compositor reads this section of the shared settings
    /// file at session start and advertises it to clients as the
    /// preferred `wp_fractional_scale`, so a `ui` app renders crisp at a
    /// fractional factor. `#[serde(default)]` keeps configs written before
    /// this field existed loadable (they take `1.0`).
    #[serde(default = "default_display_scale")]
    #[model(range = 0.5..=4.0)]
    pub scale: f64,
}

fn default_display_scale() -> f64 {
    1.0
}

fn default_night_light_temperature() -> u32 {
    3_700
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self {
            auto_brightness: false,
            scale: 1.0,
        }
    }
}

/// Pointer + keyboard behaviour the compositor's libinput/xkb configuration
/// reads at session start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from the settings; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct InputPrefs {
    /// Content follows the fingers on a touchpad, as on a phone.
    pub touchpad_natural_scroll: bool,
    /// Content follows the wheel, the way a touchpad's natural scroll does.
    pub mouse_natural_scroll: bool,
    /// A tap on the touchpad clicks.
    pub tap_to_click: bool,
    /// How fast the pointer moves, slow (0) to fast (1).
    #[model(range = 0.0..=1.0)]
    pub pointer_speed: f64,
    /// The touchpad's own pointer speed; none follows `pointer_speed`.
    #[model(range = 0.0..=1.0)]
    pub touchpad_pointer_speed: Option<f64>,
    /// The mouse's own pointer speed; none follows `pointer_speed`.
    #[model(range = 0.0..=1.0)]
    pub mouse_pointer_speed: Option<f64>,
    /// How fast a held key repeats, slow (0) to fast (1).
    #[model(range = 0.0..=1.0)]
    pub key_repeat: f64,
    /// Exact host-imported repeat pair. A Tilo slider selection clears both.
    #[model(readonly)]
    pub key_repeat_delay_ms: Option<u32>,
    #[model(readonly)]
    pub key_repeat_rate_hz: Option<u32>,
    /// xkb layout code, e.g. `"us"`, `"de"`.
    #[model(readonly)]
    pub keyboard_layout: String,
    /// Tap, then touch again and move, to drag.
    pub tap_drag: bool,
    /// Keep a tap-and-drag going after the finger lifts, until the next tap.
    pub tap_drag_lock: bool,
    /// Ignore the touchpad while typing.
    pub disable_while_typing: bool,
    /// Pressing left and right buttons together is a middle click.
    pub middle_emulation: bool,
    /// Swap the primary and secondary buttons.
    pub left_handed: bool,
    /// Whether the pointer speeds up with faster movement (`adaptive`) or
    /// moves the same distance at any speed (`flat`); capability-gated by
    /// libinput.
    #[model(choices = ["adaptive", "flat"])]
    pub accel_profile: String,
    /// Three- and four-finger gestures switch workspaces and show the
    /// overview.
    pub touchpad_gestures: bool,
    /// Which button a clickpad press means; capability-gated by libinput.
    pub touchpad_click_method: settings_store::hardware::TouchpadClickMethod,
    /// Two-finger or edge scrolling; capability-gated by libinput.
    pub touchpad_scroll_method: settings_store::hardware::TouchpadScrollMethod,
    /// Scroll-distance multiplier for wheels and touchpads.
    #[model(range = settings_store::hardware::SCROLL_SPEED_MIN..=settings_store::hardware::SCROLL_SPEED_MAX)]
    pub scroll_speed: f64,
    /// XKB compose option (`ralt`, `menu`, …), or none.
    #[model(readonly)]
    pub compose_key: Option<String>,
    /// What Caps Lock does (`ctrl`, `escape`, `backspace`), or none for
    /// Caps Lock itself.
    #[model(readonly)]
    pub caps_lock: Option<String>,
    /// Ordered XKB layouts. `keyboard_layout` remains the active first item
    /// for backward compatibility with old sessions.
    #[model(readonly)]
    pub keyboard_layouts: Vec<String>,
    /// Per-app paste chord the shell presses when it pastes for the person
    /// (`ctrl_v` or `ctrl_shift_v`), keyed by app id; apps not listed get
    /// [`settings_store::paste_shortcut::default_for`].
    #[serde(deserialize_with = "settings_store::paste_shortcut::lenient_overrides")]
    #[model(readonly)]
    pub paste_shortcuts: std::collections::BTreeMap<String, String>,
}

impl Default for InputPrefs {
    fn default() -> Self {
        // A person who never chose a layout types the machine's: this whole
        // section is written back on any input change, so a fixed US default
        // here would pin US over the layout the system was installed with.
        let keyboard_layouts = settings_store::keyboard_sources::default_sources();
        let keyboard_layout = keyboard_layouts.first().cloned().unwrap_or_default();
        Self {
            touchpad_natural_scroll: settings_store::hardware::DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            mouse_natural_scroll: settings_store::hardware::DEFAULT_MOUSE_NATURAL_SCROLL,
            tap_to_click: true,
            pointer_speed: 0.5,
            touchpad_pointer_speed: None,
            mouse_pointer_speed: None,
            key_repeat: 0.5,
            key_repeat_delay_ms: None,
            key_repeat_rate_hz: None,
            keyboard_layout,
            tap_drag: true,
            tap_drag_lock: false,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            accel_profile: "adaptive".into(),
            touchpad_gestures: true,
            touchpad_click_method: settings_store::hardware::TouchpadClickMethod::Default,
            touchpad_scroll_method: settings_store::hardware::TouchpadScrollMethod::TwoFinger,
            scroll_speed: 1.0,
            compose_key: None,
            caps_lock: None,
            keyboard_layouts,
            paste_shortcuts: std::collections::BTreeMap::new(),
        }
    }
}

/// The person's pointer and keyboard choices, read by the compositor's input
/// layer.
impl ui::storage::SettingsSchema for InputPrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::hardware::INPUT_SECTION);
    ui::settings_codec!();
}

/// This device's input: whether its touchpads are on, its physical
/// keyboards, its tablets and what the keyboard inspector shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
pub struct InputDevicePrefs {
    /// Whether touchpads send events at all. The touchpad-toggle key writes
    /// the same key, so both stay in agreement.
    pub touchpad_enabled: bool,
    /// Switch touchpads off while an external mouse is connected;
    /// capability-gated by libinput.
    pub touchpad_disabled_on_external_mouse: bool,
    /// Explicit physical model; no hardware detection is implied.
    #[model(readonly)]
    pub keyboard_model: String,
    /// The layouts each of this device's keyboards types, by its identity.
    #[serde(deserialize_with = "settings_store::keyboard_sources::lenient_profiles")]
    #[model(readonly)]
    pub keyboard_assignments:
        std::collections::BTreeMap<String, settings_store::keyboard_sources::KeyboardProfile>,
    /// Layout being inspected without changing the active input source.
    #[model(readonly)]
    pub keyboard_preview: Option<String>,
    /// Inspect the higher native XKB levels without changing typed input.
    #[model(readonly)]
    pub keyboard_preview_extra_levels: bool,
    /// Ordered native level inspected without changing the live seat.
    #[model(readonly)]
    pub keyboard_preview_level: usize,
    /// Explicit physical position inspected; never follows or records key presses.
    #[model(readonly)]
    pub keyboard_inspected_key: String,
    /// Per-tablet mapping, pressure and buttons, keyed by device identity.
    #[serde(deserialize_with = "settings_store::hardware::lenient_tablets")]
    #[model(readonly)]
    pub tablets: std::collections::BTreeMap<String, settings_store::hardware::TabletConfig>,
}

impl Default for InputDevicePrefs {
    fn default() -> Self {
        Self {
            touchpad_enabled: true,
            touchpad_disabled_on_external_mouse: false,
            keyboard_model: "pc105".into(),
            keyboard_assignments: std::collections::BTreeMap::new(),
            keyboard_preview: None,
            keyboard_preview_extra_levels: false,
            keyboard_preview_level: 0,
            keyboard_inspected_key: "AD01".into(),
            tablets: std::collections::BTreeMap::new(),
        }
    }
}

impl ui::storage::SettingsSchema for InputDevicePrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::hardware::INPUT_DEVICE_SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.input-device", label = "Input on this device")]
impl InputDevicePrefs {
    /// Give one of this device's keyboards its own layouts, the one it types
    /// with and each layout's options; `all` holds the options of the shared
    /// layouts.
    #[action(id = "set-keyboard-layouts")]
    pub fn set_keyboard_layouts(
        &mut self,
        #[param("The keyboard's device key, or `all` for the shared layouts")] keyboard: String,
        #[param(
            "Its layouts in switching order, the one it types with, and each layout's options"
        )]
        profile: settings_store::keyboard_sources::KeyboardProfile,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        device_key(&keyboard)?;
        if keyboard == settings_store::keyboard_sources::OSK {
            return Err(ui::tilo_resource::Refusal::new(
                "the on-screen keyboard's layouts are its own section's",
            ));
        }
        valid_layouts(&profile.sources, &profile.active)?;
        if let Some(layout) = profile
            .options
            .keys()
            .find(|layout| !profile.sources.contains(layout))
        {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{layout}` has options but is not one of the layouts"
            )));
        }
        if let Some(model) = profile
            .options
            .values()
            .filter_map(|options| options.model.as_deref())
            .find(|model| !valid_keyboard_model(model))
        {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{model}` is not a physical keyboard model"
            )));
        }
        self.keyboard_assignments.insert(keyboard, profile);
        Ok(())
    }

    /// Make a keyboard that types its own layouts type the shared ones
    /// again, forgetting its own list.
    #[action(id = "use-shared-layouts")]
    pub fn use_shared_layouts(
        &mut self,
        #[param("The keyboard's device key")] keyboard: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let shared = matches!(
            keyboard.as_str(),
            settings_store::keyboard_sources::ALL | settings_store::keyboard_sources::OSK
        );
        if shared || self.keyboard_assignments.remove(&keyboard).is_none() {
            return Err(ui::tilo_resource::Refusal::new(
                "this keyboard already types the shared layouts",
            ));
        }
        Ok(())
    }

    /// Choose the physical keyboard model the keyboard drawing shows and the
    /// keyboard types with.
    #[action(id = "set-keyboard-model")]
    pub fn set_keyboard_model(
        &mut self,
        #[param("`pc104`, `pc105`, `jp106` or `kr106`")] model: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !valid_keyboard_model(&model) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{model}` is not a physical keyboard model"
            )));
        }
        self.keyboard_model = model;
        self.keyboard_inspected_key = "AD01".into();
        self.keyboard_preview_level = 0;
        self.keyboard_preview_extra_levels = false;
        Ok(())
    }

    /// Draw a layout without typing with it, or, with none, what the
    /// keyboard really types.
    #[action(id = "show-keyboard-layout", housekeeping)]
    pub fn show_keyboard_layout(
        &mut self,
        #[param("An XKB layout or layout:variant (de, us:intl); none for the live keyboard")]
        layout: Option<String>,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if let Some(layout) = &layout
            && settings_store::hardware::KeyboardInputSource::parse(layout).is_none()
        {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{layout}` is not a keyboard layout"
            )));
        }
        self.keyboard_preview = layout;
        self.keyboard_preview_level = 0;
        self.keyboard_preview_extra_levels = false;
        Ok(())
    }

    /// Choose the modifier level and the physical key the keyboard drawing
    /// inspects, without typing anything.
    #[action(id = "inspect-keyboard", housekeeping)]
    pub fn inspect_keyboard(
        &mut self,
        #[param("The modifier level, counting from 0")] level: u32,
        #[param("Whether the levels past the fourth are offered")] extra_levels: bool,
        #[param("The physical key's XKB name, such as `AD01`")] key: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let level = usize::try_from(level)
            .ok()
            .filter(|level| *level < MAX_KEYBOARD_LEVELS)
            .ok_or_else(|| ui::tilo_resource::Refusal::new("no keyboard has this level"))?;
        if key.is_empty() || key.len() > 8 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{key}` is not a physical key"
            )));
        }
        self.keyboard_preview_level = level;
        self.keyboard_preview_extra_levels = extra_levels;
        self.keyboard_inspected_key = key;
        Ok(())
    }

    /// Map a drawing tablet onto one display, or every display.
    #[action(id = "set-tablet-output")]
    pub fn set_tablet_output(
        &mut self,
        #[param("The tablet's device id")] device: String,
        #[param("A display's connector, or `desktop`")] output: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let output = if output == tablet_choices::WHOLE_DESKTOP {
            None
        } else if output.is_empty() || output.len() > 128 || output.chars().any(char::is_control) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{output}` is not a display"
            )));
        } else {
            Some(output)
        };
        self.tablet(&device)?.output = output;
        Ok(())
    }

    /// Keep a tablet's aspect ratio when mapping it, or stretch it.
    #[action(id = "set-tablet-keep-aspect")]
    pub fn set_tablet_keep_aspect(
        &mut self,
        #[param("The tablet's device id")] device: String,
        on: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.tablet(&device)?.keep_aspect = on;
        Ok(())
    }

    /// Turn a tablet around for the left hand, or back.
    #[action(id = "set-tablet-left-handed")]
    pub fn set_tablet_left_handed(
        &mut self,
        #[param("The tablet's device id")] device: String,
        on: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.tablet(&device)?.left_handed = on;
        Ok(())
    }

    /// Set how firmly a tablet's pen must press.
    #[action(id = "set-tablet-tip-feel")]
    pub fn set_tablet_tip_feel(
        &mut self,
        #[param("The tablet's device id")] device: String,
        #[param("Soft (0.0) to firm (1.0)")] feel: f64,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !(feel.is_finite() && (0.0..=1.0).contains(&feel)) {
            return Err(ui::tilo_resource::Refusal::new(
                "the value must be between 0.0 and 1.0",
            ));
        }
        self.tablet(&device)?.pressure_curve =
            settings_store::hardware::pressure_curve_for_feel(feel);
        Ok(())
    }

    /// Set what one of a pen's buttons does.
    #[action(id = "set-tablet-button")]
    pub fn set_tablet_button(
        &mut self,
        #[param("The tablet's device id")] device: String,
        #[param("The button: `lower`, `upper` or `third`")] button: String,
        #[param("A pen-button choice: `left`, `right`, `disabled`, `shortcut:search`, …")]
        action: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let action = tablet_choices::stylus_button(&action).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{action}` is not a pen-button choice"))
        })?;
        let config = self.tablet(&device)?;
        match button.as_str() {
            "lower" => config.lower_button = action,
            "upper" => config.upper_button = action,
            "third" => config.third_button = action,
            _ => {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{button}` is not a pen button"
                )));
            }
        }
        Ok(())
    }

    /// Set what a pen's eraser end does.
    #[action(id = "set-tablet-eraser")]
    pub fn set_tablet_eraser(
        &mut self,
        #[param("The tablet's device id")] device: String,
        #[param("An eraser choice: `native`, `right`, `shortcut:search`, …")] action: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let action = tablet_choices::eraser(&action).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{action}` is not an eraser choice"))
        })?;
        self.tablet(&device)?.eraser = action;
        Ok(())
    }

    /// Set what a pad button does.
    #[action(id = "set-pad-button")]
    pub fn set_pad_button(
        &mut self,
        #[param("The pad's device id")] device: String,
        #[param("The button, counting from 0")] button: u32,
        #[param("`none`, `show-map` or a shortcut such as `shortcut:search`")] action: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if button >= tablet_choices::MAX_PAD_BUTTONS {
            return Err(ui::tilo_resource::Refusal::new("unknown pad button"));
        }
        let choice = tablet_choices::pad_button(&action).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{action}` is not a pad-button choice"))
        })?;
        tablet_choices::set_pad_button(self.tablet(&device)?, button, choice);
        Ok(())
    }

    /// Set what a pad's ring or strip does in one mode.
    #[action(id = "set-pad-dial")]
    pub fn set_pad_dial(
        &mut self,
        #[param("The pad's device id")] device: String,
        #[param("`ring` or `strip`")] dial: String,
        #[param("Which ring or strip, counting from 0")] index: u32,
        #[param("The mode, counting from 0")] mode: u32,
        #[param(
            "What it does: `nothing`, `scroll`, `zoom`, `brush-size`, `volume` or `brightness`"
        )]
        action: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if index >= tablet_choices::MAX_PAD_DIALS || mode >= tablet_choices::MAX_PAD_MODES {
            return Err(ui::tilo_resource::Refusal::new("unknown pad control"));
        }
        let dial = match dial.as_str() {
            "ring" => settings_store::hardware::PadDial::Ring(index),
            "strip" => settings_store::hardware::PadDial::Strip(index),
            _ => {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{dial}` is not a ring or strip"
                )));
            }
        };
        let action = tablet_choices::dial(&action).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{action}` is not a dial choice"))
        })?;
        self.tablet(&device)?.set_dial_action(dial, mode, action);
        Ok(())
    }
}

/// The most modifier levels a keyboard drawing inspects.
const MAX_KEYBOARD_LEVELS: usize = 32;

impl InputDevicePrefs {
    /// The saved settings of the tablet or pad `device`, created on first
    /// use. Keys are device identities from the compositor's snapshot, so
    /// anything else is refused rather than stored.
    fn tablet(
        &mut self,
        device: &str,
    ) -> Result<&mut settings_store::hardware::TabletConfig, ui::tilo_resource::Refusal> {
        device_key(device)?;
        Ok(self.tablets.entry(device.to_owned()).or_default())
    }
}

/// Refuse what cannot be a device identity from the compositor's snapshot.
fn device_key(key: &str) -> Result<(), ui::tilo_resource::Refusal> {
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
        return Err(ui::tilo_resource::Refusal::new("unknown device"));
    }
    Ok(())
}

/// Whether `model` is a physical keyboard model the keyboard drawing knows.
#[must_use]
pub fn valid_keyboard_model(model: &str) -> bool {
    matches!(model, "pc104" | "pc105" | "jp106" | "kr106")
}

/// Refuse layouts that are not keyboard layouts or input methods, a list
/// without the one typed with, or none.
fn valid_layouts(layouts: &[String], active: &str) -> Result<(), ui::tilo_resource::Refusal> {
    if let Some(layout) = layouts
        .iter()
        .find(|layout| settings_store::hardware::InputSource::parse(layout).is_none())
    {
        return Err(ui::tilo_resource::Refusal::new(format!(
            "`{layout}` is not a keyboard layout"
        )));
    }
    if !layouts.iter().any(|layout| layout == active) {
        return Err(ui::tilo_resource::Refusal::new(format!(
            "`{active}` is not one of the layouts"
        )));
    }
    Ok(())
}

/// A tablet's pen-button, eraser, pad-button and ring choices as Settings
/// names them: stable ids an agent passes and a menu offers.
pub mod tablet_choices {
    use settings_store::hardware::{
        EraserAction, PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig,
    };

    /// The Map-to choice for spanning every display.
    pub const WHOLE_DESKTOP: &str = "desktop";
    /// Pad buttons Settings offers a shortcut for; pads have far fewer.
    pub const MAX_PAD_BUTTONS: u32 = 32;
    /// Rings and strips Settings offers a mapping for, of each kind.
    pub const MAX_PAD_DIALS: u32 = 4;
    /// Modes per ring or strip Settings offers a mapping for.
    pub const MAX_PAD_MODES: u32 = 8;
    /// The pad-button choice for a button that does nothing.
    pub const PAD_BUTTON_NONE: &str = "none";
    /// The pad-button choice for the button that shows the pad's map.
    pub const PAD_BUTTON_SHOW_MAP: &str = "show-map";

    /// The pen-button choices besides the shortcuts, by id.
    pub const STYLUS_BUTTONS: [(&str, StylusButtonAction); 7] = [
        ("default", StylusButtonAction::Default),
        ("left", StylusButtonAction::Left),
        ("middle", StylusButtonAction::Middle),
        ("right", StylusButtonAction::Right),
        ("back", StylusButtonAction::Back),
        ("forward", StylusButtonAction::Forward),
        ("disabled", StylusButtonAction::Disabled),
    ];

    /// The eraser choices besides the shortcuts, by id.
    pub const ERASERS: [(&str, EraserAction); 6] = [
        ("native", EraserAction::Native),
        ("left", EraserAction::Left),
        ("middle", EraserAction::Middle),
        ("right", EraserAction::Right),
        ("back", EraserAction::Back),
        ("forward", EraserAction::Forward),
    ];

    /// What a ring or strip can do, by id.
    pub const DIALS: [(&str, PadDialAction); 6] = [
        ("nothing", PadDialAction::Nothing),
        ("scroll", PadDialAction::Scroll),
        ("zoom", PadDialAction::Zoom),
        ("brush-size", PadDialAction::BrushSize),
        ("volume", PadDialAction::Volume),
        ("brightness", PadDialAction::Brightness),
    ];

    /// `shortcut:<name>`, the choice running a desktop shortcut.
    #[must_use]
    pub fn shortcut_id(action: ShortcutAction) -> String {
        let name = serde_json::to_value(action)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        format!("shortcut:{name}")
    }

    /// The shortcut a `shortcut:<name>` choice runs.
    #[must_use]
    pub fn shortcut(id: &str) -> Option<ShortcutAction> {
        let name = id.strip_prefix("shortcut:")?;
        serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
    }

    #[must_use]
    pub fn stylus_button_id(action: StylusButtonAction) -> String {
        match action {
            StylusButtonAction::Shortcut(shortcut) => shortcut_id(shortcut),
            other => STYLUS_BUTTONS
                .iter()
                .find(|(_, value)| *value == other)
                .map_or("default", |(id, _)| id)
                .to_string(),
        }
    }

    #[must_use]
    pub fn stylus_button(id: &str) -> Option<StylusButtonAction> {
        STYLUS_BUTTONS
            .iter()
            .find(|(choice, _)| *choice == id)
            .map(|(_, value)| *value)
            .or_else(|| shortcut(id).map(StylusButtonAction::Shortcut))
    }

    #[must_use]
    pub fn eraser_id(action: EraserAction) -> String {
        match action {
            EraserAction::Shortcut(shortcut) => shortcut_id(shortcut),
            other => ERASERS
                .iter()
                .find(|(_, value)| *value == other)
                .map_or("native", |(id, _)| id)
                .to_string(),
        }
    }

    #[must_use]
    pub fn eraser(id: &str) -> Option<EraserAction> {
        ERASERS
            .iter()
            .find(|(choice, _)| *choice == id)
            .map(|(_, value)| *value)
            .or_else(|| shortcut(id).map(EraserAction::Shortcut))
    }

    #[must_use]
    pub fn dial_id(action: PadDialAction) -> &'static str {
        DIALS
            .iter()
            .find(|(_, value)| *value == action)
            .map_or("scroll", |(id, _)| id)
    }

    #[must_use]
    pub fn dial(id: &str) -> Option<PadDialAction> {
        DIALS
            .iter()
            .find(|(choice, _)| *choice == id)
            .map(|(_, value)| *value)
    }

    /// What a pad button does in an app without tablet support.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PadButtonChoice {
        Nothing,
        /// Shows what every control does while held.
        ShowMap,
        Shortcut(ShortcutAction),
    }

    /// The choice `config` makes for pad button `button`.
    #[must_use]
    pub fn pad_button_id(config: &TabletConfig, button: u32) -> String {
        if config.pad_map_button == Some(button) {
            return PAD_BUTTON_SHOW_MAP.to_string();
        }
        config.pad_buttons.get(&button).map_or_else(
            || PAD_BUTTON_NONE.to_string(),
            |shortcut| shortcut_id(*shortcut),
        )
    }

    /// The choice an id names; `None` when it is not one.
    #[must_use]
    pub fn pad_button(id: &str) -> Option<PadButtonChoice> {
        match id {
            PAD_BUTTON_NONE => Some(PadButtonChoice::Nothing),
            PAD_BUTTON_SHOW_MAP => Some(PadButtonChoice::ShowMap),
            _ => shortcut(id).map(PadButtonChoice::Shortcut),
        }
    }

    /// Make pad button `button` do `choice`. One button shows the map:
    /// picking it for another moves it there.
    pub fn set_pad_button(config: &mut TabletConfig, button: u32, choice: PadButtonChoice) {
        if config.pad_map_button == Some(button) {
            config.pad_map_button = None;
        }
        match choice {
            PadButtonChoice::Nothing => {
                config.pad_buttons.remove(&button);
            }
            PadButtonChoice::ShowMap => {
                config.pad_buttons.remove(&button);
                config.pad_map_button = Some(button);
            }
            PadButtonChoice::Shortcut(shortcut) => {
                config.pad_buttons.insert(button, shortcut);
            }
        }
    }
}

#[ui::tilo_resource::resource(
    kind = "settings.input",
    label = "Input",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl InputPrefs {
    /// Set how fast held keys repeat, from slow (0) to fast (1); an exact
    /// delay and rate taken from another system no longer apply.
    #[action]
    pub fn set_key_repeat(
        &mut self,
        #[param("From 0 (slow) to 1 (fast)")] rate: f64,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.key_repeat = rate;
        self.key_repeat_delay_ms = None;
        self.key_repeat_rate_hz = None;
        Ok(())
    }

    /// Type with a keyboard layout, adding it to the layouts when it is not
    /// one of them yet.
    #[action(id = "choose-layout")]
    pub fn choose_layout(
        &mut self,
        #[param("An XKB layout or layout:variant (de, us:intl)")] layout: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let next = settings_store::keyboard_sources::plan_active_source(
            &self.rich_input(&InputDevicePrefs::default()),
            &layout,
        )
        .map_err(ui::tilo_resource::Refusal::new)?;
        self.keyboard_layouts = next.keyboard_layouts;
        self.keyboard_layout = next.keyboard_layout;
        Ok(())
    }

    /// Type with these keyboard layouts, switching between them in this
    /// order.
    #[action(id = "set-layouts")]
    pub fn set_layouts(
        &mut self,
        #[param(
            "XKB layouts or layout:variants (de, us:intl) and input methods, in switching order"
        )]
        layouts: Vec<String>,
        #[param("The one typed with, one of them")] active: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        valid_layouts(&layouts, &active)?;
        self.keyboard_layouts = layouts;
        self.keyboard_layout = active;
        self.normalise_keyboard_sources();
        Ok(())
    }

    /// Set which shortcut pastes in one app when the shell pastes for the
    /// person (Super+V with an image or files), or `default` to go back to
    /// what its app id suggests (Ctrl+Shift+V for terminals, else Ctrl+V).
    #[action(id = "set-paste-shortcut")]
    pub fn paste_with(
        &mut self,
        #[param("The app's id, such as `org.wezfurlong.wezterm`")] app: String,
        #[param("The shortcut: `ctrl_v`, `ctrl_shift_v` or `default`")] shortcut: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.set_paste_shortcut(&app, &shortcut)
            .map_err(ui::tilo_resource::Refusal::new)
    }
}

impl InputPrefs {
    /// Set which shortcut pastes in `app` when the shell pastes for the
    /// person (Super+V with an image or files), or `default` to go back to
    /// what its app id suggests (Ctrl+Shift+V for terminals, else Ctrl+V).
    ///
    /// # Errors
    ///
    /// No app id, or a shortcut that is none of those.
    pub fn set_paste_shortcut(&mut self, app: &str, shortcut: &str) -> Result<(), String> {
        use settings_store::paste_shortcut as paste;
        let app = app.trim();
        if !paste::valid_app_id(app) {
            return Err("an app id is needed".into());
        }
        let chosen = match shortcut {
            "default" => None,
            other => Some(
                paste::PasteShortcut::parse(other)
                    .ok_or_else(|| format!("`{other}` is not a paste shortcut"))?,
            ),
        };
        self.paste_shortcuts
            .retain(|id, _| !id.eq_ignore_ascii_case(app));
        if let Some(chosen) = chosen {
            self.paste_shortcuts
                .insert(app.to_string(), chosen.id().to_string());
        }
        Ok(())
    }
}

impl InputPrefs {
    /// These choices on a device with `device`'s keyboards and tablets, as
    /// the compositor reads them.
    pub fn rich_input(
        &self,
        device: &InputDevicePrefs,
    ) -> settings_store::hardware::RichInputConfig {
        settings_store::hardware::RichInputConfig {
            pointer_speed: self.pointer_speed,
            touchpad_pointer_speed: self.touchpad_pointer_speed,
            mouse_pointer_speed: self.mouse_pointer_speed,
            touchpad_natural_scroll: self.touchpad_natural_scroll,
            mouse_natural_scroll: self.mouse_natural_scroll,
            tap_to_click: self.tap_to_click,
            tap_drag: self.tap_drag,
            tap_drag_lock: self.tap_drag_lock,
            disable_while_typing: self.disable_while_typing,
            middle_emulation: self.middle_emulation,
            left_handed: self.left_handed,
            accel_profile: if self.accel_profile == "flat" {
                settings_store::hardware::AccelProfile::Flat
            } else {
                settings_store::hardware::AccelProfile::Adaptive
            },
            key_repeat: self.key_repeat,
            key_repeat_delay_ms: self.key_repeat_delay_ms,
            key_repeat_rate_hz: self.key_repeat_rate_hz,
            keyboard_layout: self.keyboard_layout.clone(),
            keyboard_layouts: self.keyboard_layouts.clone(),
            keyboard_model: device.keyboard_model.clone(),
            keyboard_assignments: device.keyboard_assignments.clone(),
            compose_key: self.compose_key.clone(),
            caps_lock: self.caps_lock.clone(),
            touchpad_gestures: self.touchpad_gestures,
            touchpad_click_method: self.touchpad_click_method,
            touchpad_scroll_method: self.touchpad_scroll_method,
            scroll_speed: self.scroll_speed,
            tablets: device.tablets.clone(),
        }
    }

    /// Reconcile the legacy active layout with the ordered source list using
    /// the same validation and bounds the compositor enforces.
    pub fn normalise_keyboard_sources(&mut self) {
        let mut config = self.rich_input(&InputDevicePrefs::default());
        config.normalise_sources();
        self.keyboard_layout = config.keyboard_layout;
        self.keyboard_layouts = config.keyboard_layouts;
    }
}

/// A configurable response to a power event (lid close, power key, critical
/// battery). Serialises to the lowercase labels the compositor's power policy
/// parses (`"suspend"` / `"lock"` / `"ignore"`) — the on-disk JSON is the
/// contract shared with `tilo-power-mgmt`, not a code dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(rename_all = "lowercase")]
pub enum PowerActionPref {
    Suspend,
    Lock,
    Ignore,
}

impl PowerActionPref {
    /// The human-readable label shown in the pane's dropdowns.
    pub fn display(self) -> &'static str {
        match self {
            PowerActionPref::Suspend => "Suspend",
            PowerActionPref::Lock => "Lock screen",
            PowerActionPref::Ignore => "Do nothing",
        }
    }
}

/// The longest idle delay offered: a day, in minutes.
const MINUTES_PER_DAY: u32 = 24 * 60;

fn default_auto_suspend() -> bool {
    settings_store::power::DEFAULT_AUTO_SUSPEND
}
fn default_suspend_minutes() -> u32 {
    15
}
fn default_lock_minutes() -> u32 {
    5
}
fn default_lock_on_sleep() -> bool {
    settings_store::power::DEFAULT_LOCK_ON_SLEEP
}
fn default_lid_action() -> PowerActionPref {
    PowerActionPref::Suspend
}
fn default_power_key_action() -> PowerActionPref {
    PowerActionPref::Suspend
}
fn default_low_battery_percent() -> u32 {
    15
}
fn default_critical_battery_percent() -> u32 {
    5
}
fn default_critical_action() -> PowerActionPref {
    PowerActionPref::Suspend
}

/// Power / session-idle preferences the compositor's power-management layer
/// reads. The active power *profile* is live system state
/// (`powerprofilesctl`), not stored here. Every field added after the initial
/// two carries `#[serde(default)]` so a config written before it existed still
/// loads. The keys mirror `tilo_power_mgmt::config`'s reader exactly.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(
    check = PowerPrefs::valid,
    check_reads = "low_battery_percent critical_battery_percent",
    defaults
)]
pub struct PowerPrefs {
    /// Master toggle for idle-suspend. `false` disables it regardless of
    /// [`Self::suspend_minutes`]. Unset means the shared default
    /// ([`settings_store::power::DEFAULT_AUTO_SUSPEND`], off), the same value
    /// the compositor and the `power.auto-suspend` action read.
    #[serde(default = "default_auto_suspend")]
    pub auto_suspend: bool,
    /// Idle minutes before the session suspends while plugged in, and on
    /// battery until that has its own; `0` disables idle-suspend.
    #[serde(default = "default_suspend_minutes")]
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub suspend_minutes: u32,
    /// Idle minutes before the session suspends on battery; unset, the
    /// plugged-in timer applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub battery_suspend_minutes: Option<u32>,
    /// A power profile's own idle-suspend timer, where one was stored. It
    /// applies only while the stored profile timers truly differ
    /// ([`settings_store::power::SuspendTimers`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub saver_suspend_minutes: Option<u32>,
    /// The balanced profile's own idle-suspend timer, where one was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub balanced_suspend_minutes: Option<u32>,
    /// The performance profile's own idle-suspend timer, where one was
    /// stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub performance_suspend_minutes: Option<u32>,
    /// Idle minutes before the screen blanks; `0` disables blanking.
    #[model(range = 0..=MINUTES_PER_DAY)]
    pub blank_minutes: u32,
    /// Idle minutes before the session locks; `0` is the explicit "Never"
    /// choice. Manual lock and the mandatory pre-suspend lock remain active.
    /// A longer delay or never weakens the lock; a shorter one does not.
    #[serde(default = "default_lock_minutes")]
    #[model(weaker = "higher", weakest = 0, range = 0..=MINUTES_PER_DAY)]
    pub lock_minutes: u32,
    /// Whether waking from sleep asks for the password. An idle, explicit or
    /// in-flight lock survives either way; only the lock this device adds on
    /// the way into sleep follows this.
    #[serde(default = "default_lock_on_sleep")]
    #[model(weaker = false)]
    pub lock_on_sleep: bool,
    /// What to do when the lid closes.
    #[serde(default = "default_lid_action")]
    pub lid_close_action: PowerActionPref,
    /// What to do when the power key is pressed.
    #[serde(default = "default_power_key_action")]
    pub power_key_action: PowerActionPref,
    /// Warn (a notification) at or below this battery percentage.
    #[serde(default = "default_low_battery_percent")]
    #[model(range = 0..=100)]
    pub low_battery_percent: u32,
    /// Take [`Self::critical_action`] at or below this battery percentage.
    #[serde(default = "default_critical_battery_percent")]
    #[model(range = 0..=100)]
    pub critical_battery_percent: u32,
    /// What to do when the battery is critically low.
    #[serde(default = "default_critical_action")]
    pub critical_action: PowerActionPref,
}

/// The person's desk widgets: which they keep, when each shows itself and
/// where each rests. The compositor reads the same leaves
/// ([`settings_store::desk_widgets`]), and the desk writes back a pin,
/// where a widget was dropped, its card's width and its removal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct DeskWidgetsPrefs {
    /// Now playing, with play/pause and next.
    #[serde(default = "default_music_widget")]
    pub music: settings_store::desk_widgets::WidgetPrefs,
    /// The people in this workspace.
    #[serde(default = "default_team_widget")]
    pub team: settings_store::desk_widgets::WidgetPrefs,
    /// Today's date, and the month around it.
    #[serde(default = "default_calendar_widget")]
    pub calendar: settings_store::desk_widgets::WidgetPrefs,
    /// Processor, memory and temperature.
    #[serde(default = "default_system_monitor_widget")]
    pub system_monitor: settings_store::desk_widgets::WidgetPrefs,
}

fn default_music_widget() -> settings_store::desk_widgets::WidgetPrefs {
    settings_store::desk_widgets::WidgetPrefs::default_for(
        settings_store::desk_widgets::WidgetId::Music,
    )
}

fn default_team_widget() -> settings_store::desk_widgets::WidgetPrefs {
    settings_store::desk_widgets::WidgetPrefs::default_for(
        settings_store::desk_widgets::WidgetId::Team,
    )
}

fn default_calendar_widget() -> settings_store::desk_widgets::WidgetPrefs {
    settings_store::desk_widgets::WidgetPrefs::default_for(
        settings_store::desk_widgets::WidgetId::Calendar,
    )
}

fn default_system_monitor_widget() -> settings_store::desk_widgets::WidgetPrefs {
    settings_store::desk_widgets::WidgetPrefs::default_for(
        settings_store::desk_widgets::WidgetId::SystemMonitor,
    )
}

impl Default for DeskWidgetsPrefs {
    fn default() -> Self {
        Self {
            music: default_music_widget(),
            team: default_team_widget(),
            calendar: default_calendar_widget(),
            system_monitor: default_system_monitor_widget(),
        }
    }
}

impl ui::storage::SettingsSchema for DeskWidgetsPrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::desk_widgets::SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.desk_widgets", label = "Desk widgets")]
impl DeskWidgetsPrefs {}

/// This device's idle and button preferences, read by the compositor's
/// idle layer.
impl ui::storage::SettingsSchema for PowerPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("power");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.power",
    label = "Power",
    version = 2,
    migrate = PowerPrefs::upgrade
)]
impl PowerPrefs {
    /// Version 1 stored every profile's suspend timer; version 2 stores one
    /// only where it was set. A timer version 1 stored reads as set, with the
    /// minutes it had, so nothing needs rewriting.
    fn upgrade(
        _doc: &mut ui::tilo_resource::Doc,
        from: u32,
    ) -> Result<(), ui::tilo_resource::ModelError> {
        if from == 1 {
            Ok(())
        } else {
            Err(ui::tilo_resource::ModelError::invalid(format!(
                "no migration from {from}"
            )))
        }
    }

    /// Suspend after this many idle minutes while plugged in; 0 never.
    #[action(id = "set-suspend-plugged-in")]
    pub fn suspend_plugged_in(
        &mut self,
        #[param(
            "Idle minutes before suspending while plugged in: 0 (never), 5, 10, 15, 30, 45 or 60"
        )]
        minutes: u32,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let minutes = Self::offered_suspend(minutes)?;
        self.suspend_minutes = minutes;
        // Profile timers that agree are this one timer: they keep agreeing.
        if !self.timers().profiles_differ() {
            for timer in [
                &mut self.saver_suspend_minutes,
                &mut self.balanced_suspend_minutes,
                &mut self.performance_suspend_minutes,
            ] {
                if timer.is_some() {
                    *timer = Some(minutes);
                }
            }
        }
        Ok(())
    }

    /// Suspend after this many idle minutes on battery; 0 never.
    #[action(id = "set-suspend-on-battery")]
    pub fn suspend_on_battery(
        &mut self,
        #[param("Idle minutes before suspending on battery: 0 (never), 5, 10, 15, 30, 45 or 60")]
        minutes: u32,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.battery_suspend_minutes = Some(Self::offered_suspend(minutes)?);
        Ok(())
    }
}

impl PowerPrefs {
    /// The idle-suspend timers as stored, and which applies when.
    #[must_use]
    pub fn timers(&self) -> settings_store::power::SuspendTimers {
        settings_store::power::SuspendTimers {
            plugged_in: self.suspend_minutes,
            battery: self.battery_suspend_minutes,
            profiles: [
                self.saver_suspend_minutes,
                self.balanced_suspend_minutes,
                self.performance_suspend_minutes,
            ],
        }
    }

    fn offered_suspend(minutes: u32) -> Result<u32, ui::tilo_resource::Refusal> {
        settings_store::power::check_minutes(
            "suspend timeout",
            settings_store::power::SUSPEND_CHOICES,
            minutes,
        )
        .map_err(ui::tilo_resource::Refusal::new)
    }
}

impl Default for PowerPrefs {
    fn default() -> Self {
        Self {
            auto_suspend: default_auto_suspend(),
            suspend_minutes: default_suspend_minutes(),
            battery_suspend_minutes: None,
            saver_suspend_minutes: None,
            balanced_suspend_minutes: None,
            performance_suspend_minutes: None,
            blank_minutes: 10,
            lock_minutes: default_lock_minutes(),
            lock_on_sleep: default_lock_on_sleep(),
            lid_close_action: default_lid_action(),
            power_key_action: default_power_key_action(),
            low_battery_percent: default_low_battery_percent(),
            critical_battery_percent: default_critical_battery_percent(),
            critical_action: default_critical_action(),
        }
    }
}

/// System-update policy the update timer/service reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct UpdatePrefs {
    #[model(readonly)]
    pub auto: bool,
    /// Human-readable timestamp of the last successful check, or `"Never"`.
    #[model(readonly)]
    pub last_checked: String,
}

/// This device's update preferences.
impl ui::storage::SettingsSchema for UpdatePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("updates");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.updates",
    label = "Updates",
    version = 2,
    migrate = UpdatePrefs::upgrade
)]
impl UpdatePrefs {
    /// Version 1 also stored the update channel, which nothing read.
    fn upgrade(
        doc: &mut ui::tilo_resource::Doc,
        from: u32,
    ) -> Result<(), ui::tilo_resource::ModelError> {
        if from != 1 {
            return Err(ui::tilo_resource::ModelError::invalid(format!(
                "no migration from {from}"
            )));
        }
        if automerge::ReadDoc::get(doc, automerge::ROOT, "channel")?.is_some() {
            automerge::transaction::Transactable::delete(doc, automerge::ROOT, "channel")?;
        }
        Ok(())
    }

    /// Keep whether the machine's update timer runs, as the machine said
    /// after a change: the timer is the setting, this what it last said.
    #[action(id = "record-automatic-updates", housekeeping)]
    pub fn record_automatic(
        &mut self,
        #[param("Whether the update timer runs")] on: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.auto = on;
        Ok(())
    }

    /// Keep when updates were last checked for successfully.
    #[action(id = "record-update-check", housekeeping)]
    pub fn record_check(
        &mut self,
        #[param("When the check finished, as a person reads it")] at: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if at.trim().is_empty() {
            return Err(ui::tilo_resource::Refusal::new("a time is needed"));
        }
        self.last_checked = at;
        Ok(())
    }
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto: true,
            last_checked: "Never".to_string(),
        }
    }
}

/// Look-and-feel preferences the OS theming layer resolves into the
/// concrete [`ui::Theme`] every app draws against. `theme` is a named preset
/// (see [`AppearancePrefs::THEMES`]); `high_contrast` overrides the palette
/// with the WCAG-AAA high-contrast track; `reduce_motion` and `text_scale`
/// fold in the accessibility comforts. The compositor reads this section the
/// A theme document Theme Studio wrote, kept as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CustomTheme(pub serde_json::Value);

tilo_resource::json_model!(CustomTheme);

/// same way it reads the display/input comfort sections.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent appearance switch, not one combined state"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(
    check = AppearancePrefs::valid,
    check_reads = "sunrise sunset ui_face accent translucency",
    defaults
)]
pub struct AppearancePrefs {
    /// Light, dark, or automatic: light by day and dark by night.
    #[serde(default = "default_mode")]
    #[model(choices = ["light", "dark", settings_store::day_schedule::AUTOMATIC_MODE])]
    pub mode: String,
    /// The theme dark mode shows: a preset id or `custom:<id>`.
    #[serde(default = "default_dark_theme")]
    pub dark_theme: String,
    /// The theme light mode shows: a preset id or `custom:<id>`.
    #[serde(default = "default_light_theme")]
    pub light_theme: String,
    /// The theme documents a `custom:<id>` names. Theme Studio writes them;
    /// Settings keeps them intact and resolves them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[model(readonly)]
    pub custom_themes: Vec<CustomTheme>,
    /// Spacing density over whichever theme is shown: one of
    /// [`ui::theme::ThemeDensity`]'s `COMPACT`, `REGULAR` and `ROOMY`.
    #[serde(default = "default_density")]
    #[model(range = 0.5..=2.0)]
    pub density: f64,
    /// Force the high-contrast accessibility palette, overriding the theme.
    #[serde(default)]
    pub high_contrast: bool,
    /// Snap animations to their resting state instead of easing.
    #[serde(default)]
    pub reduce_motion: bool,
    /// Make menus, panels and sheets opaque instead of blurred.
    #[serde(default)]
    pub reduce_transparency: bool,
    /// How large text is drawn, `1.0` as the theme sets it. Scales *type
    /// only*.
    #[serde(default = "default_ui_scale")]
    #[model(range = 0.5..=3.0)]
    pub text_scale: f64,
    /// How large everything is drawn, `1.0` as the theme sets it: the "make
    /// everything bigger" knob. Scales the *whole* design system (spacing, radii, and the
    /// type scale) together via [`ui::Theme::with_ui_scale`]. Composes on top of
    /// the per-output fractional device scale the compositor reports, and text
    /// additionally picks up `text_scale`.
    #[serde(default = "default_ui_scale")]
    #[model(range = 0.5..=3.0)]
    pub ui_scale: f64,
    /// When the day ends, for the "Automatic" mode and a scheduled night
    /// light: `"sun"` follows sunrise and sunset at the time zone's location,
    /// `"custom"` uses [`Self::sunrise`] and [`Self::sunset`]
    /// (`settings_store::day_schedule`).
    #[serde(default = "default_schedule")]
    #[model(choices = ["sun", "custom"])]
    pub schedule: String,
    /// Custom sunrise, `HH:MM`; also the fallback when the sun cannot be followed.
    #[serde(default = "default_sunrise")]
    pub sunrise: String,
    /// Custom sunset, `HH:MM`; also the fallback when the sun cannot be followed.
    #[serde(default = "default_sunset")]
    pub sunset: String,
    /// Whether the wallpaper covers `"workspace"` (the one the person is on)
    /// or `"all"` workspaces. The compositor applies a change to the
    /// wallpaper they have at once, and to every one they choose after.
    #[serde(default = "default_wallpaper_scope")]
    #[model(choices = WALLPAPER_SCOPES.map(|(_, value)| value))]
    pub wallpaper_scope: String,
    /// Where the wallpaper shows: `background`, `lockscreen` or `both`. The
    /// compositor applies a change to the wallpaper the person has at once.
    #[serde(default = "default_wallpaper_target")]
    #[model(choices = WALLPAPER_TARGETS.map(|(_, value)| value))]
    pub wallpaper_target: String,
    /// The desktop's wallpaper: the id of a picture the system ships
    /// ([`tilo_wallpapers::all`]), or [`tilo_wallpapers::CUSTOM`] for the
    /// person's own picture, which the compositor keeps per device
    /// (`settings_store::appearance::WALLPAPER`). Unset shows the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(choices = tilo_wallpapers::values())]
    pub wallpaper: Option<String>,
    /// Force theme tokens onto content surfaces, with high contrast.
    #[serde(default)]
    pub forced_colors: bool,
    /// "Always distinguish similar letters": I, l, 1 and 0, O set apart in
    /// all text. `None` follows the theme's typeface (on with Hyperlegible);
    /// `distinct_letters_effective` is what the system shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_letters: Option<bool>,
    /// The interface language, a BCP 47 tag (`de-DE`), which Region &
    /// formats offers among the languages Settings ships; `None` follows the
    /// device's (`settings_store::appearance::LANGUAGE`). Every app and the
    /// desktop show their copy in it, and a message with no translation in
    /// English. Written even when `None` (as `null`), so following the
    /// device again replaces a language chosen before.
    #[serde(default)]
    pub language: Option<String>,
    /// The interface face, by any family name a theme may name (`Default`,
    /// `Hyperlegible`, `Noto Sans`); `None` follows the theme's own. The
    /// face is a role of the theme, so this only says the person would
    /// rather have theirs than the one the theme chose. Written even when
    /// `None` (as `null`), so following the theme again replaces a face
    /// chosen before.
    #[serde(default)]
    pub ui_face: Option<String>,
    /// The accent, by a name the presence palette gives a hue (`violet`,
    /// `teal`); `None` follows the theme's own. Written even when `None` (as
    /// `null`), like `ui_face`.
    #[serde(default)]
    pub accent: Option<String>,
    /// How much of the theme's translucency to keep: `1` as the theme sets
    /// it, `0` floating chrome opaque; `None` follows the theme. The
    /// accessibility option that makes panels opaque still wins. Written
    /// even when `None` (as `null`), like `ui_face`.
    #[serde(default)]
    #[model(range = 0.0..=1.0)]
    pub translucency: Option<f64>,
}

/// What the desktop shows while the person has chosen nothing: the built-in
/// dark theme, which every host resolves an appearance section without a
/// mode to (`ui::theme::resolve_theme_layers`). Settings showing another
/// default would show a choice nobody made, and paint its own window in it.
fn default_mode() -> String {
    "dark".to_string()
}

fn default_dark_theme() -> String {
    ui::theme::Preset::Dark.id().to_string()
}

fn default_light_theme() -> String {
    ui::theme::Preset::Light.id().to_string()
}

fn default_density() -> f64 {
    f64::from(ui::theme::ThemeDensity::REGULAR)
}

fn default_schedule() -> String {
    "sun".to_string()
}

fn default_sunrise() -> String {
    settings_store::day_schedule::format_hhmm(
        settings_store::day_schedule::DaySchedule::FIXED.sunrise,
    )
}

fn default_sunset() -> String {
    settings_store::day_schedule::format_hhmm(
        settings_store::day_schedule::DaySchedule::FIXED.sunset,
    )
}

fn default_wallpaper_scope() -> String {
    "workspace".to_string()
}

fn default_wallpaper_target() -> String {
    "background".to_string()
}

/// Serde default for [`AppearancePrefs::ui_scale`] and
/// [`AppearancePrefs::text_scale`] so a config written before the field
/// existed deserialises to the neutral `1.0` rather than failing the whole
/// section.
fn default_ui_scale() -> f64 {
    1.0
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            dark_theme: default_dark_theme(),
            light_theme: default_light_theme(),
            custom_themes: Vec::new(),
            density: default_density(),
            high_contrast: false,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            schedule: default_schedule(),
            sunrise: default_sunrise(),
            sunset: default_sunset(),
            wallpaper_scope: default_wallpaper_scope(),
            wallpaper_target: default_wallpaper_target(),
            wallpaper: None,
            forced_colors: false,
            distinct_letters: None,
            language: None,
            ui_face: None,
            accent: None,
            translucency: None,
        }
    }
}

impl AppearancePrefs {
    /// The settings document these preferences describe, in the shape the
    /// theme layers read.
    pub fn settings_value(&self) -> serde_json::Value {
        let mut appearance = serde_json::to_value(self).unwrap_or_default();
        if let Some(section) = appearance.as_object_mut() {
            section.insert(
                ui::theme::CUSTOM_THEMES_KEY.into(),
                serde_json::Value::Array(
                    self.custom_themes
                        .iter()
                        .map(|theme| theme.0.clone())
                        .collect(),
                ),
            );
        }
        serde_json::json!({ "appearance": appearance })
    }

    /// The day/night schedule these preferences describe, at the system time
    /// zone's location.
    pub fn schedule(&self) -> settings_store::day_schedule::Scheduled {
        let mut section = serde_json::Map::new();
        section.insert("schedule".into(), self.schedule.clone().into());
        section.insert("sunrise".into(), self.sunrise.clone().into());
        section.insert("sunset".into(), self.sunset.clone().into());
        settings_store::day_schedule::Scheduled {
            config: settings_store::day_schedule::ScheduleConfig::from_section(&section),
            location: settings_store::day_schedule::timezone_location(),
        }
    }

    /// The theme document the current mode shows, with the density over it.
    pub fn user_document(&self) -> Option<ui::theme::ThemeDocument> {
        ui::theme::user_document(&self.settings_value(), ui::theme::local_moment())
    }

    fn layers(
        &self,
        device: &AppearanceDevicePrefs,
        accessibility: ui::theme::AccessibilityLayer,
    ) -> ui::theme::ThemeLayerStack {
        ui::theme::ThemeLayerStack {
            distribution_defaults: device.distribution_defaults.clone(),
            user_theme: self.user_document(),
            finish: self.finish(),
            window: ui::WindowAppearance::default(),
            accessibility,
        }
    }

    /// The person's own face, accent and share of the theme's translucency.
    fn finish(&self) -> ui::theme::FinishLayer {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a translucency is a factor in 0..=1; f32 holds it exactly enough"
        )]
        let translucency = self.translucency.map(|value| value as f32);
        ui::theme::FinishLayer {
            ui_face: self.ui_face.clone(),
            accent: self
                .accent
                .as_deref()
                .and_then(ui::tokens::PresenceHue::from_id),
            translucency,
        }
    }

    /// Resolve these preferences into the concrete [`ui::Theme`] the OS would
    /// apply on a device with `device`'s defaults: the theme the mode shows,
    /// with the accessibility comforts folded in. This is the real "apply" contract — a consumer that wants
    /// the user's look calls this rather than hard-coding [`ui::Theme::dark`].
    #[allow(
        clippy::cast_possible_truncation,
        reason = "scales are small factors well inside f32's range"
    )]
    pub fn resolve_theme(&self, device: &AppearanceDevicePrefs) -> ui::Theme {
        // The same function the compositor uses. UI scale grows the whole
        // system first; text scale then layers extra type growth on top.
        ui::theme::resolve_theme_layers(&self.layers(
            device,
            ui::theme::AccessibilityLayer {
                high_contrast: self.high_contrast,
                forced_colors: self.forced_colors,
                text_scale: self.text_scale as f32,
                ui_scale: self.ui_scale as f32,
                reduce_motion: self.reduce_motion,
                reduce_transparency: self.reduce_transparency,
                distinct_letters: self.distinct_letters,
                ..ui::theme::AccessibilityLayer::default()
            },
        ))
    }

    /// The chosen theme before the accessibility comforts, on a device with
    /// `device`'s defaults.
    pub fn base_theme(&self, device: &AppearanceDevicePrefs) -> ui::Theme {
        ui::theme::resolve_theme_layers(
            &self.layers(device, ui::theme::AccessibilityLayer::default()),
        )
    }
}

/// This device's appearance: what its OS image ships.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
pub struct AppearanceDevicePrefs {
    /// Palette and pins shipped with the OS image.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[model(readonly)]
    pub distribution_defaults: Option<ui::theme::DistributionDefaults>,
}

impl ui::storage::SettingsSchema for AppearanceDevicePrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::appearance::DEVICE_SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.appearance-device",
    label = "Appearance on this device"
)]
impl AppearanceDevicePrefs {}

impl ui::storage::SettingsSchema for AppearancePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("appearance");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.appearance",
    label = "Appearance",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl AppearancePrefs {
    /// Set the theme every app draws with; Automatic is light by day and
    /// dark by night.
    #[action(id = "choose-theme")]
    pub fn pick_theme(
        &mut self,
        #[param(
            "The theme: Automatic, Dark, Light, Dark graphite, Dark cool or Dark soft"
        )]
        theme: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !Self::THEMES.contains(&theme.as_str()) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{theme}` is not a theme"
            )));
        }
        self.choose_theme(&theme);
        Ok(())
    }
}

impl AppearancePrefs {
    pub const THEMES: &'static [&'static str] = &[
        "Automatic",
        "Dark",
        "Light",
        "Dark graphite",
        "Dark cool",
        "Dark soft",
    ];
    /// Show `name` from [`Self::THEMES`]: Automatic, or a preset that becomes
    /// the theme of its own mode.
    pub fn choose_theme(&mut self, name: &str) {
        if settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()) {
            self.mode = "automatic".into();
        } else if let Some(preset) = ui::theme::Preset::find(name) {
            let doc = preset.document();
            self.mode = if doc.palette.dark { "dark" } else { "light" }.into();
            if doc.palette.dark {
                self.dark_theme = preset.id().into();
            } else {
                self.light_theme = preset.id().into();
            }
        }
    }

    /// The [`Self::THEMES`] entry the stored mode and themes amount to; a
    /// custom theme reads as its id.
    pub fn theme_choice(&self) -> String {
        let id = match self.mode.as_str() {
            mode if settings_store::day_schedule::is_automatic(mode) => {
                return "Automatic".into();
            }
            "light" => &self.light_theme,
            _ => &self.dark_theme,
        };
        ui::theme::Preset::find(id).map_or_else(|| id.clone(), |preset| preset.name().into())
    }
}

/// Notification-delivery policy the compositor's notification layer reads
/// when it decides whether to raise a banner, reveal its contents, sound it,
/// or mirror it to the lock screen. Every field carries `#[serde(default)]`
/// so a config written before this section existed still loads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = NotificationPrefs::valid, check_reads = "per_app", defaults)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from the settings; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct NotificationPrefs {
    /// Master "Do Not Disturb": suppress banners and sounds while set.
    #[serde(default)]
    pub do_not_disturb: bool,
    /// Show pop-up banners for incoming notifications.
    #[serde(default = "default_true")]
    pub show_banners: bool,
    /// Reveal the message body in the banner (off shows only the app name,
    /// for shoulder-surfing privacy).
    #[serde(default = "default_true")]
    pub show_previews: bool,
    /// Play a sound when a notification arrives (gated by Do Not Disturb).
    #[serde(default = "default_true")]
    pub play_sounds: bool,
    /// Mirror notifications onto the lock screen. Off by default — the lock
    /// screen is the most exposed surface, so turning it on weakens it.
    #[serde(default)]
    #[model(weaker = true)]
    pub show_on_lock_screen: bool,
    /// Whether applications may show an unread-count badge.
    #[serde(default = "default_true")]
    pub show_badges: bool,
    /// Scheduled quiet hours. Per-app priority and temporary mute rules live
    /// beside this record in the shared notification policy.
    #[serde(default)]
    pub focus: settings_store::notification_policy::FocusSchedule,
    /// Rules keyed by the launcher's verified package id. The compositor
    /// deliberately never indexes this map with an app-supplied display name.
    #[serde(default)]
    pub per_app: std::collections::BTreeMap<
        String,
        settings_store::notification_policy::AppNotificationRule,
    >,
}

fn default_true() -> bool {
    true
}

/// The person's notification policy, read by the compositor's notification
/// layer.
impl ui::storage::SettingsSchema for NotificationPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("notifications");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.notifications", label = "Notifications")]
impl NotificationPrefs {
    /// Mute one app's notifications for a while, counted from when the
    /// action was made, or unmute it.
    #[action(label = "Mute an app")]
    pub fn mute_app(
        &mut self,
        cx: &mut ui::tilo_resource::ActionCx,
        #[param("The app's id")] app: String,
        #[param("For how long: Not muted, For 1 hour, For 8 hours or For 24 hours")]
        duration: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let seconds = mute_seconds(&duration).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{duration}` is not offered"))
        })?;
        let until = (seconds > 0).then(|| (cx.now_ms() / 1000).saturating_add(seconds));
        self.per_app.entry(app).or_default().muted_until = until;
        Ok(())
    }
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            do_not_disturb: false,
            show_banners: true,
            show_previews: true,
            play_sounds: true,
            show_on_lock_screen: false,
            show_badges: true,
            focus: settings_store::notification_policy::FocusSchedule::default(),
            per_app: std::collections::BTreeMap::new(),
        }
    }
}

/// `(label, persisted value)` of which workspaces the wallpaper covers: the
/// session applies a change to the wallpaper the person has, at once.
pub const WALLPAPER_SCOPES: [(&str, &str); 2] =
    [("This workspace", "workspace"), ("Every workspace", "all")];
/// `(label, persisted value)` of where the wallpaper shows: the session
/// applies a change to the wallpaper the person has, at once.
pub const WALLPAPER_TARGETS: [(&str, &str); 3] = [
    ("Desktop", "background"),
    ("Lock screen", "lockscreen"),
    ("Desktop and lock screen", "both"),
];

/// How long an app's notifications can be muted: `(English name, seconds)`.
pub const MUTE_OPTIONS: &[(&str, i64)] = &[
    ("Not muted", 0),
    ("For 1 hour", 60 * 60),
    ("For 8 hours", 8 * 60 * 60),
    ("For 24 hours", 24 * 60 * 60),
];

/// The seconds of the mute duration named `english`.
pub fn mute_seconds(english: &str) -> Option<i64> {
    MUTE_OPTIONS
        .iter()
        .find_map(|(name, seconds)| (*name == english).then_some(*seconds))
}

/// Whether `id` can name an app in the notification policy.
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

impl ui::storage::SettingsSchema for AudioPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("audio");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.audio",
    label = "Sound",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl AudioPrefs {
    /// Play both channels in each ear, or stereo again. The compositor
    /// session keeps the mono transform; the field is changed only here.
    #[action(id = "set-mono-output")]
    pub fn set_mono_output(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.mono_output = on;
        Ok(())
    }

    /// Give calls an echo-cancelled microphone, or the microphone as it
    /// is. The compositor session provides it; the field is changed only
    /// here.
    #[action(id = "set-echo-cancellation")]
    pub fn set_echo_cancellation(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.echo_cancellation = on;
        Ok(())
    }
}

impl ui::storage::SettingsSchema for AudioDevicePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("audio_device");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.audio-device", label = "Sound on this device")]
impl AudioDevicePrefs {
    /// Remember the output device this device plays through, chosen again
    /// when Settings next finds it. Choosing an output in Sound runs this.
    #[action(id = "remember-output-device")]
    pub fn remember_output(
        &mut self,
        #[param("The output device's name")] device: String,
    ) -> Result<(), tilo_resource::Refusal> {
        self.output_device = Some(remembered(device)?);
        Ok(())
    }

    /// Remember the input device this device records from, chosen again
    /// when Settings next finds it. Choosing an input in Sound runs this.
    #[action(id = "remember-input-device")]
    pub fn remember_input(
        &mut self,
        #[param("The input device's name")] device: String,
    ) -> Result<(), tilo_resource::Refusal> {
        self.input_device = Some(remembered(device)?);
        Ok(())
    }
}

/// A device's name as it is remembered: never empty.
fn remembered(device: String) -> Result<String, tilo_resource::Refusal> {
    if device.trim().is_empty() {
        return Err(tilo_resource::Refusal::new("a device name is needed"));
    }
    Ok(device)
}

impl ui::storage::SettingsSchema for DisplayPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("display");
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.display",
    label = "Displays",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl DisplayPrefs {
    /// Let the built-in panel's brightness follow the ambient light, or
    /// not. Settings offers it where a light sensor can drive the panel; the
    /// field is changed only here.
    #[action(id = "set-automatic-brightness")]
    pub fn set_automatic_brightness(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.auto_brightness = on;
        Ok(())
    }
}

impl ui::storage::SettingsSchema for NightLightPrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::hardware::DISPLAY_PERSON_SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.night-light", label = "Night light")]
impl NightLightPrefs {}

/// Refused unless `time` is a time of day, `HH:MM`.
fn time_of_day(time: &str, what: &str) -> Result<(), ui::tilo_resource::Refusal> {
    settings_store::day_schedule::parse_hhmm(time)
        .map(drop)
        .ok_or_else(|| ui::tilo_resource::Refusal::new(format!("{what} is a time of day, HH:MM")))
}

impl AppearancePrefs {
    /// What the appearance section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        time_of_day(&self.sunrise, "the start of day")?;
        time_of_day(&self.sunset, "the start of night")?;
        ui::theme::FinishLayer {
            ui_face: self.ui_face.clone(),
            accent: None,
            translucency: None,
        }
        .validate()
        .map_err(|error| ui::tilo_resource::Refusal::new(format!("the interface face: {error}")))?;
        if let Some(accent) = &self.accent
            && ui::tokens::PresenceHue::from_id(accent).is_none()
        {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{accent}` is not an accent colour"
            )));
        }
        if let Some(translucency) = self.translucency {
            let band = f64::from(settings_store::appearance::TRANSLUCENCY_MIN)
                ..=f64::from(settings_store::appearance::TRANSLUCENCY_MAX);
            if !band.contains(&translucency) {
                return Err(ui::tilo_resource::Refusal::new(
                    "translucency is between none and the theme's own",
                ));
            }
        }
        Ok(())
    }
}

impl NotificationPrefs {
    /// What the notification section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        match self.per_app.keys().find(|app| !valid_app_id(app)) {
            Some(app) => Err(ui::tilo_resource::Refusal::new(format!(
                "`{app}` is not an app id"
            ))),
            None => Ok(()),
        }
    }
}

impl PowerPrefs {
    /// What the power section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        if self.critical_battery_percent > self.low_battery_percent {
            return Err(ui::tilo_resource::Refusal::new(
                "the critical battery level is at most the low battery level",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod paste_shortcut_tests {
    use super::InputPrefs;

    #[test]
    fn an_app_can_be_told_how_it_pastes_and_given_back_its_default() {
        let mut input = InputPrefs::default();
        input
            .set_paste_shortcut(" org.example.MyTerm ", "ctrl_shift_v")
            .unwrap();
        assert_eq!(
            input
                .paste_shortcuts
                .get("org.example.MyTerm")
                .map(String::as_str),
            Some("ctrl_shift_v")
        );
        // App ids compare case-insensitively: one entry per app.
        input
            .set_paste_shortcut("org.example.myterm", "ctrl_v")
            .unwrap();
        assert_eq!(input.paste_shortcuts.len(), 1);
        assert!(
            input
                .set_paste_shortcut("org.example.MyTerm", "ctrl+v")
                .is_err()
        );
        assert!(input.set_paste_shortcut("  ", "ctrl_v").is_err());
        input
            .set_paste_shortcut("org.example.myterm", "default")
            .unwrap();
        assert!(input.paste_shortcuts.is_empty());

        input
            .set_paste_shortcut("org.example.MyTerm", "ctrl_shift_v")
            .unwrap();
        let stored = serde_json::to_value(&input).unwrap();
        let section = stored.as_object().unwrap();
        let overrides = settings_store::paste_shortcut::overrides_from_section(section);
        assert_eq!(
            settings_store::paste_shortcut::resolve("org.example.MyTerm", &overrides),
            settings_store::paste_shortcut::PasteShortcut::CtrlShiftV
        );
        let back: InputPrefs = serde_json::from_value(stored).unwrap();
        assert_eq!(back.paste_shortcuts, input.paste_shortcuts);
    }
}

#[cfg(test)]
mod power_storage_tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]

    use super::PowerPrefs;
    use ui::tilo_resource::{Replica, Slot, evolution};

    /// Version 1 stored every profile's suspend timer as a number. Those
    /// timers read as set, with the minutes they had.
    #[test]
    fn power_settings_of_version_one_keep_every_timer() {
        let root = ui::tilo_resource::__private::ROOT;
        let mut replica = Replica::<PowerPrefs>::new();
        let doc = replica.doc_mut();
        evolution::fixture(
            doc,
            &root,
            &serde_json::json!({
                "auto_suspend": true,
                "suspend_minutes": 15,
                "saver_suspend_minutes": 5,
                "balanced_suspend_minutes": 15,
                "performance_suspend_minutes": 30,
                "blank_minutes": 10,
                "lock_minutes": 5,
            }),
        )
        .unwrap();
        Slot::Key(&root, evolution::VERSION_KEY)
            .put_scalar(doc, ui::tilo_resource::__private::ScalarValue::Uint(1))
            .unwrap();
        doc.commit();
        let mut old = doc.fork();
        assert!(evolution::migrate::<PowerPrefs>(&mut old).unwrap());
        let power = evolution::read_checked::<PowerPrefs>(&old).unwrap();
        assert_eq!(power.suspend_minutes, 15);
        assert_eq!(
            [
                power.saver_suspend_minutes,
                power.balanced_suspend_minutes,
                power.performance_suspend_minutes,
            ],
            [Some(5), Some(15), Some(30)]
        );
        assert_eq!(power.battery_suspend_minutes, None);
        assert_eq!((power.blank_minutes, power.lock_minutes), (10, 5));
    }
}
