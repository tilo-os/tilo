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
mod pointing;
mod tablet;
#[cfg(test)]
mod tests;

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
use crate::prefs::{InputDevicePrefs, InputPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, caption, error_caption, field_row, rows, section, setting_row, setting_row_described,
};
use tablet::{
    DIAL_CHOICES, MAX_PAD_BUTTONS, MAX_PAD_DIALS, MAX_PAD_MODES, device_title, dial_for_id,
    eraser_for_id, pad_button_for_id, pad_resource_id, parse_dial_action_id, set_pad_button,
    stylus_button_for_id, tablet_output_for_id, tablet_resource_id,
};

crate::effects::effect_family!(InputEffect => answer);

/// The input settings the page edits as one: the person's choices, which it
/// reads as, and this device's keyboards, tablets and touchpads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Input {
    pub(crate) person: InputPrefs,
    pub(crate) device: InputDevicePrefs,
}

impl std::ops::Deref for Input {
    type Target = InputPrefs;
    fn deref(&self) -> &InputPrefs {
        &self.person
    }
}

impl std::ops::DerefMut for Input {
    fn deref_mut(&mut self) -> &mut InputPrefs {
        &mut self.person
    }
}

impl Input {
    /// The input settings `cx` holds.
    pub(crate) fn load(cx: &PaneCx) -> Self {
        Self {
            person: cx.settings::<InputPrefs>().get(),
            device: cx.settings::<InputDevicePrefs>().get(),
        }
    }

    /// Store both: the person's in their section, this device's in its.
    ///
    /// # Errors
    /// As `ui::storage::Settings::set`.
    pub(crate) fn store(&self, cx: &PaneCx) -> std::io::Result<()> {
        cx.settings::<InputPrefs>().set(&self.person)?;
        cx.settings::<InputDevicePrefs>().set(&self.device)
    }

    /// The choices as the compositor reads them on this device.
    pub(crate) fn rich_input(&self) -> settings_store::hardware::RichInputConfig {
        self.person.rich_input(&self.device)
    }
}

crate::area::pane_area! {
    InputArea;
    fn warm(&self) {
        self.prefetch_catalog();
    }
    fn partial(&self) -> bool {
        self.catalog_pending()
    }
}

/// The Keyboard, Pointing or Spelling page, as `category` asks.
pub(crate) fn build(
    app: &crate::settings_app::SettingsApp,
    category: crate::categories::Category,
) -> Box<dyn Widget> {
    use crate::categories::Category;
    let Some(input) = area(app) else {
        return crate::widgets::page(Vec::new());
    };
    let map_input = app.keyboard_map_input();
    Box::new(
        MouseKeyboardPane::new(app.host().clone(), input)
            .with_page(match category {
                Category::Pointing => InputPage::Pointing,
                Category::Spelling => InputPage::Spelling,
                _ => InputPage::Keyboard,
            })
            .with_map_input(map_input),
    )
}

/// The window's [`InputArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&InputArea> {
    app.areas().find()
}

pub(crate) mod values;

/// A switch of the stored input section, as the page and search show it:
/// labelled `$label`, explained by its description.
macro_rules! input_switches {
    ($section:ident: $($name:ident => $field:ident, $label:literal, $id:literal;)*) => {$(
        pub(crate) fn $name(cx: &PaneCx, input: &Followed<$section>) -> Item {
            cx.row(t($label))
                .described(t(concat!($label, "-description")))
                .id($id)
                .of(input, $section::paths().$field)
                .toggle()
        }
    )*};
}

input_switches! {
    InputPrefs:
    disable_while_typing => disable_while_typing, "input-disable-while-typing", "input.disable_while_typing";
    gestures => touchpad_gestures, "input-touchpad-gestures", "input.touchpad_gestures";
    left_handed => left_handed, "input-left-handed", "input.left_handed";
    middle_click => middle_emulation, "input-middle-emulation", "input.middle_emulation";
    tap_to_click => tap_to_click, "input-tap-to-click", "input.tap_click";
}

