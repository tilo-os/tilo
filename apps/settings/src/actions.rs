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
    Doc, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec, RetryPolicy,
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
/// Typed commands whose executor is the Updates pane on this device.  They
/// never replicate: a staged image, reboot permission, and firmware inventory
/// are properties of one machine, not of a shared preference document.
pub const UPDATE_OPERATION_TYPE: &str = "settings.update-operation";
/// Typed commands for the per-user keyboard-source roster.  The compositor
/// owns application of this local durable preference.
pub const KEYBOARD_SOURCE_OPERATION_TYPE: &str = "settings.keyboard-source-operation";
/// Hardware capability command, intentionally separate from portable Power
/// preferences because a charge cap only exists on some devices.
pub const POWER_OPERATION_TYPE: &str = "settings.power-operation";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateOperation {
    Check,
    Restart,
    ReviewRollback,
    Rollback,
    RefreshFirmware,
    InstallFirmware,
    SetSchedule { schedule: String },
    SetMetered { allowed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyboardSourceOperation {
    Add { label: String },
    MoveUp { index: usize },
    MoveDown { index: usize },
    Remove { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerOperation {
    SetChargeLimit { percent: u32 },
}

/// Lifecycle reported by the pane that actually executes a device-local
/// command. Unlike a document action this is intentionally process-local and
/// never replicated or persisted as a preference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LocalOperationStatus {
    #[default]
    Idle,
    Running {
        token: String,
    },
    Succeeded {
        token: String,
        message: String,
    },
    Failed {
        token: String,
        message: String,
    },
}

/// Stable local lifecycle token. It is deliberately separate from the wire
/// payload: panes compare only the command they consumed, so an unrelated UI
/// action can never complete an agent's pending invocation.
///
/// # Errors
///
/// Returns an error when the type id or serialized payload is not a supported
/// device-local Settings operation.
pub fn local_operation_token(type_id: &str, payload: &[u8]) -> Result<String, String> {
    match type_id {
        UPDATE_OPERATION_TYPE => {
            let operation: UpdateOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        KEYBOARD_SOURCE_OPERATION_TYPE => {
            let operation: KeyboardSourceOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        POWER_OPERATION_TYPE => {
            let operation: PowerOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        _ => Err("unknown device-local operation type".into()),
    }
}

pub fn local_operation_token_for<T: Serialize>(type_id: &str, operation: &T) -> String {
    format!(
        "{type_id}:{}",
        serde_json::to_string(operation).unwrap_or_default()
    )
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

/// Build the minimal, canonical resource needed to resolve a Settings search
/// result lazily.  Search navigation is deliberately independent from the
/// eager resource snapshot: opening a category must remain available even
/// while a dynamic pane is loading or its device inventory is unavailable.
///
/// Keep this constructor next to [`open_pane_address`] so the address payload,
/// action id and resource id cannot drift apart between the search provider
/// and the full resource tree.
pub fn pane_navigation_resource(category: Category) -> Resource {
    let mut pane = Resource::new(
        pane_resource_id(category),
        "settings.section",
        category.label(),
    );
    if let Ok(open) = ActionSpec::operation(
        OPEN_PANE_ACTION,
        format!("Open {}", category.label()),
        OperationKind::View,
        OPEN_PANE_OPERATION_TYPE,
        &category.slug(),
    ) {
        pane = pane.with_action(open);
    }
    pane
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
#[allow(
    clippy::too_many_lines,
    reason = "the allow-list is a deliberately auditable stale-action boundary"
)]
fn validate_setting_target(key: &str, value: &SettingValue) -> Result<(), ActionError> {
    let text =
        || matches!(value, SettingValue::Text(text) if !text.is_empty() && text.len() <= 1024);
    let boolean = || matches!(value, SettingValue::Bool(_));
    let finite_float = || matches!(value, SettingValue::Float(number) if number.is_finite());
    let presentation_key = matches!(key.rsplit_once('.'), Some((_, "edit" | "error")));
    let valid_notification_app_key = key
        .strip_prefix("notifications.app.")
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(id, field)| {
            id.len() <= 160
                && !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                && match field {
                    "banners" | "previews" | "sounds" | "badges" | "priority"
                    | "priority_contacts" => boolean(),
                    "mute" => text(),
                    _ => false,
                }
        });
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
        | "a11y.text_size"
        | "a11y.cursor_size"
        | "a11y.color_filter"
        | "a11y.magnifier"
        | "a11y.switch_keys"
        | "a11y.switch_scan"
        | "a11y.reader_rate"
        | "a11y.reader_voice"
        | "time.timezone"
        | "power.profile"
        | "power.blank"
        | "power.suspend"
        | "power.lid"
        | "power.power_key"
        | "power.critical"
        | "power.profile_idle.saver"
        | "power.profile_idle.balanced"
        | "power.profile_idle.performance"
        | "input.accel_profile"
        | "input.compose_key"
        | "input.keyboard_layout" => text(),
        "appearance.high_contrast"
        | "appearance.reduce_motion"
        | "a11y.high_contrast"
        | "a11y.reduce_motion"
        | "a11y.large_cursor"
        | "a11y.sticky_keys"
        | "a11y.slow_keys"
        | "a11y.on_screen_keyboard"
        | "a11y.visual_alerts"
        | "a11y.screen_reader"
        | "a11y.cursor_crosshair"
        | "a11y.thick_text_caret"
        | "a11y.switch_control"
        | "time.auto"
        | "time.h24"
        | "shortcuts.vim_mode"
        | "notifications.do_not_disturb"
        | "notifications.show_banners"
        | "notifications.show_previews"
        | "notifications.play_sounds"
        | "notifications.show_on_lock_screen"
        | "notifications.show_badges"
        | "notifications.focus.enabled"
        | "network.wifi_enabled"
        | "bluetooth.enabled"
        | "audio.muted"
        | "display.auto_brightness"
        | "display.night_light"
        | "input.natural_scroll"
        | "input.tap_to_click"
        | "input.tap_drag"
        | "input.disable_while_typing"
        | "input.middle_emulation"
        | "input.left_handed"
        | "input.touchpad_gestures"
        | "text.check_spelling"
        | "text.auto_correct"
        | "power.auto_suspend"
        | "updates.auto" => boolean(),
        "audio.volume"
        | "display.brightness"
        | "display.scale"
        | "input.pointer_speed"
        | "input.touchpad_pointer_speed"
        | "input.mouse_pointer_speed"
        | "input.key_repeat" => finite_float(),
        "notifications.focus.start" | "notifications.focus.end" | "notifications.focus.days" => {
            text()
        }
        key if key.starts_with("shortcuts.") && !presentation_key => text(),
        key if key.starts_with("default_apps.") && !presentation_key => text(),
        _ if valid_notification_app_key => true,
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

/// A value-carrying boolean setting action. Unlike the palette-friendly flip
/// operation above, this is used when a retained switch must emit the exact
/// value it presents. That keeps keyboard and pointer activation on the same
/// typed action door as agents and Ctrl+K, without a replicated UI draft.
fn bool_set_spec(id: &str, label: &str, key: &'static str) -> ActionSpec {
    ActionSpec::typed(
        id,
        label,
        "On or off",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "value",
            ParamType::Boolean,
            "Whether this setting is on",
        )],
        move |args| {
            encode(&set_bool_action(
                key,
                args.boolean("value").unwrap_or(false),
            ))
        },
    )
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

/// Contract for commands that are meaningful only on the current device.  It
/// makes the non-collaborative locus explicit in palette, history and agent
/// clients without pretending a completed local command is a replicated
/// preference change.
fn local_operation_contract(tags: &[&str]) -> ActionContract {
    ActionContract {
        // The on-screen pane performs the privileged work after admission, so
        // a resource invocation is a tracked background operation, not an
        // immediate document write. It cannot be stopped safely once a system
        // command has started.
        retry: RetryPolicy::AtMostOnce,
        // No device-local operation publishes a captured inverse action. A
        // person may be able to make a new compensating choice later, but
        // that is not an undo contract.
        reversible: false,
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "string" }),
        },
        tags: tags.iter().map(|tag| (*tag).into()).collect(),
        ..ActionContract::default()
    }
}

fn local_operation_spec<T: Serialize>(
    id: impl Into<String>,
    label: impl Into<String>,
    kind: OperationKind,
    type_id: &str,
    operation: &T,
    tags: &[&str],
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, type_id, operation)
        .ok()
        .map(|spec| spec.with_contract(local_operation_contract(tags)))
}

fn encode_local_operation<T: Serialize>(type_id: &str, operation: &T) -> EncodedAction {
    EncodedAction::encode_operation(type_id, operation).unwrap_or_else(|_| EncodedAction {
        type_id: type_id.into(),
        payload: Vec::new(),
    })
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
    key: impl Into<String>,
    placeholder: &str,
    options: &'static [&'static str],
    fallback: String,
) -> ActionSpec {
    let key = key.into();
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
            encode(&set_text_action(key.clone(), v))
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
    // Dynamic child resources (installed handlers, input-source roster and
    // hardware-adjacent preferences) must describe this device's current
    // durable state, not merely the compile-time defaults.
    let d = SystemPrefs::load();
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
            Category::DefaultApps => default_apps_section(doc),
            _ => Resource::new(
                pane_resource_id(category),
                "settings.section",
                category.label(),
            ),
        };
        if let Some(open) = pane_navigation_resource(category)
            .actions
            .into_iter()
            .next()
        {
            // `&str` serialization is infallible in serde_json. Keep the
            // fallible API boundary explicit; the resource-coverage test
            // ensures a framework regression cannot silently drop this door.
            pane.actions.insert(0, open);
        }
        root = root.with_child(pane);
    }
    vec![root]
}

