//! The shared **system-preferences store** — durable per-user device and
//! session preferences the Settings panes own.
//!
//! Preferences the rest of the system reads (system sounds, night light,
//! pointer behaviour, update policy), one typed section each.
//!
//! # The document (a format contract, not a crate dependency)
//!
//! Preferences live in the person's settings beside the region's, namespaced
//! by domain so each section grows independently and unrelated writers never
//! collide:
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
    #[model(range = 2_000..=6_500)]
    pub night_light_temperature: u32,
    /// Follow the Appearance day/night schedule instead of the manual switch:
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
    /// Whether the built-in panel stays put when a convertible is turned.
    #[serde(default)]
    #[model(readonly)]
    pub rotation_lock: bool,
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
            rotation_lock: false,
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
    /// moves the same distance at any speed (`flat`); capability-guarded by
    /// libinput.
    #[model(choices = ["adaptive", "flat"])]
    pub accel_profile: String,
    /// Three- and four-finger gestures switch workspaces and show the
    /// overview.
    pub touchpad_gestures: bool,
    /// Which button a clickpad press means; capability-guarded by libinput.
    pub touchpad_click_method: settings_store::hardware::TouchpadClickMethod,
    /// Two-finger or edge scrolling; capability-guarded by libinput.
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
impl settings_model::SettingsSchema for InputPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::hardware::INPUT_SECTION);
    settings_model::settings_codec!();
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
    /// capability-guarded by libinput.
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
    /// The shape of the drawing most recently pressed; each key is its body and its legend, in
    /// that pair, so a key's place in the drawing is its shape halved.
    #[model(readonly)]
    pub keyboard_inspected_shape: usize,
    /// Per-tablet mapping, pressure and buttons, keyed by device identity.
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
            keyboard_inspected_shape: 0,
            tablets: std::collections::BTreeMap::new(),
        }
    }
}

impl settings_model::SettingsSchema for InputDevicePrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::hardware::INPUT_DEVICE_SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.input-device")]
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

    /// Give one of this device's keyboards these layouts, in switching order, typing
    /// with `active`. The options it holds for a layout it keeps stay as they were,
    /// so moving or adding a layout never changes what its Caps Lock or compose key
    /// does; a keyboard that had none of its own starts from the shared layouts'
    /// options.
    #[action(id = "set-keyboard-sources")]
    pub fn set_keyboard_sources(
        &mut self,
        #[param("The keyboard's device key")] keyboard: String,
        #[param("Its layouts in switching order")] sources: Vec<String>,
        #[param("The layout it types with")] active: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        use settings_store::keyboard_sources::{ALL, KeyboardProfile, OSK};

        device_key(&keyboard)?;
        if matches!(keyboard.as_str(), ALL | OSK) {
            return Err(ui::tilo_resource::Refusal::new(
                "the shared layouts and the on-screen keyboard's are not a keyboard's own",
            ));
        }
        valid_layouts(&sources, &active)?;
        let inherited = self
            .keyboard_assignments
            .get(ALL)
            .map_or_else(std::collections::BTreeMap::new, |shared| {
                shared.options.clone()
            });
        let mut profile = self
            .keyboard_assignments
            .get(&keyboard)
            .cloned()
            .unwrap_or_else(|| KeyboardProfile {
                sources: sources.clone(),
                active: active.clone(),
                options: inherited,
            });
        profile.sources.clone_from(&sources);
        profile.active = active;
        let kept = sources.clone();
        profile.options.retain(|layout, _| kept.contains(layout));
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
        self.keyboard_inspected_shape = 0;
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

    /// Choose the modifier level and the shape of the keyboard drawing the
    /// person pressed, without typing anything.
    #[action(id = "inspect-keyboard", housekeeping)]
    pub fn inspect_keyboard(
        &mut self,
        #[param("The pressed shape of the drawing: a key's body or its legend")] shape: u32,
        #[param("The modifier level, counting from 0")] level: u32,
        #[param("Whether the levels past the fourth are offered")] extra_levels: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let level = usize::try_from(level)
            .ok()
            .filter(|level| *level < MAX_KEYBOARD_LEVELS)
            .ok_or_else(|| ui::tilo_resource::Refusal::new("no keyboard has this level"))?;
        let shape = usize::try_from(shape)
            .map_err(|_| ui::tilo_resource::Refusal::new("the drawing has no such shape"))?;
        self.keyboard_preview_level = level;
        self.keyboard_preview_extra_levels = extra_levels;
        self.keyboard_inspected_shape = shape;
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

    /// `shortcut:<name>`, the choice running a desktop shortcut. The name is the shortcut's id
    /// in the desktop's own table, the one the Shortcuts pane lists and `rebind` takes.
    #[must_use]
    pub fn shortcut_id(action: ShortcutAction) -> String {
        format!("shortcut:{}", action.id())
    }

    /// The shortcut a `shortcut:<name>` choice runs.
    #[must_use]
    pub fn shortcut(id: &str) -> Option<ShortcutAction> {
        ShortcutAction::from_id(id.strip_prefix("shortcut:")?)
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

    /// Type with the keyboard source `value`, adding it to the sources when it is not one of
    /// them: the layout typed with as one value, as a device whose session is not running
    /// chooses it.
    #[action(id = "set-keyboard-layout")]
    pub fn set_keyboard_layout(
        &mut self,
        #[param("An XKB layout or layout:variant (de, us:intl), or an input method (ime/hangul)")]
        value: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.choose_layout(value)
    }

    /// Type with the keyboard sources `value`, in switching order: the whole source list as one
    /// value, as a device whose session is not running chooses it. The source typed with stays
    /// the one it was while it is one of them, or else the first is.
    #[action(id = "set-keyboard-layouts")]
    pub fn set_keyboard_layouts(
        &mut self,
        #[param(
            "XKB layouts or layout:variants (de, us:intl) and input methods, in switching order"
        )]
        value: Vec<String>,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let active = if value.contains(&self.keyboard_layout) {
            self.keyboard_layout.clone()
        } else {
            value.first().cloned().unwrap_or_default()
        };
        valid_layouts(&value, &active)?;
        self.keyboard_layouts = value;
        self.keyboard_layout = active;
        self.normalise_keyboard_sources();
        Ok(())
    }

    /// What the Caps Lock key does: `caps` for Caps Lock itself, or the key it stands in for,
    /// `ctrl`, `escape` or `backspace`.
    #[action(id = "set-caps-lock")]
    pub fn set_caps_lock(
        &mut self,
        #[param("`caps`, `ctrl`, `escape` or `backspace`")] behaviour: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.caps_lock = match behaviour.as_str() {
            "caps" | "caps_lock" | "none" | "disabled" | "" => None,
            "ctrl" => Some("ctrl".into()),
            "escape" => Some("escape".into()),
            "backspace" => Some("backspace".into()),
            other => {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{other}` is not what Caps Lock can do"
                )));
            }
        };
        Ok(())
    }

    /// The key that begins a compose sequence: `disabled`, `ralt`, `menu`, `caps`, `rwin` or
    /// `lwin`. On Caps Lock it is the more specific choice, and Caps Lock then does that.
    #[action(id = "set-compose-key")]
    pub fn set_compose_key(
        &mut self,
        #[param("`disabled`, `ralt`, `menu`, `caps`, `rwin` or `lwin`")] key: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.compose_key = match key.as_str() {
            "disabled" | "none" | "" => None,
            "ralt" => Some("ralt".into()),
            "menu" => Some("menu".into()),
            "caps" => Some("caps".into()),
            "rwin" => Some("rwin".into()),
            "lwin" => Some("lwin".into()),
            other => {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{other}` is not a compose key"
                )));
            }
        };
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

