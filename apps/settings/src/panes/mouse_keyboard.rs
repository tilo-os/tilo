//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! Every control is an [`InputEffect`] on durable state:
//! - **Pointer speed / per-device natural scrolling / Tap to click / Key
//!   repeat** and the other pointer and keyboard choices are the `input`
//!   section of the person's settings; the compositor reads them when it
//!   configures libinput for the session. Whether the touchpad is on, and
//!   each tablet's mapping, stay with this device.
//! - **Keyboard input sources** are per-user durable intent. Settings never
//!   invokes `localectl`; the compositor owns XKB and hot-reloads the section.
//! - **Spell checking** is the `text_assistance` section every app's text
//!   fields follow.

pub(crate) mod keyboard_editor;

use std::cell::RefCell;
use std::collections::HashMap;

use settings_store::hardware::{
    EraserAction, InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot, PadDial,
    PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig, TouchpadClickMethod,
    TouchpadScrollMethod,
};
use taffy::{NodeId, TaffyTree};
use ui::framework::{Effect as _, EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::text_assistance::TextAssistancePrefs;
use ui::theme::Theme;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource;
use ui::widget::{
    AppModel, Button, Dropdown, ModelPath, Row, Slider, Spacer, Text, TextInput, Toggle, Widget,
};
use vello::kurbo::{Point, Rect};

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::input_sources::CatalogEntry;
use crate::prefs::InputPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, caption, error_caption, field_row, rows, section, setting_row, setting_row_described,
};

/// The compose-key choice that turns it off.
const COMPOSE_DISABLED: &str = "disabled";
/// The Caps Lock choice that keeps it Caps Lock.
const CAPS_LOCK_UNCHANGED: &str = "caps_lock";

/// The Caps Lock choices, as shown and as stored in `input.caps_lock`.
pub const CAPS_LOCK_CHOICES: [(&str, Option<&str>); 4] = [
    ("Caps Lock", None),
    ("Control", Some("ctrl")),
    ("Escape", Some("escape")),
    ("Backspace", Some("backspace")),
];

fn caps_lock_label(stored: Option<&str>) -> &'static str {
    CAPS_LOCK_CHOICES
        .iter()
        .find(|(_, value)| *value == stored)
        .map_or("Caps Lock", |(label, _)| label)
}

const COMPOSE_KEY_CHOICES: &[(&str, Option<&str>)] = &[
    ("Disabled", None),
    ("Right Alt", Some("ralt")),
    ("Menu", Some("menu")),
    ("Caps Lock", Some("caps")),
    ("Right Super", Some("rwin")),
    ("Left Super", Some("lwin")),
];

fn compose_ftl(english: &str) -> &'static str {
    match english {
        "Right Alt" => "input-compose-ralt",
        "Menu" => "input-compose-menu",
        "Caps Lock" => "input-compose-caps",
        "Right Super" => "input-compose-rwin",
        "Left Super" => "input-compose-lwin",
        _ => "input-compose-disabled",
    }
}

fn compose_shown(english: &str) -> String {
    t(compose_ftl(english))
}

fn compose_english(stored: Option<&str>) -> &'static str {
    COMPOSE_KEY_CHOICES
        .iter()
        .find(|(_, value)| *value == stored)
        .map_or("Disabled", |(label, _)| label)
}

/// A matched keyboard-choice label: clear (stored `None`) or a concrete key id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredKeyChoice {
    Clear,
    Key(&'static str),
}

impl StoredKeyChoice {
    pub(crate) fn as_stored(self) -> Option<&'static str> {
        match self {
            Self::Clear => None,
            Self::Key(key) => Some(key),
        }
    }

    fn from_stored(stored: Option<&'static str>) -> Self {
        match stored {
            None => Self::Clear,
            Some(key) => Self::Key(key),
        }
    }
}

pub(crate) fn compose_from_label(label: &str) -> Option<StoredKeyChoice> {
    COMPOSE_KEY_CHOICES
        .iter()
        .find(|(english, _)| *english == label || compose_shown(english) == label)
        .map(|(_, stored)| StoredKeyChoice::from_stored(*stored))
}

fn compose_options() -> Vec<String> {
    COMPOSE_KEY_CHOICES
        .iter()
        .map(|(english, _)| compose_shown(english))
        .collect()
}

fn caps_lock_ftl(english: &str) -> &'static str {
    match english {
        "Control" => "input-caps-lock-ctrl",
        "Escape" => "input-caps-lock-esc",
        "Backspace" => "input-caps-lock-backspace",
        _ => "input-caps-lock-caps",
    }
}

fn caps_lock_shown(english: &str) -> String {
    t(caps_lock_ftl(english))
}

pub(crate) fn caps_lock_from_label(label: &str) -> Option<StoredKeyChoice> {
    CAPS_LOCK_CHOICES
        .iter()
        .find(|(english, _)| *english == label || caps_lock_shown(english) == label)
        .map(|(_, stored)| StoredKeyChoice::from_stored(*stored))
}

fn caps_lock_options() -> Vec<String> {
    CAPS_LOCK_CHOICES
        .iter()
        .map(|(english, _)| caps_lock_shown(english))
        .collect()
}

fn accel_shown(profile: &str) -> String {
    if profile == "flat" || profile == "Flat" {
        t("input-accel-flat")
    } else {
        t("input-accel-adaptive")
    }
}

pub(crate) fn accel_from_label(label: &str) -> Option<&'static str> {
    if label == "Flat" || label == t("input-accel-flat") {
        Some("flat")
    } else if label == "Adaptive" || label == t("input-accel-adaptive") {
        Some("adaptive")
    } else {
        None
    }
}

fn accel_options() -> Vec<String> {
    vec![t("input-accel-adaptive"), t("input-accel-flat")]
}
/// Touchpad click methods as `(stable id, label key, value)`, in menu order.
/// The id is what the dropdown and its action carry; only the label is
/// translated.
pub const CLICK_METHOD_CHOICES: [(&str, &str, TouchpadClickMethod); 3] = [
    (
        "default",
        "input-click-method-default",
        TouchpadClickMethod::Default,
    ),
    (
        "areas",
        "input-click-method-areas",
        TouchpadClickMethod::Areas,
    ),
    (
        "clickfinger",
        "input-click-method-clickfinger",
        TouchpadClickMethod::Clickfinger,
    ),
];

/// Touchpad scroll methods as `(stable id, label key, value)`, in menu order.
pub const SCROLL_METHOD_CHOICES: [(&str, &str, TouchpadScrollMethod); 2] = [
    (
        "two_finger",
        "input-scroll-method-two-finger",
        TouchpadScrollMethod::TwoFinger,
    ),
    (
        "edge",
        "input-scroll-method-edge",
        TouchpadScrollMethod::Edge,
    ),
];

pub fn click_method_id(method: TouchpadClickMethod) -> &'static str {
    CLICK_METHOD_CHOICES
        .iter()
        .find(|(_, _, value)| *value == method)
        .map_or("default", |(id, _, _)| id)
}

pub fn click_method_for_id(id: &str) -> Option<TouchpadClickMethod> {
    CLICK_METHOD_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
}

pub fn scroll_method_id(method: TouchpadScrollMethod) -> &'static str {
    SCROLL_METHOD_CHOICES
        .iter()
        .find(|(_, _, value)| *value == method)
        .map_or("two_finger", |(id, _, _)| id)
}

pub fn scroll_method_for_id(id: &str) -> Option<TouchpadScrollMethod> {
    SCROLL_METHOD_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
}

/// `(id, translated label)` pairs for a dropdown over stable ids.
fn localized_choices<T>(choices: &[(&str, &str, T)]) -> Vec<(String, String)> {
    choices
        .iter()
        .map(|(id, key, _)| ((*id).to_string(), t(key)))
        .collect()
}

/// What a compose-key or Caps Lock choice is called in an effect.
fn stored_ids(
    table: &'static [(&'static str, Option<&'static str>)],
    unset: &'static str,
) -> impl Iterator<Item = &'static str> {
    table.iter().map(move |(_, stored)| stored.unwrap_or(unset))
}

/// The stored value an effect's choice names: `Some(None)` clears it.
#[allow(
    clippy::option_option,
    reason = "outer: whether the choice is offered; inner: the stored value, absent to clear"
)]
fn stored_for(
    table: &'static [(&'static str, Option<&'static str>)],
    unset: &'static str,
    id: &str,
) -> Option<Option<String>> {
    if id == unset {
        return Some(None);
    }
    table
        .iter()
        .find(|(_, stored)| *stored == Some(id))
        .map(|(_, stored)| stored.map(str::to_owned))
}

/// The ids of a `(id, label key, value)` table.
fn ids<T>(table: &'static [(&'static str, &'static str, T)]) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(id, _, _)| *id)
}

/// A dropdown whose values are stable ids and whose rows are translated.
fn id_dropdown(
    choices: Vec<(String, String)>,
    selected: &str,
    test_id: &str,
    accessible_name: &str,
) -> Dropdown {
    let mut dropdown = Dropdown::from_state(
        choices.iter().map(|(id, _)| id.clone()).collect(),
        &ui::LocalState::new(selected.to_string()),
    )
    .with_id(test_id)
    .with_resolved_accessible_name(accessible_name);
    dropdown.set_value_options(choices);
    dropdown
}

/// The languages spelling can be checked in, by id, each named in its own
/// language; the empty id is the session's language.
pub const SPELLING_LANGUAGES: [(&str, &str); 9] = [
    ("", ""),
    ("en_US", "English (United States)"),
    ("en_GB", "English (United Kingdom)"),
    ("de_DE", "Deutsch"),
    ("fr_FR", "Français"),
    ("es_ES", "Español"),
    ("it_IT", "Italiano"),
    ("nl_NL", "Nederlands"),
    ("pt_BR", "Português (Brasil)"),
];

/// The spelling language choices, the session's named in the reader's
/// language.
fn spelling_language_choices() -> Vec<(String, String)> {
    SPELLING_LANGUAGES
        .iter()
        .map(|(id, name)| {
            let shown = if id.is_empty() {
                t("input-spell-language-session")
            } else {
                (*name).to_owned()
            };
            ((*id).to_owned(), shown)
        })
        .collect()
}

/// The Map-to choice for spanning every display.
pub const WHOLE_DESKTOP: &str = "desktop";
/// Pad buttons Settings offers a shortcut for; pads have far fewer.
pub const MAX_PAD_BUTTONS: u32 = 32;
/// The Pad-button choice for a button that does nothing.
const PAD_BUTTON_NONE: &str = "none";
/// The Pad-button choice for the button that shows the pad's map.
const PAD_BUTTON_SHOW_MAP: &str = "show-map";
/// Rings and strips Settings offers a mapping for, of each kind.
pub const MAX_PAD_DIALS: u32 = 4;
/// Modes per ring or strip Settings offers a mapping for.
pub const MAX_PAD_MODES: u32 = 8;

/// Resource id of a tablet's card and actions.
pub fn tablet_resource_id(device: &str) -> String {
    format!("settings.tablet:{device}")
}

/// Resource id of a tablet pad's card and actions.
pub fn pad_resource_id(device: &str) -> String {
    format!("settings.tablet-pad:{device}")
}

/// Map-to choices: the whole desktop, then each connected display by the
/// name a person recognises.
pub fn tablet_output_choices() -> Vec<(String, String)> {
    let mut choices = vec![(WHOLE_DESKTOP.to_string(), t("input-tablet-whole-desktop"))];
    if let Some(snapshot) = settings_store::hardware::DisplayHardwareSnapshot::load() {
        for placement in &snapshot.layout {
            let label = snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == placement.connector)
                .map_or_else(
                    || placement.connector.clone(),
                    |output| {
                        output
                            .descriptor
                            .monitor
                            .as_ref()
                            .map(settings_store::hardware::MonitorIdentity::display_name)
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| output.descriptor.label().to_string())
                    },
                );
            choices.push((placement.connector.clone(), label));
        }
    }
    choices
}

/// The saved output for a Map-to choice id: `Some(None)` spans the desktop,
/// `None` names a display that is not connected.
pub fn tablet_output_for_id(id: &str) -> Option<Option<String>> {
    if id == WHOLE_DESKTOP {
        return Some(None);
    }
    tablet_output_choices()
        .into_iter()
        .any(|(choice, _)| choice == id)
        .then(|| Some(id.to_string()))
}

fn shortcut_id(action: ShortcutAction) -> String {
    let name = serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    format!("shortcut:{name}")
}

fn shortcut_for_id(id: &str) -> Option<ShortcutAction> {
    let name = id.strip_prefix("shortcut:")?;
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

fn shortcut_choices() -> impl Iterator<Item = (String, String)> {
    crate::panes::keyboard_shortcuts::shortcut_actions()
        .into_iter()
        .map(|(action, label)| (shortcut_id(action), label))
}

/// Stylus button choices as `(id, label key, action)`, before the shortcuts.
const STYLUS_BUTTON_CHOICES: [(&str, &str, StylusButtonAction); 7] = [
    (
        "default",
        "input-pen-button-default",
        StylusButtonAction::Default,
    ),
    ("left", "input-pen-button-left", StylusButtonAction::Left),
    (
        "middle",
        "input-pen-button-middle",
        StylusButtonAction::Middle,
    ),
    ("right", "input-pen-button-right", StylusButtonAction::Right),
    ("back", "input-pen-button-back", StylusButtonAction::Back),
    (
        "forward",
        "input-pen-button-forward",
        StylusButtonAction::Forward,
    ),
    (
        "disabled",
        "input-pen-button-disabled",
        StylusButtonAction::Disabled,
    ),
];

/// What a pen button can do: a mouse button, nothing, or any shortcut.
pub fn stylus_button_choices() -> Vec<(String, String)> {
    localized_choices(&STYLUS_BUTTON_CHOICES)
        .into_iter()
        .chain(shortcut_choices())
        .collect()
}

pub fn stylus_button_id(action: StylusButtonAction) -> String {
    match action {
        StylusButtonAction::Shortcut(shortcut) => shortcut_id(shortcut),
        other => STYLUS_BUTTON_CHOICES
            .iter()
            .find(|(_, _, value)| *value == other)
            .map_or("default", |(id, _, _)| id)
            .to_string(),
    }
}

pub fn stylus_button_for_id(id: &str) -> Option<StylusButtonAction> {
    STYLUS_BUTTON_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
        .or_else(|| shortcut_for_id(id).map(StylusButtonAction::Shortcut))
}

/// Eraser choices as `(id, label key, action)`, before the shortcuts.
const ERASER_CHOICES: [(&str, &str, EraserAction); 6] = [
    ("native", "input-eraser-native", EraserAction::Native),
    ("left", "input-pen-button-left", EraserAction::Left),
    ("middle", "input-pen-button-middle", EraserAction::Middle),
    ("right", "input-pen-button-right", EraserAction::Right),
    ("back", "input-pen-button-back", EraserAction::Back),
    ("forward", "input-pen-button-forward", EraserAction::Forward),
];

/// What the eraser end can do: erase, click a mouse button, or any shortcut.
pub fn eraser_choices() -> Vec<(String, String)> {
    localized_choices(&ERASER_CHOICES)
        .into_iter()
        .chain(shortcut_choices())
        .collect()
}

pub fn eraser_id(action: EraserAction) -> String {
    match action {
        EraserAction::Shortcut(shortcut) => shortcut_id(shortcut),
        other => ERASER_CHOICES
            .iter()
            .find(|(_, _, value)| *value == other)
            .map_or("native", |(id, _, _)| id)
            .to_string(),
    }
}

pub fn eraser_for_id(id: &str) -> Option<EraserAction> {
    ERASER_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
        .or_else(|| shortcut_for_id(id).map(EraserAction::Shortcut))
}

/// Ring and strip choices as `(id, label key, action)`.
const DIAL_CHOICES: [(&str, &str, PadDialAction); 6] = [
    ("nothing", "input-pad-dial-nothing", PadDialAction::Nothing),
    ("scroll", "input-pad-dial-scroll", PadDialAction::Scroll),
    ("zoom", "input-pad-dial-zoom", PadDialAction::Zoom),
    (
        "brush-size",
        "input-pad-dial-brush-size",
        PadDialAction::BrushSize,
    ),
    ("volume", "input-pad-dial-volume", PadDialAction::Volume),
    (
        "brightness",
        "input-pad-dial-brightness",
        PadDialAction::Brightness,
    ),
];

/// What a ring or strip does in apps without tablet support.
pub fn dial_choices() -> Vec<(String, String)> {
    localized_choices(&DIAL_CHOICES)
}

pub fn dial_id(action: PadDialAction) -> &'static str {
    DIAL_CHOICES
        .iter()
        .find(|(_, _, value)| *value == action)
        .map_or("scroll", |(id, _, _)| id)
}