/// Personal defaults are a bounded, device-discovered hierarchy: each file
/// type or URI scheme is a resource and its action accepts only packages that
/// currently declare support for that type.  The choices are intentionally
/// rebuilt with the installed-app catalogue rather than exposing one broad
/// "set a default" text field; that gives keyboard users and agents the same
/// safe, discoverable operation as the picker in the pane.
fn default_apps_section(doc: &AutoCommit) -> Resource {
    default_apps_section_with_candidates(doc, &crate::panes::personal_apps::installed_candidates())
}

fn default_apps_section_with_candidates(
    doc: &AutoCommit,
    candidates: &[desktop_policy::AppCandidate],
) -> Resource {
    use desktop_policy::{AppCandidate, ResourceKind};

    fn key(resource: &ResourceKind) -> String {
        format!("default_apps.{}", resource.as_str().replace(':', "."))
    }
    fn option(app: &AppCandidate) -> String {
        format!("{} — {}", app.display_name, app.package_id)
    }

    let mut kinds: Vec<ResourceKind> = candidates
        .iter()
        .flat_map(|candidate| candidate.resources.iter().cloned())
        .collect();
    kinds.sort();
    kinds.dedup();

    let mut section = Resource::new(
        pane_resource_id(Category::DefaultApps),
        "settings.default-apps",
        Category::DefaultApps.label(),
    );
    for kind in kinds {
        let mut choices: Vec<String> = candidates
            .iter()
            .filter(|candidate| candidate.handles(&kind))
            .map(option)
            .collect();
        choices.sort();
        choices.dedup();
        if choices.is_empty() {
            continue;
        }
        let Some(first_choice) = choices.first().cloned() else {
            continue;
        };
        let setting_key = key(&kind);
        let fallback = doc_str(doc, &setting_key).unwrap_or(first_choice);
        let action_key = setting_key.clone();
        let spec = ActionSpec::typed(
            "set-handler",
            "Set default application",
            "Choose an installed application",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned("choice", ParamType::Text, "Installed application")
                    .with_choices(choices),
            ],
            move |args| {
                let value = args
                    .text("choice")
                    .map_or_else(|| fallback.clone(), str::to_string);
                encode(&set_text_action(action_key.clone(), value))
            },
        )
        .with_contract(setting_contract());
        // `ResourceKind` already validates the type vocabulary at manifest
        // ingestion; retaining it verbatim here keeps MIME and scheme ids
        // collision-free and makes an agent address exactly one handler.
        section = section.with_child(
            Resource::new(
                format!("settings.default-app:{}", kind.as_str()),
                "settings.default-app-handler",
                kind.display_label(),
            )
            .with_action(spec),
        );
    }
    section
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

