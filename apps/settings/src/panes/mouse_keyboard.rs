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

use std::cell::RefCell;
use std::collections::HashMap;

use settings_store::hardware::{
    EraserAction, InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot, PadDial,
    PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig, TouchpadClickMethod,
    TouchpadScrollMethod,
};
use taffy::{NodeId, TaffyTree};
use ui::framework::{ActionControl, EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::text_assistance::TextAssistancePrefs;
use ui::theme::Theme;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Binds};
use ui::widget::{
    AppModel, Button, Dropdown, ModelPath, Row, Slider, Spacer, Text, Toggle, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::input_sources::{CatalogEntry, installed_catalog};
use crate::prefs::InputPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, caption, card, choice_row, choice_row_described, error_caption, rows, setting_row,
    setting_row_described,
};

/// Bind a control to a Settings effect where it is built.
trait BindEffect<I>: ActionControl<I> {
    fn bound(self, cx: &PaneCx, effect: impl Binds<SettingsEffect, I>) -> Self
    where
        I: 'static,
    {
        cx.bind(self, effect)
    }
}

impl<I, C: ActionControl<I>> BindEffect<I> for C {}

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

/// The source id a shown label names, among `catalog`'s.
fn source_id(catalog: &[CatalogEntry], shown: &str) -> String {
    catalog
        .iter()
        .find(|entry| entry.label == shown)
        .map_or_else(|| shown.to_owned(), |entry| entry.id.clone())
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
    .with_accessible_name(accessible_name);
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

/// "Ring 1", or "Ring 1 · Mode 2" when its group has several modes.
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
    /// Turn the touchpad on or off on this device.
    #[effect(idempotent)]
    SetTouchpadEnabled { on: bool },
    /// Turn the touchpad off while a mouse is plugged in, or stop.
    #[effect(idempotent)]
    SetTouchpadOffWithMouse { on: bool },
    /// Set how fast the pointer moves with the touchpad.
    #[effect(idempotent)]
    SetTouchpadSpeed {
        #[param("Slow (0.0) to fast (1.0)")]
        speed: f64,
    },
    /// Set how fast the pointer moves with a mouse.
    #[effect(idempotent)]
    SetMouseSpeed {
        #[param("Slow (0.0) to fast (1.0)")]
        speed: f64,
    },
    /// Scroll the content with the fingers on the touchpad, or the view.
    #[effect(idempotent)]
    SetTouchpadNaturalScroll { on: bool },
    /// Scroll the content with the mouse wheel, or the view.
    #[effect(idempotent)]
    SetMouseNaturalScroll { on: bool },
    /// Click with a tap on the touchpad, or only with a press.
    #[effect(idempotent)]
    SetTapToClick { on: bool },
    /// Drag with a double tap on the touchpad, or stop.
    #[effect(idempotent)]
    SetTapDrag { on: bool },
    /// Keep a tap drag going after lifting the finger, or stop.
    #[effect(idempotent)]
    SetTapDragLock { on: bool },
    /// Ignore the touchpad while typing, or stop.
    #[effect(idempotent)]
    SetDisableWhileTyping { on: bool },
    /// Use touchpad gestures, or stop.
    #[effect(idempotent)]
    SetTouchpadGestures { on: bool },
    /// Set how a touchpad press becomes a click.
    #[effect(idempotent)]
    SetTouchpadClickMethod {
        #[param("The method", choices = ids(&CLICK_METHOD_CHOICES))]
        method: String,
    },
    /// Set how the touchpad scrolls.
    #[effect(idempotent)]
    SetTouchpadScrollMethod {
        #[param("The method", choices = ids(&SCROLL_METHOD_CHOICES))]
        method: String,
    },
    /// Swap the pointer's buttons for the left hand, or back.
    #[effect(idempotent)]
    SetLeftHanded { on: bool },
    /// Middle-click by pressing both buttons, or stop.
    #[effect(idempotent)]
    SetMiddleEmulation { on: bool },
    /// Set how the pointer speeds up with faster movement.
    #[effect(idempotent)]
    SetAccelerationProfile {
        #[param("`adaptive` or `flat`", choices = ["adaptive", "flat"])]
        profile: String,
    },
    /// Set how far one scroll step moves.
    #[effect(idempotent)]
    SetScrollSpeed {
        #[param("Slow (0.0) to fast (1.0)")]
        speed: f64,
    },
    /// Set how soon and how fast a held key repeats.
    #[effect(idempotent)]
    SetKeyRepeat {
        #[param("Slow (0.0) to fast (1.0)")]
        rate: f64,
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
    /// Add an installed input source and type with it.
    AddSource {
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
    /// Check spelling while typing, or stop.
    #[effect(idempotent)]
    SetSpellChecking { on: bool },
    /// Correct misspellings while typing, or stop.
    #[effect(idempotent)]
    SetAutoCorrect { on: bool },
    /// Check spelling in one language, or the session's.
    #[effect(idempotent)]
    SetSpellingLanguage {
        #[param(
            "A language (`de_DE`), or empty for the session's language",
            choices = SPELLING_LANGUAGES.iter().map(|(id, _)| *id)
        )]
        language: String,
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
        E::SetTouchpadEnabled { on } => input.touchpad_enabled = on,
        E::SetTouchpadOffWithMouse { on } => input.touchpad_disabled_on_external_mouse = on,
        E::SetTouchpadSpeed { speed } => input.touchpad_pointer_speed = Some(fraction(speed)?),
        E::SetMouseSpeed { speed } => input.mouse_pointer_speed = Some(fraction(speed)?),
        E::SetTouchpadNaturalScroll { on } => input.touchpad_natural_scroll = Some(on),
        E::SetMouseNaturalScroll { on } => input.mouse_natural_scroll = Some(on),
        E::SetTapToClick { on } => input.tap_to_click = on,
        E::SetTapDrag { on } => input.tap_drag = on,
        E::SetTapDragLock { on } => input.tap_drag_lock = on,
        E::SetDisableWhileTyping { on } => input.disable_while_typing = on,
        E::SetTouchpadGestures { on } => input.touchpad_gestures = on,
        E::SetTouchpadClickMethod { method } => {
            input.touchpad_click_method = click_method_for_id(&method)
                .ok_or_else(|| format!("`{method}` is not a click method"))?;
        }
        E::SetTouchpadScrollMethod { method } => {
            input.touchpad_scroll_method = scroll_method_for_id(&method)
                .ok_or_else(|| format!("`{method}` is not a scroll method"))?;
        }
        E::SetLeftHanded { on } => input.left_handed = on,
        E::SetMiddleEmulation { on } => input.middle_emulation = on,
        E::SetAccelerationProfile { profile } => {
            accel_from_label(&profile)
                .or_else(|| {
                    ["adaptive", "flat"]
                        .into_iter()
                        .find(|known| *known == profile)
                })
                .ok_or_else(|| format!("`{profile}` is not an acceleration profile"))?
                .clone_into(&mut input.accel_profile);
        }
        E::SetScrollSpeed { speed } => {
            input.scroll_speed =
                settings_store::hardware::scroll_speed_from_slider(fraction(speed)?);
        }
        E::SetKeyRepeat { rate } => {
            input.key_repeat = fraction(rate)?;
            input.key_repeat_delay_ms = None;
            input.key_repeat_rate_hz = None;
        }
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
        E::SetSpellChecking { .. } | E::SetAutoCorrect { .. } | E::SetSpellingLanguage { .. } => {}
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
    let host = app.host();
    match effect {
        InputEffect::SetSpellChecking { on } => {
            return host.update(|text: &mut TextAssistancePrefs| text.check_spelling = on);
        }
        InputEffect::SetAutoCorrect { on } => {
            return host.update(|text: &mut TextAssistancePrefs| text.auto_correct = on);
        }
        InputEffect::SetSpellingLanguage { language } => {
            if !SPELLING_LANGUAGES.iter().any(|(id, _)| *id == language) {
                return Err(format!(
                    "`{language}` is not a spelling language Settings offers"
                ));
            }
            return host.update(|text: &mut TextAssistancePrefs| text.spelling_language = language);
        }
        _ => {}
    }
    let settings = host.settings::<InputPrefs>();
    let mut input = settings.get();
    input.normalise_keyboard_sources();
    apply(&mut input, effect, app.input().catalog())?;
    input.normalise_keyboard_sources();
    settings
        .set(&input)
        .map(|()| Effected::done())
        .map_err(|error| format!("the input settings could not be saved: {error}"))
}

/// How often the attached-device snapshot is read again, so plugging a
/// mouse in shows its section without reopening Settings.
const DEVICE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// What the input area keeps for the window: the installed input sources,
/// and the attached devices as the compositor last reported them.
pub struct InputArea {
    catalog: Vec<CatalogEntry>,
    /// Why the device's layout list could not be read; `catalog` is then a
    /// short built-in list and says so.
    catalog_error: Option<String>,
    live: bool,
    devices: RefCell<(Option<ui::time::Instant>, Option<InputHardwareSnapshot>)>,
}

impl InputArea {
    /// The area of this device, or, for a capture, of sample sources and no
    /// devices reported.
    pub fn new(cx: &PaneCx) -> Self {
        let live = cx.reaches_devices();
        Self {
            catalog: if live {
                installed_catalog()
            } else {
                sample_catalog()
            },
            catalog_error: live
                .then(crate::input_sources::layout_catalog_error)
                .flatten(),
            live,
            devices: RefCell::new((None, None)),
        }
    }

    fn catalog(&self) -> &[CatalogEntry] {
        &self.catalog
    }

    /// The attached devices, read again at most once per
    /// [`DEVICE_POLL_INTERVAL`]; `None` without a compositor snapshot.
    fn devices(&self) -> Option<InputHardwareSnapshot> {
        if !self.live {
            return None;
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
        let Some(devices) = self.devices() else {
            return Vec::new();
        };
        let row = |device: &InputDeviceSnapshot, id: String, kind: &str| {
            LiveRow::new(id, kind, device_title(device)).presentation(ResourcePresentation {
                detail: Some(device.key.clone()),
                ..ResourcePresentation::default()
            })
        };
        devices
            .of_kind(InputDeviceKind::Tablet)
            .into_iter()
            .map(|device| row(device, tablet_resource_id(&device.key), "settings.tablet"))
            .chain(
                devices
                    .of_kind(InputDeviceKind::TabletPad)
                    .into_iter()
                    .map(|device| row(device, pad_resource_id(&device.key), "settings.tablet-pad")),
            )
            .collect()
    }
}

/// The input sources a capture offers.
fn sample_catalog() -> Vec<CatalogEntry> {
    [("us", "English (US)"), ("de", "German"), ("fr", "French")]
        .map(|(id, label)| CatalogEntry {
            id: id.into(),
            label: label.into(),
        })
        .to_vec()
}

pub struct MouseKeyboardPane {
    cx: PaneCx,
    input: Followed<InputPrefs>,
    text: Followed<TextAssistancePrefs>,
    /// The input section as shown: normalised.
    last: InputPrefs,
    catalog: Vec<CatalogEntry>,
    /// Why the device's layout list could not be read; `catalog` is then a
    /// short built-in list and says so.
    catalog_error: Option<String>,
    /// Which sections the attached devices call for.
    sections: InputSections,
    /// Attached drawing tablets and pads, one card each.
    tablets: Vec<InputDeviceSnapshot>,
    pads: Vec<InputDeviceSnapshot>,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl MouseKeyboardPane {
    /// The pane over `cx`'s input settings and `area`'s sources and devices.
    pub fn new(cx: PaneCx, area: &InputArea) -> Self {
        let input = cx.follow::<InputPrefs>();
        let text = cx.follow::<TextAssistancePrefs>();
        let mut last = input.get().clone();
        last.normalise_keyboard_sources();
        let mut pane = Self {
            cx,
            input,
            text,
            last,
            catalog: area.catalog().to_vec(),
            catalog_error: area.catalog_error.clone(),
            sections: InputSections::for_devices(None),
            tablets: Vec::new(),
            pads: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        };
        pane.observe_devices(area.devices().as_ref());
        pane.structure_dirty = false;
        pane
    }

    fn source_label(&self, id: &str) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.label.clone())
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // The keyboard layout comes first: it is what most people open this
        // pane to change.
        let mut cards = vec![
            self.input_sources_card(),
            self.keyboard_card(),
            self.spell_checker_card(),
        ];
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
        crate::widgets::cards(cards)
    }

    /// Take in which devices are attached; rebuild when that changes what
    /// the page shows.
    fn observe_devices(&mut self, snapshot: Option<&InputHardwareSnapshot>) {
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
            self.structure_dirty = true;
        }
    }

    fn bool_row(
        &self,
        label: &str,
        description: &str,
        set: tilo_resource::ActionRef<InputEffect, bool>,
        id: &str,
        on: bool,
    ) -> El {
        setting_row_described(
            &t(label),
            &t(description),
            Box::new(
                Toggle::from_state(&ui::LocalState::new(on))
                    .with_id(id)
                    .bound(&self.cx, effect(set)),
            ),
        )
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
            "map-to" => dropdown.bound(
                &self.cx,
                effect(InputEffect::SET_TABLET_OUTPUT)
                    .map(move |output: String| (device.clone(), output)),
            ),
            "lower-button" | "upper-button" | "third-button" => {
                let button = action.trim_end_matches("-button").to_owned();
                dropdown.bound(
                    &self.cx,
                    effect(InputEffect::SET_TABLET_BUTTON)
                        .map(move |choice: String| (device.clone(), button.clone(), choice)),
                )
            }
            "eraser" => dropdown.bound(
                &self.cx,
                effect(InputEffect::SET_TABLET_ERASER)
                    .map(move |choice: String| (device.clone(), choice)),
            ),
            other => {
                if let Some(button) = other
                    .strip_prefix("button-")
                    .and_then(|button| button.parse::<u32>().ok())
                {
                    return dropdown.bound(
                        &self.cx,
                        effect(InputEffect::SET_PAD_BUTTON)
                            .map(move |choice: String| (device.clone(), button, choice)),
                    );
                }
                match parse_dial_action_id(other) {
                    Some((dial, index, mode)) => dropdown.bound(
                        &self.cx,
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
            "keep-aspect" => toggle.bound(
                &self.cx,
                effect(InputEffect::SET_TABLET_KEEP_ASPECT)
                    .map(move |on: bool| (device.clone(), on)),
            ),
            _ => toggle.bound(
                &self.cx,
                effect(InputEffect::SET_TABLET_LEFT_HANDED)
                    .map(move |on: bool| (device.clone(), on)),
            ),
        }
    }

    fn device_slider(&self, resource: &str, device: &str, action: &str, value: f64) -> Slider {
        let device = device.to_owned();
        Slider::from_state(&ui::LocalState::new(value))
            .with_id(&format!("{resource}.{action}"))
            .bound(
                &self.cx,
                effect(InputEffect::SET_TABLET_TIP_FEEL)
                    .map(move |feel: f64| (device.clone(), feel)),
            )
    }

    /// Everything that is specific to a touchpad, starting with whether it
    /// works at all. The touchpad key on a laptop keyboard flips the same
    /// saved setting.
    fn touchpad_card(&self) -> Box<dyn Widget> {
        let last = &self.last;
        card(
            &t("input-touchpad-title"),
            rows(vec![
                self.bool_row(
                    "input-touchpad-enabled",
                    "input-touchpad-enabled-description",
                    InputEffect::SET_TOUCHPAD_ENABLED,
                    "input.touchpad_enabled",
                    last.touchpad_enabled,
                ),
                self.bool_row(
                    "input-touchpad-disable-with-mouse",
                    "input-touchpad-disable-with-mouse-description",
                    InputEffect::SET_TOUCHPAD_OFF_WITH_MOUSE,
                    "input.touchpad_disabled_on_external_mouse",
                    last.touchpad_disabled_on_external_mouse,
                ),
                setting_row(
                    &t("input-touchpad-speed"),
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            last.touchpad_pointer_speed.unwrap_or(last.pointer_speed),
                        ))
                        .bound(&self.cx, effect(InputEffect::SET_TOUCHPAD_SPEED))
                        .with_id("input.touchpad_pointer_speed"),
                    ),
                ),
                self.bool_row(
                    "input-touchpad-natural-scroll",
                    "input-touchpad-natural-scroll-description",
                    InputEffect::SET_TOUCHPAD_NATURAL_SCROLL,
                    "input.touchpad_natural_scroll",
                    last.touchpad_natural_scroll.unwrap_or(last.natural_scroll),
                ),
                self.bool_row(
                    "input-tap-to-click",
                    "input-tap-to-click-description",
                    InputEffect::SET_TAP_TO_CLICK,
                    "input.tap_click",
                    last.tap_to_click,
                ),
                self.bool_row(
                    "input-tap-drag",
                    "input-tap-drag-description",
                    InputEffect::SET_TAP_DRAG,
                    "input.tap_drag",
                    last.tap_drag,
                ),
                self.bool_row(
                    "input-tap-drag-lock",
                    "input-tap-drag-lock-description",
                    InputEffect::SET_TAP_DRAG_LOCK,
                    "input.tap_drag_lock",
                    last.tap_drag_lock,
                ),
                self.bool_row(
                    "input-disable-while-typing",
                    "input-disable-while-typing-description",
                    InputEffect::SET_DISABLE_WHILE_TYPING,
                    "input.disable_while_typing",
                    last.disable_while_typing,
                ),
                self.bool_row(
                    "input-touchpad-gestures",
                    "input-touchpad-gestures-description",
                    InputEffect::SET_TOUCHPAD_GESTURES,
                    "input.touchpad_gestures",
                    last.touchpad_gestures,
                ),
            ]),
        )
    }

    /// How a touchpad turns presses and finger movement into clicks and
    /// scrolling. Each choice is applied only to devices whose libinput
    /// driver offers it.
    fn touchpad_click_scroll_card(&self) -> Box<dyn Widget> {
        card(
            &t("input-clicking-title"),
            rows(vec![
                choice_row(
                    &t("input-click-method"),
                    Box::new(
                        id_dropdown(
                            localized_choices(&CLICK_METHOD_CHOICES),
                            click_method_id(self.last.touchpad_click_method),
                            "input.touchpad_click_method",
                            &t("input-click-method"),
                        )
                        .bound(&self.cx, effect(InputEffect::SET_TOUCHPAD_CLICK_METHOD)),
                    ),
                ),
                caption(&t("input-click-method-caption")),
                choice_row(
                    &t("input-scroll-method"),
                    Box::new(
                        id_dropdown(
                            localized_choices(&SCROLL_METHOD_CHOICES),
                            scroll_method_id(self.last.touchpad_scroll_method),
                            "input.touchpad_scroll_method",
                            &t("input-scroll-method"),
                        )
                        .bound(&self.cx, effect(InputEffect::SET_TOUCHPAD_SCROLL_METHOD)),
                    ),
                ),
            ]),
        )
    }

    /// Mice, trackballs and pointing sticks.
    fn mouse_card(&self) -> Box<dyn Widget> {
        let last = &self.last;
        card(
            &t("input-mouse-title"),
            rows(vec![
                setting_row(
                    &t("input-mouse-speed"),
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            last.mouse_pointer_speed.unwrap_or(last.pointer_speed),
                        ))
                        .bound(&self.cx, effect(InputEffect::SET_MOUSE_SPEED))
                        .with_id("input.mouse_pointer_speed"),
                    ),
                ),
                self.bool_row(
                    "input-mouse-natural-scroll",
                    "input-mouse-natural-scroll-description",
                    InputEffect::SET_MOUSE_NATURAL_SCROLL,
                    "input.mouse_natural_scroll",
                    last.mouse_natural_scroll.unwrap_or(last.natural_scroll),
                ),
            ]),
        )
    }

    /// Choices shared by every pointer, touchpad or mouse. Less-common
    /// choices still get a specific destination instead of an opaque
    /// "Advanced" bucket: people and assistive agents can tell what will be
    /// changed before opening the card.
    fn pointer_options_card(&self) -> Box<dyn Widget> {
        card(
            &t("input-pointer-title"),
            rows(vec![
                self.bool_row(
                    "input-left-handed",
                    "input-left-handed-description",
                    InputEffect::SET_LEFT_HANDED,
                    "input.left_handed",
                    self.last.left_handed,
                ),
                self.bool_row(
                    "input-middle-emulation",
                    "input-middle-emulation-description",
                    InputEffect::SET_MIDDLE_EMULATION,
                    "input.middle_emulation",
                    self.last.middle_emulation,
                ),
                choice_row(
                    &t("input-acceleration"),
                    Box::new(
                        Dropdown::from_state(
                            accel_options(),
                            &ui::LocalState::new(accel_shown(&self.last.accel_profile)),
                        )
                        .bound(
                            &self.cx,
                            effect(InputEffect::SET_ACCELERATION_PROFILE).map(|shown: String| {
                                accel_from_label(&shown).unwrap_or_default().to_owned()
                            }),
                        )
                        .with_id("input.accel_profile")
                        .with_placeholder(&accel_shown(&self.last.accel_profile)),
                    ),
                ),
                setting_row_described(
                    &t("input-scroll-speed"),
                    &t("input-scroll-speed-description"),
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            settings_store::hardware::scroll_speed_to_slider(
                                self.last.scroll_speed,
                            ),
                        ))
                        .bound(&self.cx, effect(InputEffect::SET_SCROLL_SPEED))
                        .with_id("input.scroll_speed"),
                    ),
                ),
            ]),
        )
    }

    fn keyboard_card(&self) -> Box<dyn Widget> {
        let active_sources: Vec<String> = self
            .last
            .keyboard_layouts
            .iter()
            .map(|id| self.source_label(id))
            .collect();
        let repeat_value = self.key_repeat_slider_value();
        let repeat_description = self.key_repeat_description();
        let compose_current = compose_shown(compose_english(self.last.compose_key.as_deref()));
        let compose_placeholder = compose_current.clone();
        let caps_current = caps_lock_shown(caps_lock_label(self.last.caps_lock.as_deref()));
        let caps_placeholder = caps_current.clone();
        let catalog = self.catalog.clone();
        card(
            &t("input-keyboard-title"),
            rows(vec![
                choice_row(
                    &t("input-active-source"),
                    Box::new(
                        Dropdown::from_state(
                            active_sources,
                            &ui::LocalState::new(self.source_label(&self.last.keyboard_layout)),
                        )
                        .bound(
                            &self.cx,
                            effect(InputEffect::SET_ACTIVE_SOURCE)
                                .map(move |shown: String| source_id(&catalog, &shown)),
                        )
                        .with_id("input.layout")
                        .with_placeholder(&self.source_label(&self.last.keyboard_layout)),
                    ),
                ),
                setting_row_described(
                    &t("input-key-repeat"),
                    &repeat_description,
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(repeat_value))
                            .bound(&self.cx, effect(InputEffect::SET_KEY_REPEAT))
                            .with_id("input.key_repeat"),
                    ),
                ),
                caption(&t("input-keyboard-caption")),
                choice_row(
                    &t("input-compose-key"),
                    Box::new(
                        Dropdown::from_state(
                            compose_options(),
                            &ui::LocalState::new(compose_current),
                        )
                        .bound(
                            &self.cx,
                            effect(InputEffect::SET_COMPOSE_KEY).map(|shown: String| {
                                compose_from_label(&shown)
                                    .and_then(StoredKeyChoice::as_stored)
                                    .unwrap_or(COMPOSE_DISABLED)
                                    .to_owned()
                            }),
                        )
                        .with_id("input.compose_key")
                        .with_placeholder(&compose_placeholder),
                    ),
                ),
                choice_row(
                    &t("input-caps-lock-key"),
                    Box::new(
                        Dropdown::from_state(
                            caps_lock_options(),
                            &ui::LocalState::new(caps_current),
                        )
                        .bound(
                            &self.cx,
                            effect(InputEffect::SET_CAPS_LOCK).map(|shown: String| {
                                caps_lock_from_label(&shown)
                                    .and_then(StoredKeyChoice::as_stored)
                                    .unwrap_or(CAPS_LOCK_UNCHANGED)
                                    .to_owned()
                            }),
                        )
                        .with_id("input.caps_lock")
                        .with_placeholder(&caps_placeholder),
                    ),
                ),
            ]),
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

    fn spell_checker_card(&self) -> Box<dyn Widget> {
        card(
            &t("input-spell-title"),
            rows(vec![
                setting_row_described(
                    &t("input-spell-check"),
                    &t("input-spell-check-desc"),
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.text.get().check_spelling))
                            .with_id("text.check_spelling")
                            .bound(&self.cx, effect(InputEffect::SET_SPELL_CHECKING)),
                    ),
                ),
                setting_row_described(
                    &t("input-spell-auto"),
                    &t("input-spell-auto-desc"),
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.text.get().auto_correct))
                            .with_id("text.auto_correct")
                            .bound(&self.cx, effect(InputEffect::SET_AUTO_CORRECT)),
                    ),
                ),
                choice_row(
                    &t("input-spell-language"),
                    Box::new(
                        id_dropdown(
                            spelling_language_choices(),
                            &self.text.get().spelling_language,
                            "text.spelling_language",
                            &t("input-spell-language"),
                        )
                        .bound(&self.cx, effect(InputEffect::SET_SPELLING_LANGUAGE)),
                    ),
                ),
                caption(&t("input-spell-caption")),
            ]),
        )
    }

    fn input_sources_card(&self) -> Box<dyn Widget> {
        let active_label = t("input-sources-active");
        let remove_label = t("input-sources-remove");
        let move_up = t("input-sources-move-up");
        let move_down = t("input-sources-move-down");
        let add_label = t("input-sources-add");
        let choose = t("input-sources-choose");
        let mut source_rows = Vec::new();
        for (index, id) in self.last.keyboard_layouts.iter().enumerate() {
            let mut row = Row::new()
                .push(Box::new(Text::new(&self.source_label(id)).with_size(13.0)))
                .push(Box::new(Spacer::new()));
            if id == &self.last.keyboard_layout {
                row = row.push(Box::new(Text::new(&active_label).with_size(12.0)));
            }
            let up = format!("input.source.{index}.up");
            let down = format!("input.source.{index}.down");
            let remove = format!("input.source.{index}.remove");
            let mut up_button = Button::new(&up, "↑")
                .with_size(36.0, 34.0)
                .with_accessible_name(&move_up)
                .with_enabled(index > 0);
            if index > 0 {
                up_button = self
                    .cx
                    .bind_invocation(up_button, InputEffect::MoveSourceUp { source: id.clone() });
            }
            let mut down_button = Button::new(&down, "↓")
                .with_size(36.0, 34.0)
                .with_accessible_name(&move_down)
                .with_enabled(index + 1 < self.last.keyboard_layouts.len());
            if index + 1 < self.last.keyboard_layouts.len() {
                down_button = self.cx.bind_invocation(
                    down_button,
                    InputEffect::MoveSourceDown { source: id.clone() },
                );
            }
            let mut remove_button = Button::new(&remove, &remove_label)
                .content_sized()
                .with_enabled(self.last.keyboard_layouts.len() > 1);
            if self.last.keyboard_layouts.len() > 1 {
                remove_button = self.cx.bind_invocation(
                    remove_button,
                    InputEffect::RemoveSource { source: id.clone() },
                );
            }
            row = row
                .push(Box::new(up_button))
                .push(Box::new(down_button))
                .push(Box::new(remove_button));
            source_rows.push(Box::new(row) as Box<dyn Widget>);
        }
        if let Some(error) = &self.catalog_error {
            source_rows.push(error_caption(&t_args(
                "input-catalog-error",
                &[("error", error.as_str().into())],
            )));
        }
        let available: Vec<String> = self
            .catalog
            .iter()
            .filter(|entry| {
                !self.last.keyboard_layouts.contains(&entry.id)
                    && crate::input_sources::has_room_for(&self.last.keyboard_layouts, &entry.id)
            })
            .map(|entry| entry.label.clone())
            .collect();
        if !available.is_empty() {
            source_rows.push(choice_row(
                &add_label,
                Box::new(
                    Dropdown::from_state(
                        available.clone(),
                        &ui::LocalState::new(available.first().cloned().unwrap_or_default()),
                    )
                    .bound(
                        &self.cx,
                        effect(InputEffect::ADD_SOURCE).map({
                            let catalog = self.catalog.clone();
                            move |shown: String| source_id(&catalog, &shown)
                        }),
                    )
                    .with_id("input.source.add_picker")
                    .with_placeholder(&choose),
                ),
            ));
        }
        if crate::input_sources::layouts_full(&self.last.keyboard_layouts) {
            source_rows.push(caption(&t("input-sources-full")));
        }
        source_rows.push(caption(&t("input-sources-ime")));
        card(&t("input-sources-title"), rows(source_rows))
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
    fn tablet_cards(&self) -> Vec<El> {
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
                card(
                    &device_title(device),
                    rows(vec![
                        choice_row(
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
                            Box::new(self.device_slider(
                                &resource,
                                &device.key,
                                "tip-feel",
                                settings_store::hardware::feel_for_pressure_curve(
                                    config.pressure_curve,
                                ),
                            )),
                        ),
                        choice_row(
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
                        choice_row(
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
                        choice_row_described(
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
                        choice_row_described(
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
    fn pad_cards(&self) -> Vec<El> {
        self.pads
            .iter()
            .map(|device| {
                let config = self.tablet_config(device);
                let resource = pad_resource_id(&device.key);
                let mut items: Vec<El> = (0..device.pad_buttons.min(MAX_PAD_BUTTONS))
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
                        choice_row(
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
                    items.push(caption(&t("input-pad-no-buttons")));
                }
                for (dial, modes) in pad_dials(device) {
                    for mode in 0..modes {
                        let label = dial_label(dial, mode, modes);
                        items.push(choice_row(
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
                    items.push(caption(&t("input-pad-dials-caption")));
                }
                card(&device_title(device), rows(items))
            })
            .collect()
    }
}

impl Widget for MouseKeyboardPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The controls are built from the values: a change rebuilds them.
        if self.input.refresh() {
            let mut last = self.input.get().clone();
            last.normalise_keyboard_sources();
            self.last = last;
            self.structure_dirty = true;
        }
        if self.text.refresh() {
            self.structure_dirty = true;
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // `child` was just created in this same tree, so this call is not
        // reachable to fail; if it (impossibly) did, wrap it in itself
        // rather than panic this panic=abort process.
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "MouseKeyboardPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn pane_with(input: &InputPrefs, catalog: Vec<CatalogEntry>) -> MouseKeyboardPane {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<InputPrefs>().set(input).unwrap();
        let area = InputArea {
            catalog,
            catalog_error: None,
            live: false,
            devices: RefCell::new((None, None)),
        };
        MouseKeyboardPane::new(cx, &area)
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
                "Repeat is off — move the slider to turn it on"
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
                "37 keys per second · starts after 275 ms"
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
        assert_eq!(dial_label(PadDial::Ring(0), 1, 4), "Ring 1 · Mode 2");
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

    fn set(app: &mut TestApp<SettingsShell>, effect: InputEffect) -> bool {
        app.effect(&SettingsEffect::from(effect)).is_ok()
    }

    /// Pointer and keyboard choices are the person's `input` section, the
    /// touchpad switch this device's, spelling the shared
    /// `text_assistance` section; a value outside the offered ones is refused.
    #[test]
    fn input_effects_write_the_input_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        assert!(set(&mut app, InputEffect::SetTouchpadSpeed { speed: 0.9 }));
        assert!(set(
            &mut app,
            InputEffect::SetMouseNaturalScroll { on: true }
        ));
        assert!(set(&mut app, InputEffect::SetTouchpadEnabled { on: false }));
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
        assert!(set(
            &mut app,
            InputEffect::SetTouchpadClickMethod {
                method: "areas".into()
            }
        ));
        assert!(set(&mut app, InputEffect::SetSpellChecking { on: false }));
        let input = app.settings::<InputPrefs>().get();
        assert_eq!(input.touchpad_pointer_speed, Some(0.9));
        assert_eq!(input.mouse_natural_scroll, Some(true));
        assert!(!input.touchpad_enabled);
        assert_eq!(input.compose_key.as_deref(), Some("ralt"));
        assert_eq!(input.caps_lock.as_deref(), Some("ctrl"));
        assert_eq!(input.touchpad_click_method, TouchpadClickMethod::Areas);
        assert!(!app.settings::<TextAssistancePrefs>().get().check_spelling);
        assert!(set(
            &mut app,
            InputEffect::SetSpellingLanguage {
                language: "de_DE".into()
            }
        ));
        assert_eq!(
            app.settings::<TextAssistancePrefs>()
                .get()
                .spelling_language,
            "de_DE"
        );
        assert!(!set(
            &mut app,
            InputEffect::SetSpellingLanguage {
                language: "xx_YY".into()
            }
        ));
        assert!(set(
            &mut app,
            InputEffect::SetSpellingLanguage {
                language: String::new()
            }
        ));
        assert_eq!(
            app.settings::<TextAssistancePrefs>()
                .get()
                .spelling_language,
            "",
            "empty is the session's language"
        );

        assert!(!set(&mut app, InputEffect::SetMouseSpeed { speed: 4.0 }));
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
