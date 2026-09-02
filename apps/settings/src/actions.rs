//! Settings' exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every setting the OS stores is a value in the shared document, keyed by its
//! stable dotted name (`notifications.do_not_disturb`, `appearance.theme`,
//! `input.pointer_speed`, …). Changing one is a [`SettingsAction`]: a small
//! serializable value that deterministically writes that key. The exact same
//! [`EncodedAction`] is what an on-screen control dispatches and what the Ctrl+K
//! palette dispatches — one source of truth, one replay path, one undo boundary.
//!
//! The resource tree mirrors the settings hub: the configuration root owns a
//! child per section (Notifications, Appearance, Sound, …), and each section
//! exports the [`ActionSpec`]s for the settings it owns.
//!
//! ## What is *not* an action
//!
//! * **The hardware / service apply** — pushing a preference to the audio graph,
//!   the Wi-Fi radio, the backlight, the compositor's theme, or the shared
//!   `settings.json` on disk is an external side effect *driven by* the document
//!   state (see each pane's `sync`), never part of [`Action::apply`], which only
//!   writes the CRDT scalar. A setting's value lands in the document; a separate
//!   reaction carries it to the world.
//! * **Presentation** — which pane is open, the sidebar search, scroll, a
//!   section's expand/collapse — never leaves the machine and never becomes an
//!   action.
//!
//! ## Determinism contract
//!
//! [`Action::apply`] carries the exact key **and** value in its payload and
//! writes that scalar; it never reads the RNG or the wall-clock, so two replicas
//! at the same heads land on the byte-identical result. Writing the value the
//! key already holds is skipped (a no-op leaves the document — and its heads —
//! untouched), so a double-applied or stale envelope can't churn a peer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, Resource, ResourceProvider, RetryPolicy,
};

use crate::prefs::{AppearancePrefs, SystemPrefs};

/// Registry `type_id` for every settings action.
pub const SETTINGS_ACTION_TYPE: &str = "settings.action";

/// A typed setting value carried whole in the action payload. The variant fixes
/// the Automerge scalar written, so a bool setting can never be replayed as a
/// string on a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl SettingValue {
    /// The Automerge scalar this value writes.
    fn scalar(&self) -> ScalarValue {
        match self {
            SettingValue::Bool(b) => ScalarValue::Boolean(*b),
            SettingValue::Int(i) => ScalarValue::Int(*i),
            SettingValue::Float(f) => ScalarValue::F64(*f),
            SettingValue::Text(s) => ScalarValue::Str(s.as_str().into()),
        }
    }

    /// Whether the document already holds exactly this value at `key` — the
    /// guard that keeps a re-applied action a true no-op.
    fn matches_doc(&self, doc: &Doc, key: &str) -> bool {
        match (self, doc.get(&ROOT, key)) {
            (SettingValue::Bool(b), Ok(Some((Value::Scalar(s), _)))) => {
                matches!(s.as_ref(), ScalarValue::Boolean(v) if v == b)
            }
            (SettingValue::Int(i), Ok(Some((Value::Scalar(s), _)))) => {
                matches!(s.as_ref(), ScalarValue::Int(v) if v == i)
            }
            (SettingValue::Float(f), Ok(Some((Value::Scalar(s), _)))) => {
                matches!(s.as_ref(), ScalarValue::F64(v) if (v - f).abs() < f64::EPSILON)
            }
            (SettingValue::Text(t), Ok(Some((Value::Scalar(s), _)))) => {
                matches!(s.as_ref(), ScalarValue::Str(v) if v.as_str() == t.as_str())
            }
            _ => false,
        }
    }
}

/// Every collaborative operation Settings exports on the stored preferences.
///
/// One variant: set a named setting to a value. The dotted key and the typed
/// value are both carried in the payload, so [`Action::apply`] is a pure write
/// and replays identically everywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SettingsAction {
    /// Write `value` to the setting at `key`. Idempotent: writing the value the
    /// key already holds changes nothing.
    SetSetting { key: String, value: SettingValue },
}

impl Action for SettingsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            SettingsAction::SetSetting { key, value } => {
                // Skip the write when the key already holds this value, so a
                // replayed / stale envelope leaves the document (and its heads)
                // untouched.
                if !value.matches_doc(doc, key) {
                    doc.put(&ROOT, key.as_str(), value.scalar())?;
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // A preference is a personal / per-workspace choice: apply locally and
        // replicate, no host ordering needed.
        ExecutionLocus::ClientLocal
    }
}