#[allow(
    clippy::too_many_lines,
    reason = "the ordered resource declaration mirrors the Notifications hierarchy"
)]
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
    sec = sec.with_action(bool_flip_spec(
        "badges",
        panes::notifications::BADGES_KEY,
        b(
            panes::notifications::BADGES_KEY,
            d.notifications.show_badges,
        ),
        "notification badges",
    ));
    sec = sec.with_action(bool_flip_spec(
        "focus-enabled",
        panes::notifications::FOCUS_ENABLED_KEY,
        b(
            panes::notifications::FOCUS_ENABLED_KEY,
            d.notifications.focus.enabled,
        ),
        "scheduled quiet hours",
    ));
    for (id, label, key, options, fallback) in [
        (
            "focus-start",
            "Set focus start time",
            panes::notifications::FOCUS_START_KEY,
            &[
                "18:00", "20:00", "22:00", "23:00", "00:00", "06:00", "07:00", "08:00", "09:00",
            ][..],
            "22:00",
        ),
        (
            "focus-end",
            "Set focus end time",
            panes::notifications::FOCUS_END_KEY,
            &[
                "18:00", "20:00", "22:00", "23:00", "00:00", "06:00", "07:00", "08:00", "09:00",
            ][..],
            "07:00",
        ),
        (
            "focus-days",
            "Set focus days",
            panes::notifications::FOCUS_DAYS_KEY,
            &["Every day", "Weekdays", "Weekends"][..],
            "Every day",
        ),
    ] {
        sec = sec.with_action(choice_prompt_spec(
            id,
            label,
            key,
            "Choose a focus schedule value",
            options,
            doc_str(doc, key).unwrap_or_else(|| fallback.into()),
        ));
    }
    for (app_id, app_name) in
        crate::panes::notifications::resource_notification_apps(&d.notifications)
    {
        let mut resource = Resource::new(
            format!("settings.notification-app:{app_id}"),
            "settings.notification-app",
            app_name,
        );
        for (id, field, label) in [
            ("banners", "banners", "notification banners"),
            ("previews", "previews", "message previews"),
            ("sounds", "sounds", "notification sounds"),
            ("badges", "badges", "notification badges"),
            ("priority", "priority", "priority during focus"),
            (
                "priority-contacts",
                "priority_contacts",
                "priority contacts during focus",
            ),
        ] {
            let key = format!("notifications.app.{app_id}.{field}");
            let default = match field {
                "banners" => d.notifications.show_banners,
                "previews" => d.notifications.show_previews,
                "sounds" => d.notifications.play_sounds,
                "badges" => d.notifications.show_badges,
                _ => false,
            };
            resource = resource.with_action(bool_flip_spec(id, &key, b(&key, default), label));
        }
        let key = format!("notifications.app.{app_id}.mute");
        resource = resource.with_action(choice_prompt_spec(
            "mute",
            "Mute notifications temporarily",
            key,
            "Not muted, For 1 hour, For 8 hours, For 24 hours",
            &["Not muted", "For 1 hour", "For 8 hours", "For 24 hours"],
            doc_str(doc, &format!("notifications.app.{app_id}.mute"))
                .unwrap_or_else(|| "Not muted".into()),
        ));
        sec = sec.with_child(resource);
    }
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