/// The person's widgets: which they keep, when each shows itself and
/// where each rests. The compositor reads the same leaves
/// ([`settings_store::widgets`]), and the shell writes back a pin,
/// where a widget was dropped, its card's width and its removal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct WidgetsPrefs {
    /// Now playing, with play/pause and next.
    #[serde(default = "default_music_widget")]
    pub music: settings_store::widgets::WidgetPrefs,
    /// The people in this workspace.
    #[serde(default = "default_team_widget")]
    pub team: settings_store::widgets::WidgetPrefs,
    /// Today's date, and the month around it.
    #[serde(default = "default_calendar_widget")]
    pub calendar: settings_store::widgets::WidgetPrefs,
    /// Processor, memory and temperature.
    #[serde(default = "default_system_monitor_widget")]
    pub system_monitor: settings_store::widgets::WidgetPrefs,
}

fn default_music_widget() -> settings_store::widgets::WidgetPrefs {
    settings_store::widgets::WidgetPrefs::default_for(settings_store::widgets::WidgetId::Music)
}

fn default_team_widget() -> settings_store::widgets::WidgetPrefs {
    settings_store::widgets::WidgetPrefs::default_for(settings_store::widgets::WidgetId::Team)
}

fn default_calendar_widget() -> settings_store::widgets::WidgetPrefs {
    settings_store::widgets::WidgetPrefs::default_for(settings_store::widgets::WidgetId::Calendar)
}

fn default_system_monitor_widget() -> settings_store::widgets::WidgetPrefs {
    settings_store::widgets::WidgetPrefs::default_for(
        settings_store::widgets::WidgetId::SystemMonitor,
    )
}

impl Default for WidgetsPrefs {
    fn default() -> Self {
        Self {
            music: default_music_widget(),
            team: default_team_widget(),
            calendar: default_calendar_widget(),
            system_monitor: default_system_monitor_widget(),
        }
    }
}

impl settings_model::SettingsSchema for WidgetsPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::widgets::SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.widgets")]
impl WidgetsPrefs {
    /// The widget `widget` names, by its key as the shell and Settings know it.
    fn named(
        &mut self,
        widget: &str,
    ) -> Result<&mut settings_store::widgets::WidgetPrefs, ui::tilo_resource::Refusal> {
        match widget {
            "music" => Ok(&mut self.music),
            "team" => Ok(&mut self.team),
            "calendar" => Ok(&mut self.calendar),
            "system_monitor" => Ok(&mut self.system_monitor),
            other => Err(ui::tilo_resource::Refusal::new(format!(
                "`{other}` is not a widget: music, team, calendar or system_monitor"
            ))),
        }
    }

    /// Keep `widget` on this person's shell, or take it off: the same switch Settings shows, and
    /// what a shell runs when the person keeps one or removes it.
    #[action(id = "set-widget-enabled")]
    pub fn set_widget_enabled(
        &mut self,
        #[param("The widget: music, team, calendar or system_monitor")] widget: String,
        #[param("Whether the person keeps it on their shell")] enabled: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.named(&widget)?.enabled = enabled;
        Ok(())
    }