/// Register the settings action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SettingsAction>(SETTINGS_ACTION_TYPE);
}

/// Encode any [`SettingsAction`] into the wire form the palette and the
/// on-screen controls both dispatch. The single point that turns a settings
/// action into a replayable envelope.
///
/// # Panics
///
/// Never panics: a `SettingsAction` that fails to serialize (it never should —
/// every field is a plain string, bool, int or finite float) falls back to an
/// empty-payload envelope rather than crashing the caller.
pub fn encode(action: &SettingsAction) -> EncodedAction {
    EncodedAction::encode(SETTINGS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: SETTINGS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// controls (see `crate::panes`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn set_bool_action(key: impl Into<String>, value: bool) -> SettingsAction {
    SettingsAction::SetSetting {
        key: key.into(),
        value: SettingValue::Bool(value),
    }
}

pub fn set_int_action(key: impl Into<String>, value: i64) -> SettingsAction {
    SettingsAction::SetSetting {
        key: key.into(),
        value: SettingValue::Int(value),
    }
}

pub fn set_float_action(key: impl Into<String>, value: f64) -> SettingsAction {
    SettingsAction::SetSetting {
        key: key.into(),
        value: SettingValue::Float(value),
    }
}

pub fn set_text_action(key: impl Into<String>, value: impl Into<String>) -> SettingsAction {
    SettingsAction::SetSetting {
        key: key.into(),
        value: SettingValue::Text(value.into()),
    }
}

// ===========================================================================
// Reading the current document value (presentation side — builds labels and
// flip targets; never part of `apply`).
// ===========================================================================

fn doc_bool(doc: &AutoCommit, key: &str) -> Option<bool> {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn doc_str(doc: &AutoCommit, key: &str) -> Option<String> {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(t) => Some(t.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => doc.text(&obj).ok(),
        _ => None,
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn bool_flip_spec(id: &str, key: &str, on_now: bool, noun: &str) -> ActionSpec {
    // The label always describes what the action *does*, so it reads right in
    // the palette next to its section.
    let label = if on_now {
        format!("Turn off {noun}")
    } else {
        format!("Turn on {noun}")
    };
    let action = set_bool_action(key, !on_now);
    ActionSpec::immediate(id, label.clone(), SETTINGS_ACTION_TYPE, &action)
        .unwrap_or_else(|_| {
            // The fallible builder's fallback. `ActionSpec`'s fields are
            // private now that it carries typed parameters, so construct it
            // through the constructor that takes an already-encoded action.
            ActionSpec::immediate_encoded(id, label, action.locus(), encode(&action))
        })
        .with_contract(setting_contract())
}

fn setting_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["configuration".into(), "system-setting".into()],
        ..ActionContract::default()
    }
}

fn float_prompt_spec(
    id: &str,
    label: &str,
    key: &'static str,
    placeholder: &str,
    lo: f64,
    hi: f64,
) -> ActionSpec {
    ActionSpec::typed(
        id,
        label,
        placeholder,
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("value", ParamType::Number, "New setting value")
                .with_schema(serde_json::json!({ "minimum": lo, "maximum": hi })),
        ],
        move |args| {
            let v = args.number("value").unwrap_or(lo).clamp(lo, hi);
            encode(&set_float_action(key, v))
        },
    )
    .with_contract(setting_contract())
}

fn choice_prompt_spec(
    id: &str,
    label: &str,
    key: &'static str,
    placeholder: &str,
    options: &'static [&'static str],
    fallback: String,
) -> ActionSpec {
    ActionSpec::typed(
        id,
        label,
        placeholder,
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("choice", ParamType::Text, "New setting value")
                .with_choices(options.iter().copied()),
        ],
        move |args| {
            let v = args
                .text("choice")
                .map_or_else(|| fallback.clone(), str::to_string);
            encode(&set_text_action(key, v))
        },
    )
    .with_contract(setting_contract())
}

