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
    Action, ActionAddress, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    Doc, EncodedAction, ExecutionLocus, OperationKind, ParamType, Resource, ResourceProvider,
    RetryPolicy,
};

use crate::categories::Category;
use crate::prefs::{AppearancePrefs, SystemPrefs};

/// Registry `type_id` for every settings action.
pub const SETTINGS_ACTION_TYPE: &str = "settings.action";

/// Operation protocol used by every Settings pane's exported presentation
/// action. The payload is the category's stable slug.
pub const OPEN_PANE_OPERATION_TYPE: &str = "settings.open-pane";

/// Stable action id shared by every pane resource.
pub const OPEN_PANE_ACTION: &str = "open";
pub const ACCOUNT_OPERATION_TYPE: &str = "settings.account-operation";

/// Device-local account-management commands. Payloads contain only stable ids
/// and provider kinds; credentials are collected by the Settings form and
/// never cross the resource/action boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountOperation {
    Add { kind: String },
    Open { id: String },
    Edit { id: String },
    Reauthenticate { id: String },
    SubmitForm,
    CancelForm,
    SubmitReauthentication,
    CancelReauthentication,
    Test { id: String },
    Remove { id: String },
}

/// Stable resource identity for a Settings pane.
pub fn pane_resource_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

/// Canonical address used by search, agents, accessibility, and resource-tree
/// navigation to reveal exactly one Settings pane.
pub fn open_pane_address(category: Category) -> ActionAddress {
    ActionAddress::new(pane_resource_id(category), OPEN_PANE_ACTION)
}

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
                validate_setting_target(key, value)?;
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

/// Refuse a delayed action whose target no longer belongs to Settings.  The
/// old generic setter accepted *any* root key, which meant a replay from an
/// obsolete UI could silently create an unrelated value.  Draft (`.edit`),
/// confirmation, and navigation keys are intentionally absent: they are local
/// presentation state, not collaborative resources.
fn validate_setting_target(key: &str, value: &SettingValue) -> Result<(), ActionError> {
    let text =
        || matches!(value, SettingValue::Text(text) if !text.is_empty() && text.len() <= 1024);
    let boolean = || matches!(value, SettingValue::Bool(_));
    let finite_float = || matches!(value, SettingValue::Float(number) if number.is_finite());
    let presentation_key = matches!(key.rsplit_once('.'), Some((_, "edit" | "error")));
    let allowed = match key {
        "appearance.theme"
        | "appearance.text_size"
        | "appearance.ui_scale"
        | "region.currency"
        | "region.date_format"
        | "region.time_format"
        | "region.measurement"
        | "region.number_style"
        | "region.first_day"
        | "time.timezone"
        | "power.lid"
        | "power.power_key"
        | "power.critical" => text(),
        "appearance.high_contrast"
        | "appearance.reduce_motion"
        | "time.auto"
        | "time.h24"
        | "shortcuts.vim_mode"
        | "notifications.do_not_disturb"
        | "notifications.show_banners"
        | "notifications.show_previews"
        | "notifications.play_sounds"
        | "notifications.show_on_lock_screen"
        | "notifications.show_badges"
        | "network.wifi_enabled"
        | "bluetooth.enabled"
        | "audio.muted"
        | "display.auto_brightness"
        | "display.night_light"
        | "input.natural_scroll"
        | "input.tap_to_click"
        | "power.auto_suspend"
        | "updates.auto" => boolean(),
        "audio.volume"
        | "display.brightness"
        | "display.scale"
        | "input.pointer_speed"
        | "input.key_repeat" => finite_float(),
        key if key.starts_with("shortcuts.") && !presentation_key => text(),
        key if key.starts_with("default_apps.") && !presentation_key => text(),
        // Device-mesh resources are discovered at runtime. Their stable
        // namespace is the stale-target boundary; only its documented scalar
        // preferences can enter it.
        "device_mesh.role" | "device_mesh.replication" => text(),
        "device_mesh.inbound_relay"
        | "device_mesh.remote_control_relay"
        | "device_mesh.app_serving"
        | "device_mesh.compute" => boolean(),
        key if key.starts_with("device_mesh.workspace.") && !presentation_key => boolean(),
        _ => false,
    };
    allowed.then_some(()).ok_or_else(|| {
        ActionError::Refused(format!(
            "settings target is stale, unknown, or has an invalid value: {key}"
        ))
    })
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
    provide_resources_with_accounts(doc, &[])
}