    /// When `widget` shows itself in its quiet form: after the person pauses (`pause`), always
    /// (`always`), or only when they ask (`ask`).
    #[action(id = "set-widget-show")]
    pub fn set_widget_show(
        &mut self,
        #[param("The widget: music, team, calendar or system_monitor")] widget: String,
        #[param("When it shows: pause, always or ask")] show: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        use settings_store::widgets::ShowMode;
        let show = match show.as_str() {
            "pause" => ShowMode::Pause,
            "always" => ShowMode::Always,
            "ask" => ShowMode::Ask,
            other => {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{other}` is not a show mode: pause, always or ask"
                )));
            }
        };
        self.named(&widget)?.show = show;
        Ok(())
    }

    /// Where `widget` rests on `display`: fractions of the room it moves in there, 0 against the
    /// leading or top margin and 1 against the trailing or bottom one, so it keeps to its edge
    /// when the display's resolution changes. The display it was last dropped on comes first.
    #[action(id = "place-widget")]
    pub fn place_widget(
        &mut self,
        #[param("The widget: music, team, calendar or system_monitor")] widget: String,
        #[param("The display, by its stable identity")] display: String,
        #[param("How far across the room it can move: 0 leading, 1 trailing")] x: f64,
        #[param("How far down the room it can move: 0 top, 1 bottom")] y: f64,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !(x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y))
        {
            return Err(ui::tilo_resource::Refusal::new(
                "a place is a fraction of its room, 0 to 1",
            ));
        }
        let named = self.named(&widget)?;
        named.places.retain(|place| place.display != display);
        named
            .places
            .insert(0, settings_store::widgets::Place { display, x, y });
        Ok(())
    }

    /// How wide `widget`'s open card is, in logical pixels: what the person left it at after
    /// resizing it. Each widget keeps the width it asks for by its own bounds.
    #[action(id = "resize-widget")]
    pub fn resize_widget(
        &mut self,
        #[param("The widget: music, team, calendar or system_monitor")] widget: String,
        #[param("The card's width in logical pixels, 160 to 640")] width: f64,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !(width.is_finite() && (160.0..=640.0).contains(&width)) {
            return Err(ui::tilo_resource::Refusal::new(
                "a card is 160 to 640 logical pixels wide",
            ));
        }
        self.named(&widget)?.width = Some(width);
        Ok(())
    }
}

/// This device's idle and button preferences, read by the compositor's
/// idle layer.
impl settings_model::SettingsSchema for PowerPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("power");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.power",
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
impl settings_model::SettingsSchema for UpdatePrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("updates");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.updates",
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
/// concrete [`ui::Theme`] every app draws against. `dark_theme` and `light_theme`
/// name the two sides of a look ([`ui::theme::Look`]); `high_contrast` overrides the palette
/// with the WCAG-AAA high-contrast track; `reduce_motion` and `text_scale`
/// fold in the accessibility comforts. The compositor reads this section the
/// same way it reads the display/input comfort sections.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent appearance switch, not one combined state"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(
    check = AppearancePrefs::valid,
    check_reads = "sunrise sunset ui_face accent translucency themes",
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
    /// The theme package the person applied ([`Self::apply_theme`]), by its id: the bundle's
    /// document and painters draw every app on a device that has the package installed, and a
    /// device without it shows Tilo's theme and says the package is missing. Unset shows the
    /// theme the mode and schedule show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(readonly)]
    pub theme_package: Option<String>,
    /// The person's own themes, each one a `custom:<id>` names: every role a
    /// field, changed by the same edits from Settings, the shell and an agent.
    #[serde(
        default,
        skip_serializing_if = "ui::tilo_resource::Collection::is_empty",
        with = "model_json"
    )]
    pub themes: ui::tilo_resource::Collection<ui::theme::ThemeRecord>,
    /// Spacing density over whichever theme is shown: one of
    /// [`ui::theme::ThemeDensity`]'s `COMPACT`, `REGULAR` and `LARGE`.
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
            theme_package: None,
            themes: ui::tilo_resource::Collection::default(),
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

/// The theme the installed package `id` draws on this device: its bundle loaded through
/// [`ui::theme_bundle::load_from`], which registers each painter set it names on the machine code
/// the device's daemon compiled when the package arrived, and its document. `None` when the
/// package is not installed here, or its bundle is refused: the device then shows the theme the
/// appearance's mode names, and Settings says the package is missing.
#[must_use]
pub fn applied_theme(id: &str) -> Option<ui::theme::ThemeDocument> {
    let installed = package_store::find_installed(id).ok().flatten()?;
    let compiled = runtime_portal::process_link::compiled_modules();
    ui::theme_bundle::load_from(&installed.payload_root, compiled.as_deref()).ok()?;
    ui::theme_bundle::document(&installed.payload_root).ok()
}

impl AppearancePrefs {
    /// The settings document these preferences describe, in the shape the
    /// theme layers read.
    pub fn settings_value(&self) -> serde_json::Value {
        let appearance = serde_json::to_value(self).unwrap_or_default();
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

    /// The theme document every app draws: the applied package's on a device that has it, else
    /// the theme the current mode shows, with the density over it.
    pub fn user_document(&self) -> Option<ui::theme::ThemeDocument> {
        self.theme_package
            .as_deref()
            .and_then(applied_theme)
            .or_else(|| ui::theme::user_document(&self.settings_value(), ui::theme::local_moment()))
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

impl settings_model::SettingsSchema for AppearanceDevicePrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::appearance::DEVICE_SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.appearance-device")]
impl AppearanceDevicePrefs {}

impl settings_model::SettingsSchema for AppearancePrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("appearance");
    settings_model::settings_codec!();

    /// A page follows the light or dark its browser and system show until the
    /// person picks a theme, so what it shows them chosen there is Automatic;
    /// the person's own computer starts dark.
    fn defaults_on(device: settings_model::DeviceClass) -> Self {
        let mut prefs = Self::default();
        if device == settings_model::DeviceClass::Web {
            settings_store::day_schedule::AUTOMATIC_MODE.clone_into(&mut prefs.mode);
        }
        prefs
    }
}

#[ui::tilo_resource::resource(
    kind = "settings.appearance",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl AppearancePrefs {
    /// Dress every app in a look: Tilo, Paper, Moss, Dusk or Mono, each
    /// designed for light and for dark; the mode stays as it is.
    #[action(id = "choose-theme")]
    pub fn pick_theme(
        &mut self,
        #[param("The look: Tilo, Paper, Moss, Dusk or Mono")] theme: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let look = ui::theme::Look::find(&theme).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!(
                "`{theme}` is not a look: Tilo, Paper, Moss, Dusk or Mono"
            ))
        })?;
        self.choose_look(look);
        self.theme_package = None;
        Ok(())
    }

    /// Dress every app in a theme package installed on this device: the bundle's document and
    /// the painter sets it ships, the sets drawn on the machine code the device compiled when the
    /// package arrived. The appearance names the package, so it follows the person to their other
    /// devices; one without the package installed shows Tilo's theme and says it is missing.
    #[action(
        id = "apply-theme",
        keywords = "theme, look, appearance, style, package"
    )]
    pub fn apply_theme(
        &mut self,
        #[param("An installed theme package's id", example = "so.tilo.theme.bauhaus")]
        package: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let id = package.trim();
        if id.is_empty() {
            return Err(ui::tilo_resource::Refusal::new("a theme package is needed"));
        }
        let Some(installed) = package_store::find_installed(id).ok().flatten() else {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{id}` is not a package installed on this device"
            )));
        };
        let compiled = runtime_portal::process_link::compiled_modules();
        ui::theme_bundle::load_from(&installed.payload_root, compiled.as_deref()).map_err(
            |error| {
                ui::tilo_resource::Refusal::new(format!("`{id}` does not draw a theme: {error}"))
            },
        )?;
        self.theme_package = Some(installed.id);
        Ok(())
    }

    /// Draw every app dark or light, each with the theme chosen for that
    /// mode; a schedule stops until a theme is chosen again.
    #[action(id = "set-dark-mode")]
    pub fn set_dark_mode(&mut self, on: bool) -> Result<(), ui::tilo_resource::Refusal> {
        self.mode = if on { "dark" } else { "light" }.into();
        Ok(())
    }

    /// Show every app and the desktop in one of the languages this desktop speaks, named by the
    /// tag its settings keep (`de` of `de-DE`), or follow the device's language when given none.
    /// A language Tilo cannot show is refused, naming it, and the language stays as it was.
    #[action(id = "choose-language", keywords = "language, region, interface")]
    pub fn choose_language(
        &mut self,
        #[param("A language's tag, such as `de`, or none to follow the device")] language: Option<
            String,
        >,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let language = language.filter(|tag| !tag.is_empty());
        let Some(tag) = language.as_deref() else {
            self.language = None;
            return Ok(());
        };
        let named = ui::i18n::language_of(tag);
        if !ui::i18n::spoken_languages()
            .iter()
            .any(|(spoken, _)| *spoken == named)
        {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{tag}` is not a language this desktop speaks"
            )));
        }
        self.language = Some(named);
        Ok(())
    }

    /// Show a theme in the mode of its polarity: a preset id (`dusk-dark`) or
    /// one of your own themes by its id.
    #[action(id = "use-theme")]
    pub fn use_theme(
        &mut self,
        #[param("A preset id or the id of one of your themes", example = "paper-dark")]
        theme: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let (reference, dark) = if let Some(preset) = ui::theme::Preset::find(&theme) {
            (
                preset.id().to_owned(),
                ui::theme::PresetDesign::document(preset).is_dark(),
            )
        } else {
            let own = self.theme(&theme)?;
            (format!("custom:{theme}"), own.dark)
        };
        if dark {
            self.dark_theme = reference;
        } else {
            self.light_theme = reference;
        }
        self.theme_package = None;
        Ok(())
    }

    /// Start a theme of your own from a preset or from one of your themes.
    #[action(id = "new-theme")]
    pub fn new_theme(
        &mut self,
        #[param("Id for the new theme")] id: String,
        #[param("The preset or theme to start from", example = "paper-dark")] base: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let id = self.free_id(&id)?;
        let base = match ui::theme::ThemeRecord::preset(&base) {
            Some(document) => document,
            None => self.theme(&base)?.document(&base),
        };
        self.themes.push(id, ui::theme::ThemeRecord::draft(&base));
        Ok(())
    }

    /// Take in a theme document someone shared, under `id` (its own id when
    /// empty), once it is admitted and keeps every contrast floor.
    #[action(id = "import-theme")]
    pub fn import_theme(
        &mut self,
        #[param("Id for the imported theme; empty keeps the document's")] id: String,
        #[param("The theme document, as JSON")] document: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let mut document = ui::theme::admit_document(&document)?;
        let id = if id.trim().is_empty() {
            document.id.clone()
        } else {
            id
        };
        let id = self.free_id(&id)?;
        document.id.clear();
        document.id.push_str(id.as_str());
        self.themes
            .push(id, ui::theme::ThemeRecord::from_document(&document));
        Ok(())
    }

    /// Put every role of one of your themes back as it was in the document it
    /// was made from, keeping its name.
    #[action(id = "reset-theme")]
    pub fn reset_theme(&mut self, theme: String) -> Result<(), ui::tilo_resource::Refusal> {
        let current = self.theme_mut(&theme)?;
        let origin: ui::theme::ThemeDocument = current
            .origin
            .as_deref()
            .and_then(|origin| serde_json::from_str(origin).ok())
            .ok_or_else(|| {
                ui::tilo_resource::Refusal::new("the theme keeps nothing to go back to")
            })?;
        let name = current.name.clone();
        *current = ui::theme::ThemeRecord {
            name,
            ..ui::theme::ThemeRecord::from_document(&origin)
        };
        Ok(())
    }

    /// Delete one of your themes; a mode that showed it shows Tilo's again.
    #[action(id = "delete-theme", destructive)]
    pub fn delete_theme(&mut self, theme: String) -> Result<(), ui::tilo_resource::Refusal> {
        self.themes
            .remove(&ui::tilo_resource::Id::from_raw(theme.as_str()))
            .ok_or_else(|| ui::tilo_resource::Refusal::new("there is no such theme"))?;
        let reference = format!("custom:{theme}");
        if self.dark_theme == reference {
            self.dark_theme = default_dark_theme();
        }
        if self.light_theme == reference {
            self.light_theme = default_light_theme();
        }
        Ok(())
    }

    /// Turn one of your themes dark (light ink on its surfaces) or light,
    /// keeping its hue, chroma and name.
    #[action(id = "switch-polarity")]
    pub fn switch_polarity(
        &mut self,
        theme: String,
        #[param("Whether the theme is dark")] dark: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let current = self.theme_mut(&theme)?;
        *current = current.with_polarity(dark);
        // A mode that showed it shows its own choice again: the theme no
        // longer has that mode's polarity.
        let reference = format!("custom:{theme}");
        if dark && self.light_theme == reference {
            self.light_theme = default_light_theme();
        }
        if !dark && self.dark_theme == reference {
            self.dark_theme = default_dark_theme();
        }
        Ok(())
    }

    /// Record every contrast floor one of your themes would miss with `role` set to `value`, so
    /// the editor shows the pairs under their floor while the theme is tuned. The theme itself
    /// stays as it is: a role that would break a floor is refused as an edit, and this says which
    /// pairs it would break. Without a role, it lists what the theme misses as it stands.
    #[action(id = "review-theme", housekeeping)]
    pub fn review_theme(
        &mut self,
        theme: String,
        #[param("The role, by its path in the theme")] role: String,
        #[param("The value the role is being set to")] value: Option<f64>,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let current = self.theme_mut(&theme)?;
        let findings = match value {
            Some(value) if !role.is_empty() => {
                current.clone().with_role(&role, value).findings(&theme)
            }
            _ => current.findings(&theme),
        };
        if current.findings != findings {
            current.findings = findings;
        }
        Ok(())
    }
}

impl AppearancePrefs {
    /// One of the person's themes, by id.
    ///
    /// # Errors
    /// There is none by that id.
    pub fn theme(&self, id: &str) -> Result<&ui::theme::ThemeRecord, ui::tilo_resource::Refusal> {
        self.themes
            .get(&ui::tilo_resource::Id::from_raw(id))
            .ok_or_else(|| {
                ui::tilo_resource::Refusal::new(format!("no preset or theme is called `{id}`"))
            })
    }

    fn theme_mut(
        &mut self,
        id: &str,
    ) -> Result<&mut ui::theme::ThemeRecord, ui::tilo_resource::Refusal> {
        self.themes
            .get_mut(&ui::tilo_resource::Id::from_raw(id))
            .ok_or_else(|| ui::tilo_resource::Refusal::new("there is no such theme"))
    }

    /// `id`, trimmed, when no preset or theme has it yet.
    fn free_id(&self, id: &str) -> Result<ui::tilo_resource::Id, ui::tilo_resource::Refusal> {
        let id = id.trim();
        if id.is_empty() {
            return Err(ui::tilo_resource::Refusal::new("a theme needs an id"));
        }
        let id = ui::tilo_resource::Id::from_raw(id);
        if ui::theme::Preset::find(id.as_str()).is_some() || self.themes.contains(&id) {
            return Err(ui::tilo_resource::Refusal::new(
                "a theme with that id already exists",
            ));
        }
        Ok(id)
    }
}

/// A keyed collection in the section's JSON, in the form the resource itself reads and writes.
mod model_json {
    use serde::{Deserialize as _, Serialize as _};
    use ui::tilo_resource::Model;

    pub(super) fn serialize<T: Model, S: serde::Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.to_json().serialize(serializer)
    }

    pub(super) fn deserialize<'de, T: Model, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        T::from_json(&value).map_err(serde::de::Error::custom)
    }
}

impl AppearancePrefs {
    /// The modes a person picks from, as `mode` stores them.
    pub const MODES: [&'static str; 3] = [
        settings_store::day_schedule::AUTOMATIC_MODE,
        "light",
        "dark",
    ];

    /// Wear `look` in both modes.
    pub fn choose_look(&mut self, look: ui::theme::Look) {
        self.dark_theme = look.preset(true).id().into();
        self.light_theme = look.preset(false).id().into();
    }

    /// The id of the theme the current mode shows (Automatic shows its dark one
    /// here; the page and the shell resolve the hour themselves).
    fn shown_theme(&self) -> &str {
        if self.mode == "light" {
            &self.light_theme
        } else {
            &self.dark_theme
        }
    }

    /// The look the person wears, or `None` for a theme of their own.
    pub fn look(&self) -> Option<ui::theme::Look> {
        ui::theme::Preset::find(self.shown_theme()).and_then(ui::theme::Preset::look)
    }

    /// What the look picker shows chosen: a look's id, or a theme of the
    /// person's own by its stored id.
    pub fn look_choice(&self) -> String {
        self.look().map_or_else(
            || self.shown_theme().to_owned(),
            |look| look.id().to_owned(),
        )
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
    /// Play a sound when a notification arrives (guarded by Do Not Disturb).
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
impl settings_model::SettingsSchema for NotificationPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("notifications");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.notifications")]
impl NotificationPrefs {
    /// Mute one app's notifications for a while, counted from when the
    /// action was made, or unmute it.
    #[action(keywords = "mute an app, silence an app, quiet an app, app notifications")]
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
        self.rule_for(&app)?.muted_until = until;
        Ok(())
    }

    /// Which kinds of notification one app arrives as: banners, their message
    /// text, sounds and a count on its icon. A kind left unset follows the
    /// person's rules above; setting any makes the app's own rule, so the app
    /// is listed among the apps with one.
    #[action(
        id = "set-app-kinds",
        keywords = "app notifications, per app notifications, notifications from an app"
    )]
    pub fn set_app_kinds(
        &mut self,
        #[param("The app's id")] app: String,
        #[param("Whether its banners arrive; unset follows the rules above")] banners: Option<bool>,
        #[param("Whether a banner carries its message text")] previews: Option<bool>,
        #[param("Whether it plays a sound")] sounds: Option<bool>,
        #[param("Whether its icon counts what is unread")] badges: Option<bool>,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let rule = self.rule_for(&app)?;
        rule.banners = banners;
        rule.previews = previews;
        rule.sounds = sounds;
        rule.badges = badges;
        Ok(())
    }

    /// Whether one app's notifications show on the lock screen and reach this
    /// person's other devices, each following the person's rules above when
    /// unset.
    #[action(
        id = "set-app-reach",
        keywords = "lock screen notifications, notifications on other devices, mirror notifications"
    )]
    pub fn set_app_reach(
        &mut self,
        #[param("The app's id")] app: String,
        #[param("Whether it shows on the lock screen; unset follows the rules above")]
        lock_screen: Option<bool>,
        #[param("Whether it reaches the person's other devices")] cross_device: Option<bool>,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let rule = self.rule_for(&app)?;
        rule.lock_screen = lock_screen;
        rule.cross_device = cross_device;
        Ok(())
    }

    /// Whether one app may pierce the quiet hours, by itself or for a message
    /// from a priority contact. Neither can be asked by the app.
    #[action(
        id = "set-app-priority",
        keywords = "priority notifications, important notifications, bypass quiet hours"
    )]
    pub fn set_app_priority(
        &mut self,
        #[param("The app's id")] app: String,
        #[param("Whether it reaches the person during quiet hours")] priority: bool,
        #[param("Whether a priority contact's message does")] priority_contacts: bool,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let rule = self.rule_for(&app)?;
        rule.priority = priority;
        rule.priority_contacts = priority_contacts;
        Ok(())
    }

    /// Forget one app's own rules, mute included: it follows the person's rules
    /// above again.
    #[action(
        id = "clear-app",
        keywords = "app notification rules, follow the rules above, forget app rules"
    )]
    pub fn clear_app(
        &mut self,
        #[param("The app's id")] app: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        self.per_app.remove(&app);
        Ok(())
    }

    /// Set the quiet hours' clock times and the days they keep, as one
    /// change: a start and an end in minutes after midnight, and the days as
    /// a bitmask with Monday in bit zero, zero for every day.
    #[action()]
    pub fn set_quiet_hours(
        &mut self,
        #[param("Minutes after midnight the quiet starts")] start_minute: u32,
        #[param("Minutes after midnight the quiet ends")] end_minute: u32,
        #[param("The days it keeps, Monday in bit zero; zero for every day")] weekdays: u32,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let minute = |value: u32| {
            u16::try_from(value)
                .ok()
                .filter(|minute| *minute < 24 * 60)
                .ok_or_else(|| {
                    ui::tilo_resource::Refusal::new(format!(
                        "`{value}` is not a minute after midnight"
                    ))
                })
        };
        let days = u8::try_from(weekdays)
            .ok()
            .filter(|days| *days <= 0b111_1111)
            .ok_or_else(|| {
                ui::tilo_resource::Refusal::new("the quiet hours keep days of one week")
            })?;
        self.focus.start_minute = minute(start_minute)?;
        self.focus.end_minute = minute(end_minute)?;
        self.focus.weekdays = days;
        Ok(())
    }
}

impl NotificationPrefs {
    /// One app's rule, made when it has none. A new rule needs an id an app
    /// can be known by; one stored before the id grammar tightened stays
    /// changeable.
    fn rule_for(
        &mut self,
        app: &str,
    ) -> Result<
        &mut settings_store::notification_policy::AppNotificationRule,
        ui::tilo_resource::Refusal,
    > {
        if !self.per_app.contains_key(app) && !valid_app_id(app) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{app}` is not an app id"
            )));
        }
        Ok(self.per_app.entry(app.to_owned()).or_default())
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