pub fn dial_for_id(id: &str) -> Option<PadDialAction> {
    DIAL_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
}

/// The resource action id of a ring or strip in one mode.
pub fn dial_action_id(dial: PadDial, mode: u32) -> String {
    match dial {
        PadDial::Ring(index) => format!("ring-{index}-mode-{mode}"),
        PadDial::Strip(index) => format!("strip-{index}-mode-{mode}"),
    }
}

/// A pad's rings then strips, each with how many modes Settings offers.
pub fn pad_dials(device: &InputDeviceSnapshot) -> Vec<(PadDial, u32)> {
    let offered = |controls: &[u32]| {
        (0..MAX_PAD_DIALS)
            .zip(controls.iter().copied())
            .map(|(index, modes)| (index, modes.clamp(1, MAX_PAD_MODES)))
            .collect::<Vec<_>>()
    };
    offered(&device.pad_rings)
        .into_iter()
        .map(|(index, modes)| (PadDial::Ring(index), modes))
        .chain(
            offered(&device.pad_strips)
                .into_iter()
                .map(|(index, modes)| (PadDial::Strip(index), modes)),
        )
        .collect()
}

/// What a pad button does in an app without tablet support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadButtonChoice {
    Nothing,
    /// Shows what every control does while held.
    ShowMap,
    Shortcut(ShortcutAction),
}

/// What a pad button can do: nothing, show the pad's map, or any shortcut.
pub fn pad_button_choices() -> Vec<(String, String)> {
    [
        (PAD_BUTTON_NONE.to_string(), t("input-pad-button-none")),
        (
            PAD_BUTTON_SHOW_MAP.to_string(),
            t("input-pad-button-show-map"),
        ),
    ]
    .into_iter()
    .chain(shortcut_choices())
    .collect()
}

/// The choice `config` makes for pad button `button`.
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
pub fn pad_button_for_id(id: &str) -> Option<PadButtonChoice> {
    match id {
        PAD_BUTTON_NONE => Some(PadButtonChoice::Nothing),
        PAD_BUTTON_SHOW_MAP => Some(PadButtonChoice::ShowMap),
        _ => shortcut_for_id(id).map(PadButtonChoice::Shortcut),
    }
}

/// Make pad button `button` do `choice`. One button shows the map: picking
/// it for another moves it there.
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

/// "Ring 1", or "Ring 1, mode 2" when its group has several modes.
fn dial_label(dial: PadDial, mode: u32, modes: u32) -> String {
    let (key, index) = match dial {
        PadDial::Ring(index) => ("input-pad-ring", index),
        PadDial::Strip(index) => ("input-pad-strip", index),
    };
    let number = ArgValue::from(i64::from(index) + 1);
    if modes > 1 {
        t_args(
            &format!("{key}-mode"),
            &[
                ("number", number),
                ("mode", ArgValue::from(i64::from(mode) + 1)),
            ],
        )
    } else {
        t_args(key, &[("number", number)])
    }
}

/// A tablet or pad device's card title: its name without the kernel's
/// ` Pen` / ` Pad` suffix, which the rows already make clear.
fn device_title(device: &InputDeviceSnapshot) -> String {
    let name = device.name.trim();
    let base = name
        .strip_suffix(" Pen")
        .or_else(|| name.strip_suffix(" Pad"))
        .unwrap_or(name);
    match device.kind {
        InputDeviceKind::TabletPad => t_args("input-pad-title", &[("name", ArgValue::from(base))]),
        _ => base.to_string(),
    }
}

/// The ring or strip, its index and mode a [`dial_action_id`] names.
fn parse_dial_action_id(id: &str) -> Option<(&'static str, u32, u32)> {
    let (dial, rest) = if let Some(rest) = id.strip_prefix("ring-") {
        ("ring", rest)
    } else {
        ("strip", id.strip_prefix("strip-")?)
    };
    let (index, mode) = rest.split_once("-mode-")?;
    Some((dial, index.parse().ok()?, mode.parse().ok()?))
}

/// Which pointer sections apply to the attached hardware. Without a live
/// device snapshot (no compositor, a headless render) every section shows:
/// hiding a setting because nobody told us about a device is worse than
/// showing one that does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent visibility flag per Settings section"
)]
pub struct InputSections {
    pub touchpad: bool,
    pub mouse: bool,
    /// Buttons, acceleration and scroll speed: any pointer at all.
    pub pointer: bool,
    pub tablet: bool,
}

impl InputSections {
    pub fn for_devices(devices: Option<&InputHardwareSnapshot>) -> Self {
        let Some(devices) = devices else {
            return Self {
                touchpad: true,
                mouse: true,
                pointer: true,
                tablet: true,
            };
        };
        let touchpad = devices.has(InputDeviceKind::Touchpad);
        let mouse = devices.has(InputDeviceKind::Mouse);
        Self {
            touchpad,
            mouse,
            pointer: touchpad || mouse,
            tablet: devices.has(InputDeviceKind::Tablet) || devices.has(InputDeviceKind::TabletPad),
        }
    }
}

/// Pointer, keyboard, input source, tablet and spelling choices.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum InputEffect {
    /// Read configured input sources and the mounted Add language selector.
    #[effect(returns, idempotent, device_only, kind = "read")]
    InspectInputSources {
        query: Option<String>,
        page: Option<u32>,
    },
    /// Set the compose key.
    #[effect(idempotent)]
    SetComposeKey {
        #[param("The key", choices = stored_ids(COMPOSE_KEY_CHOICES, COMPOSE_DISABLED))]
        key: String,
    },
    /// Set what the Caps Lock key does.
    #[effect(idempotent)]
    SetCapsLock {
        #[param("What it does", choices = stored_ids(&CAPS_LOCK_CHOICES, CAPS_LOCK_UNCHANGED))]
        behaviour: String,
    },
    /// Type with one of the active input sources.
    #[effect(idempotent)]
    SetActiveSource {
        #[param("The source's id, such as `us` or `de`")]
        source: String,
    },
    /// Inspect this device's mounted keyboard map and current physical feedback.
    #[effect(returns, idempotent, device_only, kind = "read")]
    InspectKeyboardMap,
    /// Read discovered keyboard scopes and their ordered configured sources.
    #[effect(returns, idempotent, device_only, kind = "read")]
    InspectKeyboardSources,
    /// Choose the discovered keyboard assignment to inspect and edit.
    #[effect(idempotent)]
    SelectKeyboardScope { scope: String },
    /// Make a keyboard that types its own sources type the shared ones again,
    /// forgetting its own list.
    UseSharedSources {
        #[param("The keyboard's device key, as `InspectKeyboardSources` reports it")]
        scope: String,
    },
    /// Select a configured source without changing what the keyboard types.
    #[effect(idempotent)]
    SelectConfiguredSource { source: String },
    /// Show or hide the selected source options.
    #[effect(idempotent)]
    SetSourceOptionsVisible { visible: bool },
    /// Show or hide key and modifier-level inspection.
    #[effect(idempotent)]
    SetSourceInspectionVisible { visible: bool },
    /// Open the separate installed-source picker.
    #[effect(idempotent)]
    OpenSourcePicker,
    /// Return to configured sources without adding anything.
    #[effect(idempotent)]
    CloseSourcePicker,
    /// Preview an installed source in the Add screen.
    #[effect(idempotent)]
    ChooseSourceToAdd { source: String },
    /// Add the previewed source without changing the active source.
    ConfirmSourceToAdd,
    /// Move a configured source before or after another stable source id.
    ReorderSource {
        source: String,
        target: String,
        after: bool,
    },
    /// Inspect any installed XKB layout without changing what the keyboard types.
    #[effect(idempotent)]
    PreviewLayout { source: String },
    /// Show the actual focused keyboard group and modifier state.
    #[effect(idempotent)]
    ShowLiveKeyboardMap,
    /// Inspect XKB levels 5–8 when the selected layout supplies them.
    #[effect(idempotent)]
    SetPreviewExtraLevels { on: bool },
    /// Inspect one of the compiled native modifier levels without applying it.
    #[effect(idempotent)]
    SetPreviewLevel { level: String },
    /// Read a physical key’s complete legends without applying a layout.
    #[effect(idempotent)]
    InspectKeyboardKey { key: String },
    /// Choose an explicit physical model for the preview and keyboard.
    #[effect(idempotent)]
    SetKeyboardModel {
        #[param("The physical model", choices = ["pc104", "pc105", "jp106", "kr106"])]
        model: String,
    },
    /// Type with the inspected layout, adding it when space is available.
    ApplyPreview,
    /// Add an installed input source and type with it.
    AddSource {
        #[param("The source's id, such as `us` or `de`")]
        source: String,
    },
    /// Replace one input source by another, in place.
    ReplaceSource {
        #[param("The source to replace")]
        from: String,
        #[param("The source's id, such as `us` or `de`")]
        source: String,
    },
    /// Move an input source earlier in the switching order.
    MoveSourceUp {
        #[param("The source's id")]
        source: String,
    },
    /// Move an input source later in the switching order.
    MoveSourceDown {
        #[param("The source's id")]
        source: String,
    },
    /// Remove an input source.
    RemoveSource {
        #[param("The source's id")]
        source: String,
    },
    /// Add an explicit word to this person's shared local dictionary.
    AddDictionaryWord {
        #[param("The spelling, including its intended capitalization")]
        word: String,
        #[param("Language tag, or empty for all languages")]
        language: String,
        #[param("The dictionary revision being edited")]
        revision: u64,
    },
    /// Edit an existing personal word, preserving the chosen casing.
    EditDictionaryWord {
        old_word: String,
        old_language: String,
        word: String,
        language: String,
        revision: u64,
    },
    /// Remove one explicit addition from every local input consumer.
    RemoveDictionaryWord {
        word: String,
        language: String,
        revision: u64,
    },
    /// Reset this person's local dictionary. Other accounts are unchanged.
    ResetDictionary { revision: u64 },
    /// Set which shortcut pastes in one app when the shell pastes for the
    /// person (Super+V with an image or files), or `default` to go back to
    /// what its app id suggests (Ctrl+Shift+V for terminals, else Ctrl+V).
    #[effect(idempotent)]
    SetPasteShortcut {
        #[param("The app's id, such as `org.wezfurlong.wezterm`")]
        app: String,
        #[param("The shortcut", choices = ["ctrl_v", "ctrl_shift_v", "default"])]
        shortcut: String,
    },
    /// Map a drawing tablet onto one display, or the whole desktop.
    #[effect(idempotent)]
    SetTabletOutput {
        #[param("The tablet's device id")]
        device: String,
        #[param("A display's connector, or `desktop`")]
        output: String,
    },
    /// Keep a tablet's aspect ratio when mapping it, or stretch it.
    #[effect(idempotent)]
    SetTabletKeepAspect {
        #[param("The tablet's device id")]
        device: String,
        on: bool,
    },
    /// Turn a tablet around for the left hand, or back.
    #[effect(idempotent)]
    SetTabletLeftHanded {
        #[param("The tablet's device id")]
        device: String,
        on: bool,
    },
    /// Set how firmly a tablet's pen must press.
    #[effect(idempotent)]
    SetTabletTipFeel {
        #[param("The tablet's device id")]
        device: String,
        #[param("Soft (0.0) to firm (1.0)")]
        feel: f64,
    },
    /// Set what one of a pen's buttons does.
    #[effect(idempotent)]
    SetTabletButton {
        #[param("The tablet's device id")]
        device: String,
        #[param("The button", choices = ["lower", "upper", "third"])]
        button: String,
        #[param("A pen-button choice: `left`, `right`, `disabled`, `shortcut:search`, …")]
        action: String,
    },
    /// Set what a pen's eraser end does.
    #[effect(idempotent)]
    SetTabletEraser {
        #[param("The tablet's device id")]
        device: String,
        #[param("An eraser choice: `native`, `right`, `shortcut:search`, …")]
        action: String,
    },
    /// Set what a pad button does.
    #[effect(idempotent)]
    SetPadButton {
        #[param("The pad's device id")]
        device: String,
        #[param("The button, counting from 0")]
        button: u32,
        #[param("`none`, `show-map` or a shortcut such as `shortcut:search`")]
        action: String,
    },
    /// Set what a pad's ring or strip does in one mode.
    #[effect(idempotent)]
    SetPadDial {
        #[param("The pad's device id")]
        device: String,
        #[param("`ring` or `strip`", choices = ["ring", "strip"])]
        dial: String,
        #[param("Which ring or strip, counting from 0")]
        index: u32,
        #[param("The mode, counting from 0")]
        mode: u32,
        #[param("What it does", choices = ids(&DIAL_CHOICES))]
        action: String,
    },
}

fn fraction(value: f64) -> Result<f64, String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("the value must be between 0.0 and 1.0".into())
    }
}

/// The saved settings of the tablet or pad `device`, created on first use.
/// Keys are device identities from the compositor's snapshot, so anything
/// else is refused rather than stored.
fn tablet_entry<'a>(
    input: &'a mut InputPrefs,
    device: &str,
) -> Result<&'a mut TabletConfig, String> {
    if device.is_empty() || device.len() > 512 || device.chars().any(char::is_control) {
        return Err("unknown tablet".into());
    }
    Ok(input.tablets.entry(device.to_string()).or_default())
}