/// Build the resource tree Settings publishes: the configuration root, a child
/// per section, each exporting the settings it owns as the same actions its
/// on-screen controls dispatch. Current values are read from `doc` (falling
/// back to the shipped defaults for a section whose pane hasn't seeded its keys
/// yet), so a bool's flip target and an enum's fallback reflect what is set.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let d = SystemPrefs::default();
    let mut root = Resource::new("settings.root", "settings.root", "Settings");
    root = root.with_child(notifications_section(doc, &d));
    root = root.with_child(appearance_section(doc, &d));
    root = root.with_child(sound_section(doc));
    root = root.with_child(displays_section(doc, &d));
    root = root.with_child(input_section(doc, &d));
    root = root.with_child(power_section(doc, &d));
    root = root.with_child(accessibility_section(doc, &d));
    root = root.with_child(updates_section(doc, &d));
    root = root.with_child(network_section(doc));
    root = root.with_child(bluetooth_section(doc));
    vec![root]
}

fn notifications_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new(
        "settings.section:notifications",
        "settings.section",
        "Notifications",
    );
    sec = sec.with_action(bool_flip_spec(
        "dnd",
        panes::notifications::DND_KEY,
        b(
            panes::notifications::DND_KEY,
            d.notifications.do_not_disturb,
        ),
        "Do Not Disturb",
    ));
    sec = sec.with_action(bool_flip_spec(
        "banners",
        panes::notifications::BANNERS_KEY,
        b(
            panes::notifications::BANNERS_KEY,
            d.notifications.show_banners,
        ),
        "notification banners",
    ));
    sec = sec.with_action(bool_flip_spec(
        "previews",
        panes::notifications::PREVIEWS_KEY,
        b(
            panes::notifications::PREVIEWS_KEY,
            d.notifications.show_previews,
        ),
        "message previews",
    ));
    sec = sec.with_action(bool_flip_spec(
        "sounds",
        panes::notifications::SOUNDS_KEY,
        b(
            panes::notifications::SOUNDS_KEY,
            d.notifications.play_sounds,
        ),
        "notification sounds",
    ));
    sec = sec.with_action(bool_flip_spec(
        "lock_screen",
        panes::notifications::LOCK_SCREEN_KEY,
        b(
            panes::notifications::LOCK_SCREEN_KEY,
            d.notifications.show_on_lock_screen,
        ),
        "lock-screen notifications",
    ));
    sec
}

fn appearance_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let s = |key: &str, default: &str| doc_str(doc, key).unwrap_or_else(|| default.to_string());
    let mut sec = Resource::new(
        "settings.section:appearance",
        "settings.section",
        "Appearance",
    );
    let cur_theme = s(panes::appearance::THEME_KEY, &d.appearance.theme);
    sec = sec.with_action(choice_prompt_spec(
        "theme",
        "Set theme",
        panes::appearance::THEME_KEY,
        "Dark, Light, Dracula, Nord, Gruvbox, Solarized",
        AppearancePrefs::THEMES,
        cur_theme,
    ));
    sec = sec.with_action(bool_flip_spec(
        "high_contrast",
        panes::appearance::HIGH_CONTRAST_KEY,
        b(
            panes::appearance::HIGH_CONTRAST_KEY,
            d.appearance.high_contrast,
        ),
        "high contrast",
    ));
    sec = sec.with_action(bool_flip_spec(
        "reduce_motion",
        panes::appearance::REDUCE_MOTION_KEY,
        b(
            panes::appearance::REDUCE_MOTION_KEY,
            d.appearance.reduce_motion,
        ),
        "reduced motion",
    ));
    sec = sec.with_action(float_prompt_spec(
        "text_size",
        "Set text size",
        panes::appearance::TEXT_SIZE_KEY,
        "0.8 – 1.5",
        0.8,
        1.5,
    ));
    sec = sec.with_action(float_prompt_spec(
        "ui_scale",
        "Set UI scale",
        panes::appearance::UI_SCALE_KEY,
        "0.8 – 2.0",
        0.8,
        2.0,
    ));
    sec
}

fn sound_section(doc: &AutoCommit) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new("settings.section:sound", "settings.section", "Sound");
    sec = sec.with_action(bool_flip_spec(
        "muted",
        panes::audio::MUTED_KEY,
        b(panes::audio::MUTED_KEY, false),
        "output mute",
    ));
    sec = sec.with_action(float_prompt_spec(
        "volume",
        "Set output volume",
        panes::audio::VOLUME_KEY,
        "0.0 – 1.0",
        0.0,
        1.0,
    ));
    sec
}