input_switches! {
    InputDevicePrefs:
    disable_with_mouse => touchpad_disabled_on_external_mouse, "input-touchpad-disable-with-mouse", "input.touchpad_disabled_on_external_mouse";
    touchpad => touchpad_enabled, "input-touchpad-enabled", "input.touchpad_enabled";
}

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
    #[effect(job, returns, idempotent, kind = "read")]
    InspectInputSources {
        query: Option<String>,
        page: Option<u32>,
    },
    /// Set the compose key.
    #[effect(idempotent)]
    #[effect(keywords = "compose, accents, special characters")]
    SetComposeKey {
        #[param("The key", choices = stored_ids(COMPOSE_KEY_CHOICES, COMPOSE_DISABLED))]
        key: String,
    },
    /// Set what the Caps Lock key does.
    #[effect(idempotent)]
    #[effect(keywords = "caps lock key, remap caps lock")]
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
    #[effect(returns, idempotent, kind = "read")]
    InspectKeyboardMap,
    /// Read discovered keyboard scopes and their ordered configured sources.
    #[effect(returns, idempotent, kind = "read")]
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
    #[effect(job, idempotent)]
    ChooseSourceToAdd { source: String },
    /// Add the previewed source without changing the active source.
    #[effect(job)]
    ConfirmSourceToAdd,
    /// Move a configured source before or after another stable source id.
    ReorderSource {
        source: String,
        target: String,
        after: bool,
    },
    /// Inspect any installed XKB layout without changing what the keyboard types.
    #[effect(job, idempotent)]
    PreviewLayout { source: String },
    /// Show the actual focused keyboard group and modifier state.
    #[effect(idempotent)]
    ShowLiveKeyboardMap,
    /// Inspect XKB levels 5–8 when the selected layout supplies them.
    #[effect(idempotent)]
    SetPreviewExtraLevels { on: bool },
    /// Inspect one of the compiled native modifier levels without applying it.
    #[effect(job, idempotent)]
    SetPreviewLevel { level: String },
    /// Read a physical key’s complete legends without applying a layout.
    #[effect(job, idempotent)]
    InspectKeyboardKey { key: String },
    /// Choose an explicit physical model for the preview and keyboard.
    #[effect(idempotent)]
    SetKeyboardModel {
        #[param("The physical model", choices = ["pc104", "pc105", "jp106", "kr106"])]
        model: String,
    },
    /// Type with the inspected layout, adding it when space is available.
    #[effect(job)]
    ApplyPreview,
    /// Add an installed input source and type with it.
    #[effect(job)]
    AddSource {
        #[param("The source's id, such as `us` or `de`")]
        source: String,
    },
    /// Replace one input source by another, in place.
    #[effect(job)]
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
fn tablet_entry<'a>(input: &'a mut Input, device: &str) -> Result<&'a mut TabletConfig, String> {
    if device.is_empty() || device.len() > 512 || device.chars().any(char::is_control) {
        return Err("unknown tablet".into());
    }
    Ok(input.device.tablets.entry(device.to_string()).or_default())
}

