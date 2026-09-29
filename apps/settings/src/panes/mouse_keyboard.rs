#![allow(
    clippy::unreachable,
    reason = "static control builders accept only their matching closed InputOperation variant"
)]

//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! Every control drives real, durable state:
//! - **Pointer speed / per-device natural scrolling / Tap to click / Key repeat** are
//!   persisted to the shared preferences store
//!   ([`crate::prefs::SystemPrefs`]); the compositor reads them when it
//!   configures libinput for the session.
//! - **Keyboard input sources** are per-user durable intent. Settings never
//!   invokes `localectl`; the compositor owns XKB and hot-reloads this file.
//
// CRDT keys this pane reads / writes:
//   `input.touchpad_pointer_speed` — f64 in `[0.0, 1.0]`
//   `input.mouse_pointer_speed`    — f64 in `[0.0, 1.0]`
//   `input.touchpad_natural_scroll` — bool
//   `input.mouse_natural_scroll`    — bool
//   `input.tap_to_click`     — bool
//   `input.key_repeat`       — f64 in `[0.0, 1.0]`
//   `input.touchpad_click_method` / `input.touchpad_scroll_method` — choice
//   `input.scroll_speed`     — multiplier; the slider edits its log position
//   `input.keyboard_layout`  — active source display label

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use settings_store::hardware::{
    EraserAction, InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot, PadDial,
    PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig, TouchpadClickMethod,
    TouchpadScrollMethod,
};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::doc::{read_bool, read_str_or_text};
use ui::i18n::ArgValue;
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{Button, Dropdown, Row, Slider, Spacer, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};
use crate::input_sources::{CatalogEntry, installed_catalog};
use crate::prefs::{InputPrefs, SystemPrefs};
use crate::widgets::{
    El, caption, card, choice_row, choice_row_described, rows, setting_row, setting_row_described,
};