pub fn provide_resources_with_accounts(
    doc: &AutoCommit,
    account_inventory: &[accounts::Account],
) -> Vec<Resource> {
    let d = SystemPrefs::default();
    let mut root = Resource::new("settings.root", "settings.root", "Settings");
    for category in Category::ALL.iter().copied() {
        let mut pane = match category {
            Category::Network => network_section(doc),
            Category::Bluetooth => bluetooth_section(doc),
            Category::Sound => sound_section(doc),
            Category::Displays => displays_section(doc, &d),
            Category::MouseKeyboard => input_section(doc, &d),
            Category::Power => power_section(doc, &d),
            Category::Appearance => appearance_section(doc, &d),
            Category::Accessibility => accessibility_section(doc, &d),
            Category::Notifications => notifications_section(doc, &d),
            Category::RegionFormats => region_section(doc, &d),
            Category::TimeDate => time_date_section(doc, &d),
            Category::KeyboardShortcuts => shortcuts_section(doc),
            Category::DeviceMesh => device_mesh_section(doc),
            Category::Updates => updates_section(doc, &d),
            Category::ExternalAccounts => accounts_section(account_inventory),
            _ => Resource::new(
                pane_resource_id(category),
                "settings.section",
                category.label(),
            ),
        };
        if let Ok(open) = ActionSpec::operation(
            OPEN_PANE_ACTION,
            format!("Open {}", category.label()),
            OperationKind::View,
            OPEN_PANE_OPERATION_TYPE,
            &category.slug(),
        ) {
            // `&str` serialization is infallible in serde_json. Keep the
            // fallible API boundary explicit; the resource-coverage test
            // ensures a framework regression cannot silently drop this door.
            pane.actions.insert(0, open);
        }
        root = root.with_child(pane);
    }
    vec![root]
}

fn account_operation_spec(
    id: &str,
    label: impl Into<String>,
    kind: OperationKind,
    operation: &AccountOperation,
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, ACCOUNT_OPERATION_TYPE, operation).ok()
}

/// Build the exact nullary door used by both account-row controls and the
/// exported hierarchy. Returns `None` only if the framework rejects a payload
/// that consists solely of bounded strings.
pub fn account_operation_binding(
    resource_id: String,
    resource_label: String,
    action_id: &str,
    action_label: &str,
    kind: OperationKind,
    operation: &AccountOperation,
    destructive: bool,
) -> Option<qos_actions::ActionBinding<()>> {
    let mut spec = account_operation_spec(action_id, action_label, kind, operation)?;
    if destructive {
        spec = spec.destructive();
    }
    let resource =
        Resource::new(resource_id, "settings.external-account", resource_label).with_action(spec);
    qos_actions::ActionBinding::unit_for(&resource, action_id).ok()
}