fn displays_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new("settings.section:display", "settings.section", "Displays");
    sec = sec.with_action(bool_flip_spec(
        "auto_brightness",
        panes::display::AUTO_BRIGHTNESS_KEY,
        b(
            panes::display::AUTO_BRIGHTNESS_KEY,
            d.display.auto_brightness,
        ),
        "automatic brightness",
    ));
    sec = sec.with_action(bool_flip_spec(
        "night_light",
        panes::display::NIGHT_LIGHT_KEY,
        b(panes::display::NIGHT_LIGHT_KEY, d.display.night_light),
        "Night Light",
    ));
    sec = sec.with_action(float_prompt_spec(
        "brightness",
        "Set brightness",
        panes::display::BRIGHTNESS_KEY,
        "0.0 – 1.0",
        0.0,
        1.0,
    ));
    sec = sec.with_action(float_prompt_spec(
        "scale",
        "Set display scale",
        panes::display::DISPLAY_SCALE_KEY,
        "1.0, 1.25, 1.5, 2.0",
        1.0,
        3.0,
    ));
    sec
}

fn input_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new(
        "settings.section:input",
        "settings.section",
        "Mouse & Keyboard",
    );
    sec = sec.with_action(bool_flip_spec(
        "natural_scroll",
        panes::mouse_keyboard::NATURAL_SCROLL_KEY,
        b(
            panes::mouse_keyboard::NATURAL_SCROLL_KEY,
            d.input.natural_scroll,
        ),
        "natural scrolling",
    ));
    sec = sec.with_action(bool_flip_spec(
        "tap_to_click",
        panes::mouse_keyboard::TAP_CLICK_KEY,
        b(panes::mouse_keyboard::TAP_CLICK_KEY, d.input.tap_to_click),
        "tap to click",
    ));
    sec = sec.with_action(float_prompt_spec(
        "pointer_speed",
        "Set pointer speed",
        panes::mouse_keyboard::POINTER_SPEED_KEY,
        "0.0 – 1.0",
        0.0,
        1.0,
    ));
    sec = sec.with_action(float_prompt_spec(
        "key_repeat",
        "Set key-repeat rate",
        panes::mouse_keyboard::KEY_REPEAT_KEY,
        "0.0 – 1.0",
        0.0,
        1.0,
    ));
    sec
}

fn power_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let s = |key: &str, default: &str| doc_str(doc, key).unwrap_or_else(|| default.to_string());
    let mut sec = Resource::new("settings.section:power", "settings.section", "Power");
    sec = sec.with_action(bool_flip_spec(
        "auto_suspend",
        panes::power::AUTO_SUSPEND_KEY,
        b(panes::power::AUTO_SUSPEND_KEY, d.power.auto_suspend),
        "automatic suspend",
    ));
    let power_actions = &["Suspend", "Lock screen", "Do nothing"];
    let cur_lid = s(panes::power::LID_KEY, d.power.lid_close_action.display());
    sec = sec.with_action(choice_prompt_spec(
        "lid",
        "Set lid-close action",
        panes::power::LID_KEY,
        "Suspend, Lock screen, Do nothing",
        power_actions,
        cur_lid,
    ));
    let cur_key = s(
        panes::power::POWER_KEY_KEY,
        d.power.power_key_action.display(),
    );
    sec = sec.with_action(choice_prompt_spec(
        "power_key",
        "Set power-key action",
        panes::power::POWER_KEY_KEY,
        "Suspend, Lock screen, Do nothing",
        power_actions,
        cur_key,
    ));
    let cur_crit = s(
        panes::power::CRITICAL_KEY,
        d.power.critical_action.display(),
    );
    sec = sec.with_action(choice_prompt_spec(
        "critical",
        "Set critical-battery action",
        panes::power::CRITICAL_KEY,
        "Suspend, Lock screen, Do nothing",
        power_actions,
        cur_crit,
    ));
    sec
}

