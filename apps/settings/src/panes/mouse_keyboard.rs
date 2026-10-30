//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! The Keyboard, Pointing and Spelling pages are view code over the person's
//! `input` section, this device's `input_device` section and the shared
//! `text_assistance` section ([`view`]):
//! - **Pointer speed / per-device natural scrolling / Tap to click / Key
//!   repeat** and the other pointer and keyboard choices are the `input`
//!   section's field edits and verbs; the compositor reads them when it
//!   configures libinput for the session. Whether the touchpad is on, and
//!   each tablet's mapping, stay with this device: the `input_device`
//!   section's fields and verbs, a tablet's offered on its live row.
//! - **Keyboard input sources** are per-user durable intent. Settings never
//!   invokes `localectl`; the compositor owns XKB and hot-reloads the section.
//!   What reads this device's installed sources, the keyboard drawing or the
//!   keyboard editor's own state is an [`InputEffect`], which writes through
//!   the sections' verbs.
//! - **Spell checking** is the `text_assistance` section every app's text
//!   fields follow.

pub(crate) mod keyboard_editor;
mod pointing;
mod tablet;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use provider_settings_ui::bound::{SettingRows as _, record_item};
use settings_store::hardware::{
    InputDeviceKind, InputDeviceSnapshot, InputHardwareSnapshot, TouchpadClickMethod,
    TouchpadScrollMethod,
};
use ui::LocalState;
use ui::framework::{Actions, Effect as _, EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::i18n::ArgValue;
use ui::text_assistance::TextAssistancePrefs;
#[cfg(test)]
use ui::theme::Theme;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource;
use ui::widget::{Button, Dropdown, Element, Row, Spacer, Text, TextInput, Widget};
use vello::kurbo::{Point, Rect};

use crate::area::AreaContext;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::input_sources::CatalogEntry;
use crate::prefs::{InputDevicePrefs, InputPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, caption, error_caption, field_row, rows, section, setting_row,
    setting_row_described,
};
use tablet::{device_title, pad_resource_id, tablet_resource_id};

crate::effects::effect_family!(InputEffect => answer);

/// The input settings the effects edit as one: the person's choices, which
/// it reads as, and this device's keyboards, tablets and touchpads.
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
    /// The input settings `storage` holds.
    pub(crate) fn load(storage: &ui::storage::AppStorage) -> Self {
        Self {
            person: storage.settings::<InputPrefs>().get(),
            device: storage.settings::<InputDevicePrefs>().get(),
        }
    }

    /// Both sections as they are now, the sources normalised as shown.
    fn shown(person: &InputPrefs, device: &InputDevicePrefs) -> Self {
        let mut input = Self {
            person: person.clone(),
            device: device.clone(),
        };
        input.normalise_keyboard_sources();
        input
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
    fn poll(&self) {
        InputArea::poll(self);
    }
    fn next_wake(&self) -> Option<ui::time::Instant> {
        InputArea::wake_deadline(self)
    }
}

/// The window's [`InputArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&InputArea> {
    app.areas().find()
}

pub(crate) mod values;