fn accounts_section(account_inventory: &[accounts::Account]) -> Resource {
    let mut section = Resource::new(
        pane_resource_id(Category::ExternalAccounts),
        "settings.accounts",
        "External Accounts",
    );
    for (kind, label) in [
        ("mail", "Add email account"),
        ("caldav", "Add calendar account"),
        ("webdav", "Add WebDAV account"),
        ("nextcloud", "Add Nextcloud account"),
    ] {
        if let Some(action) = account_operation_spec(
            &format!("add-{kind}"),
            label,
            OperationKind::View,
            &AccountOperation::Add { kind: kind.into() },
        ) {
            section = section.with_action(action);
        }
    }
    for account in account_inventory {
        let mut resource = Resource::new(
            format!("settings.account:{}", account.id),
            "settings.external-account",
            if account.display_name.is_empty() {
                account.id.to_string()
            } else {
                account.display_name.clone()
            },
        );
        for (id, label, kind, operation) in [
            (
                "open",
                "Open account",
                OperationKind::View,
                AccountOperation::Open {
                    id: account.id.to_string(),
                },
            ),
            (
                "edit",
                "Edit account details",
                OperationKind::View,
                AccountOperation::Edit {
                    id: account.id.to_string(),
                },
            ),
            (
                "reauthenticate",
                "Sign in again",
                OperationKind::View,
                AccountOperation::Reauthenticate {
                    id: account.id.to_string(),
                },
            ),
            (
                "test",
                if account.type_id.as_str() == "email" {
                    "Check server reachability"
                } else {
                    "Verify sign-in"
                },
                OperationKind::Effect,
                AccountOperation::Test {
                    id: account.id.to_string(),
                },
            ),
            (
                "remove",
                "Remove from this device",
                OperationKind::Effect,
                AccountOperation::Remove {
                    id: account.id.to_string(),
                },
            ),
        ] {
            if let Some(mut action) = account_operation_spec(id, label, kind, &operation) {
                if id == "remove" {
                    action = action.destructive();
                }
                resource = resource.with_action(action);
            }
        }
        section = section.with_child(resource);
    }
    section
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
    sec = sec.with_action(choice_prompt_spec(
        "text_size",
        "Set text size",
        panes::appearance::TEXT_SIZE_KEY,
        "Small, Default, Large, Larger",
        &["Small", "Default", "Large", "Larger"],
        s(
            panes::appearance::TEXT_SIZE_KEY,
            panes::appearance::text_size_label(d.appearance.text_scale),
        ),
    ));
    sec = sec.with_action(choice_prompt_spec(
        "ui_scale",
        "Set UI scale",
        panes::appearance::UI_SCALE_KEY,
        "90%, 100%, 110%, 125%, 150%",
        &["90%", "100%", "110%", "125%", "150%"],
        s(panes::appearance::UI_SCALE_KEY, "100%"),
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
    let mut sec = Resource::new(
        pane_resource_id(Category::Displays),
        "settings.section",
        "Displays",
    );
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
        pane_resource_id(Category::MouseKeyboard),
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

/// Region and time are deliberately represented as individual named actions,
/// not an "advanced" catch-all, so a person or an assistive agent can find a
/// setting from the same hierarchy shown in the sidebar.
fn region_section(doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    use crate::panes;
    const CURRENCY_OPTIONS: &[&str] = &[
        "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR",
    ];
    const DATE_OPTIONS: &[&str] = &[
        "ISO (2023-03-15)",
        "US (03/15/2023)",
        "European (15/03/2023)",
        "Long (15 Mar 2023)",
    ];
    const TIME_OPTIONS: &[&str] = &["24-hour (14:05)", "12-hour (2:05 PM)"];
    const MEASUREMENT_OPTIONS: &[&str] = &["Metric (°C, km, kg)", "Imperial (°F, mi, lb)"];
    const NUMBER_OPTIONS: &[&str] = &["1,234,567.89", "1.234.567,89"];
    const FIRST_DAY_OPTIONS: &[&str] = &["Monday", "Sunday", "Saturday"];
    let current = |key: &str, fallback: &str| doc_str(doc, key).unwrap_or_else(|| fallback.into());
    let mut section = Resource::new(
        pane_resource_id(Category::RegionFormats),
        "settings.section",
        "Region & Formats",
    );
    for (id, label, key, options, fallback) in [
        (
            "currency",
            "Set currency",
            panes::region::CURRENCY_KEY,
            CURRENCY_OPTIONS,
            "USD",
        ),
        (
            "date",
            "Set date format",
            panes::region::DATE_KEY,
            DATE_OPTIONS,
            "ISO",
        ),
        (
            "time",
            "Set time format",
            panes::region::TIME_KEY,
            TIME_OPTIONS,
            "24-hour (14:05)",
        ),
        (
            "measurement",
            "Set measurement units",
            panes::region::MEASUREMENT_KEY,
            MEASUREMENT_OPTIONS,
            "Metric (°C, km, kg)",
        ),
        (
            "number",
            "Set number format",
            panes::region::NUMBER_STYLE_KEY,
            NUMBER_OPTIONS,
            "1,234,567.89",
        ),
        (
            "first-day",
            "Set first day of week",
            panes::region::FIRST_DAY_KEY,
            FIRST_DAY_OPTIONS,
            "Monday",
        ),
    ] {
        // The action-spec closure requires static choices; all entries above
        // are literals and receive a static promotion here.
        section = section.with_action(choice_prompt_spec(
            id,
            label,
            key,
            label,
            options,
            current(key, fallback),
        ));
    }
    section
}

fn time_date_section(doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, fallback| doc_bool(doc, key).unwrap_or(fallback);
    let mut section = Resource::new(
        pane_resource_id(Category::TimeDate),
        "settings.section",
        "Time & Date",
    );
    section = section.with_action(bool_flip_spec(
        "automatic-time",
        panes::time_date::AUTO_TIME_KEY,
        b(panes::time_date::AUTO_TIME_KEY, true),
        "automatic time",
    ));
    section = section.with_action(bool_flip_spec(
        "24-hour",
        panes::time_date::H24_KEY,
        b(panes::time_date::H24_KEY, true),
        "24-hour time",
    ));
    section.with_action(
        ActionSpec::typed(
            "timezone",
            "Set time zone",
            "IANA time-zone identifier",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "choice",
                ParamType::Text,
                "Time zone",
            )],
            |args| {
                encode(&set_text_action(
                    panes::time_date::TIMEZONE_KEY,
                    args.text("choice").unwrap_or("UTC"),
                ))
            },
        )
        .with_contract(setting_contract()),
    )
}

fn shortcuts_section(doc: &AutoCommit) -> Resource {
    let b = |key: &str, fallback| doc_bool(doc, key).unwrap_or(fallback);
    let mut section = Resource::new(
        pane_resource_id(Category::KeyboardShortcuts),
        "settings.section",
        "Keyboard Shortcuts",
    );
    section = section.with_action(bool_flip_spec(
        "vim-mode",
        "shortcuts.vim_mode",
        b("shortcuts.vim_mode", false),
        "Vim-style desktop navigation",
    ));
    for (id, label, key) in [
        ("search", "Set Search shortcut", "shortcuts.search"),
        (
            "overview",
            "Set Workspace Overview shortcut",
            "shortcuts.overview",
        ),
        ("lock", "Set Lock Screen shortcut", "shortcuts.lock"),
        (
            "quick-settings",
            "Set Quick Settings shortcut",
            "shortcuts.quick_settings",
        ),
    ] {
        section = section.with_action(
            ActionSpec::typed(
                id,
                label,
                "e.g. Super+K",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required("choice", ParamType::Text, "Shortcut")],
                move |args| {
                    encode(&set_text_action(
                        key,
                        args.text("choice").unwrap_or_default(),
                    ))
                },
            )
            .with_contract(setting_contract()),
        );
    }
    section
}