fn bool_action(id: &str) -> tilo_actions::ActionBinding<bool> {
    match crate::actions::input_bool_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn number_action(id: &str) -> tilo_actions::ActionBinding<f64> {
    match crate::actions::input_number_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn text_action(id: &str) -> tilo_actions::ActionBinding<String> {
    match crate::actions::input_text_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn source_action_binding(source: &str, action: &str) -> tilo_actions::ActionBinding<()> {
    match crate::actions::keyboard_source_binding(source, action) {
        Some(binding) => binding,
        None => unreachable!("keyboard source action must be published"),
    }
}

pub const POINTER_SPEED_KEY: &str = "input.pointer_speed";
pub const TOUCHPAD_POINTER_SPEED_KEY: &str = "input.touchpad_pointer_speed";
pub const MOUSE_POINTER_SPEED_KEY: &str = "input.mouse_pointer_speed";
pub const NATURAL_SCROLL_KEY: &str = "input.natural_scroll";
pub const TOUCHPAD_NATURAL_SCROLL_KEY: &str = "input.touchpad_natural_scroll";
pub const MOUSE_NATURAL_SCROLL_KEY: &str = "input.mouse_natural_scroll";
pub const TAP_CLICK_KEY: &str = "input.tap_to_click";
pub const KEY_REPEAT_KEY: &str = "input.key_repeat";
pub const LAYOUT_KEY: &str = "input.keyboard_layout";
pub const TAP_DRAG_KEY: &str = "input.tap_drag";
pub const DWT_KEY: &str = "input.disable_while_typing";
pub const MIDDLE_EMULATION_KEY: &str = "input.middle_emulation";
pub const LEFT_HANDED_KEY: &str = "input.left_handed";
pub const ACCEL_PROFILE_KEY: &str = "input.accel_profile";
pub const GESTURES_KEY: &str = "input.touchpad_gestures";
pub const COMPOSE_KEY: &str = "input.compose_key";
pub const CAPS_LOCK_KEY: &str = "input.caps_lock";
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
pub const ADD_SOURCE_PICKER_KEY: &str = "input.source.add_picker";
pub const ADD_SOURCE_KEY: &str = "input.source.add";
pub const CHECK_SPELLING_KEY: &str = "text.check_spelling";

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

/// A dropdown whose values are stable ids and whose rows are translated.
fn id_dropdown(
    choices: Vec<(String, String)>,
    selected: &str,
    action: &str,
    test_id: &str,
    accessible_name: &str,
) -> Dropdown {
    let mut dropdown = Dropdown::from_state(
        choices.iter().map(|(id, _)| id.clone()).collect(),
        &ui::LocalState::new(selected.to_string()),
    )
    .on_action(text_action(action))
    .with_id(test_id)
    .with_accessible_name(accessible_name);
    dropdown.set_value_options(choices);
    dropdown
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

/// A dropdown over stable ids for one of a tablet's or pad's actions. It is
/// shown disabled if the device vanished between listing and binding.
fn device_dropdown(
    resource: &str,
    action: &str,
    choices: Vec<(String, String)>,
    selected: &str,
    accessible_name: &str,
) -> Dropdown {
    let mut dropdown = Dropdown::from_state(
        choices.iter().map(|(id, _)| id.clone()).collect(),
        &ui::LocalState::new(selected.to_string()),
    )
    .with_id(&format!("{resource}.{action}"))
    .with_accessible_name(accessible_name);
    dropdown = match crate::actions::tablet_text_binding(resource, action) {
        Some(binding) => dropdown.on_action(binding),
        None => dropdown.with_enabled(false),
    };
    dropdown.set_value_options(choices);
    dropdown
}

fn device_toggle(resource: &str, action: &str, on: bool) -> Toggle {
    let toggle =
        Toggle::from_state(&ui::LocalState::new(on)).with_id(&format!("{resource}.{action}"));
    match crate::actions::tablet_bool_binding(resource, action) {
        Some(binding) => toggle.on_action(binding),
        None => toggle.with_enabled(false),
    }
}

fn device_slider(resource: &str, action: &str, value: f64) -> Slider {
    let slider =
        Slider::from_state(&ui::LocalState::new(value)).with_id(&format!("{resource}.{action}"));
    match crate::actions::tablet_number_binding(resource, action) {
        Some(binding) => slider.on_action(binding),
        None => slider.with_enabled(false),
    }
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

/// How often the pane re-reads the attached-device snapshot, so plugging a
/// mouse in shows its section without reopening Settings.
const DEVICE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
pub const AUTO_CORRECT_KEY: &str = "text.auto_correct";

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independently-toggled preference paired with its \
              initial value for dirty-tracking; no combination is invalid"
)]
#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
pub struct MouseKeyboardPane {
    initial: InputPrefs,
    last: InputPrefs,
    initial_check_spelling: bool,
    check_spelling: bool,
    initial_auto_correct: bool,
    auto_correct: bool,
    catalog: Vec<CatalogEntry>,
    pending_add_id: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    /// Which sections the attached devices call for.
    sections: InputSections,
    /// Attached drawing tablets and pads, one card each.
    tablets: Vec<InputDeviceSnapshot>,
    pads: Vec<InputDeviceSnapshot>,
    /// Whether the sections come from a live compositor snapshot; a pane
    /// built without one (tests, headless renders) never polls for one.
    poll_devices: bool,
    next_device_poll: Option<ui::time::Instant>,
    needs_seed: bool,
    structure_dirty: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for MouseKeyboardPane {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
impl MouseKeyboardPane {
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }

    fn operation_succeeded(
        &self,
        operation: &crate::actions::KeyboardSourceOperation,
        message: impl Into<String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE,
            operation,
        );
        if matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: active } if active == token
        ) {
            self.operation_status
                .set(crate::actions::LocalOperationStatus::Succeeded {
                    token,
                    message: message.into(),
                });
        }
    }

    pub fn new() -> Self {
        let mut prefs = SystemPrefs::load().input;
        prefs.normalise_keyboard_sources();
        let text = settings_store::load_section(ui::text_assistance::SETTINGS_SECTION);
        let check_spelling = text
            .get(ui::text_assistance::CHECK_SPELLING_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let auto_correct = text
            .get(ui::text_assistance::AUTO_CORRECT_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let mut pane = Self {
            initial: prefs.clone(),
            last: prefs,
            initial_check_spelling: check_spelling,
            check_spelling,
            initial_auto_correct: auto_correct,
            auto_correct,
            catalog: installed_catalog(),
            pending_add_id: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            sections: InputSections::for_devices(None),
            tablets: Vec::new(),
            pads: Vec::new(),
            poll_devices: true,
            next_device_poll: None,
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        };
        pane.observe_devices(InputHardwareSnapshot::load().as_ref());
        pane.structure_dirty = false;
        pane
    }

    /// Test constructor with a deterministic installed-source catalogue.
    #[cfg(test)]
    fn with_catalog(mut initial: InputPrefs, catalog: Vec<CatalogEntry>) -> Self {
        initial.normalise_keyboard_sources();
        Self {
            initial: initial.clone(),
            last: initial,
            initial_check_spelling: true,
            check_spelling: true,
            initial_auto_correct: true,
            auto_correct: true,
            catalog,
            pending_add_id: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            sections: InputSections::for_devices(None),
            tablets: Vec::new(),
            pads: Vec::new(),
            poll_devices: false,
            next_device_poll: None,
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn source_label(&self, id: &str) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.label.clone())
    }

    fn source_id(&self, label: &str) -> Option<String> {
        self.catalog
            .iter()
            .find(|entry| entry.label == label)
            .map(|entry| entry.id.clone())
            .or_else(|| {
                self.last
                    .keyboard_layouts
                    .iter()
                    .find(|id| id.as_str() == label)
                    .cloned()
            })
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards = vec![
            self.keyboard_card(),
            self.spell_checker_card(),
            self.input_sources_card(),
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

    /// Re-read the attached devices at most once per [`DEVICE_POLL_INTERVAL`]
    /// and rebuild when a section appears or disappears.
    fn poll_devices(&mut self) {
        if !self.poll_devices {
            return;
        }
        let now = ui::time::now();
        if self.next_device_poll.is_some_and(|next| now < next) {
            return;
        }
        self.next_device_poll = Some(now + DEVICE_POLL_INTERVAL);
        self.observe_devices(InputHardwareSnapshot::load().as_ref());
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

    fn bool_row(label: &str, description: &str, action: &str, id: &str, on: bool) -> El {
        setting_row_described(
            &t(label),
            &t(description),
            Box::new(
                Toggle::from_state(&ui::LocalState::new(on))
                    .on_action(bool_action(action))
                    .with_id(id),
            ),
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
                Self::bool_row(
                    "input-touchpad-enabled",
                    "input-touchpad-enabled-description",
                    "touchpad-enabled",
                    "input.touchpad_enabled",
                    last.touchpad_enabled,
                ),
                Self::bool_row(
                    "input-touchpad-disable-with-mouse",
                    "input-touchpad-disable-with-mouse-description",
                    "touchpad-disabled-with-mouse",
                    "input.touchpad_disabled_on_external_mouse",
                    last.touchpad_disabled_on_external_mouse,
                ),
                setting_row(
                    &t("input-touchpad-speed"),
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            last.touchpad_pointer_speed.unwrap_or(last.pointer_speed),
                        ))
                        .on_action(number_action("touchpad-speed"))
                        .with_id("input.touchpad_pointer_speed"),
                    ),
                ),
                Self::bool_row(
                    "input-touchpad-natural-scroll",
                    "input-touchpad-natural-scroll-description",
                    "touchpad-natural-scroll",
                    "input.touchpad_natural_scroll",
                    last.touchpad_natural_scroll.unwrap_or(last.natural_scroll),
                ),
                Self::bool_row(
                    "input-tap-to-click",
                    "input-tap-to-click-description",
                    "tap-to-click",
                    "input.tap_click",
                    last.tap_to_click,
                ),
                Self::bool_row(
                    "input-tap-drag",
                    "input-tap-drag-description",
                    "tap-drag",
                    "input.tap_drag",
                    last.tap_drag,
                ),
                Self::bool_row(
                    "input-tap-drag-lock",
                    "input-tap-drag-lock-description",
                    "tap-drag-lock",
                    "input.tap_drag_lock",
                    last.tap_drag_lock,
                ),
                Self::bool_row(
                    "input-disable-while-typing",
                    "input-disable-while-typing-description",
                    "disable-while-typing",
                    "input.disable_while_typing",
                    last.disable_while_typing,
                ),
                Self::bool_row(
                    "input-touchpad-gestures",
                    "input-touchpad-gestures-description",
                    "touchpad-gestures",
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
                    Box::new(id_dropdown(
                        localized_choices(&CLICK_METHOD_CHOICES),
                        click_method_id(self.last.touchpad_click_method),
                        "touchpad-click-method",
                        "input.touchpad_click_method",
                        &t("input-click-method"),
                    )),
                ),
                caption(&t("input-click-method-caption")),
                choice_row(
                    &t("input-scroll-method"),
                    Box::new(id_dropdown(
                        localized_choices(&SCROLL_METHOD_CHOICES),
                        scroll_method_id(self.last.touchpad_scroll_method),
                        "touchpad-scroll-method",
                        "input.touchpad_scroll_method",
                        &t("input-scroll-method"),
                    )),
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
                        .on_action(number_action("mouse-speed"))
                        .with_id("input.mouse_pointer_speed"),
                    ),
                ),
                Self::bool_row(
                    "input-mouse-natural-scroll",
                    "input-mouse-natural-scroll-description",
                    "mouse-natural-scroll",
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
                Self::bool_row(
                    "input-left-handed",
                    "input-left-handed-description",
                    "left-handed",
                    "input.left_handed",
                    self.last.left_handed,
                ),
                Self::bool_row(
                    "input-middle-emulation",
                    "input-middle-emulation-description",
                    "middle-emulation",
                    "input.middle_emulation",
                    self.last.middle_emulation,
                ),
                choice_row(
                    &t("input-acceleration"),
                    Box::new(
                        Dropdown::from_state(
                            vec!["Adaptive".into(), "Flat".into()],
                            &ui::LocalState::new(if self.last.accel_profile == "flat" {
                                "Flat".into()
                            } else {
                                "Adaptive".into()
                            }),
                        )
                        .on_action(text_action("acceleration-profile"))
                        .with_id("input.accel_profile")
                        .with_placeholder(
                            if self.initial.accel_profile == "flat" {
                                "Flat"
                            } else {
                                "Adaptive"
                            },
                        ),
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
                        .on_action(number_action("scroll-speed"))
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
        card(
            "Keyboard",
            rows(vec![
                choice_row(
                    "Active input source",
                    Box::new(
                        Dropdown::from_state(
                            active_sources,
                            &ui::LocalState::new(self.source_label(&self.last.keyboard_layout)),
                        )
                        .on_action(text_action("active-source"))
                        .with_id("input.layout")
                        .with_placeholder(&self.source_label(&self.last.keyboard_layout)),
                    ),
                ),
                setting_row_described(
                    "Key repeat speed",
                    &repeat_description,
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(repeat_value))
                            .on_action(number_action("key-repeat"))
                            .with_id("input.key_repeat"),
                    ),
                ),
                caption(
                    "Changes apply to this user session through the compositor. With multiple sources, Super+Alt+Space switches to the next one and remembers it.",
                ),
                choice_row(
                    "Compose key",
                    Box::new(
                        Dropdown::from_state(
                            vec![
                                "Disabled".into(),
                                "Right Alt".into(),
                                "Menu".into(),
                                "Caps Lock".into(),
                                "Right Super".into(),
                                "Left Super".into(),
                            ],
                            &ui::LocalState::new(match self.last.compose_key.as_deref() {
                                Some("ralt") => "Right Alt".into(),
                                Some("menu") => "Menu".into(),
                                Some("caps") => "Caps Lock".into(),
                                Some("rwin") => "Right Super".into(),
                                Some("lwin") => "Left Super".into(),
                                _ => "Disabled".into(),
                            }),
                        )
                        .on_action(text_action("compose-key"))
                        .with_id("input.compose_key")
                        .with_placeholder(
                            match self.initial.compose_key.as_deref() {
                                Some("ralt") => "Right Alt",
                                Some("menu") => "Menu",
                                Some("caps") => "Caps Lock",
                                Some("rwin") => "Right Super",
                                Some("lwin") => "Left Super",
                                _ => "Disabled",
                            },
                        ),
                    ),
                ),
                choice_row(
                    "Caps Lock key",
                    Box::new(
                        Dropdown::from_state(
                            CAPS_LOCK_CHOICES
                                .iter()
                                .map(|(label, _)| (*label).to_string())
                                .collect(),
                            &ui::LocalState::new(
                                caps_lock_label(self.last.caps_lock.as_deref()).to_string(),
                            ),
                        )
                        .on_action(text_action("caps-lock"))
                        .with_id("input.caps_lock")
                        .with_placeholder(caps_lock_label(self.initial.caps_lock.as_deref())),
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
            (Some(_), Some(0)) => "Repeat is off — move the slider to turn it on".into(),
            (Some(delay), Some(rate)) => {
                format!("{rate} keys per second · starts after {delay} ms")
            }
            _ => "Faster repeat starts sooner".into(),
        }
    }

    fn spell_checker_card(&self) -> Box<dyn Widget> {
        card(
            "Spell checker",
            rows(vec![
                setting_row_described(
                    "Check spelling while typing",
                    "Underline possible misspellings in natural-language fields",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.check_spelling))
                            .on_action(bool_action("spell-checking"))
                            .with_id("text.check_spelling"),
                    ),
                ),
                setting_row_described(
                    "Auto-correct obvious misspellings",
                    "Apply only high-confidence corrections after Space, punctuation, or Enter",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.auto_correct))
                            .on_action(bool_action("auto-correct"))
                            .with_id("text.auto_correct"),
                    ),
                ),
                caption(
                    "Press Backspace immediately to restore the original word. Passwords are always excluded; other structured fields require an app to opt in.",
                ),
            ]),
        )
    }

    fn input_sources_card(&self) -> Box<dyn Widget> {
        let mut source_rows = Vec::new();
        for (index, id) in self.last.keyboard_layouts.iter().enumerate() {
            let mut row = Row::new()
                .push(Box::new(Text::new(&self.source_label(id)).with_size(13.0)))
                .push(Box::new(Spacer::new()));
            if id == &self.last.keyboard_layout {
                row = row.push(Box::new(Text::new("Active").with_size(12.0)));
            }
            let up = format!("input.source.{index}.up");
            let down = format!("input.source.{index}.down");
            let remove = format!("input.source.{index}.remove");
            let mut up_button = Button::new(&up, "↑", Some(&up))
                .with_size(36.0, 34.0)
                .with_accessible_name("Move input source up")
                .with_enabled(index > 0);
            if index > 0 {
                up_button = up_button.on_action(source_action_binding(id, "move-up"));
            }
            let mut down_button = Button::new(&down, "↓", Some(&down))
                .with_size(36.0, 34.0)
                .with_accessible_name("Move input source down")
                .with_enabled(index + 1 < self.last.keyboard_layouts.len());
            if index + 1 < self.last.keyboard_layouts.len() {
                down_button = down_button.on_action(source_action_binding(id, "move-down"));
            }
            let mut remove_button = Button::new(&remove, "Remove", Some(&remove))
                .with_size(84.0, 34.0)
                .with_enabled(self.last.keyboard_layouts.len() > 1);
            if self.last.keyboard_layouts.len() > 1 {
                remove_button = remove_button.on_action(source_action_binding(id, "remove"));
            }
            row = row
                .push(Box::new(up_button))
                .push(Box::new(down_button))
                .push(Box::new(remove_button));
            source_rows.push(Box::new(row) as Box<dyn Widget>);
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
                "Add a language, layout or input method",
                Box::new(
                    Dropdown::from_state(
                        available.clone(),
                        &ui::LocalState::new(available.first().cloned().unwrap_or_default()),
                    )
                    .on_action(text_action("add-source"))
                    .with_id("input.source.add_picker")
                    .with_placeholder("Choose a source"),
                ),
            ));
        }
        if crate::input_sources::layouts_full(&self.last.keyboard_layouts) {
            source_rows.push(caption(
                "Up to four keyboard layouts can be active at once, matching the interoperable XKB group limit.",
            ));
        }
        source_rows.push(caption(
            "Input methods (Rime, Anthy, Hangul) compose text on your first keyboard layout. While one is active, the on-screen keyboard steps aside.",
        ));
        card("Input sources", rows(source_rows))
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
                            Box::new(device_dropdown(
                                &resource,
                                "map-to",
                                outputs.clone(),
                                &mapped,
                                &t("input-tablet-map-to"),
                            )),
                        ),
                        setting_row_described(
                            &t("input-tablet-keep-aspect"),
                            &t("input-tablet-keep-aspect-description"),
                            Box::new(device_toggle(&resource, "keep-aspect", config.keep_aspect)),
                        ),
                        setting_row_described(
                            &t("input-tablet-left-handed"),
                            &t("input-tablet-left-handed-description"),
                            Box::new(device_toggle(&resource, "left-handed", config.left_handed)),
                        ),
                        setting_row_described(
                            &t("input-tablet-tip-feel"),
                            &t("input-tablet-tip-feel-description"),
                            Box::new(device_slider(
                                &resource,
                                "tip-feel",
                                settings_store::hardware::feel_for_pressure_curve(
                                    config.pressure_curve,
                                ),
                            )),
                        ),
                        choice_row(
                            &t("input-tablet-lower-button"),
                            Box::new(device_dropdown(
                                &resource,
                                "lower-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.lower_button),
                                &t("input-tablet-lower-button"),
                            )),
                        ),
                        choice_row(
                            &t("input-tablet-upper-button"),
                            Box::new(device_dropdown(
                                &resource,
                                "upper-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.upper_button),
                                &t("input-tablet-upper-button"),
                            )),
                        ),
                        choice_row_described(
                            &t("input-tablet-third-button"),
                            &t("input-tablet-third-button-description"),
                            Box::new(device_dropdown(
                                &resource,
                                "third-button",
                                stylus_button_choices(),
                                &stylus_button_id(config.third_button),
                                &t("input-tablet-third-button"),
                            )),
                        ),
                        choice_row_described(
                            &t("input-tablet-eraser"),
                            &t("input-tablet-eraser-description"),
                            Box::new(device_dropdown(
                                &resource,
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
                            Box::new(device_dropdown(
                                &resource,
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
                            Box::new(device_dropdown(
                                &resource,
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

    /// Load prefs fresh, apply `f` to the input section, persist.
    fn persist_input(f: impl FnOnce(&mut InputPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.input);
        p.input.normalise_keyboard_sources();
        let section = serde_json::to_value(&p.input)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Err(e) = settings_store::update_section("input", section) {
            warn!(target: "tilo::settings", op = "input_prefs_save", error = %e, "failed to persist input prefs");
        }
    }

    fn persist_text_preference(key: &str, value: bool) {
        let mut section = serde_json::Map::new();
        section.insert(key.to_string(), serde_json::Value::Bool(value));
        if let Err(e) =
            settings_store::update_section(ui::text_assistance::SETTINGS_SECTION, section)
        {
            warn!(target: "tilo::settings", op = "text_assistance_save", error = %e, "failed to persist text-assistance prefs");
        }
    }

    fn persist_keyboard_sources(&self) {
        let active = self.last.keyboard_layout.clone();
        let sources = self.last.keyboard_layouts.clone();
        Self::persist_input(|input| {
            input.keyboard_layout = active;
            input.keyboard_layouts = sources;
        });
    }

    fn source_action(index: usize, action: &str) -> String {
        format!("input.source.{index}.{action}")
    }

    fn consume_action(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
        if read_bool(state, root, key) != Some(true) {
            return false;
        }
        let _ = state.put(root, key, automerge::ScalarValue::Boolean(false));
        true
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(MouseKeyboardPane::new())
}

#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
impl MouseKeyboardPane {
    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            // Hardware preferences are deliberately never seeded into the
            // collaborative document. Their durable local source is the
            // input section of settings.json and all mutations arrive as
            // typed device-local effects.
            let _ = state.put(root, CHECK_SPELLING_KEY, self.initial_check_spelling);
            let _ = state.put(root, AUTO_CORRECT_KEY, self.initial_auto_correct);
            self.needs_seed = false;
        }
        /* Legacy CRDT seeding removed: input hardware preferences are local effects.
            let _ = state.put(
                root,
                TOUCHPAD_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(
                    self.initial
                        .touchpad_pointer_speed
                        .unwrap_or(self.initial.pointer_speed),
                ),
            );
            let _ = state.put(
                root,
                MOUSE_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(
                    self.initial
                        .mouse_pointer_speed
                        .unwrap_or(self.initial.pointer_speed),
                ),
            );
            let _ = state.put(root, TAP_DRAG_KEY, self.initial.tap_drag);
            let _ = state.put(root, DWT_KEY, self.initial.disable_while_typing);
            let _ = state.put(root, MIDDLE_EMULATION_KEY, self.initial.middle_emulation);
            let _ = state.put(root, LEFT_HANDED_KEY, self.initial.left_handed);
            let _ = state.put(root, GESTURES_KEY, self.initial.touchpad_gestures);
            let _ = state.put(
                root,
                ACCEL_PROFILE_KEY,
                if self.initial.accel_profile == "flat" {
                    "Flat"
                } else {
                    "Adaptive"
                },
            );
            let compose = match self.initial.compose_key.as_deref() {
                Some("ralt") => "Right Alt",
                Some("menu") => "Menu",
                Some("caps") => "Caps Lock",
                _ => "Disabled",
            };
            let _ = state.put(root, COMPOSE_KEY, compose);
            let _ = state.put(
                root,
                CAPS_LOCK_KEY,
                caps_lock_label(self.initial.caps_lock.as_deref()),
            );
            let _ = state.put(
                root,
                TOUCHPAD_NATURAL_SCROLL_KEY,
                automerge::ScalarValue::Boolean(
                    self.initial
                        .touchpad_natural_scroll
                        .unwrap_or(self.initial.natural_scroll),
                ),
            );
            let _ = state.put(
                root,
                MOUSE_NATURAL_SCROLL_KEY,
                automerge::ScalarValue::Boolean(
                    self.initial
                        .mouse_natural_scroll
                        .unwrap_or(self.initial.natural_scroll),
                ),
            );
            let _ = state.put(
                root,
                TAP_CLICK_KEY,
                automerge::ScalarValue::Boolean(self.initial.tap_to_click),
            );
            let _ = state.put(
                root,
                KEY_REPEAT_KEY,
                automerge::ScalarValue::F64(self.initial.key_repeat),
            );
            let _ = state.put(
                root,
                LAYOUT_KEY,
                self.source_label(&self.initial.keyboard_layout),
            );
            let first_available = self
                .catalog
                .iter()
                .find(|entry| !self.initial.keyboard_layouts.contains(&entry.id))
                .cloned();
            if let Some(entry) = first_available {
                let _ = state.put(root, ADD_SOURCE_PICKER_KEY, entry.label);
                self.pending_add_id = Some(entry.id);
            }
            let _ = state.put(root, ADD_SOURCE_KEY, false);
            let _ = state.put(root, CHECK_SPELLING_KEY, self.initial_check_spelling);
            let _ = state.put(root, AUTO_CORRECT_KEY, self.initial_auto_correct);
            self.last = self.initial.clone();
            self.needs_seed = false;
        */
    }

    fn sync_simple_fields(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(value) = read_bool(state, root, CHECK_SPELLING_KEY)
            && value != self.check_spelling
        {
            self.check_spelling = value;
            Self::persist_text_preference(ui::text_assistance::CHECK_SPELLING_KEY, value);
        }
        if let Some(value) = read_bool(state, root, AUTO_CORRECT_KEY)
            && value != self.auto_correct
        {
            self.auto_correct = value;
            Self::persist_text_preference(ui::text_assistance::AUTO_CORRECT_KEY, value);
        }
        /* Legacy CRDT readers removed: a remote document never applies input hardware state.
            if let Some(v) = read_f64(state, root, TOUCHPAD_POINTER_SPEED_KEY)
                && self
                    .last
                    .touchpad_pointer_speed
                    .is_none_or(|last| (v - last).abs() > 0.001)
            {
                self.last.touchpad_pointer_speed = Some(v);
                Self::persist_input(|i| i.touchpad_pointer_speed = Some(v));
            }
            if let Some(v) = read_f64(state, root, MOUSE_POINTER_SPEED_KEY)
                && self
                    .last
                    .mouse_pointer_speed
                    .is_none_or(|last| (v - last).abs() > 0.001)
            {
                self.last.mouse_pointer_speed = Some(v);
                Self::persist_input(|i| i.mouse_pointer_speed = Some(v));
            }
            if let Some(v) = read_f64(state, root, KEY_REPEAT_KEY)
                && (v - self.last.key_repeat).abs() > 0.001
            {
                self.last.key_repeat = v;
                Self::persist_input(|i| {
                    i.key_repeat = v;
                    i.key_repeat_delay_ms = None;
                    i.key_repeat_rate_hz = None;
                });
            }
            if let Some(b) = read_bool(state, root, TOUCHPAD_NATURAL_SCROLL_KEY)
                && Some(b) != self.last.touchpad_natural_scroll
            {
                self.last.touchpad_natural_scroll = Some(b);
                Self::persist_input(|i| i.touchpad_natural_scroll = Some(b));
            }
            if let Some(b) = read_bool(state, root, MOUSE_NATURAL_SCROLL_KEY)
                && Some(b) != self.last.mouse_natural_scroll
            {
                self.last.mouse_natural_scroll = Some(b);
                Self::persist_input(|i| i.mouse_natural_scroll = Some(b));
            }
            if let Some(b) = read_bool(state, root, TAP_CLICK_KEY)
                && b != self.last.tap_to_click
            {
                self.last.tap_to_click = b;
                Self::persist_input(|i| i.tap_to_click = b);
            }
            for (key, current, apply) in [
                (TAP_DRAG_KEY, self.last.tap_drag, 0u8),
                (DWT_KEY, self.last.disable_while_typing, 1),
                (MIDDLE_EMULATION_KEY, self.last.middle_emulation, 2),
                (LEFT_HANDED_KEY, self.last.left_handed, 3),
                (GESTURES_KEY, self.last.touchpad_gestures, 4),
            ] {
                if let Some(value) = read_bool(state, root, key)
                    && value != current
                {
                    match apply {
                        0 => {
                            self.last.tap_drag = value;
                            Self::persist_input(|i| i.tap_drag = value);
                        }
                        1 => {
                            self.last.disable_while_typing = value;
                            Self::persist_input(|i| i.disable_while_typing = value);
                        }
                        2 => {
                            self.last.middle_emulation = value;
                            Self::persist_input(|i| i.middle_emulation = value);
                        }
                        3 => {
                            self.last.left_handed = value;
                            Self::persist_input(|i| i.left_handed = value);
                        }
                        _ => {
                            self.last.touchpad_gestures = value;
                            Self::persist_input(|i| i.touchpad_gestures = value);
                        }
                    }
                }
            }
        */
    }

    fn sync_choice_fields(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(label) = read_str_or_text(state, root, ACCEL_PROFILE_KEY) {
            let profile = if label == "Flat" { "flat" } else { "adaptive" };
            if profile != self.last.accel_profile {
                self.last.accel_profile = profile.into();
                Self::persist_input(|i| i.accel_profile = profile.into());
            }
        }
        if let Some(label) = read_str_or_text(state, root, COMPOSE_KEY) {
            let compose = match label.as_str() {
                "Right Alt" => Some("ralt"),
                "Menu" => Some("menu"),
                "Caps Lock" => Some("caps"),
                "Right Super" => Some("rwin"),
                "Left Super" => Some("lwin"),
                _ => None,
            };
            if compose != self.last.compose_key.as_deref() {
                self.last.compose_key = compose.map(str::to_string);
                let owned = self.last.compose_key.clone();
                Self::persist_input(|i| i.compose_key = owned);
            }
        }
        if let Some(label) = read_str_or_text(state, root, CAPS_LOCK_KEY)
            && let Some((_, stored)) = CAPS_LOCK_CHOICES
                .iter()
                .find(|(choice, _)| *choice == label)
            && *stored != self.last.caps_lock.as_deref()
        {
            self.last.caps_lock = stored.map(str::to_string);
            let owned = self.last.caps_lock.clone();
            Self::persist_input(|i| i.caps_lock = owned);
        }
        // Active source is durable preference only. The session compositor's
        // file watch compiles and applies it without a privileged helper.
        if let Some(label) = read_str_or_text(state, root, LAYOUT_KEY)
            && let Some(id) = self.source_id(&label)
            && self.last.keyboard_layouts.contains(&id)
            && id != self.last.keyboard_layout
        {
            self.last.keyboard_layout = id;
            self.persist_keyboard_sources();
            self.structure_dirty = true;
        }
    }

    fn sync_add_source(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(label) = read_str_or_text(state, root, ADD_SOURCE_PICKER_KEY) {
            self.pending_add_id = self.source_id(&label);
        }
        if Self::consume_action(state, root, ADD_SOURCE_KEY)
            && let Some(id) = self.pending_add_id.clone()
            && !self.last.keyboard_layouts.contains(&id)
            && crate::input_sources::has_room_for(&self.last.keyboard_layouts, &id)
        {
            self.last.keyboard_layouts.push(id.clone());
            self.last.keyboard_layout = id;
            let _ = state.put(
                root,
                LAYOUT_KEY,
                self.source_label(&self.last.keyboard_layout),
            );
            self.persist_keyboard_sources();
            self.operation_succeeded(
                &crate::actions::KeyboardSourceOperation::Add {
                    label: self.source_label(&self.last.keyboard_layout),
                },
                "keyboard input source added",
            );
            self.structure_dirty = true;
        }
    }

    fn sync_source_order(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let source_count = self.last.keyboard_layouts.len();
        for index in 0..source_count {
            let up = Self::source_action(index, "up");
            let down = Self::source_action(index, "down");
            let remove = Self::source_action(index, "remove");
            if index > 0 && Self::consume_action(state, root, &up) {
                self.last.keyboard_layouts.swap(index - 1, index);
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::MoveUp { index },
                    "keyboard input source moved up",
                );
                self.structure_dirty = true;
                break;
            }
            if index + 1 < source_count && Self::consume_action(state, root, &down) {
                self.last.keyboard_layouts.swap(index, index + 1);
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::MoveDown { index },
                    "keyboard input source moved down",
                );
                self.structure_dirty = true;
                break;
            }
            if source_count > 1 && Self::consume_action(state, root, &remove) {
                let removed = self.last.keyboard_layouts.remove(index);
                if removed == self.last.keyboard_layout {
                    let fallback = index.min(self.last.keyboard_layouts.len().saturating_sub(1));
                    if let Some(new_active) = self.last.keyboard_layouts.get(fallback) {
                        self.last.keyboard_layout = new_active.clone();
                        let _ = state.put(
                            root,
                            LAYOUT_KEY,
                            self.source_label(&self.last.keyboard_layout),
                        );
                    }
                }
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::Remove { index },
                    "keyboard input source removed",
                );
                self.structure_dirty = true;
                break;
            }
        }
    }
}

impl Widget for MouseKeyboardPane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.poll_devices();
        self.seed_if_needed(state, root);
        self.sync_simple_fields(state, root);
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
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
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

impl ui::undo::UndoPoints for MouseKeyboardPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        // The add/reorder/remove source edge keys are transient internally,
        // but they exist only to apply a durable keyboard-source preference.
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "input",
            "Mouse & keyboard",
            "Change input preferences",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut pane = build();
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }

        // Case `imported_repeat_settings_are_shown_as_the_live_rate_or_disabled`.
        {
            let disabled = MouseKeyboardPane::with_catalog(
                InputPrefs {
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

            let exact = MouseKeyboardPane::with_catalog(
                InputPrefs {
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

            let partial = MouseKeyboardPane::with_catalog(
                InputPrefs {
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

            let clamped = MouseKeyboardPane::with_catalog(
                InputPrefs {
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
        let mut pane = MouseKeyboardPane::with_catalog(InputPrefs::default(), catalog());
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

    /// Document source selection, source latches and pointer speeds never drive the device.
    #[test]
    fn document_input_edits_are_ignored_in_favour_of_local_effects() {
        // Case `document_active_source_selection_is_ignored`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let initial = InputPrefs {
                keyboard_layouts: vec!["us".into(), "de".into()],
                ..Default::default()
            };
            let mut pane = MouseKeyboardPane::with_catalog(initial, catalog());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, LAYOUT_KEY, "German");
            pane.sync(&mut doc, &root);
            assert_eq!(pane.last.keyboard_layout, "us");
            assert_eq!(SystemPrefs::load().input.keyboard_layout, "us");
            assert!(!pane.take_structure_dirty());
        }

        // Case `document_source_latches_are_ignored`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = MouseKeyboardPane::with_catalog(InputPrefs::default(), catalog());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            let _ = doc.put(&root, ADD_SOURCE_PICKER_KEY, "German");
            let _ = doc.put(&root, ADD_SOURCE_KEY, true);
            pane.sync(&mut doc, &root);
            assert_eq!(pane.last.keyboard_layouts, ["us"]);
            assert_eq!(pane.last.keyboard_layout, "us");

            let _ = doc.put(&root, MouseKeyboardPane::source_action(1, "up"), true);
            pane.sync(&mut doc, &root);
            assert_eq!(pane.last.keyboard_layouts, ["us"]);
            assert_eq!(pane.last.keyboard_layout, "us");

            let _ = doc.put(&root, MouseKeyboardPane::source_action(0, "remove"), true);
            pane.sync(&mut doc, &root);
            assert_eq!(pane.last.keyboard_layouts, ["us"]);
            assert_eq!(pane.last.keyboard_layout, "us");
        }

        // Case `document_pointer_speed_changes_are_ignored`.
        // Device class speed sliders only travel through local effect actions.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = MouseKeyboardPane::with_catalog(InputPrefs::default(), catalog());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(
                &root,
                TOUCHPAD_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(0.9),
            );
            let _ = doc.put(
                &root,
                MOUSE_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(0.2),
            );
            pane.sync(&mut doc, &root);
            assert_eq!(pane.last.touchpad_pointer_speed, None);
            assert_eq!(pane.last.mouse_pointer_speed, None);
        }
    }
}