/// Apply `effect` to `input`, refusing what it cannot be.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per input choice keeps every validation in one place"
)]
fn apply(input: &mut Input, effect: InputEffect, catalog: &[CatalogEntry]) -> Result<(), String> {
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
            input.device.keyboard_preview = Some(source);
            input.device.keyboard_preview_level = 0;
            input.device.keyboard_preview_extra_levels = false;
        }
        E::ShowLiveKeyboardMap => {
            input.device.keyboard_preview = None;
            input.device.keyboard_preview_level = 0;
            input.device.keyboard_preview_extra_levels = false;
        }
        E::SetPreviewExtraLevels { on } => {
            let layout = input.keyboard_layout.clone();
            input.device.keyboard_preview.get_or_insert(layout);
            input.device.keyboard_preview_extra_levels = on;
            input.device.keyboard_preview_level = 0;
        }
        E::SetPreviewLevel { level } => {
            let level = level
                .parse::<usize>()
                .ok()
                .filter(|level| *level < 32)
                .ok_or("invalid keyboard preview level")?;
            let source = input
                .device
                .keyboard_preview
                .clone()
                .unwrap_or_else(|| input.keyboard_layout.clone());
            let map = crate::keyboard_preview::Preview::new(
                &source,
                &input.device.keyboard_model,
                Some(""),
                false,
            )
            .needed(String::new())?;
            if level >= map.level_count() {
                return Err("the layout does not supply this level".into());
            }
            input.device.keyboard_preview = Some(source);
            input.device.keyboard_preview_level = level;
            input.device.keyboard_preview_extra_levels = false;
        }
        E::InspectKeyboardKey { key } => {
            let source = input
                .device
                .keyboard_preview
                .as_deref()
                .unwrap_or(&input.person.keyboard_layout);
            let map = crate::keyboard_preview::Preview::new(
                source,
                &input.device.keyboard_model,
                Some(""),
                false,
            )
            .needed(String::new())?;
            if map.inspection_values(&key).is_none() {
                return Err("the physical model does not supply this key".into());
            }
            input.device.keyboard_inspected_key = key;
        }
        E::SetKeyboardModel { model } => {
            if !crate::keyboard_preview::valid_model(&model) {
                return Err("unknown physical keyboard model".into());
            }
            input.device.keyboard_model = model;
            input.device.keyboard_inspected_key = "AD01".into();
            input.device.keyboard_preview_level = 0;
            input.device.keyboard_preview_extra_levels = false;
        }
        E::ApplyPreview => {
            let source = input
                .device
                .keyboard_preview
                .clone()
                .unwrap_or_else(|| input.keyboard_layout.clone());
            let next = if input.keyboard_layouts.contains(&source) {
                E::SetActiveSource { source }
            } else {
                E::AddSource { source }
            };
            apply(input, next, catalog)?;
            input.device.keyboard_preview = None;
            input.device.keyboard_preview_level = 0;
            input.device.keyboard_preview_extra_levels = false;
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
            let output = tablet_output_for_id(&output)?;
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

fn position(input: &Input, source: &str) -> Result<usize, String> {
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
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    // An effect on the input sources needs this device's list of them,
    // which is read off the UI thread: it answers once the list is in.
    if effect.reads_sources()
        && let Some(area) = area(app)
        && area.catalog.pending()
    {
        let wait = area.catalog.waiting();
        return Ok(app.job(cx, wait, move |app, (), cx| answer(app, effect, cx)));
    }
    let answered = answer_read(app, effect.clone());
    // One that checks a level or a key against the keyboard's drawing
    // answers once the drawing is compiled, off the UI thread.
    if let Some(drawing) = crate::keyboard_preview::take_needed() {
        return Ok(app.job(
            cx,
            move || drawing.compile(),
            move |app, (), cx| answer(app, effect, cx),
        ));
    }
    answered
}

impl InputEffect {
    /// Whether the effect checks a source against this device's list of
    /// them (the list, a source to show or to add). Each answers as a job.
    fn reads_sources(&self) -> bool {
        matches!(
            self,
            Self::InspectInputSources { .. }
                | Self::ChooseSourceToAdd { .. }
                | Self::ConfirmSourceToAdd
                | Self::PreviewLayout { .. }
                | Self::ApplyPreview
                | Self::AddSource { .. }
                | Self::ReplaceSource { .. }
        )
    }
}

/// [`answer`] once what the effect reads is in.
fn answer_read(app: &SettingsApp, effect: InputEffect) -> EffectAnswer {
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
    let mut input = Input::load(host);
    input.normalise_keyboard_sources();
    keyboard_editor::apply_scoped(
        &mut input,
        effect,
        crate::panes::mouse_keyboard::area(app).ok_or_else(crate::area::missing)?,
    )?;
    input.normalise_keyboard_sources();
    input
        .store(host)
        .map(|()| Effected::done())
        .map_err(|error| format!("the input settings could not be saved: {error}"))
}

pub(crate) fn input_picker_snapshot(widget: &dyn Widget) -> Option<serde_json::Value> {
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
    let area = crate::panes::mouse_keyboard::area(app).ok_or_else(crate::area::missing)?;
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
        .then(|| app.input_picker())
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
/// could not be read, why. A device's are read off the UI thread, once,
/// when first asked for ([`InputArea::prefetch_catalog`]) — never while the
/// window opens, which shows no input source on its first page. Until they
/// are in, the list is empty and an effect that needs it answers once it is
/// ([`answer`]).
pub(crate) struct SourceCatalog {
    read: std::sync::Arc<std::sync::OnceLock<(Vec<CatalogEntry>, Option<String>)>>,
    started: std::cell::Cell<bool>,
}

impl SourceCatalog {
    /// This device's input sources, read when first asked for.
    fn of_device() -> Self {
        Self {
            read: std::sync::Arc::default(),
            started: std::cell::Cell::new(false),
        }
    }

    /// `entries`, already read; `error` says why they stand in for the
    /// device's own list.
    pub(crate) fn ready(entries: Vec<CatalogEntry>, error: Option<String>) -> Self {
        let read = std::sync::OnceLock::new();
        let _ = read.set((entries, error));
        Self {
            read: std::sync::Arc::new(read),
            started: std::cell::Cell::new(true),
        }
    }

    /// The name of source `id`, where the catalogue was read; never reads
    /// it.
    fn label_if_read(&self, id: &str) -> Option<String> {
        let (entries, _) = self.read.get()?;
        Some(
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map_or_else(|| id.to_owned(), |entry| entry.label.clone()),
        )
    }

    /// The sources, once read; none before.
    pub(crate) fn entries(&self) -> &[CatalogEntry] {
        self.read.get().map_or(&[], |(entries, _)| entries)
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.read.get()?.1.as_deref()
    }

    /// Whether the catalogue is still being read off the UI thread,
    /// starting that read if none has.
    fn pending(&self) -> bool {
        self.prefetch();
        self.read.get().is_none()
    }

    /// Start reading the device's catalogue off the UI thread, unless it
    /// has been read or is being read. The window is woken when it is in.
    fn prefetch(&self) {
        if self.started.replace(true) {
            return;
        }
        let read = std::sync::Arc::clone(&self.read);
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-input-sources",
            Box::new(move || {
                let _ = read.set(crate::input_sources::read_installed());
                wake.wake();
            }),
        );
        if let Err(error) = started {
            let _ = self.read.set((
                Vec::new(),
                Some(format!("the input sources could not be read: {error}")),
            ));
        }
    }

    /// Wait, off the UI thread, until the catalogue is in: what an effect
    /// that needs it runs before it answers.
    fn waiting(&self) -> impl FnOnce() + Send + 'static {
        self.prefetch();
        let read = std::sync::Arc::clone(&self.read);
        move || {
            read.wait();
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
    /// This device's keyboards, tablets and touchpads.
    device: Followed<InputDevicePrefs>,
    text: Followed<TextAssistancePrefs>,
    dictionary: Option<ui::personal_dictionary::Snapshot>,
    dictionary_identity: Option<[u8; 32]>,
    dictionary_word: ui::LocalState<String>,
    dictionary_language: ui::LocalState<String>,
    dictionary_edits: RefCell<HashMap<DictionaryKey, DictionaryFields>>,
    /// The input settings as shown: normalised.
    last: Input,
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
    /// The keyboard drawing the page waits for while it is compiled off
    /// the UI thread.
    drawing: RefCell<Option<crate::keyboard_preview::Preview>>,
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
        let device = cx.follow::<InputDevicePrefs>();
        let text = cx.follow::<TextAssistancePrefs>();
        let mut last = Input {
            person: input.get().clone(),
            device: device.get().clone(),
        };
        last.normalise_keyboard_sources();
        let mut pane = Self {
            page: InputPage::default(),
            cx,
            input,
            device,
            text,
            dictionary: ui::personal_dictionary::snapshot().ok(),
            dictionary_identity: ui::local_identity::bound()
                .map(|identity| identity.group_public_key),
            dictionary_word: ui::LocalState::new(String::new()),
            dictionary_language: ui::LocalState::new(String::new()),
            dictionary_edits: RefCell::new(HashMap::new()),
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
            drawing: RefCell::new(None),
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

    /// The drawing of `source` on the keyboard the page shows, once it is
    /// compiled; until then `None`, and it is compiled off the UI thread.
    fn keyboard_preview(&self, source: &str) -> Option<Result<ui::KeyboardMap, String>> {
        let input = self.preview_input(source);
        let options = settings_store::hardware::RichInputConfig {
            compose_key: input.compose_key.clone(),
            caps_lock: input.caps_lock.clone(),
            ..Default::default()
        }
        .xkb_options()
        .join(",");
        let preview = crate::keyboard_preview::Preview::new(
            source,
            &input.device.keyboard_model,
            Some(&options),
            crate::keyboard_preview::device_keyboard_is_compact(),
        );
        let drawn = preview.drawn(t("input-map-title"));
        if drawn.is_none() {
            preview.draw_in_background();
            *self.drawing.borrow_mut() = Some(preview);
        }
        drawn.map(|drawn| drawn.map(|map| map.with_input(self.map_input.clone())))
    }

    fn source_label(&self, id: &str) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.label.clone())
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        match self.page {
            InputPage::Keyboard => crate::widgets::page(self.keyboard_page()),
            InputPage::Spelling => {
                crate::widgets::page(vec![self.spell_checker_card(), self.dictionary_card()])
            }
            InputPage::Pointing => {
                let mut cards = Vec::new();
                if self.sections.touchpad {
                    cards.push(self.touchpad_card());
                    // Everything else a touchpad does is moot while it is off.
                    if self.last.device.touchpad_enabled {
                        cards.push(self.touchpad_click_scroll_card());
                    }
                }
                if self.sections.mouse {
                    cards.push(self.mouse_card());
                }
                if self.sections.pointer {
                    cards.push(self.pointer_options_card());
                }
                if self.sections.tablet {
                    cards.extend(self.tablet_cards());
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
    /// saved setting. While it is off the rest is moot, so only the switch
    /// shows.
    fn touchpad_card(&self) -> crate::widgets::SettingsSection {
        let last = &self.last;
        let mut content = vec![touchpad(&self.cx, &self.device)];
        if last.device.touchpad_enabled {
            content.extend([
                self.cx
                    .row(t("input-touchpad-disable-with-mouse"))
                    .described(t("input-touchpad-disable-with-mouse-description"))
                    .id("input.touchpad_disabled_on_external_mouse")
                    .nested()
                    .of(
                        &self.device,
                        InputDevicePrefs::paths().touchpad_disabled_on_external_mouse,
                    )
                    .toggle(),
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
                pointing::scroll_direction(
                    &self.cx,
                    &self.input,
                    false,
                    "input-touchpad-natural-scroll-description",
                    InputPrefs::paths().touchpad_natural_scroll,
                    "input.touchpad_natural_scroll",
                ),
                gestures(&self.cx, &self.input),
                disable_while_typing(&self.cx, &self.input),
            ]);
        }
        section(&t("input-touchpad-title"), rows(content))
    }

    /// Tapping, and the refinements a tap allows: dragging after a tap
    /// shows only while a tap clicks, and its lock only while tap-and-drag
    /// is on.
    fn tapping_rows(&self) -> Vec<Item> {
        let mut content = vec![tap_to_click(&self.cx, &self.input)];
        if self.last.tap_to_click {
            content.push(
                self.cx
                    .row(t("input-tap-drag"))
                    .described(t("input-tap-drag-description"))
                    .id("input.tap_drag")
                    .nested()
                    .of(&self.input, InputPrefs::paths().tap_drag)
                    .toggle(),
            );
            if self.last.tap_drag {
                content.push(
                    self.cx
                        .row(t("input-tap-drag-lock"))
                        .described(t("input-tap-drag-lock-description"))
                        .id("input.tap_drag_lock")
                        .nested()
                        .of(&self.input, InputPrefs::paths().tap_drag_lock)
                        .toggle(),
                );
            }
        }
        content
    }

    /// How a touchpad turns presses and finger movement into clicks and
    /// scrolling. Each choice is applied only to devices whose libinput
    /// driver offers it.
    fn touchpad_click_scroll_card(&self) -> crate::widgets::SettingsSection {
        let mut content = self.tapping_rows();
        content.extend([
            pointing::click_method(&self.cx, &self.input),
            pointing::scroll_method(&self.cx, &self.input),
        ]);
        section(&t("input-clicking-title"), rows(content))
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
                pointing::scroll_direction(
                    &self.cx,
                    &self.input,
                    true,
                    "input-mouse-natural-scroll-description",
                    InputPrefs::paths().mouse_natural_scroll,
                    "input.mouse_natural_scroll",
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
                left_handed(&self.cx, &self.input),
                middle_click(&self.cx, &self.input),
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
                check_spelling(&self.cx, &self.text),
                auto_correct(&self.cx, &self.text),
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
}

pub(crate) fn check_spelling(cx: &PaneCx, text: &Followed<TextAssistancePrefs>) -> Item {
    cx.row(t("input-spell-check"))
        .described(t("input-spell-check-desc"))
        .id("text.check_spelling")
        .of(text, TextAssistancePrefs::paths().check_spelling)
        .toggle()
}

pub(crate) fn auto_correct(cx: &PaneCx, text: &Followed<TextAssistancePrefs>) -> Item {
    cx.row(t("input-spell-auto"))
        .described(t("input-spell-auto-desc"))
        .id("text.auto_correct")
        .of(text, TextAssistancePrefs::paths().auto_correct)
        .toggle()
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
        let person = self.input.refresh();
        if self.device.refresh() || person {
            let mut last = Input {
                person: self.input.get().clone(),
                device: self.device.get().clone(),
            };
            last.normalise_keyboard_sources();
            self.last = last;
            self.composed.invalidate();
            self.composed.clear();
        }
        if self.text.refresh() {
            self.composed.invalidate();
            self.composed.clear();
        }
        // The drawing the page waited for was compiled.
        let drawn = self
            .drawing
            .borrow()
            .as_ref()
            .is_some_and(crate::keyboard_preview::Preview::is_drawn);
        if drawn {
            self.drawing.borrow_mut().take();
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
mod input_inspection_tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    #[test]
    fn installed_input_sources_have_a_bounded_device_read_without_mutating_preferences() {
        // The pane follows the identity the process has bound, which the
        // dictionary test swaps: a swap mid-test rebuilds the page under it.
        let _env = tilo_test_env::lock();
        let mut app = TestApp::<SettingsShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        let action = app.actions().unwrap().into_iter()
            .find(|action| action.address.action.ends_with("inspect-input-sources"))
            .expect("the installed catalogue and active source must be readable through Settings' normal door");
        assert_eq!(action.contract.kind, ui::tilo_actions::ActionKind::Read);
        let before = app.settings::<InputPrefs>().get();
        let value = app
            .call_on_finished(
                &action.address.resource,
                &action.address.action,
                serde_json::json!({}),
            )
            .unwrap()
            .value
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
    }
    #[test]
    fn input_inspection_tracks_the_actual_unselected_open_shared_picker() {
        // The pane follows the identity the process has bound, which the
        // dictionary test swaps: a swap mid-test rebuilds the page under it.
        let _env = tilo_test_env::lock();
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
            .call_on_finished(&address.resource, &address.action, serde_json::json!({}))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(closed["add_picker"]["selected"], "");
        assert_eq!(closed["add_picker"]["open"], false);
        assert!(closed["add_picker"]["value"].is_null());
        app.harness()
            .click_named("input.source.add_picker")
            .unwrap();
        app.harness().type_text("French");
        let open = app
            .call_on_finished(&address.resource, &address.action, serde_json::json!({}))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(open["add_picker"]["selected"], "");
        assert_eq!(open["add_picker"]["open"], true);
        assert_eq!(open["add_picker"]["filter"], "French");
        assert_eq!(open["add_picker"]["value"], "French");
        assert_eq!(app.settings::<InputPrefs>().get(), before);
        let filtered = app
            .call_on_finished(
                &address.resource,
                &address.action,
                serde_json::json!({"query":"French"}),
            )
            .unwrap()
            .value
            .unwrap();
        assert_eq!(filtered["total"], 1);
        assert_eq!(filtered["sources"][0]["id"], "fr");
        assert!(
            app.call_on_finished(
                &address.resource,
                &address.action,
                serde_json::json!({"page":1})
            )
            .is_err()
        );
        assert!(
            app.call_on_finished(
                &address.resource,
                &address.action,
                serde_json::json!({"query":"x".repeat(129)})
            )
            .is_err()
        );
        assert_eq!(app.settings::<InputPrefs>().get(), before);
    }
}