fn accessibility_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new(
        "settings.section:accessibility",
        "settings.section",
        "Accessibility",
    );
    sec = sec.with_action(bool_flip_spec(
        "large_cursor",
        panes::accessibility::LARGE_CURSOR_KEY,
        b(
            panes::accessibility::LARGE_CURSOR_KEY,
            d.accessibility.large_cursor,
        ),
        "the large cursor",
    ));
    sec = sec.with_action(bool_flip_spec(
        "screen_reader",
        panes::accessibility::SCREEN_READER_KEY,
        b(
            panes::accessibility::SCREEN_READER_KEY,
            d.accessibility.screen_reader,
        ),
        "the screen reader",
    ));
    sec = sec.with_action(bool_flip_spec(
        "osk",
        panes::accessibility::OSK_KEY,
        b(
            panes::accessibility::OSK_KEY,
            d.accessibility.on_screen_keyboard,
        ),
        "the on-screen keyboard",
    ));
    sec = sec.with_action(bool_flip_spec(
        "sticky_keys",
        panes::accessibility::STICKY_KEYS_KEY,
        b(
            panes::accessibility::STICKY_KEYS_KEY,
            d.accessibility.sticky_keys,
        ),
        "sticky keys",
    ));
    sec = sec.with_action(bool_flip_spec(
        "slow_keys",
        panes::accessibility::SLOW_KEYS_KEY,
        b(
            panes::accessibility::SLOW_KEYS_KEY,
            d.accessibility.slow_keys,
        ),
        "slow keys",
    ));
    sec = sec.with_action(bool_flip_spec(
        "visual_alerts",
        panes::accessibility::VISUAL_ALERTS_KEY,
        b(
            panes::accessibility::VISUAL_ALERTS_KEY,
            d.accessibility.visual_alerts,
        ),
        "visual alerts",
    ));
    sec = sec.with_action(bool_flip_spec(
        "cursor_crosshair",
        panes::accessibility::CURSOR_CROSSHAIR_KEY,
        b(
            panes::accessibility::CURSOR_CROSSHAIR_KEY,
            d.accessibility.cursor_crosshair,
        ),
        "the pointer crosshair",
    ));
    sec = sec.with_action(bool_flip_spec(
        "thick_text_caret",
        panes::accessibility::THICK_CARET_KEY,
        b(
            panes::accessibility::THICK_CARET_KEY,
            d.accessibility.thick_text_caret,
        ),
        "the thicker text cursor",
    ));
    sec = sec.with_action(bool_flip_spec(
        "switch_control",
        panes::accessibility::SWITCH_CONTROL_KEY,
        b(
            panes::accessibility::SWITCH_CONTROL_KEY,
            d.accessibility.switch_control,
        ),
        "switch control",
    ));
    sec
}

fn updates_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new("settings.section:updates", "settings.section", "Updates");
    sec = sec.with_action(bool_flip_spec(
        "auto",
        panes::updates::AUTO_KEY,
        b(panes::updates::AUTO_KEY, d.updates.auto),
        "automatic updates",
    ));
    sec
}

fn network_section(doc: &AutoCommit) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new("settings.section:network", "settings.section", "Network");
    sec = sec.with_action(bool_flip_spec(
        "wifi",
        panes::network::WIFI_ENABLED_KEY,
        b(panes::network::WIFI_ENABLED_KEY, true),
        "Wi-Fi",
    ));
    sec
}

fn bluetooth_section(doc: &AutoCommit) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut bt = Resource::new(
        "settings.section:bluetooth",
        "settings.section",
        "Bluetooth",
    );
    bt = bt.with_action(bool_flip_spec(
        "bluetooth",
        panes::bluetooth::BT_ENABLED_KEY,
        b(panes::bluetooth::BT_ENABLED_KEY, true),
        "Bluetooth",
    ));
    bt
}