/// Whether `id` can name an app when writing a new notification policy rule.
///
/// Differs from [`tilo_app_id::AppId::parse`] by allowing a single segment
/// without a dot (for host desktop-file ids such as `firefox`) and capping
/// length at 160 bytes for notification policy keys.
pub fn valid_app_id(id: &str) -> bool {
    const GRAMMAR: tilo_app_id::Grammar = tilo_app_id::Grammar::CANONICAL
        .max_bytes(160)
        .allow_single_segment();
    GRAMMAR.matches(id)
}

/// Whether `id` may remain as an already-stored key in
/// [`NotificationPrefs::per_app`] or be listed from stored settings.
///
/// Keeps the pre-unification rule (non-empty, at most 160 bytes, ASCII
/// alphanumerics plus `.`, `-`, `_`) so rules saved before the grammar
/// tightened — such as a host desktop file named `foo..desktop` yielding
/// `foo.` — stay visible in Settings, can still be removed, and never block
/// other actions on [`NotificationPrefs`].
pub fn valid_stored_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

impl settings_model::SettingsSchema for AudioPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("audio");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.audio",
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

    /// Choose the sound theme system sounds come from, by the name a person
    /// reads for it: the name a theme installed on this device gives itself,
    /// or its directory. Nothing follows the freedesktop.org theme, which
    /// every other theme inherits from.
    #[action(id = "set-system-sound-theme")]
    pub fn set_system_sound_theme(
        &mut self,
        #[param("The theme's name, or none for the freedesktop one")] theme: Option<String>,
    ) -> Result<(), tilo_resource::Refusal> {
        self.choose_sound_theme(theme.as_deref())
    }
}