/// Apply `effect` to `input`, refusing what it cannot be.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per input choice keeps every validation in one place"
)]
fn apply(
    input: &mut InputPrefs,
    effect: InputEffect,
    catalog: &[CatalogEntry],
) -> Result<(), String> {
    use InputEffect as E;
    let installed = |source: &str| catalog.iter().any(|entry| entry.id == source);
    match effect {
        E::SetComposeKey { key } => {
            input.compose_key = stored_for(COMPOSE_KEY_CHOICES, COMPOSE_DISABLED, &key)
                .ok_or_else(|| format!("`{key}` is not a compose key"))?;
        }
        E::SetCapsLock { behaviour } => {
            input.caps_lock = stored_for(&CAPS_LOCK_CHOICES, CAPS_LOCK_UNCHANGED, &behaviour)
                .ok_or_else(|| format!("`{behaviour}` is not a Caps Lock behaviour"))?;
        }
        E::SetActiveSource { source } => {
            if !input.keyboard_layouts.contains(&source) {
                return Err(format!("`{source}` is not an active input source"));
            }
            input.keyboard_layout = source;
        }
        E::PreviewLayout { source } => {
            if !installed(&source)
                || settings_store::hardware::KeyboardInputSource::parse(&source).is_none()
            {
                return Err("the source is not an installed XKB layout".into());
            }
            input.keyboard_preview = Some(source);
            input.keyboard_preview_level = 0;
            input.keyboard_preview_extra_levels = false;
        }
        E::ShowLiveKeyboardMap => {
            input.keyboard_preview = None;
            input.keyboard_preview_level = 0;
            input.keyboard_preview_extra_levels = false;
        }
        E::SetPreviewExtraLevels { on } => {
            input
                .keyboard_preview
                .get_or_insert_with(|| input.keyboard_layout.clone());
            input.keyboard_preview_extra_levels = on;
            input.keyboard_preview_level = 0;
        }
        E::SetPreviewLevel { level } => {
            let level = level
                .parse::<usize>()
                .ok()
                .filter(|level| *level < 32)
                .ok_or("invalid keyboard preview level")?;
            let source = input
                .keyboard_preview
                .as_deref()
                .unwrap_or(&input.keyboard_layout);
            let map = crate::keyboard_preview::map_with_options(
                source,
                &input.keyboard_model,
                String::new(),
                Some(""),
            )?;
            if level >= map.level_count() {
                return Err("the layout does not supply this level".into());
            }
            input.keyboard_preview = Some(source.to_owned());
            input.keyboard_preview_level = level;
            input.keyboard_preview_extra_levels = false;
        }
        E::InspectKeyboardKey { key } => {
            let source = input
                .keyboard_preview
                .as_deref()
                .unwrap_or(&input.keyboard_layout);
            let map = crate::keyboard_preview::map_with_options(
                source,
                &input.keyboard_model,
                String::new(),
                Some(""),
            )?;
            if map.inspection_values(&key).is_none() {
                return Err("the physical model does not supply this key".into());
            }
            input.keyboard_inspected_key = key;
        }
        E::SetKeyboardModel { model } => {
            if !crate::keyboard_preview::valid_model(&model) {
                return Err("unknown physical keyboard model".into());
            }
            input.keyboard_model = model;
            input.keyboard_inspected_key = "AD01".into();
            input.keyboard_preview_level = 0;
            input.keyboard_preview_extra_levels = false;
        }
        E::ApplyPreview => {
            let source = input
                .keyboard_preview
                .clone()
                .unwrap_or_else(|| input.keyboard_layout.clone());
            let next = if input.keyboard_layouts.contains(&source) {
                E::SetActiveSource { source }
            } else {
                E::AddSource { source }
            };
            apply(input, next, catalog)?;
            input.keyboard_preview = None;
            input.keyboard_preview_level = 0;
            input.keyboard_preview_extra_levels = false;
            return Ok(());
        }
        E::AddSource { source } => {
            if !installed(&source) {
                return Err(format!("`{source}` is not an installed input source"));
            }
            if input.keyboard_layouts.contains(&source) {
                return Err(format!("`{source}` is already active"));
            }
            if !crate::input_sources::has_room_for(&input.keyboard_layouts, &source) {
                return Err("the most keyboard layouts are already active".into());
            }
            input.keyboard_layout.clone_from(&source);
            input.keyboard_layouts.push(source);
        }
        E::MoveSourceUp { source } => {
            let index = position(input, &source)?;
            if index == 0 {
                return Err(format!("`{source}` is already first"));
            }
            input.keyboard_layouts.swap(index - 1, index);
        }
        E::MoveSourceDown { source } => {
            let index = position(input, &source)?;
            if index + 1 >= input.keyboard_layouts.len() {
                return Err(format!("`{source}` is already last"));
            }
            input.keyboard_layouts.swap(index, index + 1);
        }
        E::RemoveSource { source } => {
            let index = position(input, &source)?;
            if input.keyboard_layouts.len() <= 1 {
                return Err("the last input source cannot be removed".into());
            }
            input.keyboard_layouts.remove(index);
            if input.keyboard_layout == source {
                input.keyboard_layout = input
                    .keyboard_layouts
                    .get(index.min(input.keyboard_layouts.len() - 1))
                    .cloned()
                    .unwrap_or_default();
            }
        }
        E::SetTabletOutput { device, output } => {
            let output = tablet_output_for_id(&output)
                .ok_or_else(|| format!("`{output}` is not a connected display"))?;
            tablet_entry(input, &device)?.output = output;
        }
        E::SetTabletKeepAspect { device, on } => tablet_entry(input, &device)?.keep_aspect = on,
        E::SetTabletLeftHanded { device, on } => tablet_entry(input, &device)?.left_handed = on,
        E::SetTabletTipFeel { device, feel } => {
            tablet_entry(input, &device)?.pressure_curve =
                settings_store::hardware::pressure_curve_for_feel(fraction(feel)?);
        }
        E::SetTabletButton {
            device,
            button,
            action,
        } => {
            let action = stylus_button_for_id(&action)
                .ok_or_else(|| format!("`{action}` is not a pen-button choice"))?;
            let config = tablet_entry(input, &device)?;
            match button.as_str() {
                "lower" => config.lower_button = action,
                "upper" => config.upper_button = action,
                "third" => config.third_button = action,
                _ => return Err(format!("`{button}` is not a pen button")),
            }
        }
        E::SetTabletEraser { device, action } => {
            tablet_entry(input, &device)?.eraser = eraser_for_id(&action)
                .ok_or_else(|| format!("`{action}` is not an eraser choice"))?;
        }
        E::SetPadButton {
            device,
            button,
            action,
        } => {
            if button >= MAX_PAD_BUTTONS {
                return Err("unknown pad button".into());
            }
            let choice = pad_button_for_id(&action)
                .ok_or_else(|| format!("`{action}` is not a pad-button choice"))?;
            set_pad_button(tablet_entry(input, &device)?, button, choice);
        }
        E::SetPadDial {
            device,
            dial,
            index,
            mode,
            action,
        } => {
            if index >= MAX_PAD_DIALS || mode >= MAX_PAD_MODES {
                return Err("unknown pad control".into());
            }
            let dial = match dial.as_str() {
                "ring" => PadDial::Ring(index),
                "strip" => PadDial::Strip(index),
                _ => return Err(format!("`{dial}` is not a ring or strip")),
            };
            let action =
                dial_for_id(&action).ok_or_else(|| format!("`{action}` is not a dial choice"))?;
            tablet_entry(input, &device)?.set_dial_action(dial, mode, action);
        }
        E::InspectKeyboardSources
        | E::SetSourceOptionsVisible { .. }
        | E::SetSourceInspectionVisible { .. }
        | E::SelectKeyboardScope { .. }
        | E::UseSharedSources { .. }
        | E::SelectConfiguredSource { .. }
        | E::OpenSourcePicker
        | E::CloseSourcePicker
        | E::ChooseSourceToAdd { .. }
        | E::ConfirmSourceToAdd
        | E::ReplaceSource { .. }
        | E::ReorderSource { .. } => {
            return Err("this operation requires the keyboard editor".into());
        }
        E::InspectInputSources { .. }
        | E::InspectKeyboardMap
        | E::AddDictionaryWord { .. }
        | E::EditDictionaryWord { .. }
        | E::RemoveDictionaryWord { .. }
        | E::ResetDictionary { .. } => {}
        E::SetPasteShortcut { app, shortcut } => input.set_paste_shortcut(&app, &shortcut)?,
    }
    Ok(())
}

fn position(input: &InputPrefs, source: &str) -> Result<usize, String> {
    input
        .keyboard_layouts
        .iter()
        .position(|active| active == source)
        .ok_or_else(|| format!("`{source}` is not an active input source"))
}

/// Answer `effect`: change the input section as it is now, or refuse.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: InputEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if let InputEffect::InspectInputSources { query, page } = &effect {
        return inspect_input_sources(app, query.as_deref(), page.unwrap_or(0))
            .map(Effected::value);
    }
    if effect == InputEffect::InspectKeyboardMap {
        return app.keyboard_inspection().map(Effected::value);
    }
    let host = app.host();
    match effect {
        InputEffect::AddDictionaryWord {
            word,
            language,
            revision,
        } => {
            let word = ui::personal_dictionary::Word::new(
                &word,
                (!language.is_empty()).then_some(language.as_str()),
            )
            .map_err(|error| error.to_string())?;
            return ui::personal_dictionary::edit(
                revision,
                ui::personal_dictionary::Edit::Add(word),
            )
            .map(|()| Effected::done())
            .map_err(|error| error.to_string());
        }
        InputEffect::EditDictionaryWord {
            old_word,
            old_language,
            word,
            language,
            revision,
        } => {
            let old = ui::personal_dictionary::Word::new(
                &old_word,
                (!old_language.is_empty()).then_some(old_language.as_str()),
            )
            .map_err(|error| error.to_string())?;
            let new = ui::personal_dictionary::Word::new(
                &word,
                (!language.is_empty()).then_some(language.as_str()),
            )
            .map_err(|error| error.to_string())?;
            return ui::personal_dictionary::edit(
                revision,
                ui::personal_dictionary::Edit::Replace { old, new },
            )
            .map(|()| Effected::done())
            .map_err(|error| error.to_string());
        }
        InputEffect::RemoveDictionaryWord {
            word,
            language,
            revision,
        } => {
            let word = ui::personal_dictionary::Word::new(
                &word,
                (!language.is_empty()).then_some(language.as_str()),
            )
            .map_err(|error| error.to_string())?;
            return ui::personal_dictionary::edit(
                revision,
                ui::personal_dictionary::Edit::Remove(word),
            )
            .map(|()| Effected::done())
            .map_err(|error| error.to_string());
        }
        InputEffect::ResetDictionary { revision } => {
            return ui::personal_dictionary::edit(revision, ui::personal_dictionary::Edit::Reset)
                .map(|()| Effected::done())
                .map_err(|error| error.to_string());
        }
        // Not a keyboard-source edit: no keyboard scope applies to it.
        InputEffect::SetPasteShortcut { app, shortcut } => {
            let settings = host.settings::<InputPrefs>();
            let mut input = settings.get();
            input.set_paste_shortcut(&app, &shortcut)?;
            return settings
                .set(&input)
                .map(|()| Effected::done())
                .map_err(|error| format!("the input settings could not be saved: {error}"));
        }
        _ => {}
    }
    if let Some(result) = keyboard_editor::answer_editor(app, &effect) {
        return result;
    }
    let settings = host.settings::<InputPrefs>();
    let mut input = settings.get();
    input.normalise_keyboard_sources();
    keyboard_editor::apply_scoped(&mut input, effect, app.input())?;
    input.normalise_keyboard_sources();
    settings
        .set(&input)
        .map(|()| Effected::done())
        .map_err(|error| format!("the input settings could not be saved: {error}"))
}

fn input_picker_snapshot(widget: &dyn Widget) -> Option<serde_json::Value> {
    if widget.test_id() == Some("input.source.add_picker") {
        let dropdown = widget.as_any()?.downcast_ref::<Dropdown>()?;
        let accessible = dropdown.accessibility();
        return Some(serde_json::json!({
            "selected": dropdown.selected(), "open": dropdown.is_open(),
            "filter": dropdown.filter_query(),
            "value": accessible.as_ref().and_then(|node| node.value.as_ref()),
            "name": accessible.as_ref().and_then(|node| node.name.as_ref()),
        }));
    }
    let mut result = None;
    widget.for_each_child(&mut |child| {
        if result.is_none() {
            result = input_picker_snapshot(child);
        }
    });
    result
}

/// Read the same bounded catalogue and shared selector the normal pane uses.
fn inspect_input_sources(
    app: &SettingsApp,
    query: Option<&str>,
    page: u32,
) -> Result<serde_json::Value, String> {
    let query = query.unwrap_or("");
    if query.len() > 128 {
        return Err("Input source query exceeds 128 bytes".into());
    }
    let query = query.to_lowercase();
    let prefs = app.host().settings::<InputPrefs>().get();
    let area = app.input();
    let matches: Vec<_> = area
        .catalog()
        .iter()
        .filter(|entry| {
            entry.id.to_lowercase().contains(&query) || entry.label.to_lowercase().contains(&query)
        })
        .collect();
    let total = matches.len();
    let pages = total.div_ceil(32).max(1);
    let page = usize::try_from(page).map_err(|_| "Input source page is out of range")?;
    if page >= pages {
        return Err("Input source page is out of range".into());
    }
    let sources: Vec<_> = matches.into_iter().skip(page * 32).take(32).map(|entry| {
        let configured = prefs.keyboard_layouts.contains(&entry.id);
        serde_json::json!({"id": entry.id, "label": entry.label, "configured": configured,
            "can_add": !configured && crate::input_sources::has_room_for(&prefs.keyboard_layouts, &entry.id)})
    }).collect();
    let add_picker = (app.selected() == crate::categories::Category::Keyboard)
        .then(|| input_picker_snapshot(app))
        .flatten();
    Ok(serde_json::json!({"active_source": prefs.keyboard_layout,
        "configured_sources": prefs.keyboard_layouts, "page": page, "pages": pages,
        "total": total, "sources": sources, "catalogue_live": area.live,
        "catalogue_error": area.catalog_error(), "add_picker": add_picker}))
}

/// How often the attached-device snapshot is read again when it cannot be
/// watched, so plugging a mouse in still shows its section without
/// reopening Settings.
const DEVICE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// What the input area keeps for the window: the installed input sources,
/// and the attached devices as the compositor last reported them.
pub struct InputArea {
    catalog: SourceCatalog,
    live: std::cell::Cell<bool>,
    devices: std::rc::Rc<RefCell<(Option<ui::time::Instant>, Option<InputHardwareSnapshot>)>>,
    /// The watch on the compositor's input snapshot, while a pane holds it.
    following: RefCell<std::rc::Weak<crate::watch::Watched>>,
    editor: ui::LocalState<keyboard_editor::EditorState>,
    osk_always: bool,
    document: ui::storage::SettingsDocument,
}

impl InputArea {
    /// The area of this device, or, for a capture, of sample sources and no
    /// devices reported.
    pub fn new(cx: &PaneCx) -> Self {
        let live = cx.reaches_devices();
        Self {
            catalog: if live {
                SourceCatalog::of_device()
            } else {
                SourceCatalog::ready(sample_catalog(), None)
            },
            live: std::cell::Cell::new(live),
            devices: std::rc::Rc::new(RefCell::new((
                None,
                cx.scene().and_then(|scene| scene.input_hardware.clone()),
            ))),
            following: RefCell::new(std::rc::Weak::new()),
            editor: ui::LocalState::new(keyboard_editor::EditorState {
                view: if cx.scene().is_some() {
                    keyboard_editor::SourceView::Configured
                } else {
                    keyboard_editor::SourceView::Native
                },
                ..Default::default()
            }),
            document: cx.storage().settings_document().clone(),
            osk_always: cx
                .settings::<crate::accessibility_settings::AccessibilitySettings>()
                .get()
                .on_screen_keyboard
                == tilo_accessibility_settings::OnScreenKeyboard::Always,
        }
    }

    fn catalog(&self) -> &[CatalogEntry] {
        self.catalog.entries()
    }

    /// Why the device's layout list could not be read; the catalogue is
    /// then a short built-in list and says so.
    fn catalog_error(&self) -> Option<&str> {
        self.catalog.error()
    }

    /// Read this device's input sources off the UI thread, unless they have
    /// been read: the window does so once its other areas have read, so the
    /// Mouse & Keyboard pane opens on a catalogue already in.
    pub(crate) fn prefetch_catalog(&self) {
        self.catalog.prefetch();
    }

    /// Whether this device's input sources are still being read off the
    /// UI thread, starting that read if none has: the pane, which lists
    /// them, waits for it rather than parsing the layout rules itself.
    pub(crate) fn catalog_pending(&self) -> bool {
        self.catalog.pending()
    }