fn device_mesh_section(doc: &AutoCommit) -> Resource {
    let b = |key: &str, fallback| doc_bool(doc, key).unwrap_or(fallback);
    let mut section = Resource::new(
        pane_resource_id(Category::DeviceMesh),
        "settings.section",
        "Devices & Sync",
    );
    section = section.with_action(choice_prompt_spec(
        "role",
        "Set device role",
        "device_mesh.role",
        "Personal device, Available while active, Always-on server",
        &[
            "Personal device",
            "Available while active",
            "Always-on server",
        ],
        doc_str(doc, "device_mesh.role").unwrap_or_else(|| "Personal device".into()),
    ));
    section = section.with_action(choice_prompt_spec(
        "replication",
        "Set replication",
        "device_mesh.replication",
        "On demand, Workspace state, Full workspace copy",
        &["On demand", "Workspace state", "Full workspace copy"],
        doc_str(doc, "device_mesh.replication").unwrap_or_else(|| "On demand".into()),
    ));
    for (id, key, label) in [
        (
            "inbound-relay",
            "device_mesh.inbound_relay",
            "inbound relay",
        ),
        (
            "remote-control",
            "device_mesh.remote_control_relay",
            "remote-control relay",
        ),
        ("app-serving", "device_mesh.app_serving", "app serving"),
        ("compute", "device_mesh.compute", "remote compute"),
    ] {
        section = section.with_action(bool_flip_spec(id, key, b(key, false), label));
    }
    section
}