/// The Keyboard, Pointing or Spelling page as view code over the input
/// sections and what the area read: the attached devices, the installed
/// sources, the keyboard drawing and the personal dictionary. The page is
/// built again when a section changes and when what the area shows moves
/// ([`InputArea::shown`]); the devices and the dictionary are followed while
/// it is open.
pub(crate) fn view(
    cx: &ViewCx<Settings>,
    app: &Rc<SettingsApp>,
    page: InputPage,
) -> Option<Element> {
    let held = area(app)?;
    let showing = held.show();
    let shown = held.shown.clone();
    let actions = cx.actions();
    let app = Rc::clone(app);
    if page == InputPage::Spelling {
        let text = cx.settings::<TextAssistancePrefs>();
        return cx.record(&text, move |cx| {
            let (app, actions, showing) = (Rc::clone(&app), actions.clone(), Rc::clone(&showing));
            cx.switch(&shown, move |cx, _| {
                // The part keeps the dictionary followed while shown.
                let _shown = &showing;
                match area(&app) {
                    Some(area) => provider_settings_ui::widgets::page(vec![
                        spell_checker_card(cx),
                        dictionary_card(&actions, area),
                    ]),
                    None => provider_settings_ui::widgets::page(Vec::new()),
                }
            })
        });
    }
    let person = cx.settings::<InputPrefs>();
    let device = cx.settings::<InputDevicePrefs>();
    let map_input = app.keyboard_map_input();
    cx.record(&person, move |cx| {
        let (app, actions, shown, showing, device, map_input) = (
            Rc::clone(&app),
            actions.clone(),
            shown.clone(),
            Rc::clone(&showing),
            device.clone(),
            map_input.clone(),
        );
        let device_now = device.state();
        cx.switch(
            &cx.value(InputPrefs::clone),
            move |cx, person: &InputPrefs| {
                let (app, actions, shown, showing, device, map_input, person) = (
                    Rc::clone(&app),
                    actions.clone(),
                    shown.clone(),
                    Rc::clone(&showing),
                    device.clone(),
                    map_input.clone(),
                    person.clone(),
                );
                cx.switch(&device_now, move |cx, device_prefs: &InputDevicePrefs| {
                    let (app, actions, showing, device, map_input, person, device_prefs) = (
                        Rc::clone(&app),
                        actions.clone(),
                        Rc::clone(&showing),
                        device.clone(),
                        map_input.clone(),
                        person.clone(),
                        device_prefs.clone(),
                    );
                    cx.switch(&shown, move |cx, _| {
                        // The part keeps the devices followed while shown.
                        let _shown = &showing;
                        let Some(area) = area(&app) else {
                            return provider_settings_ui::widgets::page(Vec::new());
                        };
                        let shown = PageOf {
                            cx,
                            actions: &actions,
                            device: &device,
                            area,
                            last: Input::shown(&person, &device_prefs),
                            map_input: map_input.clone(),
                        };
                        match page {
                            InputPage::Pointing => shown.pointing_page(),
                            _ => provider_settings_ui::widgets::page(shown.keyboard_page()),
                        }
                    })
                })
            },
        )
    })
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

pub fn scroll_method_id(method: TouchpadScrollMethod) -> &'static str {
    SCROLL_METHOD_CHOICES
        .iter()
        .find(|(_, _, value)| *value == method)
        .map_or("two_finger", |(id, _, _)| id)
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

/// Keyboard, input source, keyboard drawing, dictionary and tablet
/// mapping choices: those that read this device (its installed sources, the
/// keyboard drawing, its displays), the keyboard editor's own state or the
/// personal dictionary. The rest are the input sections' field edits and
/// verbs.
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
    /// Map a drawing tablet onto one display, or the whole desktop.
    #[effect(idempotent)]
    SetTabletOutput {
        #[param("The tablet's device id")]
        device: String,
        #[param("A display's connector, or `desktop`")]
        output: String,
    },
}

fn fraction(value: f64) -> Result<f64, String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("the value must be between 0.0 and 1.0".into())
    }
}

/// Apply `effect` to `input`, refusing what it cannot be: what the keyboard
/// drawing inspects, and the source it applies. The keyboard editor changes
/// the sources ([`keyboard_editor::apply_scoped`]).
#[allow(
    clippy::too_many_lines,
    reason = "one arm per input choice keeps every validation in one place"
)]
fn apply(input: &mut Input, effect: InputEffect, catalog: &[CatalogEntry]) -> Result<(), String> {
    use InputEffect as E;
    let installed = |source: &str| catalog.iter().any(|entry| entry.id == source);
    match effect {
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
        E::InspectInputSources { .. }
        | E::InspectKeyboardMap
        | E::AddDictionaryWord { .. }
        | E::EditDictionaryWord { .. }
        | E::RemoveDictionaryWord { .. }
        | E::ResetDictionary { .. }
        | E::SetTabletOutput { .. } => {}
        E::InspectKeyboardSources
        | E::SetComposeKey { .. }
        | E::SetCapsLock { .. }
        | E::MoveSourceUp { .. }
        | E::MoveSourceDown { .. }
        | E::RemoveSource { .. }
        | E::SetSourceOptionsVisible { .. }
        | E::SetSourceInspectionVisible { .. }
        | E::SelectKeyboardScope { .. }
        | E::SelectConfiguredSource { .. }
        | E::OpenSourcePicker
        | E::CloseSourcePicker
        | E::ChooseSourceToAdd { .. }
        | E::ConfirmSourceToAdd
        | E::ReplaceSource { .. }
        | E::ReorderSource { .. } => {
            return Err("this operation requires the keyboard editor".into());
        }
    }
    Ok(())
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
        _ => {}
    }
    if let Some(result) = keyboard_editor::answer_editor(app, &effect) {
        return result;
    }
    if let InputEffect::SetTabletOutput { device, output } = effect {
        // Only a display connected to this device is offered.
        tablet::tablet_output_for_id(&output)?;
        return app
            .sections()
            .invoke::<InputDevicePrefs>(InputDevicePrefs::SET_TABLET_OUTPUT.with((device, output)))
            .map(|()| Effected::done());
    }
    let mut input = Input::load(app.storage());
    input.normalise_keyboard_sources();
    let before = input.clone();
    keyboard_editor::apply_scoped(
        &mut input,
        effect,
        crate::panes::mouse_keyboard::area(app).ok_or_else(crate::area::missing)?,
    )?;
    input.normalise_keyboard_sources();
    commit(app.sections(), &before, &input)
}