    /// The watch on the compositor's input snapshot for a pane that shows
    /// the attached devices, shared by every such pane; `None` for a
    /// capture's sample devices.
    fn follow(&self) -> Option<std::rc::Rc<crate::watch::Watched>> {
        if !self.live.get() {
            return None;
        }
        let snapshot: Vec<std::path::PathBuf> = settings_store::hardware::input_snapshot_path()
            .into_iter()
            .collect();
        Some(crate::watch::shared(
            &mut self.following.borrow_mut(),
            || crate::watch::Watched::files(&snapshot),
        ))
    }

    /// The attached devices, read again at most once per
    /// [`DEVICE_POLL_INTERVAL`]; `None` without a compositor snapshot.
    fn devices(&self) -> Option<InputHardwareSnapshot> {
        if !self.live.get() {
            return self.devices.borrow().1.clone();
        }
        let mut devices = self.devices.borrow_mut();
        let now = ui::time::now();
        if devices.0.is_none_or(|next| now >= next) {
            *devices = (
                Some(now + DEVICE_POLL_INTERVAL),
                InputHardwareSnapshot::load(),
            );
        }
        devices.1.clone()
    }

    /// One live row per attached drawing tablet and pad, by device id.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let inspection = LiveRow::new(
            "settings.keyboard-map",
            "settings.keyboard-map",
            t("input-map-title"),
        )
        .operation(crate::effects::SettingsEffect::from(InputEffect::InspectKeyboardMap).bound());
        let Some(devices) = self.devices() else {
            return vec![inspection];
        };
        let row = |device: &InputDeviceSnapshot, id: String, kind: &str| {
            LiveRow::new(id, kind, device_title(device)).presentation(ResourcePresentation {
                detail: Some(device.key.clone()),
                ..ResourcePresentation::default()
            })
        };
        std::iter::once(inspection)
            .chain(
                devices
                    .of_kind(InputDeviceKind::Tablet)
                    .into_iter()
                    .map(|device| row(device, tablet_resource_id(&device.key), "settings.tablet"))
                    .chain(
                        devices
                            .of_kind(InputDeviceKind::TabletPad)
                            .into_iter()
                            .map(|device| {
                                row(device, pad_resource_id(&device.key), "settings.tablet-pad")
                            }),
                    ),
            )
            .collect()
    }
}

/// The input sources an area offers and, when this device's layout list
/// could not be read, why. A device's are read once, when first asked for
/// or off the UI thread ([`InputArea::prefetch_catalog`]) — never while the
/// window opens, which shows no input source on its first page.
pub(crate) struct SourceCatalog {
    read: std::cell::OnceCell<(Vec<CatalogEntry>, Option<String>)>,
    background: crate::device_read::DeviceRead<(Vec<CatalogEntry>, Option<String>)>,
}

impl SourceCatalog {
    /// This device's input sources, read when first asked for.
    fn of_device() -> Self {
        Self {
            read: std::cell::OnceCell::new(),
            background: crate::device_read::DeviceRead::default(),
        }
    }

    /// `entries`, already read; `error` says why they stand in for the
    /// device's own list.
    pub(crate) fn ready(entries: Vec<CatalogEntry>, error: Option<String>) -> Self {
        let read = std::cell::OnceCell::new();
        let _ = read.set((entries, error));
        Self {
            read,
            background: crate::device_read::DeviceRead::default(),
        }
    }

    fn read(&self) -> &(Vec<CatalogEntry>, Option<String>) {
        self.read.get_or_init(|| {
            self.background
                .take()
                .unwrap_or_else(crate::input_sources::read_installed)
        })
    }

    pub(crate) fn entries(&self) -> &[CatalogEntry] {
        &self.read().0
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.read().1.as_deref()
    }

    /// Whether the catalogue is being read off the UI thread, started here
    /// if it has not been read. `false` once it is read, or when the host
    /// runs no background work and it is read when asked.
    fn pending(&self) -> bool {
        if self.read.get().is_some() {
            return false;
        }
        self.prefetch();
        !self.background.is_finished()
    }

    /// Start reading the device's catalogue off the UI thread, unless it
    /// has been read or is being read.
    fn prefetch(&self) {
        if self.read.get().is_none() {
            let _ = self.background.start(crate::input_sources::read_installed);
        }
    }
}

/// The input sources a capture offers.
fn sample_catalog() -> Vec<CatalogEntry> {
    [
        ("us", "English (US)"),
        ("gb", "English (UK)"),
        ("de", "German"),
        ("fr", "French"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("nl", "Dutch"),
        ("pt", "Portuguese"),
        ("se", "Swedish"),
        ("jp", "Japanese"),
    ]
    .map(|(id, label)| CatalogEntry {
        id: id.into(),
        label: label.into(),
    })
    .to_vec()
}

type DictionaryKey = (String, Option<String>);
type DictionaryFields = (ui::LocalState<String>, ui::LocalState<String>);

/// Ask the Keyboard page to show what the focused seat really types.
#[cfg(test)]
pub(crate) fn show_live_map(area: &InputArea) {
    let mut editor = area.editor.get();
    editor.view = keyboard_editor::SourceView::Live;
    area.editor.set(editor);
}

pub(crate) struct KeyboardPreviewCache {
    source: String,
    model: String,
    options: Option<String>,
    map: ui::KeyboardMap,
}

pub(crate) type KeyboardPreviewCacheHandle =
    std::rc::Rc<std::cell::RefCell<Option<KeyboardPreviewCache>>>;

/// Which of the three pages the pane shows: the keyboard (layouts and
/// typing), the pointing devices, or spelling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPage {
    #[default]
    Keyboard,
    Pointing,
    Spelling,
}

pub struct MouseKeyboardPane {
    page: InputPage,
    cx: PaneCx,
    input: Followed<InputPrefs>,
    text: Followed<TextAssistancePrefs>,
    dictionary: Option<ui::personal_dictionary::Snapshot>,
    dictionary_identity: Option<[u8; 32]>,
    dictionary_word: ui::LocalState<String>,
    dictionary_language: ui::LocalState<String>,
    dictionary_edits: RefCell<HashMap<DictionaryKey, DictionaryFields>>,
    /// The app id typed into the paste-shortcut card's field.
    paste_app: ui::LocalState<String>,
    /// The input section as shown: normalised.
    last: InputPrefs,
    catalog: Vec<CatalogEntry>,
    /// Why the device's layout list could not be read; `catalog` is then a
    /// short built-in list and says so.
    catalog_error: Option<String>,
    /// The device's layout list is still being read: the page shows what
    /// it holds, and only the chooser of layouts to add waits for it.
    catalog_pending: bool,
    /// Which sections the attached devices call for.
    sections: InputSections,
    /// Attached drawing tablets and pads, one card each.
    tablets: Vec<InputDeviceSnapshot>,
    pads: Vec<InputDeviceSnapshot>,
    composed: ui::widget::Composed,
    composed_text_growth_bits: u32,
    source_row_height: f32,
    composed_locale_revision: u64,
    map_cache: KeyboardPreviewCacheHandle,
    osk_always: bool,
    /// When the attached devices are read again if their snapshot is not
    /// watched.
    next_device_poll: Option<ui::time::Instant>,
    /// The watch that says when the attached devices changed.
    devices_watch: Option<std::rc::Rc<crate::watch::Watched>>,
    hardware_live: bool,
    hardware: std::rc::Rc<RefCell<(Option<ui::time::Instant>, Option<InputHardwareSnapshot>)>>,
    map_input: ui::KeyboardMapInput,
    editor: ui::LocalState<keyboard_editor::EditorState>,
    last_editor: keyboard_editor::EditorState,
    scopes: Vec<(String, String)>,
}

impl MouseKeyboardPane {
    /// The pane over `cx`'s input settings and `area`'s sources and devices.
    pub fn new(cx: PaneCx, area: &InputArea) -> Self {
        let input = cx.follow::<InputPrefs>();
        let text = cx.follow::<TextAssistancePrefs>();
        let mut last = input.get().clone();
        last.normalise_keyboard_sources();
        let mut pane = Self {
            page: InputPage::default(),
            cx,
            input,
            text,
            dictionary: ui::personal_dictionary::snapshot().ok(),
            dictionary_identity: ui::local_identity::bound()
                .map(|identity| identity.group_public_key),
            dictionary_word: ui::LocalState::new(String::new()),
            dictionary_language: ui::LocalState::new(String::new()),
            dictionary_edits: RefCell::new(HashMap::new()),
            paste_app: ui::LocalState::new(String::new()),
            last,
            catalog_pending: area.catalog_pending(),
            catalog: Vec::new(),
            catalog_error: None,
            sections: InputSections::for_devices(None),
            tablets: Vec::new(),
            pads: Vec::new(),
            composed: ui::widget::Composed::default(),
            composed_text_growth_bits: 1.0_f32.to_bits(),
            source_row_height: ui::tokens::Metrics::default().panel_row_height,
            composed_locale_revision: ui::i18n::I18nManager::revision(),
            map_cache: std::rc::Rc::default(),
            osk_always: area.osk_always,
            next_device_poll: None,
            devices_watch: area.follow(),
            hardware_live: area.live.get(),
            hardware: area.devices.clone(),
            map_input: ui::KeyboardMapInput::default(),
            editor: area.editor.clone(),
            last_editor: area.editor.get(),
            scopes: area.keyboard_scopes(),
        };
        if !pane.catalog_pending {
            pane.catalog = area.catalog().to_vec();
            pane.catalog_error = area.catalog_error().map(str::to_owned);
        }
        if pane.scopes.len() == 1 {
            let mut editor = pane.editor.get();
            editor.scope = pane
                .scopes
                .first()
                .map_or_else(|| "all".into(), |(id, _)| id.clone());
            pane.editor.set(editor);
        }
        pane.observe_devices(area.devices().as_ref());
        pane
    }

    /// Show one page of the input settings.
    pub(crate) fn with_page(mut self, page: InputPage) -> Self {
        self.page = page;
        self
    }

    /// Share only ephemeral physical positions with the owning Settings page.
    pub(crate) fn with_map_input(mut self, input: ui::KeyboardMapInput) -> Self {
        self.map_input = input;
        self
    }

    /// Retain one compiled preview while search reconstructs the page widgets.
    pub(crate) fn with_map_cache(mut self, cache: KeyboardPreviewCacheHandle) -> Self {
        self.map_cache = cache;
        self
    }

    fn keyboard_preview(&self, source: &str) -> Result<ui::KeyboardMap, String> {
        let input = self.preview_input(source);
        let options = settings_store::hardware::RichInputConfig {
            compose_key: input.compose_key.clone(),
            caps_lock: input.caps_lock.clone(),
            ..Default::default()
        }
        .xkb_options()
        .join(",");
        let options = Some(options);
        let mut cache = self.map_cache.borrow_mut();
        if let Some(cache) = cache.as_ref()
            && cache.source == source
            && cache.model == input.keyboard_model
            && cache.options == options
        {
            return Ok(cache.map.clone());
        }
        let map = crate::keyboard_preview::map_for_form(
            source,
            &input.keyboard_model,
            t("input-map-title"),
            options.as_deref(),
            crate::keyboard_preview::device_keyboard_is_compact(),
        )?
        .with_input(self.map_input.clone());
        *cache = Some(KeyboardPreviewCache {
            source: source.to_owned(),
            model: input.keyboard_model,
            options,
            map: map.clone(),
        });
        Ok(map)
    }