impl ResourceProvider for crate::SettingsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Level-triggered: recomputed each frame from the shared document, which
        // every pane keeps reconciled with the on-disk settings store.
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SettingsApp;
    use crate::panes;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    fn find<'a>(sections: &'a [Resource], id: &str) -> &'a Resource {
        sections[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("section {id} present"))
    }

    /// Ctrl+K on Settings yields the tree: one `settings.root` with a
    /// `settings.section` child per category, each exporting the settings it
    /// owns. Drives the real resource hook on the app.
    #[test]
    fn ctrl_k_publishes_the_settings_tree() {
        let app = SettingsApp::new();
        let doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one configuration root");
        let root = &resources[0];
        assert_eq!(root.kind, "settings.root");
        assert!(
            root.children.len() >= 8,
            "a child per section: {}",
            root.children.len()
        );

        let notif = find(&resources, "settings.section:notifications");
        let ids: Vec<&str> = notif.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"dnd"), "{ids:?}");
        assert!(ids.contains(&"banners"), "{ids:?}");
        assert!(ids.contains(&"lock_screen"), "{ids:?}");

        let appearance = find(&resources, "settings.section:appearance");
        let ids: Vec<&str> = appearance.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"theme"), "{ids:?}");
        assert!(ids.contains(&"ui_scale"), "{ids:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Do Not Disturb")
                || l.contains("theme")
                || l.contains("Settings")),
            "{labels:?}"
        );
    }

    /// A bool flip spec's target follows the current document value: on when the
    /// key is off, off when it is on.
    #[test]
    fn bool_flip_target_follows_current_value() {
        let mut doc = AutoCommit::new();
        let _ = doc.put(
            &ROOT,
            panes::notifications::DND_KEY,
            ScalarValue::Boolean(false),
        );
        let notif = {
            let app = SettingsApp::new();
            let res = Widget::resources(&app, &doc);
            find(&res, "settings.section:notifications").clone()
        };
        let dnd = notif.actions.iter().find(|a| a.id == "dnd").unwrap();
        // Off now → the flip turns it on.
        assert_eq!(
            immediate_encode(dnd),
            encode(&set_bool_action(panes::notifications::DND_KEY, true))
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// writes the setting and records exactly one undo boundary, and Ctrl+Z
    /// reverts precisely that write.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        // Seed the key off, as a pane would.
        let _ = state.doc.put(
            &ROOT,
            panes::notifications::DND_KEY,
            ScalarValue::Boolean(false),
        );
        let before_heads = state.doc.get_heads();

        let action = set_bool_action(panes::notifications::DND_KEY, true);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            doc_bool(&state.doc, panes::notifications::DND_KEY),
            Some(true),
            "the action set DND on"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            doc_bool(&state.doc, panes::notifications::DND_KEY),
            Some(false),
            "undo reverted the setting and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = AutoCommit::new();
        a.put(
            &ROOT,
            panes::appearance::THEME_KEY,
            ScalarValue::Str("Dark".into()),
        )
        .unwrap();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_text_action(panes::appearance::THEME_KEY, "Nord");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            doc_str(&a, panes::appearance::THEME_KEY).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            doc_str(&b, panes::appearance::THEME_KEY),
            doc_str(&a, panes::appearance::THEME_KEY)
        );
    }

    /// Writing the value a key already holds is a no-op: the document's heads are
    /// unchanged (so a replayed / stale envelope can't churn a peer).
    #[test]
    fn setting_to_current_value_is_a_no_op() {
        let mut doc = AutoCommit::new();
        let _ = doc.put(
            &ROOT,
            panes::display::NIGHT_LIGHT_KEY,
            ScalarValue::Boolean(true),
        );
        doc.commit();
        let before = doc.get_heads();
        let action = set_bool_action(panes::display::NIGHT_LIGHT_KEY, true);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(
            doc.get_heads(),
            before,
            "setting the value it already holds changed nothing"
        );
    }

    /// Single source of truth: a prompted "Set theme" spec and the text builder
    /// encode byte-identically for the same choice.
    #[test]
    fn palette_and_builder_encode_identically() {
        let app = SettingsApp::new();
        let doc = AutoCommit::new();
        let res = Widget::resources(&app, &doc);
        let appearance = find(&res, "settings.section:appearance");
        let theme = appearance.actions.iter().find(|a| a.id == "theme").unwrap();
        let from_spec = prompted_encode(theme, "Gruvbox");
        let from_builder = encode(&set_text_action(panes::appearance::THEME_KEY, "Gruvbox"));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for theme"
        );

        // The typed action contract rejects an unrecognised choice before its
        // builder runs, so invalid palette input cannot silently masquerade as
        // a successful write of the current value.
        let from_typo = prompted_encode(theme, "not-a-theme");
        assert_eq!(
            from_typo.type_id,
            qos_actions::resource::INVALID_ARGUMENTS_TYPE
        );
        assert!(
            String::from_utf8_lossy(&from_typo.payload).contains("must be one of"),
            "the refusal explains the declared choices"
        );
    }
}