/// Write what `after` changed of `before` through the input sections'
/// verbs, each checking what it writes.
fn commit(sections: &crate::sections::Sections, before: &Input, after: &Input) -> EffectAnswer {
    let (person, device) = (&after.person, &after.device);
    if person.keyboard_layouts != before.person.keyboard_layouts
        || person.keyboard_layout != before.person.keyboard_layout
    {
        sections.invoke::<InputPrefs>(InputPrefs::SET_LAYOUTS.with((
            person.keyboard_layouts.clone(),
            person.keyboard_layout.clone(),
        )))?;
    }
    for (keyboard, profile) in &device.keyboard_assignments {
        if before.device.keyboard_assignments.get(keyboard) != Some(profile) {
            sections.invoke::<InputDevicePrefs>(
                InputDevicePrefs::SET_KEYBOARD_LAYOUTS.with((keyboard.clone(), profile.clone())),
            )?;
        }
    }
    for keyboard in before
        .device
        .keyboard_assignments
        .keys()
        .filter(|keyboard| !device.keyboard_assignments.contains_key(*keyboard))
    {
        sections.invoke::<InputDevicePrefs>(
            InputDevicePrefs::USE_SHARED_LAYOUTS.with(keyboard.clone()),
        )?;
    }
    // What the keyboard drawing inspects, as the verbs below leave it.
    let mut inspected = (
        before.device.keyboard_preview_level,
        before.device.keyboard_preview_extra_levels,
        before.device.keyboard_inspected_key.clone(),
    );
    if device.keyboard_model != before.device.keyboard_model {
        sections.invoke::<InputDevicePrefs>(
            InputDevicePrefs::SET_KEYBOARD_MODEL.with(device.keyboard_model.clone()),
        )?;
        inspected = (0, false, "AD01".into());
    }
    if device.keyboard_preview != before.device.keyboard_preview {
        sections.invoke::<InputDevicePrefs>(
            InputDevicePrefs::SHOW_KEYBOARD_LAYOUT.with(device.keyboard_preview.clone()),
        )?;
        inspected.0 = 0;
        inspected.1 = false;
    }
    let wanted = (
        device.keyboard_preview_level,
        device.keyboard_preview_extra_levels,
        device.keyboard_inspected_key.clone(),
    );
    if wanted != inspected {
        let level = u32::try_from(wanted.0).map_err(|_| "invalid keyboard preview level")?;
        sections.invoke::<InputDevicePrefs>(
            InputDevicePrefs::INSPECT_KEYBOARD.with((level, wanted.1, wanted.2)),
        )?;
    }
    Ok(Effected::done())
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
    let prefs = app.storage().settings::<InputPrefs>().get();
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

/// What a shown page holds while it follows the area: the watch on the
/// compositor's input snapshot, where it can be watched.
pub(crate) struct Showing {
    watch: Option<Rc<crate::watch::Watched>>,
}

/// The personal dictionary as a page last showed it, and whose it was.
#[derive(Clone, Default, PartialEq)]
struct Dictionary {
    snapshot: Option<ui::personal_dictionary::Snapshot>,
    identity: Option<[u8; 32]>,
}

impl Dictionary {
    fn read() -> Self {
        Self {
            snapshot: ui::personal_dictionary::snapshot().ok(),
            identity: ui::local_identity::bound().map(|identity| identity.group_public_key),
        }
    }
}

/// What a page shows of the area: the attached devices, the keyboard
/// editor, the dictionary and how many keyboard drawings landed.
#[derive(Clone, PartialEq)]
struct Seen {
    devices: Option<InputHardwareSnapshot>,
    editor: keyboard_editor::EditorState,
    dictionary: Dictionary,
    drawings: u64,
}

/// What the input area keeps for the window: the installed input sources,
/// the attached devices as the compositor last reported them, the keyboard
/// editor's state, and what the Spelling page shows of the dictionary.
pub struct InputArea {
    /// The input sections' actions its live rows offer.
    sections: crate::sections::Sections,
    catalog: SourceCatalog,
    live: Cell<bool>,
    /// The attached devices as last read, and when the live rows read them
    /// again.
    devices: Rc<RefCell<(Option<ui::time::Instant>, Option<InputHardwareSnapshot>)>>,
    /// When a shown page reads the attached devices again if their snapshot
    /// is not watched; `None` until it first reads them.
    next_device_poll: Cell<Option<ui::time::Instant>>,
    /// The watch on the compositor's input snapshot, while a page holds it.
    following: RefCell<Weak<crate::watch::Watched>>,
    /// What tells the area a page shows it.
    showing: RefCell<Weak<Showing>>,
    editor: LocalState<keyboard_editor::EditorState>,
    osk_always: bool,
    document: ui::storage::SettingsDocument,
    /// The keyboard drawing a page waits for while it is compiled off the
    /// UI thread.
    drawing: RefCell<Option<crate::keyboard_preview::Preview>>,
    /// How many drawings a page waited for have landed.
    drawings: Cell<u64>,
    dictionary: RefCell<Dictionary>,
    dictionary_word: LocalState<String>,
    dictionary_language: LocalState<String>,
    dictionary_edits: RefCell<HashMap<DictionaryKey, DictionaryFields>>,
    /// Moves whenever what a page shows of the area does.
    shown: LocalState<u64>,
    /// What [`Self::shown`] last moved for.
    seen: RefCell<Option<Seen>>,
}

impl InputArea {
    /// The area of this device, or, for a capture, of sample sources and no
    /// devices reported.
    pub fn new(cx: &AreaContext) -> Self {
        let live = cx.devices;
        let mut area = Self::over(
            cx,
            if live {
                SourceCatalog::of_device()
            } else {
                SourceCatalog::ready(sample_catalog(), None)
            },
            cx.scene().and_then(|scene| scene.input_hardware.clone()),
        );
        area.live.set(live);
        area.editor.set(keyboard_editor::EditorState {
            view: if cx.scene().is_some() {
                keyboard_editor::SourceView::Configured
            } else {
                keyboard_editor::SourceView::Native
            },
            ..Default::default()
        });
        area.osk_always = cx
            .storage
            .settings::<crate::accessibility_settings::AccessibilitySettings>()
            .get()
            .on_screen_keyboard
            == tilo_accessibility_settings::OnScreenKeyboard::Always;
        area
    }

    /// The area over `catalog` and the attached `devices`, reading nothing
    /// of the device.
    fn over(
        cx: &AreaContext,
        catalog: SourceCatalog,
        devices: Option<InputHardwareSnapshot>,
    ) -> Self {
        Self {
            sections: cx.sections.clone(),
            catalog,
            live: Cell::new(false),
            devices: Rc::new(RefCell::new((None, devices))),
            next_device_poll: Cell::new(None),
            following: RefCell::new(Weak::new()),
            showing: RefCell::new(Weak::new()),
            editor: LocalState::new(keyboard_editor::EditorState::default()),
            osk_always: false,
            document: cx.storage.settings_document().clone(),
            drawing: RefCell::new(None),
            drawings: Cell::new(0),
            dictionary: RefCell::new(Dictionary::default()),
            dictionary_word: LocalState::new(String::new()),
            dictionary_language: LocalState::new(String::new()),
            dictionary_edits: RefCell::new(HashMap::new()),
            shown: LocalState::new(0),
            seen: RefCell::new(None),
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

    /// The watch on the compositor's input snapshot for a page that shows
    /// the attached devices, shared by every such page; `None` for a
    /// capture's sample devices.
    fn follow(&self) -> Option<Rc<crate::watch::Watched>> {
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

    /// Follow the attached devices and the dictionary while a page shows
    /// them; pages share one follow. A page that starts following reads
    /// both now, so it opens on them.
    fn show(&self) -> Rc<Showing> {
        if let Some(held) = self.showing.borrow().upgrade() {
            return held;
        }
        let showing = Rc::new(Showing {
            watch: self.follow(),
        });
        *self.showing.borrow_mut() = Rc::downgrade(&showing);
        self.next_device_poll.set(None);
        self.read_devices(showing.watch.as_deref());
        let scopes = self.scopes_read();
        if let [(only, _)] = scopes.as_slice() {
            let mut editor = self.editor.get();
            if &editor.scope != only {
                editor.scope.clone_from(only);
                self.editor.set(editor);
            }
        }
        self.fit_scope();
        self.read_dictionary();
        *self.seen.borrow_mut() = Some(self.seen_now());
        showing
    }

    /// Read the attached devices again when due ([`Self::devices_due`]).
    fn read_devices(&self, watch: Option<&crate::watch::Watched>) {
        if self.live.get() && self.devices_due(watch) {
            self.devices.borrow_mut().1 = InputHardwareSnapshot::load();
        }
    }

    /// Whether a shown page reads the attached devices again: on its first
    /// look, when their watch says the compositor's snapshot changed, or,
    /// when it cannot be watched, once per [`DEVICE_POLL_INTERVAL`].
    fn devices_due(&self, watch: Option<&crate::watch::Watched>) -> bool {
        let now = ui::time::now();
        let next = self.next_device_poll.get();
        let first = next.is_none();
        let changed = match watch {
            Some(watch) if watch.is_complete() => watch.take(),
            _ => next.is_some_and(|deadline| now >= deadline),
        };
        if first || changed {
            self.next_device_poll.set(Some(now + DEVICE_POLL_INTERVAL));
        }
        first || changed
    }

    /// When the window must wake to read the attached devices: only while a
    /// page shows them and their snapshot cannot be watched.
    fn wake_deadline(&self) -> Option<ui::time::Instant> {
        let showing = self.showing.borrow().upgrade()?;
        match &showing.watch {
            Some(watch) if watch.is_complete() => None,
            _ if self.live.get() => self.next_device_poll.get(),
            _ => None,
        }
    }

    /// Take in what moved since the last frame while a page shows the area:
    /// the attached devices, the dictionary, a keyboard drawing that landed
    /// and the keyboard editor; [`Self::shown`] moves when any did.
    fn poll(&self) {
        let Some(showing) = self.showing.borrow().upgrade() else {
            return;
        };
        self.read_devices(showing.watch.as_deref());
        self.fit_scope();
        self.read_dictionary();
        let drawn = self
            .drawing
            .borrow()
            .as_ref()
            .is_some_and(crate::keyboard_preview::Preview::is_drawn);
        if drawn {
            self.drawing.borrow_mut().take();
            self.drawings.set(self.drawings.get().wrapping_add(1));
        }
        let now = self.seen_now();
        let mut seen = self.seen.borrow_mut();
        if seen.as_ref() != Some(&now) {
            *seen = Some(now);
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn seen_now(&self) -> Seen {
        Seen {
            devices: self.devices.borrow().1.clone(),
            editor: self.editor.get(),
            dictionary: self.dictionary.borrow().clone(),
            drawings: self.drawings.get(),
        }
    }

    /// The keyboard the editor edits is one attached: when it went away,
    /// the editor edits the first offered.
    fn fit_scope(&self) {
        let scopes = self.scopes_read();
        let mut editor = self.editor.get();
        if !scopes.iter().any(|(id, _)| id == &editor.scope) {
            editor.scope = scopes
                .first()
                .map_or_else(|| "all".into(), |(id, _)| id.clone());
            editor.selected = None;
            editor.adding = false;
            editor.pending = None;
            self.editor.set(editor);
        }
    }

    /// Read the dictionary and whose it is; a new person starts with empty
    /// fields, and edits of words no longer there are dropped.
    fn read_dictionary(&self) {
        let now = Dictionary::read();
        let mut dictionary = self.dictionary.borrow_mut();
        if *dictionary == now {
            return;
        }
        if now.identity != dictionary.identity {
            self.dictionary_word.set(String::new());
            self.dictionary_language.set(String::new());
            self.dictionary_edits.borrow_mut().clear();
        }
        self.dictionary_edits
            .borrow_mut()
            .retain(|(word, language), _| {
                now.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .words
                        .iter()
                        .any(|entry| &entry.spelling == word && &entry.language == language)
                })
            });
        *dictionary = now;
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

    /// The attached devices as last read, never reading them.
    fn devices_read(&self) -> Option<InputHardwareSnapshot> {
        self.devices.borrow().1.clone()
    }

    /// The keyboards the editor offers, as the attached devices were last
    /// read.
    fn scopes_read(&self) -> Vec<(String, String)> {
        keyboard_editor::scopes(self.devices_read().as_ref(), self.osk_always)
    }

    /// One live row for the keyboard drawing, and one per attached drawing
    /// tablet and pad, by device id, offering its choices bound to it.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let inspection = LiveRow::new(
            "settings.keyboard-map",
            "settings.keyboard-map",
            t("input-map-title"),
        )
        .operation(SettingsEffect::from(InputEffect::InspectKeyboardMap).bound());
        let Some(devices) = self.devices() else {
            return vec![inspection];
        };
        let row = |device: &InputDeviceSnapshot, id: String, kind: &str| {
            LiveRow::new(id, kind, device_title(device)).presentation(ResourcePresentation {
                detail: Some(device.key.clone()),
                ..ResourcePresentation::default()
            })
        };
        let offer =
            |row: LiveRow, decls: Vec<Option<tilo_resource::ActionDecl<InputDevicePrefs>>>| {
                decls.into_iter().flatten().fold(row, |row, decl| {
                    match self.sections.operation(&decl) {
                        Some(operation) => row.operation(operation),
                        None => row,
                    }
                })
            };
        std::iter::once(inspection)
            .chain(
                devices
                    .of_kind(InputDeviceKind::Tablet)
                    .into_iter()
                    .map(|device| {
                        offer(
                            row(device, tablet_resource_id(&device.key), "settings.tablet"),
                            tablet::pen_operations(&device.key),
                        )
                    })
                    .chain(
                        devices
                            .of_kind(InputDeviceKind::TabletPad)
                            .into_iter()
                            .map(|device| {
                                offer(
                                    row(
                                        device,
                                        pad_resource_id(&device.key),
                                        "settings.tablet-pad",
                                    ),
                                    tablet::pad_operations(&device.key),
                                )
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

/// What the Keyboard and Pointing pages are built over: the `input` section
/// the view holds (`cx`), this device's section, what the area read, and the
/// effects of Settings the page's controls invoke.
struct PageOf<'a> {
    cx: &'a ViewCx<InputPrefs>,
    actions: &'a Actions<Settings>,
    device: &'a ui::framework::Setting<InputDevicePrefs>,
    area: &'a InputArea,
    /// Both sections as they are now, the sources normalised.
    last: Input,
    map_input: ui::KeyboardMapInput,
}

impl PageOf<'_> {
    /// `control` (a button), invoking `effect`.
    fn invoke<C: ui::framework::ActionControl<()>>(&self, control: C, effect: InputEffect) -> C {
        self.actions
            .bind_effect_invocation(control, &SettingsEffect::from(effect))
    }

    /// The row `row` builds over this device's section.
    fn on_device(&self, row: impl FnOnce(&ViewCx<InputDevicePrefs>) -> Item) -> Option<Item> {
        record_item(self.cx, self.device, row)
    }

    /// The Pointing page: the touchpad, mouse and pointer choices the
    /// attached devices call for, then one card per drawing tablet.
    fn pointing_page(&self) -> Element {
        let snapshot = self.area.devices_read();
        let sections = InputSections::for_devices(snapshot.as_ref());
        let mut cards: Vec<Element> = Vec::new();
        if sections.touchpad {
            cards.push(self.touchpad_card().into_element());
            // Everything else a touchpad does is moot while it is off.
            if self.last.device.touchpad_enabled {
                cards.push(self.touchpad_click_scroll_card().into_element());
            }
        }
        if sections.mouse {
            cards.push(self.mouse_card().into_element());
        }
        if sections.pointer {
            cards.push(self.pointer_options_card().into_element());
        }
        if sections.tablet {
            let owned = |kind| {
                snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.of_kind(kind).into_iter().cloned().collect())
                    .unwrap_or_default()
            };
            let tablets: Vec<InputDeviceSnapshot> = owned(InputDeviceKind::Tablet);
            let pads: Vec<InputDeviceSnapshot> = owned(InputDeviceKind::TabletPad);
            let actions = self.actions.clone();
            let outputs = tablet::tablet_output_choices();
            let configs = self.last.device.tablets.clone();
            if let Some(tablets) = self.cx.record(self.device, move |cx| {
                let cards = tablet::tablet_cards(cx, &actions, &tablets, &pads, &configs, &outputs);
                provider_settings_ui::widgets::page_of(
                    cards
                        .into_iter()
                        .map(SettingsSection::into_element)
                        .collect(),
                )
            }) {
                cards.push(tablets);
            }
        }
        provider_settings_ui::widgets::page_of(cards)
    }

    /// Everything that is specific to a touchpad, starting with whether it
    /// works at all. The touchpad key on a laptop keyboard flips the same
    /// saved setting. While it is off the rest is moot, so only the switch
    /// shows.
    fn touchpad_card(&self) -> SettingsSection {
        let mut content: Vec<Item> = self.on_device(touchpad).into_iter().collect();
        if self.last.device.touchpad_enabled {
            content.extend(self.on_device(|cx| {
                cx.setting(t("input-touchpad-disable-with-mouse"))
                    .described(t("input-touchpad-disable-with-mouse-description"))
                    .id("input.touchpad_disabled_on_external_mouse")
                    .nested()
                    .toggle(InputDevicePrefs::paths().touchpad_disabled_on_external_mouse)
            }));
            content.extend([
                self.cx
                    .setting(t("input-touchpad-speed"))
                    .id("input.touchpad_pointer_speed")
                    .named("input-touchpad-speed")
                    .slider_with(
                        |input: &InputPrefs| {
                            input.touchpad_pointer_speed.unwrap_or(input.pointer_speed)
                        },
                        InputPrefs::paths().touchpad_pointer_speed,
                        |speed| fraction(speed).map(Some),
                    ),
                pointing::scroll_direction(
                    self.cx,
                    false,
                    "input-touchpad-natural-scroll-description",
                    InputPrefs::paths().touchpad_natural_scroll,
                    "input.touchpad_natural_scroll",
                ),
                gestures(self.cx),
                disable_while_typing(self.cx),
            ]);
        }
        section(&t("input-touchpad-title"), rows(content))
    }

    /// Tapping, and the refinements a tap allows: dragging after a tap
    /// shows only while a tap clicks, and its lock only while tap-and-drag
    /// is on.
    fn tapping_rows(&self) -> Vec<Item> {
        let mut content = vec![tap_to_click(self.cx)];
        if self.last.tap_to_click {
            content.push(
                self.cx
                    .setting(t("input-tap-drag"))
                    .described(t("input-tap-drag-description"))
                    .id("input.tap_drag")
                    .nested()
                    .toggle(InputPrefs::paths().tap_drag),
            );
            if self.last.tap_drag {
                content.push(
                    self.cx
                        .setting(t("input-tap-drag-lock"))
                        .described(t("input-tap-drag-lock-description"))
                        .id("input.tap_drag_lock")
                        .nested()
                        .toggle(InputPrefs::paths().tap_drag_lock),
                );
            }
        }
        content
    }

    /// How a touchpad turns presses and finger movement into clicks and
    /// scrolling. Each choice is applied only to devices whose libinput
    /// driver offers it.
    fn touchpad_click_scroll_card(&self) -> SettingsSection {
        let mut content = self.tapping_rows();
        content.extend([
            pointing::click_method(self.cx),
            pointing::scroll_method(self.cx),
        ]);
        section(&t("input-clicking-title"), rows(content))
    }

    /// Mice, trackballs and pointing sticks.
    fn mouse_card(&self) -> SettingsSection {
        section(
            &t("input-mouse-title"),
            rows(vec![
                self.cx
                    .setting(t("input-mouse-speed"))
                    .id("input.mouse_pointer_speed")
                    .named("input-mouse-speed")
                    .slider_with(
                        |input: &InputPrefs| {
                            input.mouse_pointer_speed.unwrap_or(input.pointer_speed)
                        },
                        InputPrefs::paths().mouse_pointer_speed,
                        |speed| fraction(speed).map(Some),
                    ),
                pointing::scroll_direction(
                    self.cx,
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
    fn pointer_options_card(&self) -> SettingsSection {
        section(
            &t("input-pointer-title"),
            rows(vec![
                left_handed(self.cx),
                middle_click(self.cx),
                self.cx
                    .setting(t("input-acceleration"))
                    .id("input.accel_profile")
                    .among(
                        InputPrefs::paths().accel_profile,
                        ["adaptive".to_owned(), "flat".to_owned()],
                        |profile: &String| accel_shown(profile),
                    ),
                self.cx
                    .setting(t("input-scroll-speed"))
                    .described(t("input-scroll-speed-description"))
                    .id("input.scroll_speed")
                    .named("input-scroll-speed")
                    .slider_with(
                        |input: &InputPrefs| {
                            settings_store::hardware::scroll_speed_to_slider(input.scroll_speed)
                        },
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
            .setting(t("input-key-repeat"))
            .described(key_repeat_description(&self.last.person))
            .id("input.key_repeat")
            .named(t("input-key-repeat"))
            .slider_verb(key_repeat_slider_value, InputPrefs::SET_KEY_REPEAT)
    }
}

/// The normal slider owns one linked speed/delay curve. Imported desktop
/// settings retain their exact physical pair until changed, so render the
/// imported rate rather than an unrelated legacy slider fraction.
fn key_repeat_slider_value(input: &InputPrefs) -> f64 {
    let Some((_, rate)) = input.key_repeat_delay_ms.zip(input.key_repeat_rate_hz) else {
        return input.key_repeat;
    };
    let min = settings_store::hardware::KEY_REPEAT_SLIDER_MIN_HZ;
    let max = settings_store::hardware::KEY_REPEAT_SLIDER_MAX_HZ;
    (f64::from(rate.saturating_sub(min)) / f64::from(max - min)).clamp(0.0, 1.0)
}

fn key_repeat_description(input: &InputPrefs) -> String {
    match (input.key_repeat_delay_ms, input.key_repeat_rate_hz) {
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

fn spell_checker_card(cx: &ViewCx<TextAssistancePrefs>) -> SettingsSection {
    section(
        &t("input-spell-title"),
        rows(vec![
            check_spelling(cx),
            auto_correct(cx),
            cx.setting(t("input-spell-language"))
                .id("text.spelling_language")
                .keyed(
                    TextAssistancePrefs::paths().spelling_language,
                    spelling_language_choices(),
                    |text: &TextAssistancePrefs| text.spelling_language.clone(),
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
            provider_settings_ui::widgets::note(&t("input-spell-caption")),
        ]),
    )
}

/// The personal dictionary: a word to add, each word kept, editable in
/// place, and a reset; the fields keep what is typed while the page is
/// built again.
fn dictionary_card(actions: &Actions<Settings>, area: &InputArea) -> SettingsSection {
    let dictionary = area.dictionary.borrow();
    let Some(snapshot) = &dictionary.snapshot else {
        return section(
            &t("input-dictionary-title"),
            rows(vec![provider_settings_ui::widgets::note(&t(
                "input-dictionary-unavailable",
            ))]),
        );
    };
    let revision = snapshot.revision;
    let word = area.dictionary_word.clone();
    let language = area.dictionary_language.clone();
    let add = actions.bind_effect(
        Button::new("dictionary.add", &t("input-dictionary-add")),
        effect(InputEffect::ADD_DICTIONARY_WORD)
            .map(move |()| (word.get(), language.get(), revision)),
    );
    let mut children = vec![
        provider_settings_ui::widgets::note(&t("input-dictionary-caption")),
        field_row(
            &t("input-dictionary-word"),
            Box::new(
                TextInput::from_state(&area.dictionary_word)
                    .with_id("dictionary.word")
                    .with_accessible_name(t("input-dictionary-word"))
                    .with_placeholder(t("input-dictionary-word-placeholder"))
                    .with_spellcheck(false)
                    .with_autocorrect(false),
            ),
        ),
        provider_settings_ui::widgets::field_row_described(
            &t("input-dictionary-language"),
            &t("input-dictionary-language-caption"),
            Box::new(
                TextInput::from_state(&area.dictionary_language)
                    .with_id("dictionary.language")
                    .with_accessible_name(t("input-dictionary-language"))
                    .with_placeholder(t("input-dictionary-all-languages"))
                    .with_spellcheck(false)
                    .with_autocorrect(false),
            ),
        ),
        provider_settings_ui::widgets::row(Box::new(add)),
    ];
    for (index, entry) in snapshot.words.iter().enumerate() {
        children.push(dictionary_word_row(actions, area, index, entry, revision));
    }
    children.push(provider_settings_ui::widgets::row(Box::new(
        actions.bind_effect_invocation(
            Button::new("dictionary.reset", &t("input-dictionary-reset"))
                .with_enabled(!snapshot.words.is_empty()),
            &SettingsEffect::from(InputEffect::ResetDictionary { revision }),
        ),
    )));
    section(&t("input-dictionary-title"), rows(children))
}

fn dictionary_word_row(
    actions: &Actions<Settings>,
    area: &InputArea,
    index: usize,
    entry: &ui::personal_dictionary::Word,
    revision: u64,
) -> Item {
    let key = (entry.spelling.clone(), entry.language.clone());
    let (word, language) = area
        .dictionary_edits
        .borrow_mut()
        .entry(key)
        .or_insert_with(|| {
            (
                LocalState::new(entry.spelling.clone()),
                LocalState::new(entry.language.clone().unwrap_or_default()),
            )
        })
        .clone();
    let new_word = word.clone();
    let new_language = language.clone();
    let old_word = entry.spelling.clone();
    let old_language = entry.language.clone().unwrap_or_default();
    let save = actions.bind_effect(
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
    let remove = actions.bind_effect_invocation(
        Button::new(
            &format!("dictionary.{index}.remove"),
            &t("input-dictionary-remove"),
        ),
        &SettingsEffect::from(InputEffect::RemoveDictionaryWord {
            word: entry.spelling.clone(),
            language: entry.language.clone().unwrap_or_default(),
            revision,
        }),
    );
    // A word is edited in place: two fields and two actions on one line.
    provider_settings_ui::widgets::row(Box::new(
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

/// A switch of the input section, as the page and search show it: labelled
/// `$label`, explained by its description.
macro_rules! input_switches {
    ($section:ident: $($name:ident => $field:ident, $label:literal, $id:literal;)*) => {$(
        pub(crate) fn $name(cx: &ViewCx<$section>) -> Item {
            cx.setting(t($label))
                .described(t(concat!($label, "-description")))
                .id($id)
                .toggle($section::paths().$field)
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

pub(crate) fn check_spelling(cx: &ViewCx<TextAssistancePrefs>) -> Item {
    cx.setting(t("input-spell-check"))
        .described(t("input-spell-check-desc"))
        .id("text.check_spelling")
        .toggle(TextAssistancePrefs::paths().check_spelling)
}

pub(crate) fn auto_correct(cx: &ViewCx<TextAssistancePrefs>) -> Item {
    cx.setting(t("input-spell-auto"))
        .described(t("input-spell-auto-desc"))
        .id("text.auto_correct")
        .toggle(TextAssistancePrefs::paths().auto_correct)
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