    fn source_label(&self, id: &str) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.label.clone())
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        match self.page {
            InputPage::Keyboard => {
                let mut page = self.keyboard_page();
                page.push(self.paste_shortcut_card());
                crate::widgets::page(page)
            }
            InputPage::Spelling => {
                crate::widgets::page(vec![self.spell_checker_card(), self.dictionary_card()])
            }
            InputPage::Pointing => {
                let mut cards = Vec::new();
                if self.sections.touchpad {
                    cards.push(self.touchpad_card());
                    cards.push(self.touchpad_click_scroll_card());
                }
                if self.sections.mouse {
                    cards.push(self.mouse_card());
                }
                if self.sections.pointer {
                    cards.push(self.pointer_options_card());
                }
                if self.sections.tablet {
                    cards.extend(self.tablet_cards());
                    cards.extend(self.pad_cards());
                }
                crate::widgets::page(cards)
            }
        }
    }

    /// Whether to read the attached devices again: on the first sync, when
    /// their watch says the compositor's snapshot changed, or, when it
    /// cannot be watched, once per [`DEVICE_POLL_INTERVAL`].
    fn devices_due(&mut self) -> bool {
        let now = ui::time::now();
        let first = self.next_device_poll.is_none();
        let changed = match &self.devices_watch {
            Some(watch) if watch.is_complete() => watch.take(),
            _ => self
                .next_device_poll
                .is_some_and(|deadline| now >= deadline),
        };
        if first || changed {
            self.next_device_poll = Some(now + DEVICE_POLL_INTERVAL);
        }
        first || changed
    }

    /// Take in which devices are attached; rebuild when that changes what
    /// the page shows.
    fn observe_devices(&mut self, snapshot: Option<&InputHardwareSnapshot>) {
        let scopes = keyboard_editor::scopes(snapshot, self.osk_always);
        if scopes != self.scopes {
            self.scopes = scopes;
            let mut editor = self.editor.get();
            if !self.scopes.iter().any(|(id, _)| id == &editor.scope) {
                editor.scope = self
                    .scopes
                    .first()
                    .map_or_else(|| "all".into(), |(id, _)| id.clone());
                editor.selected = None;
                editor.adding = false;
                editor.pending = None;
                self.editor.set(editor);
            }
            self.composed.clear();
            self.composed.invalidate();
        }
        let sections = InputSections::for_devices(snapshot);
        let owned = |kind| {
            snapshot
                .map(|snapshot| snapshot.of_kind(kind).into_iter().cloned().collect())
                .unwrap_or_default()
        };
        let tablets: Vec<InputDeviceSnapshot> = owned(InputDeviceKind::Tablet);
        let pads: Vec<InputDeviceSnapshot> = owned(InputDeviceKind::TabletPad);
        if sections != self.sections || tablets != self.tablets || pads != self.pads {
            self.sections = sections;
            self.tablets = tablets;
            self.pads = pads;
            self.composed.invalidate();
            self.composed.clear();
        }
    }

    fn field_row(
        &self,
        label: &str,
        description: &str,
        field: tilo_resource::Path<InputPrefs, bool>,
        id: &str,
        on: bool,
    ) -> Item {
        self.cx
            .row(t(label))
            .described(t(description))
            .id(id)
            .toggle(&ui::LocalState::new(on), field)
    }

    /// A dropdown over stable ids for one of a tablet's or pad's choices.
    fn device_dropdown(
        &self,
        resource: &str,
        device: &str,
        action: &str,
        choices: Vec<(String, String)>,
        selected: &str,
        accessible_name: &str,
    ) -> Dropdown {
        let dropdown = id_dropdown(
            choices,
            selected,
            &format!("{resource}.{action}"),
            accessible_name,
        );
        let device = device.to_owned();
        match action {
            "map-to" => self.cx.bind(
                dropdown,
                effect(InputEffect::SET_TABLET_OUTPUT)
                    .map(move |output: String| (device.clone(), output)),
            ),
            "lower-button" | "upper-button" | "third-button" => {
                let button = action.trim_end_matches("-button").to_owned();
                self.cx.bind(
                    dropdown,
                    effect(InputEffect::SET_TABLET_BUTTON)
                        .map(move |choice: String| (device.clone(), button.clone(), choice)),
                )
            }
            "eraser" => self.cx.bind(
                dropdown,
                effect(InputEffect::SET_TABLET_ERASER)
                    .map(move |choice: String| (device.clone(), choice)),
            ),
            other => {
                if let Some(button) = other
                    .strip_prefix("button-")
                    .and_then(|button| button.parse::<u32>().ok())
                {
                    return self.cx.bind(
                        dropdown,
                        effect(InputEffect::SET_PAD_BUTTON)
                            .map(move |choice: String| (device.clone(), button, choice)),
                    );
                }
                match parse_dial_action_id(other) {
                    Some((dial, index, mode)) => self.cx.bind(
                        dropdown,
                        effect(InputEffect::SET_PAD_DIAL).map(move |choice: String| {
                            (device.clone(), dial.to_owned(), index, mode, choice)
                        }),
                    ),
                    None => dropdown.with_enabled(false),
                }
            }
        }
    }

    fn device_toggle(&self, resource: &str, device: &str, action: &str, on: bool) -> Toggle {
        let toggle =
            Toggle::from_state(&ui::LocalState::new(on)).with_id(&format!("{resource}.{action}"));
        let device = device.to_owned();
        match action {
            "keep-aspect" => self.cx.bind(
                toggle,
                effect(InputEffect::SET_TABLET_KEEP_ASPECT)
                    .map(move |on: bool| (device.clone(), on)),
            ),
            _ => self.cx.bind(
                toggle,
                effect(InputEffect::SET_TABLET_LEFT_HANDED)
                    .map(move |on: bool| (device.clone(), on)),
            ),
        }
    }

    fn device_slider(&self, resource: &str, device: &str, action: &str, value: f64) -> Slider {
        let device = device.to_owned();
        self.cx.bind(
            Slider::from_state(&ui::LocalState::new(value))
                .with_id(&format!("{resource}.{action}")),
            effect(InputEffect::SET_TABLET_TIP_FEEL).map(move |feel: f64| (device.clone(), feel)),
        )
    }

    /// Everything that is specific to a touchpad, starting with whether it
    /// works at all. The touchpad key on a laptop keyboard flips the same
    /// saved setting.
    fn touchpad_card(&self) -> crate::widgets::SettingsSection {
        let last = &self.last;
        section(
            &t("input-touchpad-title"),
            rows(vec![
                self.field_row(
                    "input-touchpad-enabled",
                    "input-touchpad-enabled-description",
                    InputPrefs::paths().touchpad_enabled,
                    "input.touchpad_enabled",
                    last.touchpad_enabled,
                ),
                self.field_row(
                    "input-touchpad-disable-with-mouse",
                    "input-touchpad-disable-with-mouse-description",
                    InputPrefs::paths().touchpad_disabled_on_external_mouse,
                    "input.touchpad_disabled_on_external_mouse",
                    last.touchpad_disabled_on_external_mouse,
                ),
                self.cx
                    .row(t("input-touchpad-speed"))
                    .id("input.touchpad_pointer_speed")
                    .named("input-touchpad-speed")
                    .slider_with(
                        &ui::LocalState::new(
                            last.touchpad_pointer_speed.unwrap_or(last.pointer_speed),
                        ),
                        InputPrefs::paths().touchpad_pointer_speed,
                        |speed| fraction(speed).map(Some),
                    ),
                self.field_row(
                    "input-touchpad-natural-scroll",
                    "input-touchpad-natural-scroll-description",
                    InputPrefs::paths().touchpad_natural_scroll,
                    "input.touchpad_natural_scroll",
                    last.touchpad_natural_scroll,
                ),
                self.field_row(
                    "input-tap-to-click",
                    "input-tap-to-click-description",
                    InputPrefs::paths().tap_to_click,
                    "input.tap_click",
                    last.tap_to_click,
                ),
                self.field_row(
                    "input-tap-drag",
                    "input-tap-drag-description",
                    InputPrefs::paths().tap_drag,
                    "input.tap_drag",
                    last.tap_drag,
                ),
                self.field_row(
                    "input-tap-drag-lock",
                    "input-tap-drag-lock-description",
                    InputPrefs::paths().tap_drag_lock,
                    "input.tap_drag_lock",
                    last.tap_drag_lock,
                ),
                self.field_row(
                    "input-disable-while-typing",
                    "input-disable-while-typing-description",
                    InputPrefs::paths().disable_while_typing,
                    "input.disable_while_typing",
                    last.disable_while_typing,
                ),
                self.field_row(
                    "input-touchpad-gestures",
                    "input-touchpad-gestures-description",
                    InputPrefs::paths().touchpad_gestures,
                    "input.touchpad_gestures",
                    last.touchpad_gestures,
                ),
            ]),
        )
    }

    /// How a touchpad turns presses and finger movement into clicks and
    /// scrolling. Each choice is applied only to devices whose libinput
    /// driver offers it.
    fn touchpad_click_scroll_card(&self) -> crate::widgets::SettingsSection {
        section(
            &t("input-clicking-title"),
            rows(vec![
                self.cx
                    .row(t("input-click-method"))
                    .described(t("input-click-method-caption"))
                    .id("input.touchpad_click_method")
                    .keyed_choice(
                        localized_choices(&CLICK_METHOD_CHOICES),
                        click_method_id(self.last.touchpad_click_method),
                        InputPrefs::paths().touchpad_click_method,
                        |id: String| {
                            click_method_for_id(&id)
                                .ok_or_else(|| format!("`{id}` is not a click method"))
                        },
                    ),
                self.cx
                    .row(t("input-scroll-method"))
                    .id("input.touchpad_scroll_method")
                    .keyed_choice(
                        localized_choices(&SCROLL_METHOD_CHOICES),
                        scroll_method_id(self.last.touchpad_scroll_method),
                        InputPrefs::paths().touchpad_scroll_method,
                        |id: String| {
                            scroll_method_for_id(&id)
                                .ok_or_else(|| format!("`{id}` is not a scroll method"))
                        },
                    ),
            ]),
        )
    }

    /// Mice, trackballs and pointing sticks.
    fn mouse_card(&self) -> crate::widgets::SettingsSection {
        let last = &self.last;
        section(
            &t("input-mouse-title"),
            rows(vec![
                self.cx
                    .row(t("input-mouse-speed"))
                    .id("input.mouse_pointer_speed")
                    .named("input-mouse-speed")
                    .slider_with(
                        &ui::LocalState::new(
                            last.mouse_pointer_speed.unwrap_or(last.pointer_speed),
                        ),
                        InputPrefs::paths().mouse_pointer_speed,
                        |speed| fraction(speed).map(Some),
                    ),
                self.field_row(
                    "input-mouse-natural-scroll",
                    "input-mouse-natural-scroll-description",
                    InputPrefs::paths().mouse_natural_scroll,
                    "input.mouse_natural_scroll",
                    last.mouse_natural_scroll,
                ),
            ]),
        )
    }

    /// Choices shared by every pointer, touchpad or mouse. Less-common
    /// choices still get a specific destination instead of an opaque
    /// "Advanced" bucket: people and assistive agents can tell what will be
    /// changed before opening the card.
    fn pointer_options_card(&self) -> crate::widgets::SettingsSection {
        section(
            &t("input-pointer-title"),
            rows(vec![
                self.field_row(
                    "input-left-handed",
                    "input-left-handed-description",
                    InputPrefs::paths().left_handed,
                    "input.left_handed",
                    self.last.left_handed,
                ),
                self.field_row(
                    "input-middle-emulation",
                    "input-middle-emulation-description",
                    InputPrefs::paths().middle_emulation,
                    "input.middle_emulation",
                    self.last.middle_emulation,
                ),
                self.cx
                    .row(t("input-acceleration"))
                    .id("input.accel_profile")
                    .choice(
                        accel_options(),
                        &ui::LocalState::new(accel_shown(&self.last.accel_profile)),
                        InputPrefs::paths().accel_profile,
                        |shown: String| {
                            accel_from_label(&shown)
                                .map(str::to_owned)
                                .ok_or_else(|| format!("`{shown}` is not an acceleration profile"))
                        },
                    ),
                self.cx
                    .row(t("input-scroll-speed"))
                    .described(t("input-scroll-speed-description"))
                    .id("input.scroll_speed")
                    .named("input-scroll-speed")
                    .slider_with(
                        &ui::LocalState::new(settings_store::hardware::scroll_speed_to_slider(
                            self.last.scroll_speed,
                        )),
                        InputPrefs::paths().scroll_speed,
                        |position| {
                            fraction(position)
                                .map(settings_store::hardware::scroll_speed_from_slider)
                        },
                    ),
            ]),
        )
    }

    /// The key repeat row: how soon and how fast a held key repeats.
    fn key_repeat_row(&self) -> Item {
        self.cx
            .row(t("input-key-repeat"))
            .described(self.key_repeat_description())
            .id("input.key_repeat")
            .named(t("input-key-repeat"))
            .slider_verb(
                &ui::LocalState::new(self.key_repeat_slider_value()),
                InputPrefs::SET_KEY_REPEAT,
            )
    }

    /// The normal slider owns one linked speed/delay curve. Imported desktop
    /// settings retain their exact physical pair until changed, so render the
    /// imported rate rather than an unrelated legacy slider fraction.
    fn key_repeat_slider_value(&self) -> f64 {
        let Some((_, rate)) = self
            .last
            .key_repeat_delay_ms
            .zip(self.last.key_repeat_rate_hz)
        else {
            return self.last.key_repeat;
        };
        let min = settings_store::hardware::KEY_REPEAT_SLIDER_MIN_HZ;
        let max = settings_store::hardware::KEY_REPEAT_SLIDER_MAX_HZ;
        (f64::from(rate.saturating_sub(min)) / f64::from(max - min)).clamp(0.0, 1.0)
    }

    fn key_repeat_description(&self) -> String {
        match (self.last.key_repeat_delay_ms, self.last.key_repeat_rate_hz) {
            (Some(_), Some(0)) => t("input-key-repeat-off"),
            (Some(delay), Some(rate)) => t_args(
                "input-key-repeat-rate",
                &[
                    ("rate", ArgValue::from(i64::from(rate))),
                    ("delay", ArgValue::from(i64::from(delay))),
                ],
            ),
            _ => t("input-key-repeat-faster"),
        }
    }

    fn spell_checker_card(&self) -> crate::widgets::SettingsSection {
        section(
            &t("input-spell-title"),
            rows(vec![
                self.cx
                    .row(t("input-spell-check"))
                    .described(t("input-spell-check-desc"))
                    .id("text.check_spelling")
                    .toggle(
                        &ui::LocalState::new(self.text.get().check_spelling),
                        TextAssistancePrefs::paths().check_spelling,
                    ),
                self.cx
                    .row(t("input-spell-auto"))
                    .described(t("input-spell-auto-desc"))
                    .id("text.auto_correct")
                    .toggle(
                        &ui::LocalState::new(self.text.get().auto_correct),
                        TextAssistancePrefs::paths().auto_correct,
                    ),
                self.cx
                    .row(t("input-spell-language"))
                    .id("text.spelling_language")
                    .keyed_choice(
                        spelling_language_choices(),
                        &self.text.get().spelling_language,
                        TextAssistancePrefs::paths().spelling_language,
                        |language: String| {
                            if SPELLING_LANGUAGES.iter().any(|(id, _)| *id == language) {
                                Ok(language)
                            } else {
                                Err(format!(
                                    "`{language}` is not a spelling language Settings offers"
                                ))
                            }
                        },
                    ),
                crate::widgets::note(&t("input-spell-caption")),
            ]),
        )
    }

    fn dictionary_card(&self) -> crate::widgets::SettingsSection {
        let Some(snapshot) = &self.dictionary else {
            return section(
                &t("input-dictionary-title"),
                rows(vec![crate::widgets::note(&t(
                    "input-dictionary-unavailable",
                ))]),
            );
        };
        let revision = snapshot.revision;
        let word = self.dictionary_word.clone();
        let language = self.dictionary_language.clone();
        let add = self.cx.bind(
            Button::new("dictionary.add", &t("input-dictionary-add")),
            effect(InputEffect::ADD_DICTIONARY_WORD)
                .map(move |()| (word.get(), language.get(), revision)),
        );
        let mut children = vec![
            crate::widgets::note(&t("input-dictionary-caption")),
            field_row(
                &t("input-dictionary-word"),
                Box::new(
                    TextInput::from_state(&self.dictionary_word)
                        .with_id("dictionary.word")
                        .with_accessible_name(t("input-dictionary-word"))
                        .with_placeholder(t("input-dictionary-word-placeholder"))
                        .with_spellcheck(false)
                        .with_autocorrect(false),
                ),
            ),
            crate::widgets::field_row_described(
                &t("input-dictionary-language"),
                &t("input-dictionary-language-caption"),
                Box::new(
                    TextInput::from_state(&self.dictionary_language)
                        .with_id("dictionary.language")
                        .with_accessible_name(t("input-dictionary-language"))
                        .with_placeholder(t("input-dictionary-all-languages"))
                        .with_spellcheck(false)
                        .with_autocorrect(false),
                ),
            ),
            crate::widgets::row(Box::new(add)),
        ];
        for (index, entry) in snapshot.words.iter().enumerate() {
            children.push(self.dictionary_word_row(index, entry, revision));
        }
        children.push(crate::widgets::row(Box::new(
            self.cx.bind_invocation(
                Button::new("dictionary.reset", &t("input-dictionary-reset"))
                    .with_enabled(!snapshot.words.is_empty()),
                InputEffect::ResetDictionary { revision },
            ),
        )));
        section(&t("input-dictionary-title"), rows(children))
    }

    /// Which shortcut pastes in an app when the shell pastes for the person
    /// (Super+V with an image or files): terminals as their app id suggests,
    /// and any app the person names here, so an unusual terminal is fixed
    /// without a code change.
    fn paste_shortcut_card(&self) -> crate::widgets::SettingsSection {
        let set = |id: &str, label: &str, shortcut: &'static str| {
            let app = self.paste_app.clone();
            self.cx.bind(
                Button::new(id, label),
                effect(InputEffect::SET_PASTE_SHORTCUT)
                    .map(move |()| (app.get(), shortcut.to_string())),
            )
        };
        let mut children = vec![
            crate::widgets::note(&t("input-paste-caption")),
            field_row(
                &t("input-paste-app"),
                Box::new(
                    TextInput::from_state(&self.paste_app)
                        .with_id("paste.app")
                        .with_accessible_name(t("input-paste-app"))
                        .with_placeholder(t("input-paste-app-placeholder"))
                        .with_spellcheck(false)
                        .with_autocorrect(false),
                ),
            ),
            crate::widgets::row(Box::new(
                Row::new()
                    .push(Box::new(set(
                        "paste.ctrl_shift_v",
                        &t("input-paste-ctrl-shift-v"),
                        "ctrl_shift_v",
                    )))
                    .push(Box::new(set(
                        "paste.ctrl_v",
                        &t("input-paste-ctrl-v"),
                        "ctrl_v",
                    ))),
            )),
        ];
        for (index, (app, shortcut)) in self.last.paste_shortcuts.iter().enumerate() {
            let shown = if shortcut == "ctrl_shift_v" {
                t("input-paste-ctrl-shift-v")
            } else {
                t("input-paste-ctrl-v")
            };
            children.push(setting_row(
                &t_args(
                    "input-paste-entry",
                    &[
                        ("app", ArgValue::from(app.as_str())),
                        ("shortcut", ArgValue::from(shown)),
                    ],
                ),
                Box::new(self.cx.bind_invocation(
                    Button::new(&format!("paste.{index}.remove"), &t("input-paste-remove")),
                    InputEffect::SetPasteShortcut {
                        app: app.clone(),
                        shortcut: "default".into(),
                    },
                )),
            ));
        }
        section(&t("input-paste-title"), rows(children))
    }

    fn dictionary_word_row(
        &self,
        index: usize,
        entry: &ui::personal_dictionary::Word,
        revision: u64,
    ) -> Item {
        let key = (entry.spelling.clone(), entry.language.clone());
        let (word, language) = self
            .dictionary_edits
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                (
                    ui::LocalState::new(entry.spelling.clone()),
                    ui::LocalState::new(entry.language.clone().unwrap_or_default()),
                )
            })
            .clone();
        let new_word = word.clone();
        let new_language = language.clone();
        let old_word = entry.spelling.clone();
        let old_language = entry.language.clone().unwrap_or_default();
        let save = self.cx.bind(
            Button::new(
                &format!("dictionary.{index}.save"),
                &t("input-dictionary-save"),
            ),
            effect(InputEffect::EDIT_DICTIONARY_WORD).map(move |()| {
                (
                    old_word.clone(),
                    old_language.clone(),
                    new_word.get(),
                    new_language.get(),
                    revision,
                )
            }),
        );
        let remove = self.cx.bind_invocation(
            Button::new(
                &format!("dictionary.{index}.remove"),
                &t("input-dictionary-remove"),
            ),
            InputEffect::RemoveDictionaryWord {
                word: entry.spelling.clone(),
                language: entry.language.clone().unwrap_or_default(),
                revision,
            },
        );
        // A word is edited in place: two fields and two actions on one line.
        crate::widgets::row(Box::new(
            Row::new()
                .push(Box::new(
                    TextInput::from_state(&word)
                        .with_id(&format!("dictionary.{index}.word"))
                        .with_accessible_name(t("input-dictionary-word"))
                        .with_spellcheck(false)
                        .with_autocorrect(false),
                ))
                .push(Box::new(
                    TextInput::from_state(&language)
                        .with_id(&format!("dictionary.{index}.language"))
                        .with_accessible_name(t("input-dictionary-language"))
                        .with_placeholder(t("input-dictionary-all-languages"))
                        .with_spellcheck(false)
                        .with_autocorrect(false),
                ))
                .push(Box::new(save))
                .push(Box::new(remove)),
        ))
    }

    fn tablet_config(
        &self,
        device: &InputDeviceSnapshot,
    ) -> settings_store::hardware::TabletConfig {
        self.last
            .tablets
            .get(&device.key)
            .cloned()
            .unwrap_or_default()
    }

    /// One card per attached drawing tablet. Settings are stored per device
    /// identity, so each tablet keeps its own mapping, feel and buttons.
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative row list mirroring the visible tablet card"
    )]
    fn tablet_cards(&self) -> Vec<crate::widgets::SettingsSection> {
        let outputs = tablet_output_choices();
        self.tablets
            .iter()
            .map(|device| {
                let config = self.tablet_config(device);
                let resource = tablet_resource_id(&device.key);
                let mapped = config
                    .output
                    .clone()
                    .filter(|output| outputs.iter().any(|(id, _)| id == output))
                    .unwrap_or_else(|| WHOLE_DESKTOP.to_string());
                section(
                    &device_title(device),
                    rows(vec![
                        setting_row(
                            &t("input-tablet-map-to"),
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                "map-to",
                                outputs.clone(),
                                &mapped,
                                &t("input-tablet-map-to"),
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-keep-aspect"),
                            &t("input-tablet-keep-aspect-description"),
                            Box::new(self.device_toggle(
                                &resource,
                                &device.key,
                                "keep-aspect",
                                config.keep_aspect,
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-left-handed"),
                            &t("input-tablet-left-handed-description"),
                            Box::new(self.device_toggle(
                                &resource,
                                &device.key,
                                "left-handed",
                                config.left_handed,
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-tip-feel"),
                            &t("input-tablet-tip-feel-description"),
                            crate::widgets::trailing_slider(Box::new(self.device_slider(
                                &resource,
                                &device.key,
                                "tip-feel",
                                settings_store::hardware::feel_for_pressure_curve(
                                    config.pressure_curve,
                                ),
                            ))),
                        ),
                        setting_row(
                            &t("input-tablet-lower-button"),
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                "lower-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.lower_button),
                                &t("input-tablet-lower-button"),
                            )),
                        ),
                        setting_row(
                            &t("input-tablet-upper-button"),
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                "upper-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.upper_button),
                                &t("input-tablet-upper-button"),
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-third-button"),
                            &t("input-tablet-third-button-description"),
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                "third-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.third_button),
                                &t("input-tablet-third-button"),
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-eraser"),
                            &t("input-tablet-eraser-description"),
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                "eraser",
                                eraser_choices(),
                                &eraser_id(config.eraser),
                                &t("input-tablet-eraser"),
                            )),
                        ),
                    ]),
                )
            })
            .collect()
    }

    /// One card per tablet pad: a shortcut for each of its buttons, then
    /// what each ring and strip does in each mode. Tablet-aware apps get
    /// the unmapped buttons, rings and strips themselves.
    fn pad_cards(&self) -> Vec<crate::widgets::SettingsSection> {
        self.pads
            .iter()
            .map(|device| {
                let config = self.tablet_config(device);
                let resource = pad_resource_id(&device.key);
                let mut items: Vec<Item> = (0..device.pad_buttons.min(MAX_PAD_BUTTONS))
                    .map(|button| {
                        let label = t_args(
                            "input-pad-button",
                            &[("number", ArgValue::from(i64::from(button) + 1))],
                        );
                        if device.pad_mode_buttons.contains(&button) {
                            return setting_row_described(
                                &label,
                                &t("input-pad-mode-button-description"),
                                Box::new(Spacer::new()),
                            );
                        }
                        setting_row(
                            &label,
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                &format!("button-{button}"),
                                pad_button_choices(),
                                &pad_button_id(&config, button),
                                &label,
                            )),
                        )
                    })
                    .collect();
                if items.is_empty() {
                    items.push(crate::widgets::note(&t("input-pad-no-buttons")));
                }
                for (dial, modes) in pad_dials(device) {
                    for mode in 0..modes {
                        let label = dial_label(dial, mode, modes);
                        items.push(setting_row(
                            &label,
                            Box::new(self.device_dropdown(
                                &resource,
                                &device.key,
                                &dial_action_id(dial, mode),
                                dial_choices(),
                                dial_id(config.dial_action(dial, mode)),
                                &label,
                            )),
                        ));
                    }
                }
                if !pad_dials(device).is_empty() {
                    items.push(crate::widgets::note(&t("input-pad-dials-caption")));
                }
                section(&device_title(device), rows(items))
            })
            .collect()
    }
}