#[allow(
    clippy::too_many_lines,
    reason = "the ordered resource declaration mirrors the Input hierarchy"
)]
fn input_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| doc_bool(doc, key).unwrap_or(default);
    let mut sec = Resource::new(
        pane_resource_id(Category::MouseKeyboard),
        "settings.section",
        "Mouse & Keyboard",
    );
    sec = sec.with_action(bool_flip_spec(
        "touchpad_natural_scroll",
        panes::mouse_keyboard::TOUCHPAD_NATURAL_SCROLL_KEY,
        b(
            panes::mouse_keyboard::TOUCHPAD_NATURAL_SCROLL_KEY,
            d.input
                .touchpad_natural_scroll
                .unwrap_or(d.input.natural_scroll),
        ),
        "touchpad natural scrolling",
    ));
    sec = sec.with_action(bool_flip_spec(
        "mouse_natural_scroll",
        panes::mouse_keyboard::MOUSE_NATURAL_SCROLL_KEY,
        b(
            panes::mouse_keyboard::MOUSE_NATURAL_SCROLL_KEY,
            d.input
                .mouse_natural_scroll
                .unwrap_or(d.input.natural_scroll),
        ),
        "mouse natural scrolling",
    ));
    sec = sec.with_action(bool_flip_spec(
        "tap_to_click",
        panes::mouse_keyboard::TAP_CLICK_KEY,
        b(panes::mouse_keyboard::TAP_CLICK_KEY, d.input.tap_to_click),
        "tap to click",
    ));
    sec = sec.with_action(float_prompt_spec(
        "touchpad_pointer_speed",
        "Set touchpad speed",
        panes::mouse_keyboard::TOUCHPAD_POINTER_SPEED_KEY,
        "0.0 – 1.0",
        0.0,
        1.0,
    ));
    sec = sec.with_action(float_prompt_spec(
        "mouse_pointer_speed",
        "Set mouse speed",
        panes::mouse_keyboard::MOUSE_POINTER_SPEED_KEY,
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
    for (id, key, label) in [
        (
            "tap-drag",
            panes::mouse_keyboard::TAP_DRAG_KEY,
            "tap and drag",
        ),
        (
            "disable-while-typing",
            panes::mouse_keyboard::DWT_KEY,
            "disable touchpad while typing",
        ),
        (
            "middle-emulation",
            panes::mouse_keyboard::MIDDLE_EMULATION_KEY,
            "middle-button emulation",
        ),
        (
            "left-handed",
            panes::mouse_keyboard::LEFT_HANDED_KEY,
            "left-handed pointer buttons",
        ),
        (
            "touchpad-gestures",
            panes::mouse_keyboard::GESTURES_KEY,
            "touchpad gestures",
        ),
        (
            "spell-checking",
            panes::mouse_keyboard::CHECK_SPELLING_KEY,
            "spell checking while typing",
        ),
        (
            "auto-correct",
            panes::mouse_keyboard::AUTO_CORRECT_KEY,
            "automatic spelling correction",
        ),
    ] {
        sec = sec.with_action(bool_flip_spec(id, key, b(key, false), label));
    }
    for (id, label, key, options, fallback) in [
        (
            "acceleration-profile",
            "Set pointer acceleration profile",
            panes::mouse_keyboard::ACCEL_PROFILE_KEY,
            &["Adaptive", "Flat"][..],
            "Adaptive",
        ),
        (
            "compose-key",
            "Set compose key",
            panes::mouse_keyboard::COMPOSE_KEY,
            &[
                "Disabled",
                "Right Alt",
                "Menu",
                "Caps Lock",
                "Right Super",
                "Left Super",
            ][..],
            "Disabled",
        ),
    ] {
        sec = sec.with_action(choice_prompt_spec(
            id,
            label,
            key,
            "Choose an input preference",
            options,
            doc_str(doc, key).unwrap_or_else(|| fallback.into()),
        ));
    }
    // Input-source edits are device-local commands: they mutate the local
    // compositor roster rather than a portable setting.  Keep each installed
    // source bounded and addressable, matching the rows in the pane.
    let mut sources = d.input.keyboard_layouts.clone();
    if sources.is_empty() {
        sources.push(d.input.keyboard_layout.clone());
    }
    let catalog = crate::input_sources::installed_catalog();
    let mut add_choices: Vec<String> = catalog
        .iter()
        .filter(|entry| !sources.contains(&entry.id))
        .map(|entry| entry.label.clone())
        .collect();
    add_choices.sort();
    if !add_choices.is_empty()
        && sources.len() < settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
    {
        let Some(fallback) = add_choices.first().cloned() else {
            return sec;
        };
        let choices = add_choices;
        sec = sec.with_action(
            ActionSpec::typed_operation(
                "add-source",
                "Add keyboard input source",
                "Choose an installed keyboard layout",
                OperationKind::Effect,
                KEYBOARD_SOURCE_OPERATION_TYPE,
                vec![
                    ActionParam::required_owned("label", ParamType::Text, "Installed layout")
                        .with_choices(choices),
                ],
                move |args| {
                    let label = args
                        .text("label")
                        .map_or_else(|| fallback.clone(), str::to_string);
                    encode_local_operation(
                        KEYBOARD_SOURCE_OPERATION_TYPE,
                        &KeyboardSourceOperation::Add { label },
                    )
                },
            )
            .with_contract(local_operation_contract(&["device-local", "input-source"])),
        );
    }
    for (index, source) in sources.iter().enumerate() {
        let label = catalog
            .iter()
            .find(|entry| entry.id == *source)
            .map_or_else(|| source.clone(), |entry| entry.label.clone());
        let mut resource = Resource::new(
            format!("settings.input-source:{source}"),
            "settings.input-source",
            label,
        );
        if index > 0
            && let Some(action) = local_operation_spec(
                "move-up",
                "Move input source up",
                OperationKind::Effect,
                KEYBOARD_SOURCE_OPERATION_TYPE,
                &KeyboardSourceOperation::MoveUp { index },
                &["device-local", "input-source"],
            )
        {
            resource = resource.with_action(action);
        }
        if index + 1 < sources.len()
            && let Some(action) = local_operation_spec(
                "move-down",
                "Move input source down",
                OperationKind::Effect,
                KEYBOARD_SOURCE_OPERATION_TYPE,
                &KeyboardSourceOperation::MoveDown { index },
                &["device-local", "input-source"],
            )
        {
            resource = resource.with_action(action);
        }
        if sources.len() > 1
            && let Some(action) = local_operation_spec(
                "remove",
                "Remove input source",
                OperationKind::Effect,
                KEYBOARD_SOURCE_OPERATION_TYPE,
                &KeyboardSourceOperation::Remove { index },
                &["device-local", "input-source"],
            )
        {
            resource = resource.with_action(action);
        }
        sec = sec.with_child(resource);
    }
    sec
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered resource declaration mirrors the Power hierarchy"
)]
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
    // These choices deliberately mirror the controls' durable document
    // values, not the power daemon's transient availability. A hardware-only
    // capability is exported below only when the local probe says it exists.
    let suspend_options = &[
        "5 minutes",
        "10 minutes",
        "15 minutes",
        "30 minutes",
        "45 minutes",
        "1 hour",
        "Never",
    ];
    for (id, label, key, options, fallback) in [
        (
            "profile",
            "Set power profile",
            panes::power::PROFILE_KEY,
            &["Power Saver", "Balanced", "Performance"][..],
            "Balanced",
        ),
        (
            "suspend-after",
            "Set idle suspend delay",
            panes::power::SUSPEND_KEY,
            suspend_options,
            "30 minutes",
        ),
        (
            "blank-after",
            "Set screen blank delay",
            panes::power::BLANK_KEY,
            &[
                "1 minute",
                "5 minutes",
                "10 minutes",
                "15 minutes",
                "30 minutes",
                "Never",
            ][..],
            "10 minutes",
        ),
        (
            "saver-suspend",
            "Set Power Saver suspend delay",
            panes::power::SAVER_SUSPEND_KEY,
            suspend_options,
            "15 minutes",
        ),
        (
            "balanced-suspend",
            "Set Balanced suspend delay",
            panes::power::BALANCED_SUSPEND_KEY,
            suspend_options,
            "30 minutes",
        ),
        (
            "performance-suspend",
            "Set Performance suspend delay",
            panes::power::PERFORMANCE_SUSPEND_KEY,
            suspend_options,
            "Never",
        ),
    ] {
        sec = sec.with_action(choice_prompt_spec(
            id,
            label,
            key,
            "Choose a power preference",
            options,
            s(key, fallback),
        ));
    }
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
    if let Some(capability) = os_actions::charge_capability() {
        let fallback = capability.current;
        let choices: Vec<String> = capability.choices.iter().map(u32::to_string).collect();
        sec = sec.with_action(
            ActionSpec::typed_operation(
                "set-charge-cap",
                "Set battery charge cap",
                "Choose a supported charge percentage",
                OperationKind::Effect,
                POWER_OPERATION_TYPE,
                vec![
                    ActionParam::required_owned("percent", ParamType::Text, "Charge percentage")
                        .with_choices(choices),
                ],
                move |args| {
                    let percent = args
                        .text("percent")
                        .and_then(|value| value.parse::<u32>().ok())
                        .unwrap_or(fallback);
                    encode_local_operation(
                        POWER_OPERATION_TYPE,
                        &PowerOperation::SetChargeLimit { percent },
                    )
                },
            )
            .with_contract(local_operation_contract(&["device-local", "hardware"]))
            .privileged(),
        );
    }
    sec
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered declaration mirrors the visible Accessibility pane and keeps every resource action auditable in one place"
)]
fn accessibility_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let s = |key: &str, default: &str| doc_str(doc, key).unwrap_or_else(|| default.to_string());
    let mut sec = Resource::new(
        "settings.section:accessibility",
        "settings.section",
        "Accessibility",
    );
    for (id, key, label) in [
        (
            "high-contrast",
            panes::accessibility::HIGH_CONTRAST_KEY,
            "Set high contrast",
        ),
        (
            "reduce-motion",
            panes::accessibility::REDUCE_MOTION_KEY,
            "Set reduced motion",
        ),
        (
            "large-cursor",
            panes::accessibility::LARGE_CURSOR_KEY,
            "Set large cursor",
        ),
        (
            "sticky-keys",
            panes::accessibility::STICKY_KEYS_KEY,
            "Set sticky keys",
        ),
        (
            "slow-keys",
            panes::accessibility::SLOW_KEYS_KEY,
            "Set slow keys",
        ),
        (
            "visual-alerts",
            panes::accessibility::VISUAL_ALERTS_KEY,
            "Set visual alerts",
        ),
        (
            "pointer-crosshair",
            panes::accessibility::CURSOR_CROSSHAIR_KEY,
            "Set pointer crosshair",
        ),
        (
            "thick-text-caret",
            panes::accessibility::THICK_CARET_KEY,
            "Set thicker text cursor",
        ),
        (
            "switch-control",
            panes::accessibility::SWITCH_CONTROL_KEY,
            "Set switch control",
        ),
    ] {
        sec = sec.with_action(bool_set_spec(id, label, key));
    }
    if panes::accessibility::osk_service_available() {
        sec = sec.with_action(bool_set_spec(
            "on-screen-keyboard",
            "Set on-screen keyboard",
            panes::accessibility::OSK_KEY,
        ));
    }
    if panes::accessibility::screen_reader_available() {
        sec = sec.with_action(bool_set_spec(
            "screen-reader",
            "Set screen reader",
            panes::accessibility::SCREEN_READER_KEY,
        ));
    }
    for (id, label, key, placeholder, options, fallback) in [
        (
            "text-size",
            "Set text size",
            panes::accessibility::TEXT_SIZE_KEY,
            "Small, Default, Large, Larger",
            &["Small", "Default", "Large", "Larger"][..],
            crate::panes::appearance::text_size_label(d.appearance.text_scale),
        ),
        (
            "cursor-size",
            "Set cursor size",
            panes::accessibility::CURSOR_SIZE_KEY,
            "Default, Large, Larger, Largest",
            &["Default", "Large", "Larger", "Largest"][..],
            crate::prefs::AccessibilityPrefs::cursor_size_label(d.accessibility.cursor_size),
        ),
        (
            "colour-filter",
            "Set colour filter",
            panes::accessibility::COLOR_FILTER_KEY,
            "Off, Greyscale, Protanopia, Deuteranopia, Tritanopia",
            &[
                "Off",
                "Greyscale",
                "Protanopia",
                "Deuteranopia",
                "Tritanopia",
            ][..],
            d.accessibility.color_filter.label(),
        ),
        (
            "magnifier",
            "Set magnifier lens",
            panes::accessibility::MAGNIFIER_KEY,
            "Off, 2×, 3×",
            &["Off", "2×", "3×"][..],
            match d.accessibility.magnifier_zoom {
                2 => "2×",
                3 => "3×",
                _ => "Off",
            },
        ),
        (
            "switch-keys",
            "Set switch keys",
            panes::accessibility::SWITCH_KEYS_KEY,
            "Tab / Enter, Space / Enter",
            &["Tab / Enter", "Space / Enter"][..],
            d.accessibility.switch_key_pair.label(),
        ),
        (
            "switch-scan",
            "Set automatic switch scan",
            panes::accessibility::SWITCH_SCAN_KEY,
            "Manual, 1 second, 2 seconds, 3 seconds",
            &["Manual", "1 second", "2 seconds", "3 seconds"][..],
            match d.accessibility.switch_scan_ms {
                1000 => "1 second",
                2000 => "2 seconds",
                3000 => "3 seconds",
                _ => "Manual",
            },
        ),
    ] {
        sec = sec.with_action(choice_prompt_spec(
            id,
            label,
            key,
            placeholder,
            options,
            s(key, fallback),
        ));
    }
    if panes::accessibility::screen_reader_available() {
        for (id, label, key, placeholder, options, fallback) in [
            (
                "speech-rate",
                "Set screen-reader speech rate",
                panes::accessibility::READER_RATE_KEY,
                "Slow, Default, Fast, Very fast",
                &["Slow", "Default", "Fast", "Very fast"][..],
                match d.accessibility.screen_reader_rate {
                    0..=75 => "Slow",
                    76..=115 => "Default",
                    116..=160 => "Fast",
                    _ => "Very fast",
                },
            ),
            (
                "speech-voice",
                "Set screen-reader voice",
                panes::accessibility::READER_VOICE_KEY,
                "Default, Male, Female",
                &["Default", "Male", "Female"][..],
                d.accessibility.screen_reader_voice.label(),
            ),
        ] {
            sec = sec.with_action(choice_prompt_spec(
                id,
                label,
                key,
                placeholder,
                options,
                s(key, fallback),
            ));
        }
    }
    sec
}