impl ResourceProvider for crate::SettingsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources_with_accounts(doc, self.account_resource_inventory())
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

    /// Ctrl+K on Settings yields the tree: one `settings.root` with exactly one
    /// stable `settings.section` child per category. Every pane exports its
    /// View operation; panes with mutable settings additionally export the
    /// collaborative document actions they own. Drives the app's real hook.
    #[test]
    fn ctrl_k_publishes_the_settings_tree() {
        let app = SettingsApp::new();
        let doc = AutoCommit::new();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one configuration root");
        let root = &resources[0];
        assert_eq!(root.kind, "settings.root");
        assert_eq!(root.children.len(), Category::ALL.len());
        for category in Category::ALL.iter().copied() {
            let address = open_pane_address(category);
            let pane = root
                .children
                .iter()
                .find(|pane| pane.id.as_str() == address.resource)
                .unwrap_or_else(|| panic!("{} owns a resource", category.label()));
            assert_eq!(pane.label, category.label());
            let open = qos_actions::door::resolve(&resources, &address)
                .unwrap_or_else(|| panic!("{} exports open", category.label()));
            assert_eq!(open.kind, OperationKind::View);
            assert_eq!(open.contract, ActionContract::view());
        }

        let notif = find(&resources, "settings.section:notifications");
        let ids: Vec<&str> = notif.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"dnd"), "{ids:?}");
        assert!(ids.contains(&"banners"), "{ids:?}");
        assert!(ids.contains(&"lock_screen"), "{ids:?}");
        let dnd = notif
            .actions
            .iter()
            .find(|action| action.id == "dnd")
            .unwrap();
        assert_eq!(dnd.kind, OperationKind::Document);
        assert!(dnd.contract.tags.iter().any(|tag| tag == "system-setting"));

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
            labels.iter().any(|label| label == "Network — Open Network"),
            "{labels:?}"
        );
    }

    #[test]
    fn pane_addresses_are_stable_and_unique() {
        let mut addresses = std::collections::HashSet::new();
        for category in Category::ALL.iter().copied() {
            let address = open_pane_address(category);
            assert_eq!(
                address.resource,
                format!("settings.section:{}", category.slug())
            );
            assert_eq!(address.action, OPEN_PANE_ACTION);
            assert!(
                addresses.insert(address),
                "duplicate address for {category:?}"
            );
        }
    }

    #[test]
    fn account_removal_resource_requires_host_confirmation() {
        let mut account = accounts::Account::new("stable-account", "nextcloud");
        account.display_name = "Work cloud".into();
        let resources = provide_resources_with_accounts(&AutoCommit::new(), &[account]);
        let accounts = find(&resources, &pane_resource_id(Category::ExternalAccounts));
        let account = accounts
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "settings.account:stable-account")
            .expect("account nested beneath External Accounts");
        let remove = account
            .actions
            .iter()
            .find(|action| action.id == "remove")
            .expect("remove resource action");
        assert_eq!(remove.kind, OperationKind::Effect);
        assert_eq!(remove.contract.risk, qos_actions::RiskLevel::Destructive);
        assert_eq!(
            remove.contract.confirmation,
            qos_actions::ConfirmationPolicy::Confirm
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

    #[test]
    fn stale_or_presentation_targets_are_refused() {
        let mut doc = AutoCommit::new();
        for action in [
            set_bool_action("appearance.high_contrast.edit", true),
            set_text_action("settings.select.appearance", "true"),
            set_text_action("unknown.preference", "value"),
        ] {
            assert!(matches!(
                action.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
        }
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

        // The shared typed door rejects an unrecognised choice before the
        // application's builder can write junk or silently reinterpret it.
        let typo = serde_json::Map::from_iter([(
            "choice".to_owned(),
            serde_json::Value::String("not-a-theme".to_owned()),
        )]);
        let error = theme
            .build_json(&typo)
            .expect_err("unknown theme is invalid");
        assert!(error.to_string().contains("must be one of"), "{error}");
    }
}