impl Widget for MouseKeyboardPane {
    ui::forward_to_composed!(composed);

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.hardware_live && self.devices_due() {
            let snapshot = InputHardwareSnapshot::load();
            self.hardware.borrow_mut().1.clone_from(&snapshot);
            self.observe_devices(snapshot.as_ref());
        }
        if !self.hardware_live {
            let snapshot = self.hardware.borrow().1.clone();
            if let Some(snapshot) = snapshot {
                self.observe_devices(Some(&snapshot));
            }
        }
        let editor = self.editor.get();
        if editor != self.last_editor {
            self.last_editor = editor;
            self.composed.invalidate();
            self.composed.clear();
        }
        let identity = ui::local_identity::bound().map(|identity| identity.group_public_key);
        let dictionary = ui::personal_dictionary::snapshot().ok();
        if identity != self.dictionary_identity {
            self.dictionary_word = ui::LocalState::new(String::new());
            self.dictionary_language = ui::LocalState::new(String::new());
            self.dictionary_edits.borrow_mut().clear();
        }
        if dictionary != self.dictionary || identity != self.dictionary_identity {
            self.dictionary = dictionary;
            self.dictionary_identity = identity;
            self.dictionary_edits
                .borrow_mut()
                .retain(|(word, language), _| {
                    self.dictionary.as_ref().is_some_and(|snapshot| {
                        snapshot
                            .words
                            .iter()
                            .any(|entry| &entry.spelling == word && &entry.language == language)
                    })
                });
            self.composed.invalidate();
            self.composed.clear();
        }
        // The controls are built from the values: a change rebuilds them.
        if self.input.refresh() {
            let mut last = self.input.get().clone();
            last.normalise_keyboard_sources();
            self.last = last;
            self.composed.invalidate();
            self.composed.clear();
        }
        if self.text.refresh() {
            self.composed.invalidate();
            self.composed.clear();
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let growth_bits = theme.text_growth().to_bits();
        let source_row_height = (theme.text(ui::tokens::TextRole::Body).line_height_px()
            + theme.tokens.spacing.s2 * 2.0)
            .max(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s2);
        if self.source_row_height.to_bits() != source_row_height.to_bits() {
            self.source_row_height = source_row_height;
            self.composed.clear();
        }
        let locale_revision = ui::i18n::I18nManager::revision();
        if self.composed_text_growth_bits != growth_bits
            || self.composed_locale_revision != locale_revision
        {
            self.composed.clear();
            self.composed_text_growth_bits = growth_bits;
            self.composed_locale_revision = locale_revision;
        }
        // Layout is not a model change. Retain open/search/focus caches until
        // observed settings or device changes require new controls.
        let composition = self
            .composed
            .take()
            .unwrap_or_else(|| self.build_composition());
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        crate::panes::pane_node(taffy, child, node_map, self)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        match &self.devices_watch {
            Some(watch) if watch.is_complete() => None,
            _ => self.next_device_poll,
        }
    }

    fn name(&self) -> &'static str {
        "MouseKeyboardPane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::host::testing::{INPUT, TEXT_ASSISTANCE, edit_field};
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;
    use winit::event::WindowEvent;