/// Bind an Accessibility switch to the value-carrying action exported by the
/// real Accessibility resource. The widget still reads its checked state from
/// the canonical CRDT key; it merely emits through this brokered door instead
/// of writing the document itself.
pub fn accessibility_bool_binding(action_id: &str) -> Option<qos_actions::ActionBinding<bool>> {
    let resource = accessibility_section(&AutoCommit::new(), &SystemPrefs::default());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    qos_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

/// Bind an Accessibility chooser to its exact typed resource action. The
/// choices are validated by the same `ActionSpec` used for Ctrl+K before a
/// control invocation reaches the host.
pub fn accessibility_text_binding(action_id: &str) -> Option<qos_actions::ActionBinding<String>> {
    let resource = accessibility_section(&AutoCommit::new(), &SystemPrefs::default());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    qos_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered local-operation declarations mirror the Updates pane"
)]
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
    for (id, label, operation) in [
        ("check", "Check for updates", UpdateOperation::Check),
        (
            "restart",
            "Restart into staged update",
            UpdateOperation::Restart,
        ),
        (
            "rollback",
            "Roll back to previous image",
            UpdateOperation::Rollback,
        ),
        (
            "refresh-firmware",
            "Refresh firmware updates",
            UpdateOperation::RefreshFirmware,
        ),
        (
            "install-firmware",
            "Install firmware updates",
            UpdateOperation::InstallFirmware,
        ),
    ] {
        if let Some(mut action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            UPDATE_OPERATION_TYPE,
            &operation,
            &["device-local", "system-update"],
        ) {
            if matches!(
                operation,
                UpdateOperation::Check | UpdateOperation::RefreshFirmware
            ) {
                action.contract.retry = RetryPolicy::Idempotent;
            }
            if matches!(
                operation,
                UpdateOperation::Restart
                    | UpdateOperation::Rollback
                    | UpdateOperation::InstallFirmware
            ) {
                action = action.destructive();
            }
            sec = sec.with_action(action);
        }
    }
    // Reviewing the rollback caveats only navigates to the already-visible
    // pane. It does not start an operation or create a history entry.
    if let Ok(review) = ActionSpec::operation(
        "review-rollback",
        "Review rollback",
        OperationKind::View,
        UPDATE_OPERATION_TYPE,
        &UpdateOperation::ReviewRollback,
    ) {
        sec = sec.with_action(review);
    }
    for (id, label, schedule) in [
        (
            "schedule-system",
            "Use system update schedule",
            "System schedule",
        ),
        ("schedule-daily", "Check for updates daily", "Daily"),
        ("schedule-weekly", "Check for updates weekly", "Weekly"),
    ] {
        if let Some(mut action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            UPDATE_OPERATION_TYPE,
            &UpdateOperation::SetSchedule {
                schedule: schedule.into(),
            },
            &["device-local", "system-update"],
        ) {
            action.contract.retry = RetryPolicy::Idempotent;
            sec = sec.with_action(action);
        }
    }
    for (id, label, allowed) in [
        ("allow-metered", "Allow updates on metered networks", true),
        ("avoid-metered", "Avoid updates on metered networks", false),
    ] {
        if let Some(mut action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            UPDATE_OPERATION_TYPE,
            &UpdateOperation::SetMetered { allowed },
            &["device-local", "system-update"],
        ) {
            action.contract.retry = RetryPolicy::Idempotent;
            sec = sec.with_action(action);
        }
    }
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

    #[test]
    fn power_resource_covers_every_non_hardware_preference_control() {
        let resources = provide_resources(&AutoCommit::new());
        let power = find(&resources, &pane_resource_id(Category::Power));
        let ids: std::collections::HashSet<&str> = power
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for id in [
            "profile",
            "auto_suspend",
            "suspend-after",
            "blank-after",
            "saver-suspend",
            "balanced-suspend",
            "performance-suspend",
            "lid",
            "power_key",
            "critical",
        ] {
            assert!(ids.contains(id), "missing canonical Power action: {id}");
        }
        if let Some(action) = power
            .actions
            .iter()
            .find(|action| action.id == "set-charge-cap")
        {
            assert_eq!(action.kind, OperationKind::Effect);
            assert_eq!(action.locus, ExecutionLocus::ClientLocal);
            assert_eq!(action.operation_type.as_deref(), Some(POWER_OPERATION_TYPE));
        }
    }

    #[test]
    fn notification_and_input_resources_cover_their_durable_preferences() {
        let resources = provide_resources(&AutoCommit::new());
        let notifications = find(&resources, &pane_resource_id(Category::Notifications));
        let notification_ids: std::collections::HashSet<&str> = notifications
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for id in [
            "badges",
            "focus-enabled",
            "focus-start",
            "focus-end",
            "focus-days",
        ] {
            assert!(
                notification_ids.contains(id),
                "missing canonical Notifications action: {id}"
            );
        }
        let input = find(&resources, &pane_resource_id(Category::MouseKeyboard));
        let input_ids: std::collections::HashSet<&str> = input
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for id in [
            "tap-drag",
            "disable-while-typing",
            "middle-emulation",
            "left-handed",
            "touchpad-gestures",
            "spell-checking",
            "auto-correct",
            "acceleration-profile",
            "compose-key",
        ] {
            assert!(
                input_ids.contains(id),
                "missing canonical input action: {id}"
            );
        }
        assert!(
            input.children.iter().all(|source| {
                source.kind != "settings.input-source"
                    || source.actions.iter().all(|action| {
                        action.locus == ExecutionLocus::ClientLocal
                            && action.operation_type.as_deref()
                                == Some(KEYBOARD_SOURCE_OPERATION_TYPE)
                    })
            }),
            "each input-source row exposes only local typed commands"
        );
    }

    #[test]
    fn update_operations_are_typed_local_effects_with_destructive_guards() {
        let resources = provide_resources(&AutoCommit::new());
        let updates = find(&resources, &pane_resource_id(Category::Updates));
        for id in [
            "check",
            "restart",
            "rollback",
            "refresh-firmware",
            "install-firmware",
        ] {
            let action = updates
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing update operation: {id}"));
            assert_eq!(action.kind, OperationKind::Effect);
            assert_eq!(action.locus, ExecutionLocus::ClientLocal);
            assert_eq!(
                action.operation_type.as_deref(),
                Some(UPDATE_OPERATION_TYPE)
            );
            assert!(action.contract.tags.iter().any(|tag| tag == "device-local"));
        }
        for id in ["restart", "rollback", "install-firmware"] {
            assert!(
                updates
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .is_some_and(|action| !matches!(
                        action.contract.confirmation,
                        qos_actions::ConfirmationPolicy::None
                    ))
            );
        }
        let review = updates
            .actions
            .iter()
            .find(|action| action.id == "review-rollback")
            .expect("rollback review is discoverable");
        assert_eq!(review.kind, OperationKind::View);
        assert!(matches!(
            review.contract.execution,
            ExecutionMode::Immediate
        ));
    }

    #[test]
    fn accepted_local_operations_advertise_background_string_results() {
        fn walk(resource: &Resource, out: &mut Vec<ActionSpec>) {
            out.extend(
                resource
                    .actions
                    .iter()
                    .filter(|action| {
                        matches!(
                            action.operation_type.as_deref(),
                            Some(
                                UPDATE_OPERATION_TYPE
                                    | KEYBOARD_SOURCE_OPERATION_TYPE
                                    | POWER_OPERATION_TYPE
                            )
                        ) && action.kind == OperationKind::Effect
                    })
                    .cloned(),
            );
            for child in &resource.children {
                walk(child, out);
            }
        }

        let mut actions = Vec::new();
        for root in provide_resources(&AutoCommit::new()) {
            walk(&root, &mut actions);
        }
        assert!(!actions.is_empty());
        for action in actions {
            assert!(matches!(
                action.contract.execution,
                ExecutionMode::Background { cancellable: false }
            ));
            assert!(action.contract.validate_declaration().is_empty());
            assert!(
                action
                    .contract
                    .result
                    .validate(&qos_actions::ActionOutput {
                        value: Some(serde_json::Value::String("done".into())),
                        resources: Vec::new(),
                    })
                    .is_ok()
            );
        }
    }

    #[test]
    fn accessibility_controls_bind_to_their_exported_typed_actions() {
        let resources = provide_resources(&AutoCommit::new());
        let accessibility = find(&resources, &pane_resource_id(Category::Accessibility));
        let high_contrast = accessibility
            .actions
            .iter()
            .find(|action| action.id == "high-contrast")
            .expect("Accessibility exports the high-contrast control action");
        assert_eq!(high_contrast.params().len(), 1);
        assert_eq!(high_contrast.params()[0].ty, ParamType::Boolean);

        let binding = accessibility_bool_binding("high-contrast")
            .expect("the displayed switch has a matching exported action");
        let invocation = binding.invoke(true).expect("true is a valid switch value");
        assert_eq!(
            invocation.address,
            ActionAddress::new(pane_resource_id(Category::Accessibility), "high-contrast")
        );
        assert_eq!(
            invocation.arguments.get("value"),
            Some(&serde_json::Value::Bool(true))
        );
        let encoded = high_contrast
            .build_json(&invocation.arguments)
            .expect("the resource accepts the control invocation");
        let action = registry()
            .decode(&encoded)
            .expect("settings action decodes");
        let mut doc = AutoCommit::new();
        action
            .apply(&mut doc)
            .expect("the setting target is accepted");
        assert_eq!(
            doc_bool(&doc, panes::accessibility::HIGH_CONTRAST_KEY),
            Some(true)
        );

        let text = accessibility_text_binding("text-size")
            .expect("the displayed chooser has a matching exported action");
        assert!(text.invoke("not a size".to_string()).is_err());
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

    #[test]
    fn default_app_handlers_are_bounded_canonical_child_resources() {
        use desktop_policy::{AppCandidate, ResourceKind};

        let pdf = ResourceKind::extension("pdf").expect("valid extension");
        let browser = AppCandidate {
            package_id: "org.qos.Reader".into(),
            display_name: "Reader".into(),
            resources: [pdf.clone()].into(),
        };
        let alternate = AppCandidate {
            package_id: "org.qos.Archive".into(),
            display_name: "Archive".into(),
            resources: [pdf].into(),
        };
        let section =
            default_apps_section_with_candidates(&AutoCommit::new(), &[browser, alternate]);
        let handler = section
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "settings.default-app:ext:pdf")
            .expect("a resource for each declared file type");
        let action = handler
            .actions
            .iter()
            .find(|action| action.id == "set-handler")
            .expect("handler resource has one canonical mutation");
        assert_eq!(action.params().len(), 1);
        assert_eq!(action.params()[0].choices.len(), 2);
        let args = serde_json::Map::from_iter([(
            "choice".to_owned(),
            serde_json::Value::String("Reader — org.qos.Reader".to_owned()),
        )]);
        let encoded = action
            .build_json(&args)
            .expect("declared handler is accepted");
        let mut doc = AutoCommit::new();
        registry()
            .decode(&encoded)
            .expect("settings action decodes")
            .apply(&mut doc)
            .expect("handler selection is a valid setting target");
        assert_eq!(
            doc_str(&doc, "default_apps.ext.pdf").as_deref(),
            Some("Reader — org.qos.Reader")
        );
        let invalid = serde_json::Map::from_iter([(
            "choice".to_owned(),
            serde_json::Value::String("Unknown — org.qos.Unknown".to_owned()),
        )]);
        assert!(
            action.build_json(&invalid).is_err(),
            "a stale or undiscovered handler is refused before dispatch"
        );
    }
}