impl AudioPrefs {
    /// [`Self::set_system_sound_theme`], as the section keeps the choice: a
    /// theme is one name in the sound directories, never a path.
    fn choose_sound_theme(&mut self, theme: Option<&str>) -> Result<(), tilo_resource::Refusal> {
        self.system_sounds.theme = match theme.map(str::trim) {
            None | Some("") => None,
            Some(theme) if theme == settings_store::system_sounds::DEFAULT_THEME => None,
            Some(theme) => {
                if !settings_store::system_sounds::is_valid_theme_name(theme) {
                    return Err(tilo_resource::Refusal::new(
                        "a sound theme is named by one path component",
                    ));
                }
                Some(theme.to_owned())
            }
        };
        Ok(())
    }
}

impl settings_model::SettingsSchema for AudioDevicePrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("audio_device");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.audio-device")]
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

impl settings_model::SettingsSchema for DisplayPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("display");
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(
    kind = "settings.display",
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

    /// Keep the built-in panel put when the device turns, or let it follow.
    #[action(id = "set-rotation-lock")]
    pub fn set_rotation_lock(&mut self, locked: bool) -> Result<(), tilo_resource::Refusal> {
        self.rotation_lock = locked;
        Ok(())
    }
}

impl settings_model::SettingsSchema for NightLightPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::hardware::DISPLAY_PERSON_SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.night-light")]
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
        for id in self.themes.ids() {
            if id.as_str().trim().is_empty() || ui::theme::Preset::find(id.as_str()).is_some() {
                return Err(ui::tilo_resource::Refusal::new(format!(
                    "`{}` cannot name a theme of your own",
                    id.as_str()
                )));
            }
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
        match self.per_app.keys().find(|app| !valid_stored_app_id(app)) {
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
        let back: InputPrefs = serde_json::from_value(stored).unwrap();
        assert_eq!(back.paste_shortcuts, input.paste_shortcuts);
    }
}