    #[test]
    fn mounted_dictionary_actions_add_edit_remove_reset_and_survive_restart() {
        use settings_store::personal_dictionary::{Service, Vocabulary, host::Host};
        use std::sync::Arc;
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                ui::local_identity::install_source(None);
            }
        }
        let _guard = tilo_test_env::lock();
        let _restore = Restore;
        ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
            ui::local_identity::LocalIdentity {
                group_public_key: [1; 32],
                display_name: String::new(),
                avatar_color: [0; 3],
            },
        ))));
        let profile = tempfile::tempdir().unwrap();
        let service = Arc::new(Service::new(profile.path().join("dictionary")).unwrap());
        let source = Arc::new(Host::new(
            Arc::new(settings_store::HostDocument::new(
                serde_json::Map::new(),
                |_| Ok(()),
            )),
            service.clone(),
            Arc::new(|| Some([1; 32])),
        ));
        let mut app = TestApp::<SettingsShell>::for_person(&"01".repeat(32)).unwrap();
        let mut host =
            ui::personal_dictionary::TestHost::new(source, "so.tilo.settings", [1; 32]).unwrap();
        app.harness().resize((1200, 2200)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Spelling);
        app.settle();
        app.harness().click_named("dictionary.word").unwrap();
        app.harness().type_text("Tilo");
        app.harness().click_named("dictionary.add").unwrap();
        app.settle();
        host.flush().unwrap();
        app.settle();
        let reader = service.reader().unwrap();
        assert_eq!(
            reader.snapshot().unwrap().hints("en").collect::<Vec<_>>(),
            ["Tilo"]
        );
        app.effect(&SettingsEffect::Input(InputEffect::EditDictionaryWord {
            old_word: "Tilo".into(),
            old_language: String::new(),
            word: "TiLo".into(),
            language: "nl".into(),
            revision: 1,
        }))
        .unwrap();
        host.flush().unwrap();
        app.settle();
        assert_eq!(
            reader.snapshot().unwrap().hints("nl").collect::<Vec<_>>(),
            ["TiLo"]
        );
        let reopened = Service::new(profile.path().join("dictionary")).unwrap();
        assert_eq!(
            reopened.bind(Some([1; 32])).unwrap().snapshot().unwrap(),
            reader.snapshot().unwrap()
        );
        if let Some(path) = std::env::var_os("TILO_DICTIONARY_LIVE_SCREENSHOT") {
            app.harness().render_png(std::path::Path::new(&path));
        }
        app.harness().click_named("dictionary.0.remove").unwrap();
        app.settle();
        host.flush().unwrap();
        app.settle();
        assert!(reader.snapshot().unwrap().words.is_empty());
        app.effect(&SettingsEffect::Input(InputEffect::AddDictionaryWord {
            word: "Élodie".into(),
            language: "fr".into(),
            revision: 3,
        }))
        .unwrap();
        host.flush().unwrap();
        app.settle();
        app.harness().click_named("dictionary.reset").unwrap();
        app.settle();
        host.flush().unwrap();
        assert!(reader.snapshot().unwrap().words.is_empty());
        ui::local_identity::install_source(Some(Arc::new(ui::local_identity::NoLocalIdentity)));
        app.settle();
        assert!(ui::personal_dictionary::snapshot().is_err());
        assert!(!app.harness().targets().contains_key("dictionary.word"));
        assert!(!app.harness().targets().contains_key("dictionary.0.word"));
    }

    #[test]
    fn personal_dictionary_editor_renders_preserved_words_and_clear_controls() {
        use settings_store::personal_dictionary::{Edit, Service, Vocabulary, Word};
        let profile = tempfile::tempdir().unwrap();
        let service = Service::new(profile.path().join("dictionary")).unwrap();
        let reader = service.bind(Some([1; 32])).unwrap();
        for (word, language) in [("Tilo", None), ("Élodie", Some("fr")), ("東京", Some("ja"))] {
            let revision = reader.snapshot().unwrap().revision;
            service
                .edit(
                    &reader,
                    revision,
                    Edit::Add(Word::new(word, language).unwrap()),
                )
                .unwrap();
        }
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = InputArea::new(&cx);
        let mut pane = MouseKeyboardPane::new(cx, &area);
        pane.dictionary = Some(reader.snapshot().unwrap());
        let frame = ui::headless::FrameHarness::new(
            ui::State::new(),
            crate::widgets::page(vec![pane.dictionary_card()]),
            (1100, 760),
        );
        for control in [
            "dictionary.word",
            "dictionary.language",
            "dictionary.add",
            "dictionary.0.save",
            "dictionary.0.remove",
            "dictionary.reset",
        ] {
            assert!(
                frame.targets().contains_key(control),
                "missing shared dictionary control: {control}"
            );
        }
        if let Some(path) = std::env::var_os("TILO_DICTIONARY_SCREENSHOT") {
            frame.render_png(std::path::Path::new(&path));
        }
    }

    fn picker_node(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("input.source.add_picker") {
            return Some(node);
        }
        node.children.iter().find_map(picker_node)
    }

    fn spelling_language_value(node: &ui::headless::InspectionNode) -> Option<&str> {
        if let Some(semantics) = &node.semantics
            && semantics.role == ui::a11y::Role::ComboBox
            && semantics.name.as_deref() == Some(t("input-spell-language").as_str())
        {
            return semantics.value.as_deref();
        }
        node.children.iter().find_map(spelling_language_value)
    }

    pub(super) fn show_add_source(app: &mut TestApp<SettingsShell>, viewport: (u32, u32)) {
        crate::effects::show_pane(app, crate::categories::Category::Keyboard);
        app.settle();
        // The layouts to add are read off the UI thread; the chooser offers
        // them once the read has landed.
        tilo_test_env::wait_until("the device's layouts are read", || {
            app.settle();
            app.with_widget::<crate::settings_app::SettingsApp, _>(|settings| {
                !settings.input().catalog_pending()
            }) == Some(true)
        });
        for _ in 0..5 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        for _ in 0..30 {
            app.settle();
            let rect = app
                .harness()
                .targets()
                .get("input.source.add_picker")
                .copied()
                .unwrap();
            if rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1) {
                return;
            }
            app.harness().send(&ui::synthetic_input::cursor_moved(
                f64::from(viewport.0) - 60.0,
                f64::from(viewport.1) / 2.0,
            ));
            app.harness().scroll(0.0, -3.0);
        }
        panic!(
            "the actual Settings input-source chooser must become visible by ordinary scrolling"
        );
    }

    #[test]
    fn dropdown_language_mounted_settings_flow_is_clear_and_keyboard_operable() {
        let mut failures = Vec::new();
        for (name, viewport, theme) in [
            ("light-wide", (1472, 920), Theme::light()),
            ("dark-narrow", (420, 640), Theme::dark().with_ui_scale(1.5)),
            (
                "contrast-narrow",
                (420, 640),
                Theme::high_contrast().with_ui_scale(1.5),
            ),
        ] {
            let mut app = TestApp::<SettingsShell>::new().unwrap();
            app.harness().resize(viewport).unwrap();
            app.harness().set_theme(theme);
            crate::effects::show_pane(&mut app, crate::categories::Category::Spelling);
            app.settle();
            assert_eq!(
                spelling_language_value(&app.harness().inspection().root),
                Some(t("input-spell-language-session").as_str()),
                "the valid empty stable value must show its human language label",
            );
            let active_before = app.settings::<InputPrefs>().get().keyboard_layout;
            show_add_source(&mut app, viewport);
            let snapshot = app.harness().inspection();
            let control = picker_node(&snapshot.root)
                .unwrap()
                .semantics
                .as_ref()
                .unwrap();
            if control.value.is_some() {
                failures.push(format!(
                    "{name}: uninstalled source looks selected: {:?}",
                    control.value
                ));
            }
            let out = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("dropdown-review")
                .join(name);
            app.harness().render_png(&out.join("closed.png"));
            app.harness()
                .click_named("input.source.add_picker")
                .unwrap();
            // The list opens on the next frame, as it does for a person.
            app.settle();
            app.harness().render_png(&out.join("open.png"));
            app.harness().type_text("zzzz");
            app.harness().render_png(&out.join("no-matches.png"));
            app.harness().key("Escape");
            app.settle();
            app.harness()
                .click_named("input.source.add_picker")
                .unwrap_or_else(|error| {
                    panic!(
                        "{name}: the chooser is still on the page after Escape closed its list \
                         ({error}); showing {:?}",
                        app.with_widget::<crate::settings_app::SettingsApp, _>(|settings| (
                            settings.selected(),
                            settings.sidebar_open()
                        ))
                    )
                });
            app.settle();
            app.harness().type_text("French");
            app.harness().render_png(&out.join("filtered.png"));
            app.harness().key("Return");
            app.settle();
            tilo_test_env::wait_until("normal typed AddSource completes", || {
                app.settle();
                app.settings::<InputPrefs>()
                    .get()
                    .keyboard_layouts
                    .contains(&"fr".into())
            });
            let prefs = app.settings::<InputPrefs>().get();
            assert_ne!(
                prefs.keyboard_layout, active_before,
                "adding a layout from the page types with it"
            );
            app.harness().render_png(&out.join("selected.png"));
            assert!(app.harness().component_contract_violations().is_empty());
            let audit = app.harness().native_app_audit();
            std::fs::write(
                out.join("native-audit.json"),
                serde_json::to_vec_pretty(&audit).unwrap(),
            )
            .unwrap();
            assert!(audit.is_certified(), "{name}: {audit:?}");
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    fn pane_with(input: &InputPrefs, catalog: Vec<CatalogEntry>) -> MouseKeyboardPane {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<InputPrefs>().set(input).unwrap();
        let area = InputArea {
            catalog: SourceCatalog::ready(catalog, None),
            live: std::cell::Cell::new(false),
            devices: std::rc::Rc::new(RefCell::new((None, None))),
            following: RefCell::new(std::rc::Weak::new()),
            editor: ui::LocalState::new(keyboard_editor::EditorState {
                view: if cx.scene().is_some() {
                    keyboard_editor::SourceView::Configured
                } else {
                    keyboard_editor::SourceView::Native
                },
                ..Default::default()
            }),
            osk_always: false,
            document: cx.storage().settings_document().clone(),
        };
        MouseKeyboardPane::new(cx, &area)
    }

    /// A shown pane reads the attached devices when the compositor's
    /// snapshot changes, never on a timer; only a snapshot that cannot be
    /// watched is read once a second.
    #[test]
    fn attached_devices_are_read_when_their_snapshot_changes_not_on_a_timer() {
        let long_ago = || {
            ui::time::now()
                .checked_sub(std::time::Duration::from_secs(5))
                .unwrap()
        };
        let mut pane = pane_with(&InputPrefs::default(), catalog());
        pane.hardware_live = true;
        let watch = std::rc::Rc::new(crate::watch::Watched::by_hand());
        pane.devices_watch = Some(std::rc::Rc::clone(&watch));
        assert!(pane.devices_due(), "the first sync reads them");
        assert!(!pane.devices_due());
        pane.next_device_poll = Some(long_ago());
        assert!(!pane.devices_due(), "an idle pane reads nothing on a timer");
        assert_eq!(pane.next_wake_deadline(), None, "nor sets a deadline");
        watch.touch();
        assert!(pane.devices_due(), "a changed snapshot is read");
        assert!(!pane.devices_due(), "once");

        let root = tempfile::tempdir().unwrap();
        pane.devices_watch = Some(std::rc::Rc::new(crate::watch::Watched::files(&[root
            .path()
            .join("absent")
            .join("input-state.json")])));
        assert!(pane.next_wake_deadline().is_some());
        pane.next_device_poll = Some(long_ago());
        assert!(
            pane.devices_due(),
            "an unwatchable snapshot is read each second"
        );
    }

    /// The area watches the file the compositor publishes the attached
    /// devices in, and every pane showing them shares that one watch.
    #[test]
    fn the_input_area_watches_the_compositors_input_snapshot() {
        let runtime = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let snapshot = settings_store::hardware::input_snapshot_path().unwrap();
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        let pane = pane_with(&InputPrefs::default(), catalog());
        let area = InputArea {
            catalog: SourceCatalog::ready(catalog(), None),
            live: std::cell::Cell::new(true),
            devices: std::rc::Rc::new(RefCell::new((None, None))),
            following: RefCell::new(std::rc::Weak::new()),
            editor: ui::LocalState::new(keyboard_editor::EditorState::default()),
            osk_always: false,
            document: pane.cx.storage().settings_document().clone(),
        };
        let watch = area.follow().expect("a live area follows its devices");
        assert!(watch.is_complete());
        assert!(std::rc::Rc::ptr_eq(&watch, &area.follow().unwrap()));
        assert!(watch.take(), "the first look reads the snapshot");
        InputHardwareSnapshot::default().save().unwrap();
        tilo_test_env::wait_until("a published snapshot to be flagged", || watch.take());
    }

    fn catalog() -> Vec<CatalogEntry> {
        vec![
            CatalogEntry {
                id: "us".into(),
                label: "English (US)".into(),
            },
            CatalogEntry {
                id: "de".into(),
                label: "German".into(),
            },
            CatalogEntry {
                id: "fr".into(),
                label: "French".into(),
            },
        ]
    }

    /// The pane lays out and imported repeat settings show the live rate or disabled.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "off, pass-through and clamped slider values are exact sentinels, not computed"
    )]
    fn input_pane_lays_out_and_shows_imported_repeat_settings() {
        // Case `builds_layout`.
        {
            let mut pane = pane_with(&InputPrefs::default(), catalog());
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }

        // Case `imported_repeat_settings_are_shown_as_the_live_rate_or_disabled`.
        {
            let disabled = pane_with(
                &InputPrefs {
                    key_repeat: 0.5,
                    key_repeat_delay_ms: Some(500),
                    key_repeat_rate_hz: Some(0),
                    ..Default::default()
                },
                catalog(),
            );
            assert_eq!(disabled.key_repeat_slider_value(), 0.0);
            assert_eq!(
                disabled.key_repeat_description(),
                "Repeat is off. Move the slider to turn it on."
            );

            let exact = pane_with(
                &InputPrefs {
                    key_repeat: 0.5,
                    key_repeat_delay_ms: Some(275),
                    key_repeat_rate_hz: Some(37),
                    ..Default::default()
                },
                catalog(),
            );
            assert!((exact.key_repeat_slider_value() - 0.675).abs() < f64::EPSILON);
            assert_eq!(
                exact.key_repeat_description(),
                "37 keys per second, starts after 275 ms"
            );

            let partial = pane_with(
                &InputPrefs {
                    key_repeat: 0.25,
                    key_repeat_rate_hz: Some(50),
                    ..Default::default()
                },
                catalog(),
            );
            assert_eq!(partial.key_repeat_slider_value(), 0.25);
            assert_eq!(
                partial.key_repeat_description(),
                "Faster repeat starts sooner"
            );

            let clamped = pane_with(
                &InputPrefs {
                    key_repeat_delay_ms: Some(200),
                    key_repeat_rate_hz: Some(1_000),
                    ..Default::default()
                },
                catalog(),
            );
            assert_eq!(clamped.key_repeat_slider_value(), 1.0);
        }
    }

    #[test]
    fn pointer_sections_follow_the_attached_devices() {
        let with = |kinds: &[InputDeviceKind]| InputHardwareSnapshot {
            devices: kinds
                .iter()
                .map(|kind| settings_store::hardware::InputDeviceSnapshot {
                    kind: *kind,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let everything = InputSections::for_devices(None);
        assert!(everything.touchpad && everything.mouse && everything.pointer);

        let desktop = InputSections::for_devices(Some(&with(&[
            InputDeviceKind::Mouse,
            InputDeviceKind::Keyboard,
        ])));
        assert!(!desktop.touchpad, "no touchpad, no touchpad settings");
        assert!(desktop.mouse && desktop.pointer);
        assert!(!desktop.tablet);

        let laptop = InputSections::for_devices(Some(&with(&[InputDeviceKind::Touchpad])));
        assert!(laptop.touchpad && laptop.pointer && !laptop.mouse);

        let keyboard_only = InputSections::for_devices(Some(&with(&[InputDeviceKind::Keyboard])));
        assert!(
            !keyboard_only.pointer,
            "buttons need a pointer to belong to"
        );

        let pad_only = InputSections::for_devices(Some(&with(&[InputDeviceKind::TabletPad])));
        assert!(pad_only.tablet);
    }

    #[test]
    fn tablet_and_pad_choices_travel_as_stable_ids() {
        let actions = [
            StylusButtonAction::Default,
            StylusButtonAction::Right,
            StylusButtonAction::Disabled,
            StylusButtonAction::Shortcut(ShortcutAction::Search),
        ];
        for action in actions {
            assert_eq!(
                stylus_button_for_id(&stylus_button_id(action)),
                Some(action)
            );
        }
        assert_eq!(
            stylus_button_id(StylusButtonAction::Shortcut(
                ShortcutAction::WorkspaceOverview
            )),
            "shortcut:workspace_overview"
        );
        assert_eq!(stylus_button_for_id("Right click"), None);
        let choices = stylus_button_choices();
        assert_eq!(choices[3], ("right".into(), "Right click".into()));
        assert!(
            choices.iter().any(|(id, _)| id == "shortcut:search"),
            "every listed shortcut can go on a pen button"
        );

        assert_eq!(pad_button_for_id("none"), Some(PadButtonChoice::Nothing));
        let mut pad = TabletConfig::default();
        set_pad_button(&mut pad, 2, PadButtonChoice::Shortcut(ShortcutAction::Lock));
        assert_eq!(
            pad_button_for_id(&pad_button_id(&pad, 2)),
            Some(PadButtonChoice::Shortcut(ShortcutAction::Lock))
        );
        set_pad_button(&mut pad, 2, PadButtonChoice::ShowMap);
        assert_eq!(pad_button_id(&pad, 2), "show-map");
        assert!(
            pad.pad_buttons.is_empty(),
            "the map button runs no shortcut"
        );
        set_pad_button(&mut pad, 0, PadButtonChoice::ShowMap);
        assert_eq!(pad.pad_map_button, Some(0), "one button shows the map");
        assert_eq!(pad_button_id(&pad, 2), "none");
        assert_eq!(pad_button_for_id("left"), None, "a pad runs shortcuts only");

        for id in ["native", "left", "back", "shortcut:search"] {
            let action = eraser_for_id(id).expect("an offered eraser choice");
            assert_eq!(eraser_id(action), id);
        }
        assert!(
            eraser_choices()
                .iter()
                .any(|(id, _)| id == "shortcut:search"),
            "every listed shortcut can go on the eraser"
        );
        assert_eq!(eraser_for_id("disabled"), None);
        for (id, _) in dial_choices() {
            let action = dial_for_id(&id).expect("an offered dial choice");
            assert_eq!(dial_id(action), id);
        }
        assert_eq!(dial_for_id("shortcut:search"), None);

        assert_eq!(tablet_output_for_id(WHOLE_DESKTOP), Some(None));
        assert_eq!(tablet_output_choices()[0].0, WHOLE_DESKTOP);
    }

    #[test]
    fn a_card_appears_for_each_attached_tablet_and_pad() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let device =
            |kind, name: &str, pad_buttons| settings_store::hardware::InputDeviceSnapshot {
                key: settings_store::hardware::input_device_key(0x056a, 0x0374, name),
                name: name.into(),
                kind,
                pad_buttons,
                ..Default::default()
            };
        let mut pad = device(InputDeviceKind::TabletPad, "Wacom Intuos S Pad", 4);
        pad.pad_rings = vec![4];
        pad.pad_strips = vec![1, 1];
        pad.pad_mode_buttons = vec![0];
        let snapshot = InputHardwareSnapshot {
            devices: vec![
                device(InputDeviceKind::Tablet, "Wacom Intuos S Pen", 0),
                pad,
                device(InputDeviceKind::Mouse, "Mouse", 0),
            ],
            ..Default::default()
        };
        let mut pane = pane_with(&InputPrefs::default(), catalog());
        pane.observe_devices(Some(&snapshot));
        assert!(pane.take_structure_dirty(), "new devices rebuild the page");
        assert_eq!(pane.tablets.len(), 1);
        assert_eq!(pane.pads.len(), 1);
        assert_eq!(device_title(&pane.tablets[0]), "Wacom Intuos S");
        assert_eq!(device_title(&pane.pads[0]), "Wacom Intuos S buttons");
        assert_eq!(pane.tablet_cards().len(), 1);
        assert_eq!(pane.pad_cards().len(), 1);
        assert_eq!(
            pad_dials(&pane.pads[0]),
            [
                (PadDial::Ring(0), 4),
                (PadDial::Strip(0), 1),
                (PadDial::Strip(1), 1)
            ],
            "a row per ring mode and per strip"
        );
        assert_eq!(dial_label(PadDial::Ring(0), 1, 4), "Ring 1, mode 2");
        assert_eq!(dial_label(PadDial::Strip(1), 0, 1), "Strip 2");
        pane.observe_devices(Some(&snapshot));
        assert!(
            !pane.take_structure_dirty(),
            "nothing changed, nothing rebuilt"
        );

        let mut theme_taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut theme_taffy, &Theme::dark(), &mut node_map);

        pane.observe_devices(Some(&InputHardwareSnapshot::default()));
        assert!(pane.take_structure_dirty(), "unplugging removes the cards");
        assert!(pane.tablets.is_empty() && pane.pads.is_empty());
    }

    #[test]
    fn touchpad_choices_travel_as_stable_ids() {
        for method in TouchpadClickMethod::ALL {
            assert_eq!(click_method_for_id(click_method_id(method)), Some(method));
        }
        for method in TouchpadScrollMethod::ALL {
            assert_eq!(scroll_method_for_id(scroll_method_id(method)), Some(method));
        }
        assert_eq!(click_method_for_id("Button areas"), None);
        let labels = localized_choices(&CLICK_METHOD_CHOICES);
        assert_eq!(labels[1], ("areas".into(), "Bottom-right corner".into()));
    }

    #[test]
    fn keyboard_preview_actions_are_published_and_do_not_apply_until_requested() {
        use ui::tilo_resource::Effect as _;
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let before = app.settings::<InputPrefs>().get();
        let preview = InputEffect::PreviewLayout {
            source: "de".into(),
        };
        let model = InputEffect::SetKeyboardModel {
            model: "pc104".into(),
        };
        let levels = InputEffect::SetPreviewExtraLevels { on: true };
        let effects = [
            preview.clone(),
            model.clone(),
            levels.clone(),
            InputEffect::SetPreviewLevel { level: "1".into() },
            InputEffect::ShowLiveKeyboardMap,
            InputEffect::ApplyPreview,
        ];
        let actions = app.actions().unwrap();
        for effect in &effects {
            assert!(
                actions
                    .iter()
                    .any(|action| action.address.action == effect.id()),
                "the actual Settings publisher must advertise {}",
                effect.id()
            );
        }
        assert!(set(&mut app, preview));
        assert!(set(&mut app, model));
        assert!(set(&mut app, levels));
        let inspected = app.settings::<InputPrefs>().get();
        assert_eq!(inspected.keyboard_layout, before.keyboard_layout);
        assert_eq!(inspected.keyboard_layouts, before.keyboard_layouts);
        assert_eq!(inspected.keyboard_model, "pc104");
        assert_eq!(inspected.keyboard_preview.as_deref(), Some("de"));
        assert!(inspected.keyboard_preview_extra_levels);
        assert!(set(&mut app, InputEffect::ApplyPreview));
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "de");
        assert!(!set(
            &mut app,
            InputEffect::PreviewLayout {
                source: "../../private".into()
            }
        ));
        assert!(!set(
            &mut app,
            InputEffect::SetKeyboardModel {
                model: "../../private".into()
            }
        ));
        assert!(!set(
            &mut app,
            InputEffect::SetPreviewLevel { level: "99".into() }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetPreviewLevel { level: "1".into() }
        ));
        assert_eq!(app.settings::<InputPrefs>().get().keyboard_preview_level, 1);
        assert!(set(&mut app, InputEffect::ShowLiveKeyboardMap));
        let input = app.settings::<InputPrefs>().get();
        assert!(input.keyboard_preview.is_none());
        assert_eq!(input.keyboard_preview_level, 0);
        assert_eq!(input.keyboard_layout, "de");
    }

    #[test]
    fn keyboard_map_read_is_published_device_scoped_and_never_changes_preferences() {
        use ui::tilo_resource::Effect as _;
        let mut app = TestApp::<SettingsShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        let effect = InputEffect::InspectKeyboardMap;
        let actions = app.actions().unwrap();
        let action = actions
            .iter()
            .find(|action| action.address.action == effect.id())
            .unwrap();
        assert_eq!(action.contract.kind, ui::tilo_actions::ActionKind::Read);
        assert!(
            action
                .contract
                .tags
                .iter()
                .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY)
        );
        let address = action.address.clone();
        let before = app.settings::<InputPrefs>().get();
        let value = app
            .call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(value["page_visible"], false);
        assert_eq!(value["pressed_physical_keys"], serde_json::json!([]));
        assert_eq!(app.settings::<InputPrefs>().get(), before);
        let denied = app.call_by(
            ui::tilo_actions::ActionInvoker::actor("unrelated-collaborator"),
            &address.resource,
            &address.action,
            serde_json::json!({}),
        );
        assert!(
            matches!(denied, Err(ui::tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "local_device_only")
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }

    #[test]
    fn keyboard_map_read_uses_the_mounted_preview_and_live_key_release_state() {
        use ui::tilo_resource::Effect as _;
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let read = |app: &mut TestApp<SettingsShell>| {
            app.call_on(
                "settings.keyboard-map",
                InputEffect::InspectKeyboardMap.id(),
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap()
        };
        crate::effects::show_pane(&mut app, crate::categories::Category::Keyboard);
        assert!(set(
            &mut app,
            InputEffect::PreviewLayout {
                source: "de".into()
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::InspectKeyboardKey { key: "AD06".into() }
        ));
        let before = app.settings::<InputPrefs>().get();
        app.settle();
        app.harness().dispatch_to_root(&WindowEvent::Focused(true));
        let preview = read(&mut app);
        assert_eq!(preview["mode"], "preview");
        assert_eq!(preview["available"], true);
        assert_eq!(preview["source"], "de");
        assert_eq!(preview["inspected_key"], "AD06");
        assert!(
            preview["selected_symbols"]
                .as_array()
                .unwrap()
                .iter()
                .any(|symbol| symbol == "z")
        );
        let key = |state| {
            ui::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Code(
                        winit::keyboard::KeyCode::KeyY,
                    ),
                    state,
                },
                winit::keyboard::Key::Character("z".into()),
                Some("z".into()),
                winit::keyboard::KeyLocation::Standard,
                false,
                Some("z".into()),
                winit::keyboard::Key::Character("z".into()),
            ))
        };
        app.harness()
            .dispatch_to_root(&key(winit::event::ElementState::Pressed));
        assert_eq!(
            read(&mut app)["pressed_physical_keys"],
            serde_json::json!(["AD06"])
        );
        app.harness()
            .dispatch_to_root(&key(winit::event::ElementState::Released));
        assert_eq!(
            read(&mut app)["pressed_physical_keys"],
            serde_json::json!([])
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
        assert!(set(&mut app, InputEffect::ShowLiveKeyboardMap));
        let live = read(&mut app);
        assert_eq!(live["mode"], "live");
        assert_eq!(
            live["available"], false,
            "native evidence is mandatory, never preferred-layout fallback"
        );
        assert_eq!(live["selected_symbols"], serde_json::json!([]));
        app.harness()
            .dispatch_to_root(&key(winit::event::ElementState::Pressed));
        app.harness().dispatch_to_root(&WindowEvent::Focused(false));
        let blurred = read(&mut app);
        assert_eq!(blurred["foreground"], false);
        assert_eq!(blurred["pressed_physical_keys"], serde_json::json!([]));
        let about = crate::categories::Category::About;
        app.effect_on(
            &crate::effects::pane_row_id(about),
            &crate::effects::open_pane(about),
        )
        .unwrap();
        let off_page = read(&mut app);
        assert_eq!(off_page["page_visible"], false);
        assert_eq!(off_page["selected_symbols"], serde_json::json!([]));
        assert_eq!(off_page["pressed_physical_keys"], serde_json::json!([]));
    }

    #[test]
    fn physical_key_inspection_is_published_validated_and_never_applies_layout() {
        use ui::tilo_resource::Effect as _;
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let before = app.settings::<InputPrefs>().get();
        let inspect = InputEffect::InspectKeyboardKey { key: "AD06".into() };
        assert!(
            app.actions()
                .unwrap()
                .iter()
                .any(|action| action.address.action == inspect.id())
        );
        assert!(set(&mut app, inspect));
        let inspected = app.settings::<InputPrefs>().get();
        assert_eq!(inspected.keyboard_inspected_key, "AD06");
        assert_eq!(inspected.keyboard_layout, before.keyboard_layout);
        assert_eq!(inspected.keyboard_layouts, before.keyboard_layouts);
        assert_eq!(inspected.keyboard_preview, before.keyboard_preview);
        assert!(!set(
            &mut app,
            InputEffect::InspectKeyboardKey {
                key: "../../private".into()
            }
        ));
        assert!(!set(
            &mut app,
            InputEffect::InspectKeyboardKey { key: "AE13".into() }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetKeyboardModel {
                model: "jp106".into()
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::InspectKeyboardKey { key: "AE13".into() }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetKeyboardModel {
                model: "pc104".into()
            }
        ));
        assert_eq!(
            app.settings::<InputPrefs>().get().keyboard_inspected_key,
            "AD01",
            "a model switch must not leave an absent JIS position selected"
        );
    }

    fn set(app: &mut TestApp<SettingsShell>, effect: InputEffect) -> bool {
        app.effect(&SettingsEffect::from(effect)).is_ok()
    }

    /// Pointer and keyboard choices are the person's `input` section, the
    /// touchpad switch this device's, spelling the shared
    /// `text_assistance` section; a value outside the offered ones is refused.
    #[test]
    fn input_effects_write_the_input_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, INPUT, "touchpad_pointer_speed", 0.9).unwrap();
        edit_field(&mut app, INPUT, "mouse_natural_scroll", true).unwrap();
        edit_field(&mut app, INPUT, "touchpad_enabled", false).unwrap();
        assert!(set(
            &mut app,
            InputEffect::SetComposeKey { key: "ralt".into() }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetCapsLock {
                behaviour: "ctrl".into()
            }
        ));
        edit_field(&mut app, INPUT, "touchpad_click_method", "areas").unwrap();
        edit_field(&mut app, TEXT_ASSISTANCE, "check_spelling", false).unwrap();
        let input = app.settings::<InputPrefs>().get();
        assert_eq!(input.touchpad_pointer_speed, Some(0.9));
        assert!(input.mouse_natural_scroll);
        assert!(!input.touchpad_enabled);
        let active = settings_store::keyboard_sources::effective(
            &input.rich_input(),
            settings_store::keyboard_sources::ALL,
        );
        assert_eq!(active.compose_key.as_deref(), Some("ralt"));
        assert_eq!(active.caps_lock.as_deref(), Some("ctrl"));
        assert_eq!(input.touchpad_click_method, TouchpadClickMethod::Areas);
        assert!(!app.settings::<TextAssistancePrefs>().get().check_spelling);
        edit_field(&mut app, TEXT_ASSISTANCE, "spelling_language", "de_DE").unwrap();
        assert_eq!(
            app.settings::<TextAssistancePrefs>()
                .get()
                .spelling_language,
            "de_DE"
        );
        edit_field(&mut app, TEXT_ASSISTANCE, "spelling_language", "").unwrap();
        assert_eq!(
            app.settings::<TextAssistancePrefs>()
                .get()
                .spelling_language,
            "",
            "empty is the session's language"
        );

        assert!(edit_field(&mut app, INPUT, "mouse_pointer_speed", 4.0).is_err());
        assert!(!set(
            &mut app,
            InputEffect::SetComposeKey { key: "f13".into() }
        ));
        assert_eq!(app.settings::<InputPrefs>().get(), input);
    }

    /// Input sources are added, ordered, switched and removed by id; the
    /// last one stays.
    #[test]
    fn input_sources_are_added_ordered_and_removed() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let sources = |app: &TestApp<SettingsShell>| {
            let input = app.settings::<InputPrefs>().get();
            (input.keyboard_layouts.clone(), input.keyboard_layout)
        };
        assert!(set(
            &mut app,
            InputEffect::AddSource {
                source: "de".into()
            }
        ));
        assert_eq!(sources(&app), (vec!["us".into(), "de".into()], "de".into()));
        assert!(!set(
            &mut app,
            InputEffect::AddSource {
                source: "xx".into()
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::MoveSourceUp {
                source: "de".into()
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetActiveSource {
                source: "us".into()
            }
        ));
        assert_eq!(sources(&app), (vec!["de".into(), "us".into()], "us".into()));
        assert!(set(
            &mut app,
            InputEffect::RemoveSource {
                source: "us".into()
            }
        ));
        assert_eq!(sources(&app), (vec!["de".into()], "de".into()));
        assert!(!set(
            &mut app,
            InputEffect::RemoveSource {
                source: "de".into()
            }
        ));
    }

    /// A tablet's choices are kept per device id; a pad's controls by
    /// button and dial.
    #[test]
    fn tablet_effects_keep_each_device_apart() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        assert!(set(
            &mut app,
            InputEffect::SetTabletEraser {
                device: "pen-a".into(),
                action: "right".into(),
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetPadDial {
                device: "pad-a".into(),
                dial: "ring".into(),
                index: 0,
                mode: 1,
                action: "zoom".into(),
            }
        ));
        assert!(!set(
            &mut app,
            InputEffect::SetTabletButton {
                device: "pen-a".into(),
                button: "fourth".into(),
                action: "left".into(),
            }
        ));
        let input = app.settings::<InputPrefs>().get();
        assert_eq!(input.tablets["pen-a"].eraser, EraserAction::Right);
        assert_eq!(
            input.tablets["pad-a"].dial_action(PadDial::Ring(0), 1),
            PadDialAction::Zoom
        );
        assert_eq!(
            parse_dial_action_id("strip-1-mode-0"),
            Some(("strip", 1, 0))
        );
        assert_eq!(
            parse_dial_action_id(&dial_action_id(PadDial::Ring(2), 3)),
            Some(("ring", 2, 3))
        );
    }
}

#[cfg(test)]
mod input_inspection_tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    #[test]
    fn installed_input_sources_have_a_bounded_device_read_without_mutating_preferences() {
        let mut app = TestApp::<SettingsShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        let action = app.actions().unwrap().into_iter()
            .find(|action| action.address.action.ends_with("inspect-input-sources"))
            .expect("the installed catalogue and active source must be readable through Settings' normal door");
        assert_eq!(action.contract.kind, ui::tilo_actions::ActionKind::Read);
        assert!(
            action
                .contract
                .tags
                .iter()
                .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY)
        );
        let before = app.settings::<InputPrefs>().get();
        let value = app
            .call_on(
                &action.address.resource,
                &action.address.action,
                serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(value["active_source"], before.keyboard_layout);
        assert_eq!(
            value["configured_sources"],
            serde_json::json!(before.keyboard_layouts)
        );
        assert_eq!(value["page"], 0);
        assert!(value["sources"].as_array().unwrap().len() <= 32);
        assert!(value["total"].as_u64().unwrap() > 0);
        assert!(
            value["add_picker"].is_null(),
            "an off-page selector must not pretend to be mounted"
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
        let refusal = app.call_by(
            ui::tilo_actions::ActionInvoker::actor("unrelated-collaborator"),
            &action.address.resource,
            &action.address.action,
            serde_json::json!({}),
        );
        assert!(
            matches!(refusal, Err(ui::tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "local_device_only")
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }
    #[test]
    fn input_inspection_tracks_the_actual_unselected_open_shared_picker() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        super::tests::show_add_source(&mut app, (900, 700));
        let address = app
            .actions()
            .unwrap()
            .into_iter()
            .find(|action| action.address.action.ends_with("inspect-input-sources"))
            .unwrap()
            .address;
        let before = app.settings::<InputPrefs>().get();
        let closed = app
            .call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(closed["add_picker"]["selected"], "");
        assert_eq!(closed["add_picker"]["open"], false);
        assert!(closed["add_picker"]["value"].is_null());
        app.harness()
            .click_named("input.source.add_picker")
            .unwrap();
        app.harness().type_text("French");
        let open = app
            .call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(open["add_picker"]["selected"], "");
        assert_eq!(open["add_picker"]["open"], true);
        assert_eq!(open["add_picker"]["filter"], "French");
        assert_eq!(open["add_picker"]["value"], "French");
        assert_eq!(app.settings::<InputPrefs>().get(), before);
        let filtered = app
            .call_on(
                &address.resource,
                &address.action,
                serde_json::json!({"query":"French"}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(filtered["total"], 1);
        assert_eq!(filtered["sources"][0]["id"], "fr");
        assert!(
            app.call_on(
                &address.resource,
                &address.action,
                serde_json::json!({"page":1})
            )
            .is_err()
        );
        assert!(
            app.call_on(
                &address.resource,
                &address.action,
                serde_json::json!({"query":"x".repeat(129)})
            )
            .is_err()
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }
}