#[cfg(test)]
mod power_storage_tests {

    use super::PowerPrefs;
    use ui::tilo_resource::{Replica, Slot, evolution};

    /// Version 1 stored every profile's suspend timer as a number. Those
    /// timers read as set, with the minutes they had.
    #[test]
    fn power_settings_of_version_one_keep_every_timer() {
        let root = ui::tilo_resource::__private::ROOT;
        let mut replica = Replica::<PowerPrefs>::new().in_app("so.tilo.model", "Model");
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

#[cfg(test)]
#[path = "own_themes_tests.rs"]
mod own_themes_tests;

#[cfg(test)]
mod theme_tests {

    use super::AppearancePrefs;

    /// Every look the toolkit ships can be chosen through the published action, by the name a
    /// person reads; it dresses both modes and leaves the mode alone. A name no look has is
    /// refused.
    #[test]
    fn every_shipped_look_can_be_chosen() {
        for look in ui::theme::Look::ALL {
            let mut prefs = AppearancePrefs {
                mode: "light".into(),
                ..AppearancePrefs::default()
            };
            prefs.pick_theme(look.name().to_owned()).unwrap();
            assert_eq!(prefs.dark_theme, look.preset(true).id());
            assert_eq!(prefs.light_theme, look.preset(false).id());
            assert_eq!(prefs.mode, "light", "{}", look.name());
            assert_eq!(prefs.look(), Some(look));
            assert_eq!(prefs.look_choice(), look.id());
        }
        assert!(
            AppearancePrefs::default()
                .pick_theme("Crimson".to_owned())
                .is_err()
        );
        let custom = AppearancePrefs {
            dark_theme: "custom:ember".into(),
            ..AppearancePrefs::default()
        };
        assert_eq!(custom.look(), None);
        assert_eq!(custom.look_choice(), "custom:ember");
    }
}

#[cfg(test)]
mod notification_app_id_tests {
    use super::{NotificationPrefs, notification_prefs, valid_app_id, valid_stored_app_id};
    use ui::tilo_resource::{Fields, Invoke, Replica, Resource};

    #[test]
    fn notification_app_ids_allow_single_segments_within_160_bytes_and_refuse_empty_segments() {
        let longest = "a".repeat(160);
        let too_long = "a".repeat(161);
        let too_long_dotted = format!("a.{}", "b".repeat(159));
        for (id, expected) in [
            ("so.tilo.chat", true),
            ("org.example.My_App-2", true),
            ("firefox", true),
            ("kde4-konsole", true),
            ("-foo", true),
            ("-org.example.App", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            (too_long_dotted.as_str(), false),
            ("", false),
            (".foo", false),
            ("foo.", false),
            ("a..b", false),
            ("..", false),
            (".", false),
            ("not an id", false),
            ("org/example", false),
        ] {
            assert_eq!(valid_app_id(id), expected, "{id:?}");
            let mut replica = Replica::<NotificationPrefs>::new().in_app("so.tilo.model", "Model");
            let result = replica.apply(NotificationPrefs::MUTE_APP.with(
                notification_prefs::MuteApp {
                    app: id.to_owned(),
                    duration: "For 1 hour".to_owned(),
                },
            ));
            assert_eq!(result.is_ok(), expected, "{id:?} gave {result:?}");
        }
    }

    /// One app's own rule says which kinds of notification it arrives as and
    /// what it may do; a kind left unset follows the person's rules, and the
    /// verbs change only the fields they name.
    #[test]
    fn an_apps_own_rule_says_which_kinds_arrive_and_the_rest_follow_the_person() {
        let mut replica = Replica::<NotificationPrefs>::new().in_app("so.tilo.model", "Model");
        replica
            .apply(
                NotificationPrefs::SET_APP_KINDS.with(notification_prefs::SetAppKinds {
                    app: "so.tilo.mail".into(),
                    banners: Some(false),
                    previews: None,
                    sounds: Some(true),
                    badges: None,
                }),
            )
            .unwrap();
        let mail = replica.state().unwrap().per_app["so.tilo.mail"].clone();
        assert_eq!(
            (mail.banners, mail.previews, mail.sounds, mail.badges),
            (Some(false), None, Some(true), None)
        );
        replica
            .apply(
                NotificationPrefs::SET_APP_REACH.with(notification_prefs::SetAppReach {
                    app: "so.tilo.mail".into(),
                    lock_screen: Some(true),
                    cross_device: None,
                }),
            )
            .unwrap();
        replica
            .apply(
                NotificationPrefs::SET_APP_PRIORITY.with(notification_prefs::SetAppPriority {
                    app: "so.tilo.mail".into(),
                    priority: true,
                    priority_contacts: false,
                }),
            )
            .unwrap();
        let state = replica.state().unwrap();
        let mail = &state.per_app["so.tilo.mail"];
        assert_eq!((mail.banners, mail.sounds), (Some(false), Some(true)));
        assert_eq!((mail.lock_screen, mail.cross_device), (Some(true), None));
        assert!(mail.priority && !mail.priority_contacts);
        // The compositor reads the rule through the shared policy, as it decides a banner.
        let mut document = serde_json::Map::new();
        document.insert(
            "notifications".into(),
            serde_json::to_value(&state).unwrap(),
        );
        let policy =
            settings_store::notification_policy::NotificationPolicy::from_document(&document);
        let effective = policy.effective_for(Some("so.tilo.mail"), 0, 0, 0);
        assert!(!effective.banners, "the app's own rule narrows banners");
        assert!(effective.sounds, "and says its sounds arrive");
        assert!(
            effective.previews,
            "what it leaves unset follows the person"
        );
        assert!(effective.lock_screen);
        assert!(effective.cross_device);
    }

    #[test]
    fn stored_rule_allows_dnd_and_removal_while_new_bad_keys_are_refused() {
        assert!(valid_stored_app_id("foo."));
        assert!(!valid_app_id("foo."));

        let mut doc = ui::tilo_resource::Doc::new();
        NotificationPrefs::initialize(&mut doc).unwrap();
        let mut stored = NotificationPrefs::default();
        stored.per_app.insert(
            "foo.".into(),
            settings_store::notification_policy::AppNotificationRule {
                banners: Some(false),
                ..Default::default()
            },
        );
        stored
            .write_fields(&mut doc, &ui::tilo_resource::__private::ROOT)
            .unwrap();
        doc.commit();
        let mut replica =
            Replica::<NotificationPrefs>::from_doc(doc).in_app("so.tilo.model", "Model");

        // Existing stored `foo.` never blocks switching Do Not Disturb.
        replica
            .apply(Invoke::edit(&NotificationPrefs::paths().do_not_disturb.set(true)).unwrap())
            .unwrap();
        assert!(replica.state().unwrap().do_not_disturb);

        // Adding a new rule under `a..b` is refused by `mute_app`, while muting the
        // already-stored `foo.` rule succeeds.
        assert!(
            replica
                .apply(
                    NotificationPrefs::MUTE_APP.with(notification_prefs::MuteApp {
                        app: "a..b".into(),
                        duration: "For 1 hour".into(),
                    })
                )
                .is_err()
        );
        assert!(!replica.state().unwrap().per_app.contains_key("a..b"));
        replica
            .apply(
                NotificationPrefs::MUTE_APP.with(notification_prefs::MuteApp {
                    app: "foo.".into(),
                    duration: "For 1 hour".into(),
                }),
            )
            .unwrap();
        assert!(
            replica.state().unwrap().per_app["foo."]
                .muted_until
                .is_some()
        );

        // Removing the stored `foo.` rule succeeds.
        replica
            .apply(Invoke::edit(&NotificationPrefs::paths().per_app.remove("foo.")).unwrap())
            .unwrap();
        assert!(!replica.state().unwrap().per_app.contains_key("foo."));
    }
}

#[cfg(test)]
mod audio_theme_tests {
    use super::AudioPrefs;
    use settings_store::system_sounds::DEFAULT_THEME;

    #[test]
    fn a_sound_theme_is_chosen_by_one_name_or_left_to_the_system() {
        let mut audio = AudioPrefs::default();
        assert!(audio.system_sounds.theme.is_none());

        audio.choose_sound_theme(Some("ocean")).unwrap();
        assert_eq!(audio.system_sounds.theme.as_deref(), Some("ocean"));
        // Following the freedesktop theme is no choice of its own, and an
        // empty one is no name at all.
        audio.choose_sound_theme(Some(DEFAULT_THEME)).unwrap();
        assert!(audio.system_sounds.theme.is_none());
        audio.choose_sound_theme(Some(" ")).unwrap();
        assert!(audio.system_sounds.theme.is_none());
        audio.choose_sound_theme(None).unwrap();
        assert!(audio.system_sounds.theme.is_none());

        // A theme is one name in the sound directories, never a path.
        assert!(audio.choose_sound_theme(Some("../../etc")).is_err());
        assert!(audio.system_sounds.theme.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// A language is taken by the tag the desktop speaks, refused with the language left as it
    /// was when it names one it does not, and none follows the device again.
    #[test]
    fn a_language_is_one_the_desktop_speaks() {
        let mut appearance = Replica::<AppearancePrefs>::new().in_app("so.tilo.model", "Model");
        appearance
            .apply(AppearancePrefs::CHOOSE_LANGUAGE.with(Some("de-DE".to_owned())))
            .expect("a language the desktop speaks");
        assert_eq!(appearance.state().unwrap().language.as_deref(), Some("de"));
        let refused = appearance
            .apply(AppearancePrefs::CHOOSE_LANGUAGE.with(Some("tlh".to_owned())))
            .expect_err("a language the desktop does not speak");
        assert!(format!("{refused:?}").contains("speaks"), "{refused:?}");
        assert_eq!(
            appearance.state().unwrap().language.as_deref(),
            Some("de"),
            "a refused language changes nothing"
        );
        appearance
            .apply(AppearancePrefs::CHOOSE_LANGUAGE.with(Option::<String>::None))
            .expect("following the device again");
        assert_eq!(appearance.state().unwrap().language, None);
    }
}
