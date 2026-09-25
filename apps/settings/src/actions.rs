//! Settings' exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every portable preference the OS stores is a value in the shared document,
//! keyed by its stable dotted name (`notifications.do_not_disturb`,
//! `appearance.theme`, `input.pointer_speed`, …). Changing one is a
//! [`SettingsAction`]: a small serializable value that deterministically writes
//! that key. Hardware/service controls use a separate typed local-operation
//! protocol: the visible pane owns the privileged command and the palette uses
//! the same bounded request/lifecycle, without making a device action replay on
//! a peer.
//!
//! The resource tree mirrors the settings hub: the configuration root owns a
//! child per section (Notifications, Appearance, Sound, …), and each section
//! exports the [`ActionSpec`]s for the settings it owns.
//!
//! ## What is *not* an action
//!
//! * **Hardware / service effects** — changing an audio graph, radio,
//!   backlight, update service, or other device authority uses a typed local
//!   operation. The executor issues a one-shot token and the owning pane
//!   consumes only that exact command; replicated document state can never
//!   trigger the effect. Portable preferences alone use [`Action::apply`].
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

#![allow(
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::redundant_locals,
    clippy::too_many_lines,
    clippy::unreachable,
    reason = "resource builders operate on closed, locally-constructed action schemas; fallible public resolution remains bounded and returns Option/Result"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::doc::{read_bool, read_str_or_text};

use tilo_actions::{
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
/// Input and pointer configuration belongs to the machine running Settings.
/// It is applied by the local compositor from `settings.json`, so it must not
/// be represented by a replayable `SettingsAction`.
pub const INPUT_OPERATION_TYPE: &str = "settings.input-operation";
/// Clock service and clock-display commands are likewise local effects: a
/// peer must never change this machine's timedated service or locale file.
pub const TIME_DATE_OPERATION_TYPE: &str = "settings.time-date-operation";
/// Hardware capability command, intentionally separate from portable Power
/// preferences because a charge cap only exists on some devices.
pub const POWER_OPERATION_TYPE: &str = "settings.power-operation";
/// Hardware-network commands. They intentionally address the current
/// machine's radios and diagnostics, rather than a replicated preference.
pub const NETWORK_OPERATION_TYPE: &str = "settings.network-operation";
/// Adapter and device discovery commands owned by the local `BlueZ` service.
pub const BLUETOOTH_OPERATION_TYPE: &str = "settings.bluetooth-operation";
/// Storage maintenance and tool-handoff commands scoped to this device.
pub const STORAGE_OPERATION_TYPE: &str = "settings.storage-operation";
/// `PipeWire` device and stream-routing commands scoped to this device.
pub const AUDIO_OPERATION_TYPE: &str = "settings.audio-operation";
/// Compositor-backed output configuration commands. These stage or confirm a
/// transaction for the current machine; they never become shared preferences.
pub const DISPLAY_OPERATION_TYPE: &str = "settings.display-operation";
pub const PRINTER_OPERATION_TYPE: &str = "settings.printer-operation";
pub const USER_OPERATION_TYPE: &str = "settings.user-operation";
/// Local-model lifecycle commands.  Model packages, accelerator support, and
/// the supervised server belong to this machine, so these requests are never
/// replicated as collaborative preferences.
pub const AGENT_MODEL_OPERATION_TYPE: &str = "settings.agent-model-operation";
/// Device-mesh role, relay services and workspace pinning configure this
/// machine's trust boundary. They are local effects, never CRDT preferences.
pub const DEVICE_MESH_OPERATION_TYPE: &str = "settings.device-mesh-operation";
pub const AGENT_PROVIDER_OPERATION_TYPE: &str = "settings.agent-provider-operation";

/// Collaboration policy for every action exported by Settings. There is no
/// implicit/default bucket: a new operation type must be added here before it
/// can be published, so hardware effects cannot accidentally become replayable
/// document changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingScope {
    PortableDocument,
    DeviceLocalEffect,
    Presentation,
}

pub(crate) const DEVICE_LOCAL_OPERATION_TYPES: &[&str] = &[
    ACCOUNT_OPERATION_TYPE,
    UPDATE_OPERATION_TYPE,
    KEYBOARD_SOURCE_OPERATION_TYPE,
    INPUT_OPERATION_TYPE,
    TIME_DATE_OPERATION_TYPE,
    POWER_OPERATION_TYPE,
    NETWORK_OPERATION_TYPE,
    BLUETOOTH_OPERATION_TYPE,
    STORAGE_OPERATION_TYPE,
    AUDIO_OPERATION_TYPE,
    DISPLAY_OPERATION_TYPE,
    PRINTER_OPERATION_TYPE,
    USER_OPERATION_TYPE,
    AGENT_MODEL_OPERATION_TYPE,
    DEVICE_MESH_OPERATION_TYPE,
    AGENT_PROVIDER_OPERATION_TYPE,
];

pub fn classify_action(spec: &ActionSpec) -> Option<SettingScope> {
    match spec.kind {
        OperationKind::Document => Some(SettingScope::PortableDocument),
        OperationKind::Effect
            if spec
                .operation_type
                .as_deref()
                .is_some_and(|type_id| DEVICE_LOCAL_OPERATION_TYPES.contains(&type_id)) =>
        {
            Some(SettingScope::DeviceLocalEffect)
        }
        OperationKind::View => Some(SettingScope::Presentation),
        OperationKind::Effect => None,
    }
}

/// Hard ceiling for any live/discovered collection published by Settings.
///
/// A compositor, service, or corrupted local inventory is not allowed to make
/// opening global search allocate an unbounded resource/action tree. Exact
/// lookup is scoped to one section below and uses the same ceiling.
const MAX_DYNAMIC_RESOURCES: usize = 256;
/// Hard ceiling for choices/actions attached to one dynamic resource.
const MAX_DYNAMIC_CHOICES: usize = 128;

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
    SetAutomatic { enabled: bool },
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

/// Every hardware-backed Mouse & Keyboard preference exposed by Settings.
/// The enum is intentionally closed: agents cannot smuggle arbitrary input
/// keys or compositor options through the local-operation boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputOperation {
    SetTouchpadNaturalScroll { enabled: bool },
    SetMouseNaturalScroll { enabled: bool },
    SetTapToClick { enabled: bool },
    SetTouchpadPointerSpeed { value: f64 },
    SetMousePointerSpeed { value: f64 },
    SetKeyRepeat { value: f64 },
    SetTapDrag { enabled: bool },
    SetDisableWhileTyping { enabled: bool },
    SetMiddleEmulation { enabled: bool },
    SetLeftHanded { enabled: bool },
    SetTouchpadGestures { enabled: bool },
    SetAccelerationProfile { profile: String },
    SetComposeKey { key: String },
    SetActiveSource { label: String },
}

/// Time & Date controls which reach a system service or durable local locale
/// configuration. They deliberately use the same bounded effect protocol as
/// their visible controls and resource actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeDateOperation {
    SetAutomaticTime { enabled: bool },
    SetTimezone { timezone: String },
    SetHourCycle { h24: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerOperation {
    SetChargeLimit { percent: u32 },
    SetProfile { profile: String },
    SetAutoSuspend { enabled: bool },
    SetBlankTimeout { label: String },
    SetLockTimeout { label: String },
    SetSuspendTimeout { profile: String, label: String },
    SetLidAction { action: String },
    SetPowerKeyAction { action: String },
    SetCriticalAction { action: String },
}

/// Bounded commands owned by the Network pane. Radio state and diagnostics
/// belong to the current device and must never replay on another machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkOperation {
    Refresh,
    SetWifiEnabled { enabled: bool },
    SetWiredEnabled { enabled: bool },
    SetAirplaneMode { enabled: bool },
    SetHotspot { enabled: bool },
    SetMobileBroadband { enabled: bool },
    RunDiagnostics,
}

/// Bounded Bluetooth commands which are meaningful only on the machine that
/// owns the adapter. Pairing is intentionally not a shared preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BluetoothOperation {
    SetPowered { enabled: bool },
    StartScan,
    Refresh,
    Pair { path: String },
    Connect { path: String },
    Disconnect { path: String },
    Forget { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageOperation {
    CleanupCaches,
    FreeSyncedFiles,
    OpenDisks,
    OpenBackups,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioOperation {
    SetOutputVolume {
        value: f64,
    },
    SetOutputMuted {
        muted: bool,
    },
    SetInputVolume {
        value: f64,
    },
    SetBalance {
        value: f64,
    },
    SetMono {
        enabled: bool,
    },
    SetDeviceProfile {
        device_id: u32,
        profile_index: u32,
    },
    RouteStream {
        stream_id: u32,
        playback: bool,
        device_id: Option<u32>,
    },
    SetStreamVolume {
        stream_id: u32,
        value: f64,
    },
    SetStreamMuted {
        stream_id: u32,
        muted: bool,
    },
    ToggleDiagnostics,
    TestChannel {
        left: bool,
    },
    ToggleMicrophoneMeter,
    SelectOutput {
        device_id: u32,
    },
    SelectInput {
        device_id: u32,
    },
    RoutePlayback {
        stream_id: u32,
        device_id: u32,
    },
    RouteRecording {
        stream_id: u32,
        device_id: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DisplayOperation {
    SetBrightness {
        value: f64,
    },
    SetAutoBrightness {
        enabled: bool,
    },
    SetNightLight {
        enabled: bool,
    },
    SetNightLightTemperature {
        label: String,
    },
    SetScale {
        label: String,
    },
    SetPrimary {
        connector: String,
        primary: bool,
    },
    SetOutputScale {
        connector: String,
        label: String,
    },
    SetPosition {
        connector: String,
        label: String,
    },
    SetMirror {
        connector: String,
        label: String,
    },
    SetModeLabel {
        connector: String,
        label: String,
    },
    SetTransformLabel {
        connector: String,
        label: String,
    },
    SetColorProfileLabel {
        connector: String,
        label: String,
    },
    SetEnabled {
        connector: String,
        enabled: bool,
    },
    SetMode {
        connector: String,
        mode: settings_store::hardware::OutputMode,
    },
    SetTransform {
        connector: String,
        transform: settings_store::hardware::OutputTransform,
    },
    SetHdr {
        connector: String,
        enabled: bool,
    },
    SetVrr {
        connector: String,
        enabled: bool,
    },
    SetColorProfile {
        connector: String,
        profile: Option<String>,
    },
    Apply,
    Confirm,
    Revert,
}

/// Process-local delivery envelope for device effects. This value is never
/// serialized into Automerge: the collaborative document contains portable
/// preferences only, while the selected pane consumes one typed command.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalPaneCommand {
    Updates(UpdateOperation),
    Power(PowerOperation),
    Network(NetworkOperation),
    Bluetooth(BluetoothOperation),
    Audio(AudioOperation),
    Display(DisplayOperation),
    Storage(StorageOperation),
    Printer(PrinterOperation),
    User(UserOperation),
    AgentModel(AgentModelOperation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrinterOperation {
    Refresh,
    Discover,
    SetDefault { name: String },
    SetEnabled { name: String, enabled: bool },
    Remove { name: String },
    CancelJob { id: String },
    RetryJob { id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserOperation {
    SetAdmin {
        username: String,
        admin: bool,
    },
    SetAutologin {
        username: String,
        enabled: bool,
    },
    Remove {
        username: String,
        remove_files: bool,
    },
}

/// A bounded model-manager request.  Package identifiers are revalidated
/// against a freshly-read manager snapshot immediately before the pane starts
/// the command; no URL, path, credentials, or arbitrary manager arguments
/// cross the resource boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentModelOperation {
    StageAccelerator,
    Install { id: String, revision: String },
    Start { id: String },
    Stop { id: String },
    Retry { id: String, revision: String },
    Remove { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceMeshOperation {
    SetRole { role: String },
    SetReplication { replication: String },
    SetService { service: String, enabled: bool },
    SetWorkspace { id: u64, enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentProviderOperation {
    Add {
        protocol: String,
    },
    SetProtocol {
        id: String,
        protocol: String,
    },
    SetEnabled {
        id: String,
        enabled: bool,
    },
    Remove {
        id: String,
    },
    SetDefault {
        id: String,
    },
    SetDetails {
        id: String,
        label: String,
        endpoint: String,
        model: Option<String>,
    },
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
        INPUT_OPERATION_TYPE => {
            let operation: InputOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        TIME_DATE_OPERATION_TYPE => {
            let operation: TimeDateOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        POWER_OPERATION_TYPE => {
            let operation: PowerOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        NETWORK_OPERATION_TYPE => {
            let operation: NetworkOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        BLUETOOTH_OPERATION_TYPE => {
            let operation: BluetoothOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        STORAGE_OPERATION_TYPE => {
            let operation: StorageOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        AUDIO_OPERATION_TYPE => {
            let operation: AudioOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        DISPLAY_OPERATION_TYPE => {
            let operation: DisplayOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        PRINTER_OPERATION_TYPE => {
            let operation: PrinterOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        USER_OPERATION_TYPE => {
            let operation: UserOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        AGENT_MODEL_OPERATION_TYPE => {
            let operation: AgentModelOperation =
                serde_json::from_slice(payload).map_err(|e| e.to_string())?;
            Ok(local_operation_token_for(type_id, &operation))
        }
        DEVICE_MESH_OPERATION_TYPE => {
            let operation: DeviceMeshOperation =
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
fn input_bool_operation(template: &InputOperation, enabled: bool) -> InputOperation {
    match template {
        InputOperation::SetTouchpadNaturalScroll { .. } => {
            InputOperation::SetTouchpadNaturalScroll { enabled }
        }
        InputOperation::SetMouseNaturalScroll { .. } => {
            InputOperation::SetMouseNaturalScroll { enabled }
        }
        InputOperation::SetTapToClick { .. } => InputOperation::SetTapToClick { enabled },
        InputOperation::SetTapDrag { .. } => InputOperation::SetTapDrag { enabled },
        InputOperation::SetDisableWhileTyping { .. } => {
            InputOperation::SetDisableWhileTyping { enabled }
        }
        InputOperation::SetMiddleEmulation { .. } => InputOperation::SetMiddleEmulation { enabled },
        InputOperation::SetLeftHanded { .. } => InputOperation::SetLeftHanded { enabled },
        InputOperation::SetTouchpadGestures { .. } => {
            InputOperation::SetTouchpadGestures { enabled }
        }
        _ => unreachable!("input bool resources only contain boolean operations"),
    }
}

fn input_number_operation(template: &InputOperation, value: f64) -> InputOperation {
    match template {
        InputOperation::SetTouchpadPointerSpeed { .. } => {
            InputOperation::SetTouchpadPointerSpeed { value }
        }
        InputOperation::SetMousePointerSpeed { .. } => {
            InputOperation::SetMousePointerSpeed { value }
        }
        InputOperation::SetKeyRepeat { .. } => InputOperation::SetKeyRepeat { value },
        _ => unreachable!("input number resources only contain numeric operations"),
    }
}

fn input_choice_spec(
    id: &str,
    label: &str,
    choices: &[String],
    operation: fn(String) -> InputOperation,
) -> ActionSpec {
    let choices = choices.to_vec();
    let fallback = choices.first().cloned().unwrap_or_default();
    ActionSpec::typed_operation(
        id,
        label,
        "Choose an input preference",
        OperationKind::Effect,
        INPUT_OPERATION_TYPE,
        vec![
            ActionParam::required_owned("choice", ParamType::Text, "Device-local input value")
                .with_choices(choices),
        ],
        move |args| {
            encode_local_operation(
                INPUT_OPERATION_TYPE,
                &operation(
                    args.text("choice")
                        .map_or_else(|| fallback.clone(), str::to_string),
                ),
            )
        },
    )
    .with_contract(local_operation_contract(&["device-local", "input"]))
}

#[allow(
    clippy::too_many_lines,
    reason = "the allow-list is a deliberately auditable stale-action boundary"
)]
fn validate_setting_target(key: &str, value: &SettingValue) -> Result<(), ActionError> {
    let text =
        || matches!(value, SettingValue::Text(text) if !text.is_empty() && text.len() <= 1024);
    let boolean = || matches!(value, SettingValue::Bool(_));
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
        | "a11y.reader_voice" => text(),
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
        | "shortcuts.vim_mode"
        | "notifications.do_not_disturb"
        | "notifications.show_banners"
        | "notifications.show_previews"
        | "notifications.play_sounds"
        | "notifications.show_on_lock_screen"
        | "notifications.show_badges"
        | "notifications.focus.enabled"
        | "text.check_spelling"
        | "text.auto_correct" => boolean(),
        "notifications.focus.start" | "notifications.focus.end" | "notifications.focus.days" => {
            text()
        }
        // Shortcut keys are an intentionally closed catalogue.  The old
        // prefix rule let a delayed/stale action create arbitrary
        // `shortcuts.*` scalars that the compositor neither knew nor applied.
        // The same catalogue drives the recorder rows and resource actions.
        key if crate::panes::keyboard_shortcuts::is_durable_key(key) && !presentation_key => {
            text() || (key == "shortcuts.vim_mode" && boolean())
        }
        key if key.starts_with("default_apps.") && !presentation_key => text(),
        _ if valid_notification_app_key => true,
        // Trust, service and residency policy is deliberately absent: those
        // settings use typed device-local effect operations and must never
        // enter the replayable SettingsAction namespace.
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

fn immediate_local_effect_spec<T: Serialize>(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &str,
    operation: &T,
    tags: &[&str],
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, OperationKind::Effect, type_id, operation)
        .ok()
        .map(|spec| {
            spec.with_contract(ActionContract {
                retry: RetryPolicy::AtMostOnce,
                reversible: false,
                tags: tags.iter().map(|tag| (*tag).into()).collect(),
                ..ActionContract::default()
            })
        })
}

fn local_bool_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(bool) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "On or off on this device",
        OperationKind::Effect,
        type_id,
        vec![ActionParam::required(
            "value",
            ParamType::Boolean,
            "Enabled",
        )],
        move |args| build(args.boolean("value").unwrap_or(false)),
    )
    .with_contract(local_operation_contract(tags))
}

fn local_number_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(f64) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "A value from 0 to 1",
        OperationKind::Effect,
        type_id,
        vec![
            ActionParam::required("value", ParamType::Number, "Value")
                .with_schema(serde_json::json!({ "minimum": 0.0, "maximum": 1.0 })),
        ],
        move |args| build(args.number("value").unwrap_or(0.0)),
    )
    .with_contract(local_operation_contract(tags))
}

fn local_choice_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    choices: Vec<String>,
    tags: &'static [&'static str],
    build: impl Fn(String) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "Choose a value available on this device",
        OperationKind::Effect,
        type_id,
        vec![ActionParam::required("choice", ParamType::Text, "Choice").with_choices(choices)],
        move |args| build(args.text("choice").unwrap_or_default().to_string()),
    )
    .with_contract(local_operation_contract(tags))
}

fn encode_local_operation<T: Serialize>(type_id: &str, operation: &T) -> EncodedAction {
    EncodedAction::encode_operation(type_id, operation).unwrap_or_else(|_| EncodedAction {
        type_id: type_id.into(),
        payload: Vec::new(),
    })
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
        root = root.with_child(section_resource(category, doc, &d, account_inventory));
    }
    vec![root]
}

fn section_resource(
    category: Category,
    doc: &AutoCommit,
    prefs: &SystemPrefs,
    account_inventory: &[accounts::Account],
) -> Resource {
    let mut pane = match category {
        Category::Network => network_section(doc),
        Category::Bluetooth => bluetooth_section(doc),
        Category::Sound => sound_section(doc),
        Category::Displays => displays_section(doc, prefs),
        Category::MouseKeyboard => input_section(doc, prefs),
        Category::Power => power_section(doc, prefs),
        Category::Appearance => appearance_section(doc, prefs),
        Category::Accessibility => accessibility_section(doc, prefs),
        Category::Notifications => notifications_section(doc, prefs),
        Category::RegionFormats => region_section(doc, prefs),
        Category::TimeDate => time_date_section(doc, prefs),
        Category::KeyboardShortcuts => shortcuts_section(doc),
        Category::DeviceMesh => device_mesh_section(doc),
        Category::Updates => updates_section(),
        Category::ExternalAccounts => accounts_section(account_inventory),
        Category::AgentModels => agent_models_section(),
        Category::DefaultApps => default_apps_section(doc),
        Category::Storage => storage_section(),
        Category::Printers => printers_section(),
        Category::Users => users_section(),
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
        pane.actions.insert(0, open);
    }
    pane
}

fn category_for_resource_id(id: &str) -> Option<Category> {
    if let Some(category) = Category::ALL
        .iter()
        .copied()
        .find(|category| pane_resource_id(*category) == id)
    {
        return Some(category);
    }
    let mappings = [
        ("settings.default-app:", Category::DefaultApps),
        ("settings.account:", Category::ExternalAccounts),
        ("settings.notification-app:", Category::Notifications),
        ("settings.audio-output:", Category::Sound),
        ("settings.audio-input:", Category::Sound),
        ("settings.audio-playback:", Category::Sound),
        ("settings.audio-recording:", Category::Sound),
        ("settings.display-output:", Category::Displays),
        ("settings.input-source:", Category::MouseKeyboard),
        ("settings.bluetooth-device:", Category::Bluetooth),
        ("settings.printer:", Category::Printers),
        ("settings.print-job:", Category::Printers),
        ("settings.user:", Category::Users),
        ("settings.device-mesh-workspace:", Category::DeviceMesh),
        (
            "settings.agent-model-recommendation:",
            Category::AgentModels,
        ),
        ("settings.agent-model:", Category::AgentModels),
        ("settings.agent-provider:", Category::AgentModels),
    ];
    mappings
        .into_iter()
        .find_map(|(prefix, category)| id.starts_with(prefix).then_some(category))
}

fn find_resource(mut resource: Resource, id: &str) -> Option<Resource> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .drain(..)
        .find_map(|child| find_resource(child, id))
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
    for kind in kinds.into_iter().take(MAX_DYNAMIC_RESOURCES) {
        let mut choices: Vec<String> = candidates
            .iter()
            .filter(|candidate| candidate.handles(&kind))
            .map(option)
            .take(MAX_DYNAMIC_CHOICES)
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
        let fallback = read_str_or_text(doc, &ROOT, &setting_key).unwrap_or(first_choice);
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
    let mut spec =
        ActionSpec::operation(id, label, kind, ACCOUNT_OPERATION_TYPE, operation).ok()?;
    if matches!(operation, AccountOperation::Test { .. }) {
        spec = spec.with_contract(local_operation_contract(&[
            "device-local",
            "accounts",
            "network-probe",
        ]));
    }
    Some(spec)
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
) -> Option<tilo_actions::ActionBinding<()>> {
    let mut spec = account_operation_spec(action_id, action_label, kind, operation)?;
    if destructive {
        spec = spec.destructive();
    }
    let resource =
        Resource::new(resource_id, "settings.external-account", resource_label).with_action(spec);
    tilo_actions::ActionBinding::unit_for(&resource, action_id).ok()
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
    for account in account_inventory.iter().take(MAX_DYNAMIC_RESOURCES) {
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
    let b = |key: &str, default: bool| read_bool(doc, &ROOT, key).unwrap_or(default);
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
            read_str_or_text(doc, &ROOT, key).unwrap_or_else(|| fallback.into()),
        ));
    }
    for (app_id, app_name) in
        crate::panes::notifications::resource_notification_apps(&d.notifications)
            .into_iter()
            .take(MAX_DYNAMIC_RESOURCES)
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
            read_str_or_text(doc, &ROOT, format!("notifications.app.{app_id}.mute"))
                .unwrap_or_else(|| "Not muted".into()),
        ));
        sec = sec.with_child(resource);
    }
    sec
}

fn appearance_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let b = |key: &str, default: bool| read_bool(doc, &ROOT, key).unwrap_or(default);
    let s = |key: &str, default: &str| {
        read_str_or_text(doc, &ROOT, key).unwrap_or_else(|| default.to_string())
    };
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

#[allow(
    clippy::too_many_lines,
    reason = "resource tree mirrors Sound hierarchy"
)]
fn sound_section(doc: &AutoCommit) -> Resource {
    let _ = doc;
    let mut sec = Resource::new("settings.section:sound", "settings.section", "Sound");
    sec = sec.with_action(local_bool_spec(
        "muted",
        "Set output mute",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "hardware"],
        |muted| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetOutputMuted { muted },
            )
        },
    ));
    sec = sec.with_action(local_number_spec(
        "volume",
        "Set output volume",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "hardware"],
        |value| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetOutputVolume { value },
            )
        },
    ));
    sec = sec.with_action(local_number_spec(
        "balance",
        "Set output balance",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "session"],
        |value| encode_local_operation(AUDIO_OPERATION_TYPE, &AudioOperation::SetBalance { value }),
    ));
    sec = sec.with_action(local_bool_spec(
        "mono",
        "Set mono output",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "session"],
        |enabled| {
            encode_local_operation(AUDIO_OPERATION_TYPE, &AudioOperation::SetMono { enabled })
        },
    ));
    sec = sec.with_action(local_number_spec(
        "input-volume",
        "Set input volume",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "hardware"],
        |value| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetInputVolume { value },
            )
        },
    ));
    for (id, label, operation) in [
        (
            "diagnostics",
            "Show or hide speaker diagnostics",
            AudioOperation::ToggleDiagnostics,
        ),
        (
            "test-left",
            "Test left speaker",
            AudioOperation::TestChannel { left: true },
        ),
        (
            "test-right",
            "Test right speaker",
            AudioOperation::TestChannel { left: false },
        ),
        (
            "microphone-meter",
            "Start or stop microphone meter",
            AudioOperation::ToggleMicrophoneMeter,
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            AUDIO_OPERATION_TYPE,
            &operation,
            &["device-local", "audio", "diagnostic"],
        ) {
            sec = sec.with_action(action);
        }
    }
    let graph = media::audio::enumerate();
    if !graph.sinks.is_empty() {
        let choices = graph
            .sinks
            .iter()
            .take(MAX_DYNAMIC_CHOICES)
            .map(|device| device.description.clone())
            .collect::<Vec<_>>();
        let devices = graph
            .sinks
            .iter()
            .take(MAX_DYNAMIC_CHOICES)
            .map(|device| (device.description.clone(), device.id))
            .collect::<Vec<_>>();
        sec = sec.with_action(local_choice_spec(
            "output-device",
            "Set output device",
            AUDIO_OPERATION_TYPE,
            choices,
            &["device-local", "audio", "live-device"],
            move |choice| {
                let device_id = devices
                    .iter()
                    .find(|(label, _)| label == &choice)
                    .map_or(u32::MAX, |(_, id)| *id);
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SelectOutput { device_id },
                )
            },
        ));
    }
    if !graph.sources.is_empty() {
        let choices = graph
            .sources
            .iter()
            .take(MAX_DYNAMIC_CHOICES)
            .map(|device| device.description.clone())
            .collect::<Vec<_>>();
        let devices = graph
            .sources
            .iter()
            .take(MAX_DYNAMIC_CHOICES)
            .map(|device| (device.description.clone(), device.id))
            .collect::<Vec<_>>();
        sec = sec.with_action(local_choice_spec(
            "input-device",
            "Set input device",
            AUDIO_OPERATION_TYPE,
            choices,
            &["device-local", "audio", "live-device"],
            move |choice| {
                let device_id = devices
                    .iter()
                    .find(|(label, _)| label == &choice)
                    .map_or(u32::MAX, |(_, id)| *id);
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SelectInput { device_id },
                )
            },
        ));
    }
    for device in graph.sinks.iter().take(MAX_DYNAMIC_RESOURCES) {
        let mut resource = Resource::new(
            format!("settings.audio-output:{}", device.id),
            "settings.audio-output",
            device.description.clone(),
        );
        if let Some(action) = local_operation_spec(
            "select",
            "Use as output device",
            OperationKind::Effect,
            AUDIO_OPERATION_TYPE,
            &AudioOperation::SelectOutput {
                device_id: device.id,
            },
            &["device-local", "audio", "live-device"],
        ) {
            resource = resource.with_action(action);
        }
        sec = sec.with_child(resource);
    }
    for device in graph.sources.iter().take(MAX_DYNAMIC_RESOURCES) {
        let mut resource = Resource::new(
            format!("settings.audio-input:{}", device.id),
            "settings.audio-input",
            device.description.clone(),
        );
        if let Some(action) = local_operation_spec(
            "select",
            "Use as input device",
            OperationKind::Effect,
            AUDIO_OPERATION_TYPE,
            &AudioOperation::SelectInput {
                device_id: device.id,
            },
            &["device-local", "audio", "live-device"],
        ) {
            resource = resource.with_action(action);
        }
        sec = sec.with_child(resource);
    }
    for hardware in graph.hardware.iter().take(MAX_DYNAMIC_RESOURCES) {
        let profiles: Vec<_> = hardware
            .profiles
            .iter()
            .filter(|profile| profile.available)
            .take(MAX_DYNAMIC_CHOICES)
            .map(|profile| (profile.description.clone(), profile.index))
            .collect();
        if profiles.is_empty() {
            continue;
        }
        let choices = profiles.iter().map(|(label, _)| label.clone()).collect();
        let device_id = hardware.id;
        let action = local_choice_spec(
            "profile",
            "Set audio device profile",
            AUDIO_OPERATION_TYPE,
            choices,
            &["device-local", "audio", "live-device"],
            move |choice| {
                let profile_index = profiles
                    .iter()
                    .find(|(label, _)| label == &choice)
                    .map_or(u32::MAX, |(_, index)| *index);
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetDeviceProfile {
                        device_id,
                        profile_index,
                    },
                )
            },
        );
        sec = sec.with_child(
            Resource::new(
                format!("settings.audio-device:{}", hardware.id),
                "settings.audio-device",
                hardware.description.clone(),
            )
            .with_action(action),
        );
    }
    for stream in graph
        .playback_streams
        .iter()
        .chain(graph.record_streams.iter())
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let devices = if stream.playback {
            &graph.sinks
        } else {
            &graph.sources
        };
        let mut route_choices = vec![crate::panes::audio::FOLLOW_DEFAULT.to_string()];
        route_choices.extend(
            devices
                .iter()
                .take(MAX_DYNAMIC_CHOICES - 1)
                .map(|device| device.description.clone()),
        );
        let route_devices: Vec<_> = devices
            .iter()
            .take(MAX_DYNAMIC_CHOICES - 1)
            .map(|device| (device.description.clone(), device.id))
            .collect();
        let stream_id = stream.id;
        let playback = stream.playback;
        let route = local_choice_spec(
            "route",
            "Set audio stream route",
            AUDIO_OPERATION_TYPE,
            route_choices,
            &["device-local", "audio", "live-stream"],
            move |choice| {
                let device_id = if choice == crate::panes::audio::FOLLOW_DEFAULT {
                    None
                } else {
                    route_devices
                        .iter()
                        .find(|(label, _)| label == &choice)
                        .map(|(_, id)| *id)
                };
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::RouteStream {
                        stream_id,
                        playback,
                        device_id,
                    },
                )
            },
        );
        let stream_id = stream.id;
        let volume = local_number_spec(
            "volume",
            "Set audio stream volume",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "live-stream"],
            move |value| {
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetStreamVolume { stream_id, value },
                )
            },
        );
        let stream_id = stream.id;
        let muted = local_bool_spec(
            "muted",
            "Set audio stream mute",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "live-stream"],
            move |muted| {
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetStreamMuted { stream_id, muted },
                )
            },
        );
        let resource = audio_stream_resource(stream.id, stream.playback, &stream.label(), route)
            .with_action(volume)
            .with_action(muted);
        sec = sec.with_child(resource);
    }
    sec
}

fn audio_control_resource(action: ActionSpec) -> Resource {
    Resource::new(
        pane_resource_id(Category::Sound),
        "settings.section",
        "Sound",
    )
    .with_action(action)
}

pub fn audio_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let action = match action_id {
        "muted" => local_bool_spec(
            "muted",
            "Set output mute",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "hardware"],
            |muted| {
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetOutputMuted { muted },
                )
            },
        ),
        "mono" => local_bool_spec(
            "mono",
            "Set mono output",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "session"],
            |enabled| {
                encode_local_operation(AUDIO_OPERATION_TYPE, &AudioOperation::SetMono { enabled })
            },
        ),
        _ => return None,
    };
    bool_binding_for(&audio_control_resource(action), action_id)
}

pub fn audio_unit_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<()>> {
    tilo_actions::ActionBinding::unit_for(&sound_section(&AutoCommit::new()), action_id).ok()
}

pub fn audio_number_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<f64>> {
    let action = match action_id {
        "volume" => local_number_spec(
            "volume",
            "Set output volume",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "hardware"],
            |value| {
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetOutputVolume { value },
                )
            },
        ),
        "input-volume" => local_number_spec(
            "input-volume",
            "Set input volume",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "hardware"],
            |value| {
                encode_local_operation(
                    AUDIO_OPERATION_TYPE,
                    &AudioOperation::SetInputVolume { value },
                )
            },
        ),
        "balance" => local_number_spec(
            "balance",
            "Set output balance",
            AUDIO_OPERATION_TYPE,
            &["device-local", "audio", "session"],
            |value| {
                encode_local_operation(AUDIO_OPERATION_TYPE, &AudioOperation::SetBalance { value })
            },
        ),
        _ => return None,
    };
    let resource = audio_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

pub fn audio_device_binding(
    action_id: &'static str,
    devices: Vec<(String, u32)>,
) -> Option<tilo_actions::ActionBinding<String>> {
    if devices.is_empty() || devices.len() > MAX_DYNAMIC_CHOICES {
        return None;
    }
    let choices = devices.iter().map(|(name, _)| name.clone()).collect();
    let label = if action_id == "output-device" {
        "Set output device"
    } else if action_id == "input-device" {
        "Set input device"
    } else {
        return None;
    };
    let action = local_choice_spec(
        action_id,
        label,
        AUDIO_OPERATION_TYPE,
        choices,
        &["device-local", "audio", "live-device"],
        move |choice| {
            let device_id = devices
                .iter()
                .find(|(name, _)| name == &choice)
                .map_or(u32::MAX, |(_, id)| *id);
            let operation = if action_id == "output-device" {
                AudioOperation::SelectOutput { device_id }
            } else {
                AudioOperation::SelectInput { device_id }
            };
            encode_local_operation(AUDIO_OPERATION_TYPE, &operation)
        },
    );
    let resource = audio_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn audio_profile_binding(
    device_id: u32,
    profiles: Vec<(String, u32)>,
) -> Option<tilo_actions::ActionBinding<String>> {
    if profiles.is_empty() || profiles.len() > MAX_DYNAMIC_CHOICES {
        return None;
    }
    let choices = profiles.iter().map(|(label, _)| label.clone()).collect();
    let action = local_choice_spec(
        "profile",
        "Set audio device profile",
        AUDIO_OPERATION_TYPE,
        choices,
        &["device-local", "audio", "live-device"],
        move |choice| {
            let profile_index = profiles
                .iter()
                .find(|(label, _)| label == &choice)
                .map_or(u32::MAX, |(_, index)| *index);
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetDeviceProfile {
                    device_id,
                    profile_index,
                },
            )
        },
    );
    let resource = Resource::new(
        format!("settings.audio-device:{device_id}"),
        "settings.audio-device",
        format!("Audio device {device_id}"),
    )
    .with_action(action);
    tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |choice| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(choice))])
    })
    .ok()
}

fn audio_stream_resource(
    stream_id: u32,
    playback: bool,
    label: &str,
    action: ActionSpec,
) -> Resource {
    Resource::new(
        format!(
            "settings.audio-{}:{stream_id}",
            if playback { "playback" } else { "recording" }
        ),
        if playback {
            "settings.audio-playback"
        } else {
            "settings.audio-recording"
        },
        label.to_string(),
    )
    .with_action(action)
}

pub fn audio_stream_route_binding(
    stream_id: u32,
    playback: bool,
    stream_label: &str,
    devices: Vec<(String, u32)>,
) -> Option<tilo_actions::ActionBinding<String>> {
    if devices.len() >= MAX_DYNAMIC_CHOICES {
        return None;
    }
    let mut choices = vec![crate::panes::audio::FOLLOW_DEFAULT.to_string()];
    choices.extend(devices.iter().map(|(label, _)| label.clone()));
    let action = local_choice_spec(
        "route",
        "Set audio stream route",
        AUDIO_OPERATION_TYPE,
        choices,
        &["device-local", "audio", "live-stream"],
        move |choice| {
            let device_id = if choice == crate::panes::audio::FOLLOW_DEFAULT {
                None
            } else {
                devices
                    .iter()
                    .find(|(label, _)| label == &choice)
                    .map(|(_, id)| *id)
            };
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::RouteStream {
                    stream_id,
                    playback,
                    device_id,
                },
            )
        },
    );
    let resource = audio_stream_resource(stream_id, playback, stream_label, action);
    tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |choice| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn audio_stream_number_binding(
    stream_id: u32,
    playback: bool,
    stream_label: &str,
) -> Option<tilo_actions::ActionBinding<f64>> {
    let action = local_number_spec(
        "volume",
        "Set audio stream volume",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "live-stream"],
        move |value| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetStreamVolume { stream_id, value },
            )
        },
    );
    let resource = audio_stream_resource(stream_id, playback, stream_label, action);
    tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |value| {
        serde_json::Map::from_iter([("value".into(), serde_json::json!(value))])
    })
    .ok()
}

pub fn audio_stream_bool_binding(
    stream_id: u32,
    playback: bool,
    stream_label: &str,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let action = local_bool_spec(
        "muted",
        "Set audio stream mute",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "live-stream"],
        move |muted| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetStreamMuted { stream_id, muted },
            )
        },
    );
    bool_binding_for(
        &audio_stream_resource(stream_id, playback, stream_label, action),
        "muted",
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "resource tree mirrors display capability hierarchy"
)]
fn displays_section(_doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    let mut sec = Resource::new(
        pane_resource_id(Category::Displays),
        "settings.section",
        "Displays",
    );
    sec = sec.with_action(local_bool_spec(
        "auto_brightness",
        "Set automatic brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetAutoBrightness { enabled },
            )
        },
    ));
    sec = sec.with_action(local_bool_spec(
        "night_light",
        "Set Night Light",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLight { enabled },
            )
        },
    ));
    sec = sec.with_action(local_choice_spec(
        "night_light_temperature",
        "Set Night Light warmth",
        DISPLAY_OPERATION_TYPE,
        vec!["Subtle · 4500 K", "Warm · 3700 K", "Very warm · 2800 K"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLightTemperature { label },
            )
        },
    ));
    sec = sec.with_action(local_number_spec(
        "brightness",
        "Set brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetBrightness { value },
            )
        },
    ));
    // The Display pane persists its scale as the same human-readable labels
    // shown in its picker (for example `150%`), not as a raw factor.  Keeping
    // the resource action on that wire format means Ctrl+K, agents and the
    // visible dropdown all reach the pane through one typed value contract.
    sec = sec.with_action(local_choice_spec(
        "scale",
        "Set display scale",
        DISPLAY_OPERATION_TYPE,
        vec!["100%", "125%", "150%", "175%", "200%"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetScale { label },
            )
        },
    ));
    // Output topology is supplied by the compositor's live, PID-validated
    // snapshot.  Do not turn an absent/stale snapshot into invented monitors.
    if let Some(snapshot) = settings_store::hardware::DisplayHardwareSnapshot::load() {
        let output_labels: Vec<_> = snapshot
            .outputs
            .iter()
            .take(MAX_DYNAMIC_RESOURCES)
            .map(|output| {
                (
                    output.descriptor.connector.clone(),
                    output.descriptor.label().to_string(),
                )
            })
            .collect();
        for output in snapshot.outputs.into_iter().take(MAX_DYNAMIC_RESOURCES) {
            let descriptor = output.descriptor;
            let connector = descriptor.connector.clone();
            let mut resource = Resource::new(
                format!("settings.display-output:{connector}"),
                "settings.display-output",
                descriptor.label().to_string(),
            );
            let operation_connector = connector.clone();
            resource = resource.with_action(local_bool_spec(
                "enabled",
                "Set display enabled",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |enabled| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetEnabled {
                            connector: operation_connector.clone(),
                            enabled,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "scale",
                "Set output scale",
                DISPLAY_OPERATION_TYPE,
                vec!["100%", "125%", "150%", "175%", "200%"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetOutputScale {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let position_choices: Vec<String> = std::iter::once("Automatic".into())
                .chain(
                    output_labels
                        .iter()
                        .filter(|(candidate, _)| candidate != &connector)
                        .flat_map(|(_, label)| {
                            ["Left", "Right", "Above", "Below"]
                                .map(|side| format!("{side} of {label}"))
                        }),
                )
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "position",
                "Set output position",
                DISPLAY_OPERATION_TYPE,
                position_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetPosition {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let mirror_choices: Vec<String> = std::iter::once("Extend workspace".into())
                .chain(
                    output_labels
                        .iter()
                        .filter(|(candidate, _)| candidate != &connector)
                        .map(|(_, label)| format!("Mirror {label}")),
                )
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "mirror",
                "Set output mirroring",
                DISPLAY_OPERATION_TYPE,
                mirror_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetMirror {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_bool_spec(
                "primary",
                "Set primary display",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |primary| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetPrimary {
                            connector: operation_connector.clone(),
                            primary,
                        },
                    )
                },
            ));
            let mode_choices = descriptor
                .modes
                .iter()
                .take(MAX_DYNAMIC_CHOICES)
                .map(|mode| mode.label())
                .collect();
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "mode",
                "Set resolution and refresh rate",
                DISPLAY_OPERATION_TYPE,
                mode_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetModeLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "transform",
                "Set display orientation",
                DISPLAY_OPERATION_TYPE,
                vec![
                    "Landscape",
                    "Portrait right",
                    "Upside down",
                    "Portrait left",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetTransformLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            if descriptor.hdr {
                let operation_connector = connector.clone();
                resource = resource.with_action(local_bool_spec(
                    "hdr",
                    "Set HDR",
                    DISPLAY_OPERATION_TYPE,
                    &["device-local", "display", "live-output"],
                    move |enabled| {
                        encode_local_operation(
                            DISPLAY_OPERATION_TYPE,
                            &DisplayOperation::SetHdr {
                                connector: operation_connector.clone(),
                                enabled,
                            },
                        )
                    },
                ));
            }
            if descriptor.vrr {
                let operation_connector = connector.clone();
                resource = resource.with_action(local_bool_spec(
                    "vrr",
                    "Set variable refresh rate",
                    DISPLAY_OPERATION_TYPE,
                    &["device-local", "display", "live-output"],
                    move |enabled| {
                        encode_local_operation(
                            DISPLAY_OPERATION_TYPE,
                            &DisplayOperation::SetVrr {
                                connector: operation_connector.clone(),
                                enabled,
                            },
                        )
                    },
                ));
            }
            let mut profile_choices = vec!["System default".to_string()];
            profile_choices.extend(
                descriptor
                    .color_profiles
                    .iter()
                    .take(MAX_DYNAMIC_CHOICES - 1)
                    .filter_map(|id| {
                        output
                            .color_profile_labels
                            .get(id)
                            .map(|name| format!("{name} · {id}"))
                    }),
            );
            let operation_connector = connector.clone();
            resource = resource.with_action(local_choice_spec(
                "color-profile",
                "Set colour profile",
                DISPLAY_OPERATION_TYPE,
                profile_choices,
                &["device-local", "display", "live-output"],
                move |label| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::SetColorProfileLabel {
                            connector: operation_connector.clone(),
                            label,
                        },
                    )
                },
            ));
            sec = sec.with_child(resource);
        }
    }
    for (id, label, operation) in [
        (
            "apply",
            "Apply staged display changes",
            DisplayOperation::Apply,
        ),
        ("confirm", "Keep display changes", DisplayOperation::Confirm),
        ("revert", "Revert display changes", DisplayOperation::Revert),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            DISPLAY_OPERATION_TYPE,
            &operation,
            &["device-local", "display", "transaction"],
        ) {
            sec = sec.with_action(action);
        }
    }
    sec
}

fn display_control_resource(action: ActionSpec) -> Resource {
    Resource::new(
        pane_resource_id(Category::Displays),
        "settings.section",
        "Displays",
    )
    .with_action(action)
}

pub fn display_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let action = match action_id {
        "auto_brightness" => local_bool_spec(
            "auto_brightness",
            "Set automatic brightness",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetAutoBrightness { enabled },
                )
            },
        ),
        "night_light" => local_bool_spec(
            "night_light",
            "Set Night Light",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLight { enabled },
                )
            },
        ),
        _ => return None,
    };
    bool_binding_for(&display_control_resource(action), action_id)
}

pub fn display_choice_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let action = match action_id {
        "night_light_temperature" => local_choice_spec(
            action_id,
            "Set Night Light warmth",
            DISPLAY_OPERATION_TYPE,
            vec!["Subtle · 4500 K", "Warm · 3700 K", "Very warm · 2800 K"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            &["device-local", "display", "hardware"],
            |label| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLightTemperature { label },
                )
            },
        ),
        _ => return None,
    };
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn display_number_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<f64>> {
    let action = match action_id {
        "brightness" => local_number_spec(
            "brightness",
            "Set brightness",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |value| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetBrightness { value },
                )
            },
        ),
        _ => return None,
    };
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

pub fn display_scale_binding() -> Option<tilo_actions::ActionBinding<String>> {
    let action = local_choice_spec(
        "scale",
        "Set display scale",
        DISPLAY_OPERATION_TYPE,
        vec!["100%", "125%", "150%", "175%", "200%"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "display", "hardware"],
        |label| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetScale { label },
            )
        },
    );
    let resource = display_control_resource(action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn display_unit_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<()>> {
    let resource = displays_section(&AutoCommit::new(), &SystemPrefs::default());
    tilo_actions::ActionBinding::unit_for(&resource, action_id).ok()
}

fn display_output_control_resource(connector: &str, action: ActionSpec) -> Resource {
    Resource::new(
        format!("settings.display-output:{connector}"),
        "settings.display-output",
        connector.to_string(),
    )
    .with_action(action)
}

pub fn display_output_bool_binding(
    action_id: &'static str,
    connector: String,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let operation_connector = connector.clone();
    let action = match action_id {
        "enabled" => local_bool_spec(
            "enabled",
            "Set display enabled",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetEnabled {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        "primary" => local_bool_spec(
            "primary",
            "Set primary display",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |primary| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetPrimary {
                        connector: operation_connector.clone(),
                        primary,
                    },
                )
            },
        ),
        "hdr" => local_bool_spec(
            "hdr",
            "Set HDR",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetHdr {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        "vrr" => local_bool_spec(
            "vrr",
            "Set variable refresh rate",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "live-output"],
            move |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetVrr {
                        connector: operation_connector.clone(),
                        enabled,
                    },
                )
            },
        ),
        _ => return None,
    };
    bool_binding_for(
        &display_output_control_resource(&connector, action),
        action_id,
    )
}

pub fn display_output_choice_binding(
    action_id: &'static str,
    connector: String,
    choices: Vec<String>,
) -> Option<tilo_actions::ActionBinding<String>> {
    if choices.is_empty() || choices.len() > MAX_DYNAMIC_CHOICES {
        return None;
    }
    let operation_connector = connector.clone();
    let label = match action_id {
        "scale" => "Set output scale",
        "position" => "Set output position",
        "mirror" => "Set output mirroring",
        "mode" => "Set resolution and refresh rate",
        "transform" => "Set display orientation",
        "color-profile" => "Set colour profile",
        _ => return None,
    };
    let action = local_choice_spec(
        action_id,
        label,
        DISPLAY_OPERATION_TYPE,
        choices,
        &["device-local", "display", "live-output"],
        move |label| {
            let operation = match action_id {
                "scale" => DisplayOperation::SetOutputScale {
                    connector: operation_connector.clone(),
                    label,
                },
                "position" => DisplayOperation::SetPosition {
                    connector: operation_connector.clone(),
                    label,
                },
                "mirror" => DisplayOperation::SetMirror {
                    connector: operation_connector.clone(),
                    label,
                },
                "mode" => DisplayOperation::SetModeLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                "transform" => DisplayOperation::SetTransformLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                "color-profile" => DisplayOperation::SetColorProfileLabel {
                    connector: operation_connector.clone(),
                    label,
                },
                _ => unreachable!("action id is validated before constructing the closure"),
            };
            encode_local_operation(DISPLAY_OPERATION_TYPE, &operation)
        },
    );
    let resource = display_output_control_resource(&connector, action);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered resource declaration mirrors the Input hierarchy"
)]
fn input_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let mut sec = Resource::new(
        pane_resource_id(Category::MouseKeyboard),
        "settings.section",
        "Mouse & Keyboard",
    );
    for (id, label, operation) in [
        (
            "touchpad-natural-scroll",
            "Set touchpad natural scrolling",
            InputOperation::SetTouchpadNaturalScroll { enabled: false },
        ),
        (
            "mouse-natural-scroll",
            "Set mouse natural scrolling",
            InputOperation::SetMouseNaturalScroll { enabled: false },
        ),
        (
            "tap-to-click",
            "Set tap to click",
            InputOperation::SetTapToClick { enabled: false },
        ),
        (
            "tap-drag",
            "Set tap and drag",
            InputOperation::SetTapDrag { enabled: false },
        ),
        (
            "disable-while-typing",
            "Set disable while typing",
            InputOperation::SetDisableWhileTyping { enabled: false },
        ),
        (
            "middle-emulation",
            "Set middle-button emulation",
            InputOperation::SetMiddleEmulation { enabled: false },
        ),
        (
            "left-handed",
            "Set left-handed pointer buttons",
            InputOperation::SetLeftHanded { enabled: false },
        ),
        (
            "touchpad-gestures",
            "Set touchpad gestures",
            InputOperation::SetTouchpadGestures { enabled: false },
        ),
    ] {
        let operation = operation;
        sec = sec.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "On or off on this device",
                OperationKind::Effect,
                INPUT_OPERATION_TYPE,
                vec![ActionParam::required(
                    "value",
                    ParamType::Boolean,
                    "Whether this local preference is enabled",
                )],
                move |args| {
                    encode_local_operation(
                        INPUT_OPERATION_TYPE,
                        &input_bool_operation(&operation, args.boolean("value").unwrap_or(false)),
                    )
                },
            )
            .with_contract(local_operation_contract(&["device-local", "input"])),
        );
    }
    for (id, label, operation) in [
        (
            "touchpad-speed",
            "Set touchpad speed",
            InputOperation::SetTouchpadPointerSpeed { value: 0.0 },
        ),
        (
            "mouse-speed",
            "Set mouse speed",
            InputOperation::SetMousePointerSpeed { value: 0.0 },
        ),
        (
            "key-repeat",
            "Set key-repeat rate",
            InputOperation::SetKeyRepeat { value: 0.0 },
        ),
    ] {
        let operation = operation;
        sec = sec.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "0.0 – 1.0",
                OperationKind::Effect,
                INPUT_OPERATION_TYPE,
                vec![
                    ActionParam::required("value", ParamType::Number, "New local input value")
                        .with_schema(serde_json::json!({"minimum": 0.0, "maximum": 1.0})),
                ],
                move |args| {
                    encode_local_operation(
                        INPUT_OPERATION_TYPE,
                        &input_number_operation(
                            &operation,
                            args.number("value").unwrap_or(0.0).clamp(0.0, 1.0),
                        ),
                    )
                },
            )
            .with_contract(local_operation_contract(&["device-local", "input"])),
        );
    }
    sec = sec.with_action(input_choice_spec(
        "acceleration-profile",
        "Set pointer acceleration profile",
        &["Adaptive".into(), "Flat".into()],
        |choice| InputOperation::SetAccelerationProfile { profile: choice },
    ));
    sec = sec.with_action(input_choice_spec(
        "compose-key",
        "Set compose key",
        &[
            "Disabled".into(),
            "Right Alt".into(),
            "Menu".into(),
            "Caps Lock".into(),
            "Right Super".into(),
            "Left Super".into(),
        ],
        |choice| InputOperation::SetComposeKey { key: choice },
    ));
    // Text assistance is portable user text preference rather than a hardware
    // command, and therefore intentionally remains in SettingsAction.
    for (id, key, label) in [
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
        sec = sec.with_action(bool_flip_spec(
            id,
            key,
            read_bool(doc, &ROOT, key).unwrap_or(true),
            label,
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
    let active_choices: Vec<String> = sources
        .iter()
        .filter_map(|source| {
            catalog
                .iter()
                .find(|entry| entry.id == *source)
                .map(|entry| entry.label.clone())
        })
        .collect();
    if !active_choices.is_empty() {
        sec = sec.with_action(input_choice_spec(
            "active-source",
            "Set active keyboard input source",
            &active_choices,
            |label| InputOperation::SetActiveSource { label },
        ));
    }
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
    for (index, source) in sources.iter().take(MAX_DYNAMIC_RESOURCES).enumerate() {
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
fn power_section(_doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    let mut sec = Resource::new("settings.section:power", "settings.section", "Power");
    sec = sec.with_action(local_bool_spec(
        "auto_suspend",
        "Set automatic suspend",
        POWER_OPERATION_TYPE,
        &["device-local", "power"],
        |enabled| {
            encode_local_operation(
                POWER_OPERATION_TYPE,
                &PowerOperation::SetAutoSuspend { enabled },
            )
        },
    ));
    let suspend_options = vec![
        "5 minutes",
        "10 minutes",
        "15 minutes",
        "30 minutes",
        "45 minutes",
        "1 hour",
        "Never",
    ];
    for (id, label, options, profile) in [
        (
            "profile",
            "Set power profile",
            vec!["Power Saver", "Balanced", "Performance"],
            "active",
        ),
        (
            "suspend-after",
            "Set idle suspend delay",
            suspend_options.clone(),
            "default",
        ),
        (
            "blank-after",
            "Set screen blank delay",
            vec![
                "1 minute",
                "5 minutes",
                "10 minutes",
                "15 minutes",
                "30 minutes",
                "Never",
            ],
            "blank",
        ),
        (
            "lock-after",
            "Set automatic lock delay",
            vec![
                "1 minute",
                "5 minutes",
                "10 minutes",
                "15 minutes",
                "30 minutes",
                "Never",
            ],
            "lock",
        ),
        (
            "saver-suspend",
            "Set Power Saver suspend delay",
            suspend_options.clone(),
            "saver",
        ),
        (
            "balanced-suspend",
            "Set Balanced suspend delay",
            suspend_options.clone(),
            "balanced",
        ),
        (
            "performance-suspend",
            "Set Performance suspend delay",
            suspend_options,
            "performance",
        ),
    ] {
        sec = sec.with_action(local_choice_spec(
            id,
            label,
            POWER_OPERATION_TYPE,
            options.into_iter().map(str::to_string).collect(),
            &["device-local", "power"],
            move |choice| {
                let operation = match profile {
                    "active" => PowerOperation::SetProfile { profile: choice },
                    "blank" => PowerOperation::SetBlankTimeout { label: choice },
                    "lock" => PowerOperation::SetLockTimeout { label: choice },
                    profile => PowerOperation::SetSuspendTimeout {
                        profile: profile.into(),
                        label: choice,
                    },
                };
                encode_local_operation(POWER_OPERATION_TYPE, &operation)
            },
        ));
    }
    for (id, label, kind) in [
        ("lid", "Set lid-close action", "lid"),
        ("power_key", "Set power-key action", "power-key"),
        ("critical", "Set critical-battery action", "critical"),
    ] {
        sec = sec.with_action(local_choice_spec(
            id,
            label,
            POWER_OPERATION_TYPE,
            ["Suspend", "Lock screen", "Do nothing"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            &["device-local", "power"],
            move |action| {
                let operation = match kind {
                    "lid" => PowerOperation::SetLidAction { action },
                    "power-key" => PowerOperation::SetPowerKeyAction { action },
                    _ => PowerOperation::SetCriticalAction { action },
                };
                encode_local_operation(POWER_OPERATION_TYPE, &operation)
            },
        ));
    }
    if let Some(capability) = os_actions::charge_capability() {
        sec = sec.with_action(charge_cap_spec(&capability.choices, capability.current));
    }
    sec
}

fn charge_cap_spec(choices: &[u32], fallback: u32) -> ActionSpec {
    let choices = choices
        .iter()
        .map(|percent| format!("{percent}%"))
        .collect::<Vec<_>>();
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
                .and_then(|value| value.trim_end_matches('%').parse::<u32>().ok())
                .unwrap_or(fallback);
            encode_local_operation(
                POWER_OPERATION_TYPE,
                &PowerOperation::SetChargeLimit { percent },
            )
        },
    )
    .with_contract(local_operation_contract(&["device-local", "hardware"]))
    .privileged()
}

pub fn power_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    bool_binding_for(
        &power_section(&AutoCommit::new(), &SystemPrefs::default()),
        action_id,
    )
}

pub fn power_text_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = power_section(&AutoCommit::new(), &SystemPrefs::default());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn power_charge_binding(
    choices: &[u32],
    fallback: u32,
) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = Resource::new(
        pane_resource_id(Category::Power),
        "settings.section",
        "Power",
    )
    .with_action(charge_cap_spec(choices, fallback));
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |percent| {
        serde_json::Map::from_iter([("percent".to_string(), serde_json::Value::String(percent))])
    })
    .ok()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered declaration mirrors the visible Accessibility pane and keeps every resource action auditable in one place"
)]
fn accessibility_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    use crate::panes;
    let s = |key: &str, default: &str| {
        read_str_or_text(doc, &ROOT, key).unwrap_or_else(|| default.to_string())
    };
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
pub fn accessibility_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = accessibility_section(&AutoCommit::new(), &SystemPrefs::default());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

/// Bind an Accessibility chooser to its exact typed resource action. The
/// choices are validated by the same `ActionSpec` used for Ctrl+K before a
/// control invocation reaches the host.
pub fn accessibility_text_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = accessibility_section(&AutoCommit::new(), &SystemPrefs::default());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered local-operation declarations mirror the Updates pane"
)]
fn updates_section() -> Resource {
    let mut sec = Resource::new("settings.section:updates", "settings.section", "Updates");
    // The timer is an effective property of this machine, not a shared
    // preference.  It gets a typed local request so its result can be read
    // back from systemd and reported to the caller.
    sec = sec.with_action(local_bool_spec(
        "automatic",
        "Set automatic updates",
        UPDATE_OPERATION_TYPE,
        &["device-local", "system-update"],
        |enabled| {
            encode_local_operation(
                UPDATE_OPERATION_TYPE,
                &UpdateOperation::SetAutomatic { enabled },
            )
        },
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
    sec = sec.with_action(local_choice_spec(
        "schedule",
        "Set update schedule",
        UPDATE_OPERATION_TYPE,
        vec!["System schedule", "Daily", "Weekly"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        &["device-local", "system-update"],
        |schedule| {
            encode_local_operation(
                UPDATE_OPERATION_TYPE,
                &UpdateOperation::SetSchedule { schedule },
            )
        },
    ));
    sec = sec.with_action(local_bool_spec(
        "metered",
        "Set metered-network update policy",
        UPDATE_OPERATION_TYPE,
        &["device-local", "system-update"],
        |allowed| {
            encode_local_operation(
                UPDATE_OPERATION_TYPE,
                &UpdateOperation::SetMetered { allowed },
            )
        },
    ));
    sec
}

pub fn updates_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = updates_section();
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".into(), serde_json::json!(value))])
    })
    .ok()
}

pub fn updates_schedule_binding() -> Option<tilo_actions::ActionBinding<String>> {
    let resource = updates_section();
    let spec = resource
        .actions
        .iter()
        .find(|action| action.id == "schedule")?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn updates_unit_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<()>> {
    tilo_actions::ActionBinding::unit_for(&updates_section(), action_id).ok()
}

fn network_section(_doc: &AutoCommit) -> Resource {
    let mut sec = Resource::new("settings.section:network", "settings.section", "Network");
    sec = sec.with_action(local_bool_spec(
        "wifi",
        "Set Wi-Fi power",
        NETWORK_OPERATION_TYPE,
        &["device-local", "network", "hardware"],
        |enabled| {
            encode_local_operation(
                NETWORK_OPERATION_TYPE,
                &NetworkOperation::SetWifiEnabled { enabled },
            )
        },
    ));
    // These controls address adapters and radios present on *this* machine.
    // They are local effects with a status lifecycle, not shared settings.
    for (id, label, operation) in [
        ("refresh", "Refresh network list", NetworkOperation::Refresh),
        (
            "run-diagnostics",
            "Run network diagnostics",
            NetworkOperation::RunDiagnostics,
        ),
        (
            "enable-wired",
            "Enable wired networking",
            NetworkOperation::SetWiredEnabled { enabled: true },
        ),
        (
            "disable-wired",
            "Disable wired networking",
            NetworkOperation::SetWiredEnabled { enabled: false },
        ),
        (
            "enable-airplane-mode",
            "Enable airplane mode",
            NetworkOperation::SetAirplaneMode { enabled: true },
        ),
        (
            "disable-airplane-mode",
            "Disable airplane mode",
            NetworkOperation::SetAirplaneMode { enabled: false },
        ),
        (
            "enable-hotspot",
            "Enable hotspot",
            NetworkOperation::SetHotspot { enabled: true },
        ),
        (
            "disable-hotspot",
            "Disable hotspot",
            NetworkOperation::SetHotspot { enabled: false },
        ),
        (
            "enable-mobile-broadband",
            "Enable mobile broadband",
            NetworkOperation::SetMobileBroadband { enabled: true },
        ),
        (
            "disable-mobile-broadband",
            "Disable mobile broadband",
            NetworkOperation::SetMobileBroadband { enabled: false },
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            NETWORK_OPERATION_TYPE,
            &operation,
            &["device-local", "network"],
        ) {
            sec = sec.with_action(action);
        }
    }
    sec
}

fn bluetooth_section(_doc: &AutoCommit) -> Resource {
    let mut bt = Resource::new(
        "settings.section:bluetooth",
        "settings.section",
        "Bluetooth",
    );
    bt = bt.with_action(local_bool_spec(
        "bluetooth",
        "Set Bluetooth power",
        BLUETOOTH_OPERATION_TYPE,
        &["device-local", "bluetooth", "hardware"],
        |enabled| {
            encode_local_operation(
                BLUETOOTH_OPERATION_TYPE,
                &BluetoothOperation::SetPowered { enabled },
            )
        },
    ));
    for (id, label, operation) in [
        (
            "scan",
            "Scan for Bluetooth devices",
            BluetoothOperation::StartScan,
        ),
        (
            "refresh",
            "Refresh Bluetooth devices",
            BluetoothOperation::Refresh,
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            BLUETOOTH_OPERATION_TYPE,
            &operation,
            &["device-local", "bluetooth"],
        ) {
            bt = bt.with_action(action);
        }
    }
    // Device resources are deliberately live: an object path is meaningful
    // only while BlueZ advertises it. The host revalidates it again before
    // latching the pane command, closing the stale-resource race.
    for device in os_actions::bluez::devices()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let mut resource = Resource::new(
            format!("settings.bluetooth-device:{}", device.path),
            "settings.bluetooth-device",
            device.name.clone(),
        );
        let operation = if !device.paired {
            Some((
                "pair",
                "Pair",
                BluetoothOperation::Pair {
                    path: device.path.clone(),
                },
            ))
        } else if device.connected {
            Some((
                "disconnect",
                "Disconnect",
                BluetoothOperation::Disconnect {
                    path: device.path.clone(),
                },
            ))
        } else {
            Some((
                "connect",
                "Connect",
                BluetoothOperation::Connect {
                    path: device.path.clone(),
                },
            ))
        };
        if let Some((id, label, operation)) = operation
            && let Some(action) = local_operation_spec(
                id,
                label,
                OperationKind::Effect,
                BLUETOOTH_OPERATION_TYPE,
                &operation,
                &["device-local", "bluetooth", "live-device"],
            )
        {
            resource = resource.with_action(action);
        }
        if device.paired
            && let Some(action) = local_operation_spec(
                "forget",
                "Forget device",
                OperationKind::Effect,
                BLUETOOTH_OPERATION_TYPE,
                &BluetoothOperation::Forget { path: device.path },
                &["device-local", "bluetooth", "destructive", "live-device"],
            )
        {
            resource = resource.with_action(action.destructive());
        }
        bt = bt.with_child(resource);
    }
    bt
}

fn bool_binding_for(
    resource: &Resource,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

pub fn network_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    bool_binding_for(&network_section(&AutoCommit::new()), action_id)
}

pub fn bluetooth_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    bool_binding_for(&bluetooth_section(&AutoCommit::new()), action_id)
}

fn storage_section() -> Resource {
    let mut storage = Resource::new("settings.section:storage", "settings.section", "Storage");
    for (id, label, operation) in [
        (
            "cleanup-caches",
            "Clean application caches",
            StorageOperation::CleanupCaches,
        ),
        (
            "free-synced-files",
            "Free downloaded synced files",
            StorageOperation::FreeSyncedFiles,
        ),
        ("open-disks", "Open Disks", StorageOperation::OpenDisks),
        (
            "open-backups",
            "Open Backups",
            StorageOperation::OpenBackups,
        ),
    ] {
        if let Some(mut action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            STORAGE_OPERATION_TYPE,
            &operation,
            &["device-local", "storage"],
        ) {
            if matches!(
                operation,
                StorageOperation::CleanupCaches | StorageOperation::FreeSyncedFiles
            ) {
                action = action.destructive();
            }
            storage = storage.with_action(action);
        }
    }
    storage
}

#[allow(
    clippy::too_many_lines,
    reason = "resource tree mirrors printer hierarchy"
)]
fn printers_section() -> Resource {
    let mut sec = Resource::new(
        pane_resource_id(Category::Printers),
        "settings.section",
        "Printers",
    );
    for (id, label, operation) in [
        ("refresh", "Refresh printers", PrinterOperation::Refresh),
        (
            "discover",
            "Find network printers",
            PrinterOperation::Discover,
        ),
    ] {
        if let Some(action) = local_operation_spec(
            id,
            label,
            OperationKind::Effect,
            PRINTER_OPERATION_TYPE,
            &operation,
            &["device-local", "printers"],
        ) {
            sec = sec.with_action(action);
        }
    }
    for queue in os_actions::printing::queues()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let mut resource = Resource::new(
            format!("settings.printer:{}", queue.name),
            "settings.printer",
            queue.name.clone(),
        );
        for (id, label, operation) in [
            (
                "set-default",
                "Set as default printer",
                PrinterOperation::SetDefault {
                    name: queue.name.clone(),
                },
            ),
            (
                if queue.state == os_actions::printing::QueueState::Paused {
                    "enable"
                } else {
                    "pause"
                },
                if queue.state == os_actions::printing::QueueState::Paused {
                    "Enable printer"
                } else {
                    "Pause printer"
                },
                PrinterOperation::SetEnabled {
                    name: queue.name.clone(),
                    enabled: queue.state == os_actions::printing::QueueState::Paused,
                },
            ),
            (
                "remove",
                "Remove printer",
                PrinterOperation::Remove {
                    name: queue.name.clone(),
                },
            ),
        ] {
            if let Some(mut action) = local_operation_spec(
                id,
                label,
                OperationKind::Effect,
                PRINTER_OPERATION_TYPE,
                &operation,
                &["device-local", "printers", "live-queue"],
            ) {
                if matches!(operation, PrinterOperation::Remove { .. }) {
                    action = action.destructive();
                }
                resource = resource.with_action(action);
            }
        }
        sec = sec.with_child(resource);
    }
    for job in os_actions::printing::jobs()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let mut resource = Resource::new(
            format!("settings.print-job:{}", job.id),
            "settings.print-job",
            job.id.clone(),
        );
        for (id, label, operation) in [
            (
                "cancel",
                "Cancel print job",
                PrinterOperation::CancelJob { id: job.id.clone() },
            ),
            (
                "retry",
                "Retry print job",
                PrinterOperation::RetryJob { id: job.id.clone() },
            ),
        ] {
            if let Some(action) = local_operation_spec(
                id,
                label,
                OperationKind::Effect,
                PRINTER_OPERATION_TYPE,
                &operation,
                &["device-local", "printers", "live-job"],
            ) {
                resource = resource.with_action(action);
            }
        }
        sec = sec.with_child(resource);
    }
    sec
}

fn users_section() -> Resource {
    let mut sec = Resource::new(
        pane_resource_id(Category::Users),
        "settings.section",
        "Users",
    );
    for user in crate::panes::users::users_for_display()
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        if user.current {
            continue;
        }
        let mut resource = Resource::new(
            format!("settings.user:{}", user.username),
            "settings.user",
            user.full_name.clone(),
        );
        for (id, label, operation) in [
            (
                "set-admin",
                if user.admin {
                    "Make standard user"
                } else {
                    "Make administrator"
                },
                UserOperation::SetAdmin {
                    username: user.username.clone(),
                    admin: !user.admin,
                },
            ),
            (
                "set-autologin",
                if user.automatic_login {
                    "Disable automatic login"
                } else {
                    "Enable automatic login"
                },
                UserOperation::SetAutologin {
                    username: user.username.clone(),
                    enabled: !user.automatic_login,
                },
            ),
            (
                "remove-keep-files",
                "Remove account, keep files",
                UserOperation::Remove {
                    username: user.username.clone(),
                    remove_files: false,
                },
            ),
            (
                "remove-files",
                "Remove account and files",
                UserOperation::Remove {
                    username: user.username.clone(),
                    remove_files: true,
                },
            ),
        ] {
            if let Some(action) = local_operation_spec(
                id,
                label,
                OperationKind::Effect,
                USER_OPERATION_TYPE,
                &operation,
                &["device-local", "users", "live-account"],
            ) {
                resource = resource.with_action(action);
            }
        }
        sec = sec.with_child(resource);
    }
    sec
}

/// The model manager is the source of truth for the dynamic package tree.
/// Building the palette snapshot is deliberately best-effort: stale entries
/// are rejected again by the local executor before any manager command runs.
#[allow(
    clippy::too_many_lines,
    reason = "the resource tree keeps each live model phase beside its bounded action"
)]
fn agent_models_section() -> Resource {
    use crate::panes::agent_models::{self, AcceleratorSetupState, LocalModelPhase};

    let mut section = Resource::new(
        pane_resource_id(Category::AgentModels),
        "settings.agent-models",
        Category::AgentModels.label(),
    );
    for (id, label, protocol) in [
        ("add-anthropic", "Add Anthropic provider", "anthropic"),
        (
            "add-openai-compatible",
            "Add compatible API provider",
            "openai-compatible",
        ),
    ] {
        if let Some(action) = immediate_local_effect_spec(
            id,
            label,
            AGENT_PROVIDER_OPERATION_TYPE,
            &AgentProviderOperation::Add {
                protocol: protocol.into(),
            },
            &["device-local", "agent-provider"],
        ) {
            section = section.with_action(action);
        }
    }
    let provider_config = agent_provider_config::ProviderConfig::load();
    for profile in provider_config
        .profiles
        .iter()
        .filter(|profile| profile.location != agent_provider_config::EndpointLocation::ThisDevice)
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let mut resource = Resource::new(
            format!("settings.agent-provider:{}", profile.id),
            "settings.agent-provider",
            profile.display_label(),
        );
        let alternate = match profile.protocol {
            agent_provider_config::ProviderProtocol::Anthropic => "openai-compatible",
            agent_provider_config::ProviderProtocol::OpenAiCompatible => "anthropic",
        };
        for (id, label, operation) in [
            (
                "set-protocol",
                format!("Use {alternate} protocol"),
                AgentProviderOperation::SetProtocol {
                    id: profile.id.clone(),
                    protocol: alternate.into(),
                },
            ),
            (
                if profile.enabled { "disable" } else { "enable" },
                if profile.enabled {
                    "Disable provider".into()
                } else {
                    "Enable provider".into()
                },
                AgentProviderOperation::SetEnabled {
                    id: profile.id.clone(),
                    enabled: !profile.enabled,
                },
            ),
            (
                "remove",
                "Remove provider".into(),
                AgentProviderOperation::Remove {
                    id: profile.id.clone(),
                },
            ),
        ] {
            if let Some(mut action) = immediate_local_effect_spec(
                id,
                label,
                AGENT_PROVIDER_OPERATION_TYPE,
                &operation,
                &["device-local", "agent-provider"],
            ) {
                if matches!(operation, AgentProviderOperation::Remove { .. }) {
                    action = action.destructive();
                }
                resource = resource.with_action(action);
            }
        }
        let details_id = profile.id.clone();
        resource = resource.with_action(
            ActionSpec::typed_operation(
                "set-details",
                "Update provider details",
                "Provider name",
                OperationKind::Effect,
                AGENT_PROVIDER_OPERATION_TYPE,
                vec![
                    ActionParam::required("label", ParamType::Text, "Provider name"),
                    ActionParam::required("endpoint", ParamType::Text, "HTTPS API endpoint"),
                    ActionParam::optional("model", ParamType::Text, "Default model id"),
                ],
                move |args| {
                    encode_local_operation(
                        AGENT_PROVIDER_OPERATION_TYPE,
                        &AgentProviderOperation::SetDetails {
                            id: details_id.clone(),
                            label: args.text("label").unwrap_or_default().to_string(),
                            endpoint: args.text("endpoint").unwrap_or_default().to_string(),
                            model: args.text("model").map(str::to_string),
                        },
                    )
                },
            )
            .with_contract(ActionContract {
                retry: RetryPolicy::AtMostOnce,
                reversible: false,
                tags: vec!["device-local".into(), "agent-provider".into()],
                ..ActionContract::default()
            }),
        );
        if profile.default_model.is_some()
            && let Some(action) = immediate_local_effect_spec(
                "set-default",
                "Use by default",
                AGENT_PROVIDER_OPERATION_TYPE,
                &AgentProviderOperation::SetDefault {
                    id: profile.id.clone(),
                },
                &["device-local", "agent-provider"],
            )
        {
            resource = resource.with_action(action);
        }
        section = section.with_child(resource);
    }
    let Ok(snapshot) = agent_models::live_snapshot() else {
        return section;
    };
    if snapshot.accelerator_setup.as_ref().is_some_and(|setup| {
        matches!(
            setup.status,
            AcceleratorSetupState::Available | AcceleratorSetupState::Error { .. }
        )
    }) && let Some(action) = local_operation_spec(
        "stage-accelerator",
        "Set up accelerator support",
        OperationKind::Effect,
        AGENT_MODEL_OPERATION_TYPE,
        &AgentModelOperation::StageAccelerator,
        &["device-local", "agent-models", "live-accelerator"],
    ) {
        section = section.with_action(action);
    }
    let install_available = snapshot.hardware.fit != agent_models::HardwareFit::Unsupported
        && snapshot
            .accelerator_setup
            .as_ref()
            .is_none_or(|setup| !setup.status.blocks_model_setup());
    if let Some(package) = snapshot.recommendation {
        let resource = Resource::new(
            format!("settings.agent-model-recommendation:{}", package.id),
            "settings.agent-model-package",
            package.name.clone(),
        );
        if install_available
            && let Some(action) = local_operation_spec(
                "install",
                "Accept license and install on this device",
                OperationKind::Effect,
                AGENT_MODEL_OPERATION_TYPE,
                &AgentModelOperation::Install {
                    id: package.id,
                    revision: package.revision,
                },
                &["device-local", "agent-models", "signed-package"],
            )
        {
            section = section.with_child(resource.with_action(action));
        } else {
            section = section.with_child(resource);
        }
    }
    for model in snapshot.models.into_iter().take(MAX_DYNAMIC_RESOURCES) {
        let mut resource = Resource::new(
            format!("settings.agent-model:{}", model.package.id),
            "settings.agent-model",
            model.package.name.clone(),
        );
        let operation = match model.phase {
            LocalModelPhase::Ready { running: true, .. } => Some((
                "stop",
                "Stop local model",
                AgentModelOperation::Stop {
                    id: model.package.id.clone(),
                },
            )),
            LocalModelPhase::Ready { running: false, .. } => Some((
                "start",
                "Start local model",
                AgentModelOperation::Start {
                    id: model.package.id.clone(),
                },
            )),
            LocalModelPhase::Error { .. } => Some((
                "retry",
                "Retry model installation",
                AgentModelOperation::Retry {
                    id: model.package.id.clone(),
                    revision: model.package.revision.clone(),
                },
            )),
            LocalModelPhase::Downloading { .. } | LocalModelPhase::Installing { .. } => None,
        };
        if let Some((id, label, operation)) = operation
            && let Some(action) = local_operation_spec(
                id,
                label,
                OperationKind::Effect,
                AGENT_MODEL_OPERATION_TYPE,
                &operation,
                &["device-local", "agent-models", "live-model"],
            )
        {
            resource = resource.with_action(action);
        }
        if matches!(
            model.phase,
            LocalModelPhase::Ready { .. } | LocalModelPhase::Error { .. }
        ) && let Some(action) = local_operation_spec(
            "remove",
            "Remove local model",
            OperationKind::Effect,
            AGENT_MODEL_OPERATION_TYPE,
            &AgentModelOperation::Remove {
                id: model.package.id,
            },
            &["device-local", "agent-models", "live-model", "destructive"],
        ) {
            resource = resource.with_action(action.destructive());
        }
        section = section.with_child(resource);
    }
    section
}

pub fn agent_provider_action_resources() -> Resource {
    agent_models_section()
}

pub fn agent_provider_binding(
    section: &Resource,
    profile_id: Option<&str>,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<()>> {
    let resource = match profile_id {
        None => section.clone(),
        Some(profile_id) => section
            .children
            .iter()
            .find(|resource| {
                resource.id.as_str() == format!("settings.agent-provider:{profile_id}")
            })?
            .clone(),
    };
    tilo_actions::ActionBinding::unit_for(&resource, action_id).ok()
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
    let current = |key: &str, fallback: &str| {
        read_str_or_text(doc, &ROOT, key).unwrap_or_else(|| fallback.into())
    };
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

fn time_date_section(_doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
    let mut section = Resource::new(
        pane_resource_id(Category::TimeDate),
        "settings.section",
        "Time & Date",
    );
    for (id, label, operation) in [
        (
            "automatic-time",
            "Set automatic time",
            TimeDateOperation::SetAutomaticTime { enabled: false },
        ),
        (
            "24-hour",
            "Set 24-hour time",
            TimeDateOperation::SetHourCycle { h24: false },
        ),
    ] {
        section = section.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "On or off on this device",
                OperationKind::Effect,
                TIME_DATE_OPERATION_TYPE,
                vec![ActionParam::required(
                    "value",
                    ParamType::Boolean,
                    "Whether this local time control is enabled",
                )],
                move |args| {
                    encode_local_operation(
                        TIME_DATE_OPERATION_TYPE,
                        &time_bool_operation(&operation, args.boolean("value").unwrap_or(false)),
                    )
                },
            )
            .with_contract(local_operation_contract(&["device-local", "time-date"])),
        );
    }
    section.with_action(
        ActionSpec::typed_operation(
            "timezone",
            "Set time zone",
            "IANA time-zone identifier",
            OperationKind::Effect,
            TIME_DATE_OPERATION_TYPE,
            vec![ActionParam::required(
                "choice",
                ParamType::Text,
                "Time zone",
            )],
            |args| {
                encode_local_operation(
                    TIME_DATE_OPERATION_TYPE,
                    &TimeDateOperation::SetTimezone {
                        timezone: args.text("choice").unwrap_or("UTC").to_string(),
                    },
                )
            },
        )
        .with_contract(local_operation_contract(&["device-local", "time-date"])),
    )
}

fn time_bool_operation(template: &TimeDateOperation, value: bool) -> TimeDateOperation {
    match template {
        TimeDateOperation::SetAutomaticTime { .. } => {
            TimeDateOperation::SetAutomaticTime { enabled: value }
        }
        TimeDateOperation::SetHourCycle { .. } => TimeDateOperation::SetHourCycle { h24: value },
        TimeDateOperation::SetTimezone { .. } => {
            unreachable!("time boolean resource must be boolean")
        }
    }
}

/// Bind a Mouse & Keyboard boolean control to the exact action published in
/// the local resource tree. This keeps pointer, keyboard and a11y activation
/// on the same typed door as palette and agent requests.
pub fn input_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = input_section(&AutoCommit::new(), &SystemPrefs::load());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".into(), serde_json::Value::Bool(value))])
    })
    .ok()
}

pub fn input_number_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<f64>> {
    let resource = input_section(&AutoCommit::new(), &SystemPrefs::load());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".into(), serde_json::json!(value))])
    })
    .ok()
}

pub fn input_text_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = input_section(&AutoCommit::new(), &SystemPrefs::load());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    let parameter = if spec.params().iter().any(|param| param.name == "choice") {
        "choice"
    } else if spec.params().iter().any(|param| param.name == "label") {
        "label"
    } else {
        "value"
    };
    tilo_actions::ActionBinding::new(&resource, spec, move |value| {
        serde_json::Map::from_iter([(parameter.into(), serde_json::Value::String(value))])
    })
    .ok()
}

pub fn keyboard_source_binding(
    source: &str,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<()>> {
    let resource = input_section(&AutoCommit::new(), &SystemPrefs::load())
        .children
        .into_iter()
        .find(|resource| resource.id.as_str() == format!("settings.input-source:{source}"))?;
    tilo_actions::ActionBinding::unit_for(&resource, action_id).ok()
}

pub fn time_date_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = time_date_section(&AutoCommit::new(), &SystemPrefs::load());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".into(), serde_json::Value::Bool(value))])
    })
    .ok()
}

pub fn time_date_text_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = time_date_section(&AutoCommit::new(), &SystemPrefs::load());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("choice".into(), serde_json::Value::String(value))])
    })
    .ok()
}

fn shortcuts_section(doc: &AutoCommit) -> Resource {
    let b = |key: &str, fallback| read_bool(doc, &ROOT, key).unwrap_or(fallback);
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
    for (key, shortcut_label) in crate::panes::keyboard_shortcuts::resource_entries() {
        // Resource action ids are stable, readable derivatives of their
        // durable key rather than a hand-maintained subset of the recorder
        // table. Dots become dashes to stay within the action-id vocabulary.
        let id = key
            .strip_prefix("shortcuts.")
            .unwrap_or(key)
            .replace('_', "-");
        let label = format!("Set {shortcut_label} shortcut");
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

#[allow(
    clippy::too_many_lines,
    reason = "the declaration keeps the closed device-local trust and Continuity action catalogue auditable in one place"
)]
fn device_mesh_section(_doc: &AutoCommit) -> Resource {
    let mut section = Resource::new(
        pane_resource_id(Category::DeviceMesh),
        "settings.section",
        "Devices & Sync",
    );
    section = section.with_action(device_mesh_choice_spec(
        "role",
        "Set device role on this device",
        &[
            "Personal device",
            "Available while active",
            "Always-on server",
        ],
        |role| DeviceMeshOperation::SetRole { role },
    ));
    section = section.with_action(device_mesh_choice_spec(
        "replication",
        "Set local replication policy",
        &["On demand", "Workspace state", "Full workspace copy"],
        |replication| DeviceMeshOperation::SetReplication { replication },
    ));
    for (id, service, label) in [
        ("inbound-relay", "inbound_relay", "Set inbound relay"),
        (
            "remote-control",
            "remote_control_relay",
            "Set remote-control relay",
        ),
        ("app-serving", "app_serving", "Set app serving"),
        ("compute", "compute", "Set remote compute"),
    ] {
        section = section.with_action(device_mesh_bool_spec(id, label, service));
    }
    for (workspace_id, name) in crate::panes::device_mesh::resource_workspaces()
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let resource_id = format!("settings.device-mesh-workspace:{workspace_id}");
        let action = ActionSpec::typed_operation(
            "set-available",
            "Set availability on this device",
            "On or off on this device",
            OperationKind::Effect,
            DEVICE_MESH_OPERATION_TYPE,
            vec![ActionParam::required(
                "value",
                ParamType::Boolean,
                "Whether this workspace is available on this device",
            )],
            move |args| {
                encode_local_operation(
                    DEVICE_MESH_OPERATION_TYPE,
                    &DeviceMeshOperation::SetWorkspace {
                        id: workspace_id,
                        enabled: args.boolean("value").unwrap_or(false),
                    },
                )
            },
        )
        .with_contract(local_operation_contract(&[
            "device-local",
            "device-mesh",
            "workspace",
        ]));
        section = section.with_child(
            Resource::new(resource_id, "settings.device-mesh-workspace", name).with_action(action),
        );
    }
    for (id, key, label) in [
        (
            "continuity-enabled",
            "device_mesh.continuity.enabled",
            "Continuity on this device",
        ),
        (
            "continuity-clipboard",
            "device_mesh.continuity.clipboard",
            "Continuity clipboard",
        ),
        (
            "continuity-notifications",
            "device_mesh.continuity.notifications",
            "Continuity notifications",
        ),
        (
            "continuity-wifi-settings",
            "device_mesh.continuity.wifi_settings",
            "Continuity Wi-Fi and settings",
        ),
        (
            "continuity-media",
            "device_mesh.continuity.media",
            "Continuity media",
        ),
        (
            "continuity-display-input",
            "device_mesh.continuity.display_input",
            "Continuity display and input",
        ),
        (
            "continuity-file-transfer",
            "device_mesh.continuity.file_transfer",
            "Continuity file transfer",
        ),
        (
            "continuity-realtime-sessions",
            "device_mesh.continuity.realtime_sessions",
            "Continuity realtime sessions",
        ),
        (
            "continuity-typed-gamepad",
            "device_mesh.continuity.typed_gamepad",
            "Continuity typed gamepad forwarding",
        ),
        (
            "continuity-app-continuations",
            "device_mesh.continuity.app_continuations",
            "Continuity app continuations",
        ),
    ] {
        let service = key.strip_prefix("device_mesh.").unwrap_or(key).to_string();
        section = section.with_action(device_mesh_bool_spec(id, label, &service));
    }
    section
}

fn device_mesh_choice_spec(
    id: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    operation: fn(String) -> DeviceMeshOperation,
) -> ActionSpec {
    let spec = ActionSpec::typed_operation(
        id,
        label,
        "Choose a value",
        OperationKind::Effect,
        DEVICE_MESH_OPERATION_TYPE,
        vec![
            ActionParam::required("choice", ParamType::Text, "Device-local value")
                .with_choices(choices.iter().copied()),
        ],
        move |args| {
            encode_local_operation(
                DEVICE_MESH_OPERATION_TYPE,
                &operation(args.text("choice").unwrap_or_default().to_string()),
            )
        },
    )
    .with_contract(local_operation_contract(&[
        "device-local",
        "security",
        "device-mesh",
    ]));
    if id == "role" {
        spec.destructive()
    } else {
        spec
    }
}

fn device_mesh_bool_spec(id: &str, label: &str, service: &str) -> ActionSpec {
    let service = service.to_string();
    ActionSpec::typed_operation(
        id,
        label,
        "On or off on this device",
        OperationKind::Effect,
        DEVICE_MESH_OPERATION_TYPE,
        vec![ActionParam::required(
            "value",
            ParamType::Boolean,
            "Whether this local service is enabled",
        )],
        move |args| {
            encode_local_operation(
                DEVICE_MESH_OPERATION_TYPE,
                &DeviceMeshOperation::SetService {
                    service: service.clone(),
                    enabled: args.boolean("value").unwrap_or(false),
                },
            )
        },
    )
    .with_contract(local_operation_contract(&[
        "device-local",
        "security",
        "device-mesh",
    ]))
}

pub fn device_mesh_bool_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = device_mesh_section(&AutoCommit::new());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

pub fn device_mesh_text_binding(action_id: &str) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = device_mesh_section(&AutoCommit::new());
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
}

pub fn device_mesh_workspace_binding(
    workspace_id: u64,
) -> Option<tilo_actions::ActionBinding<bool>> {
    let resource = device_mesh_section(&AutoCommit::new())
        .children
        .into_iter()
        .find(|resource| {
            resource.id.as_str() == format!("settings.device-mesh-workspace:{workspace_id}")
        })?;
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == "set-available")?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(value))])
    })
    .ok()
}

impl ResourceProvider for crate::SettingsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources_with_accounts(doc, self.account_resource_inventory())
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "settings.root" {
            // Exact root lookup is intentionally shallow. Building every live
            // hardware inventory merely to resolve the root made one address
            // proportional to every device, stream, account, job, and model.
            let root = Category::ALL.iter().copied().fold(
                Resource::new("settings.root", "settings.root", "Settings"),
                |root, category| root.with_child(pane_navigation_resource(category)),
            );
            return Some(root);
        }
        let category = category_for_resource_id(id)?;
        let prefs = SystemPrefs::load();
        find_resource(
            section_resource(category, doc, &prefs, self.account_resource_inventory()),
            id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
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
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
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
            let open = tilo_actions::door::resolve(&resources, &address)
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
    fn exact_root_lookup_is_shallow_and_section_lookup_is_canonical() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let doc = AutoCommit::new();
        let root =
            ResourceProvider::resource(&app, &doc, "settings.root").expect("root resolves exactly");
        assert_eq!(root.children.len(), Category::ALL.len());
        assert!(
            root.children
                .iter()
                .all(|section| section.children.is_empty()),
            "resolving the root must not enumerate live dynamic inventories"
        );

        let appearance =
            ResourceProvider::resource(&app, &doc, &pane_resource_id(Category::Appearance))
                .expect("one section resolves without rebuilding every other section");
        assert_eq!(appearance.label, Category::Appearance.label());
        assert!(appearance.actions.iter().any(|action| action.id == "theme"));
    }

    #[test]
    fn adversarial_account_inventory_is_hard_bounded() {
        let accounts: Vec<_> = (0..100_001)
            .map(|index| accounts::Account::new(format!("account-{index}"), "nextcloud"))
            .collect();
        let section = accounts_section(&accounts);
        assert_eq!(section.children.len(), MAX_DYNAMIC_RESOURCES);
        assert_eq!(
            section.children[0].id.as_str(),
            "settings.account:account-0"
        );
        assert_eq!(
            section.children[MAX_DYNAMIC_RESOURCES - 1].id.as_str(),
            "settings.account:account-255"
        );
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
        assert_eq!(remove.contract.risk, tilo_actions::RiskLevel::Destructive);
        assert_eq!(
            remove.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let test = account
            .actions
            .iter()
            .find(|action| action.id == "test")
            .expect("account test resource action");
        assert_eq!(
            test.contract.execution,
            ExecutionMode::Background { cancellable: false }
        );
        assert!(matches!(test.contract.result, ResultSpec::Json { .. }));
    }

    #[test]
    fn storage_reclamation_requires_host_confirmation() {
        let storage = storage_section();
        for id in ["cleanup-caches", "free-synced-files"] {
            let action = storage
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("{id} action is published"));
            assert_eq!(action.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                action.contract.execution,
                ExecutionMode::Background { cancellable: false }
            );
        }
    }

    /// A bool flip spec's target follows the current document value: on when the
    /// key is off, off when it is on.
    #[test]
    fn bool_flip_target_follows_current_value() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let mut doc = AutoCommit::new();
        let _ = doc.put(
            &ROOT,
            panes::notifications::DND_KEY,
            ScalarValue::Boolean(false),
        );
        let notif = {
            let app = config.app();
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
            "lock-after",
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
            "touchpad-natural-scroll",
            "mouse-natural-scroll",
            "tap-to-click",
            "touchpad-speed",
            "mouse-speed",
            "key-repeat",
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
    fn every_visible_shortcut_has_a_canonical_resource_action() {
        let resources = provide_resources(&AutoCommit::new());
        let shortcuts = find(&resources, &pane_resource_id(Category::KeyboardShortcuts));
        let action_ids: std::collections::HashSet<&str> = shortcuts
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        assert!(
            action_ids.contains("vim-mode"),
            "missing canonical Vim navigation action"
        );

        for (key, label) in crate::panes::keyboard_shortcuts::resource_entries() {
            let id = key
                .strip_prefix("shortcuts.")
                .unwrap_or(key)
                .replace('_', "-");
            assert!(
                action_ids.contains(id.as_str()),
                "missing canonical shortcut action for {label} ({key})"
            );
            let action = shortcuts
                .actions
                .iter()
                .find(|action| action.id == id)
                .expect("action was just found");
            assert_eq!(action.kind, OperationKind::Document);
            assert_eq!(action.locus, ExecutionLocus::ClientLocal);
        }
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
            "automatic",
            "schedule",
            "metered",
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
                        tilo_actions::ConfirmationPolicy::None
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
    fn hardware_network_controls_are_bounded_local_effects() {
        let resources = provide_resources(&AutoCommit::new());
        let network = find(&resources, &pane_resource_id(Category::Network));
        for id in [
            "refresh",
            "run-diagnostics",
            "enable-wired",
            "disable-wired",
            "enable-airplane-mode",
            "disable-airplane-mode",
            "enable-hotspot",
            "disable-hotspot",
            "enable-mobile-broadband",
            "disable-mobile-broadband",
        ] {
            let action = network
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing network operation: {id}"));
            assert_eq!(action.kind, OperationKind::Effect);
            assert_eq!(action.locus, ExecutionLocus::ClientLocal);
            assert_eq!(
                action.operation_type.as_deref(),
                Some(NETWORK_OPERATION_TYPE)
            );
            assert!(action.contract.tags.iter().any(|tag| tag == "device-local"));
        }
    }

    #[test]
    fn bluetooth_discovery_controls_are_bounded_local_effects() {
        let resources = provide_resources(&AutoCommit::new());
        let bluetooth = find(&resources, &pane_resource_id(Category::Bluetooth));
        for id in ["scan", "refresh"] {
            let action = bluetooth
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing Bluetooth operation: {id}"));
            assert_eq!(action.kind, OperationKind::Effect);
            assert_eq!(action.locus, ExecutionLocus::ClientLocal);
            assert_eq!(
                action.operation_type.as_deref(),
                Some(BLUETOOTH_OPERATION_TYPE)
            );
            assert!(action.contract.tags.iter().any(|tag| tag == "device-local"));
        }
    }

    #[test]
    fn display_operation_tokens_are_typed_and_stable() {
        let operation = DisplayOperation::SetMode {
            connector: "card0/HDMI-A-1".into(),
            mode: settings_store::hardware::OutputMode {
                width: 2560,
                height: 1440,
                refresh_mhz: 143_850,
            },
        };
        let payload = serde_json::to_vec(&operation).expect("display operation serializes");
        assert_eq!(
            local_operation_token(DISPLAY_OPERATION_TYPE, &payload),
            Ok(local_operation_token_for(
                DISPLAY_OPERATION_TYPE,
                &operation
            ))
        );
        assert!(local_operation_token(DISPLAY_OPERATION_TYPE, br#"{\"set_mode\":{}}"#).is_err());
    }

    #[test]
    fn input_and_time_controls_cannot_enter_settings_action_replay() {
        let mut doc = AutoCommit::new();
        for (key, value) in [
            (
                panes::mouse_keyboard::TAP_CLICK_KEY,
                SettingValue::Bool(true),
            ),
            (
                panes::mouse_keyboard::TOUCHPAD_POINTER_SPEED_KEY,
                SettingValue::Float(0.8),
            ),
            (
                panes::mouse_keyboard::ACCEL_PROFILE_KEY,
                SettingValue::Text("Flat".into()),
            ),
            (
                panes::mouse_keyboard::LAYOUT_KEY,
                SettingValue::Text("English (US)".into()),
            ),
            (panes::time_date::AUTO_TIME_KEY, SettingValue::Bool(false)),
            (panes::time_date::H24_KEY, SettingValue::Bool(false)),
            (
                panes::time_date::TIMEZONE_KEY,
                SettingValue::Text("Europe/Amsterdam".into()),
            ),
        ] {
            assert!(
                SettingsAction::SetSetting {
                    key: key.into(),
                    value
                }
                .apply(&mut doc)
                .is_err()
            );
        }
        assert!(
            doc.get(&ROOT, panes::mouse_keyboard::TAP_CLICK_KEY)
                .unwrap()
                .is_none()
        );
        assert!(
            doc.get(&ROOT, panes::time_date::TIMEZONE_KEY)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn input_and_time_resources_and_controls_share_local_effect_protocols() {
        let resources = provide_resources(&AutoCommit::new());
        let input = find(&resources, &pane_resource_id(Category::MouseKeyboard));
        let time = find(&resources, &pane_resource_id(Category::TimeDate));
        for (resource, ids, operation_type) in [
            (
                &input,
                &[
                    "tap-to-click",
                    "touchpad-speed",
                    "active-source",
                    "compose-key",
                ][..],
                INPUT_OPERATION_TYPE,
            ),
            (
                &time,
                &["automatic-time", "24-hour", "timezone"][..],
                TIME_DATE_OPERATION_TYPE,
            ),
        ] {
            for id in ids {
                let action = resource
                    .actions
                    .iter()
                    .find(|action| action.id == *id)
                    .unwrap_or_else(|| panic!("missing local action {id}"));
                assert_eq!(action.kind, OperationKind::Effect);
                assert_eq!(action.operation_type.as_deref(), Some(operation_type));
                assert!(action.contract.tags.iter().any(|tag| tag == "device-local"));
            }
        }
        let input = input_bool_binding("tap-to-click").expect("visible toggle binding");
        let invocation = input.invoke(true).expect("boolean input is valid");
        assert_eq!(
            invocation.address,
            ActionAddress::new(pane_resource_id(Category::MouseKeyboard), "tap-to-click")
        );
        let time = time_date_bool_binding("automatic-time").expect("visible time binding");
        assert_eq!(
            time.invoke(false).unwrap().arguments.get("value"),
            Some(&serde_json::Value::Bool(false))
        );
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
                                    | INPUT_OPERATION_TYPE
                                    | TIME_DATE_OPERATION_TYPE
                                    | POWER_OPERATION_TYPE
                                    | NETWORK_OPERATION_TYPE
                                    | BLUETOOTH_OPERATION_TYPE
                                    | STORAGE_OPERATION_TYPE
                                    | AUDIO_OPERATION_TYPE
                                    | DISPLAY_OPERATION_TYPE
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
                    .validate(&tilo_actions::ActionOutput {
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
            read_bool(&doc, &ROOT, panes::accessibility::HIGH_CONTRAST_KEY),
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
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            read_bool(&state.doc, &ROOT, panes::notifications::DND_KEY),
            Some(true),
            "the action set DND on"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            read_bool(&state.doc, &ROOT, panes::notifications::DND_KEY),
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
        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            read_str_or_text(&a, &ROOT, panes::appearance::THEME_KEY).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            read_str_or_text(&b, &ROOT, panes::appearance::THEME_KEY),
            read_str_or_text(&a, &ROOT, panes::appearance::THEME_KEY)
        );
    }

    #[test]
    fn stale_or_presentation_targets_are_refused() {
        let mut doc = AutoCommit::new();
        for action in [
            set_bool_action("appearance.high_contrast.edit", true),
            set_text_action("settings.select.appearance", "true"),
            set_text_action("shortcuts.no-longer-supported", "Super+Shift+K"),
            set_text_action("unknown.preference", "value"),
        ] {
            assert!(matches!(
                action.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
        }
    }

    #[test]
    fn continuity_actions_are_typed_device_local_effects() {
        let resources = provide_resources(&AutoCommit::new());
        let devices = find(&resources, &pane_resource_id(Category::DeviceMesh));
        let targets = [
            ("continuity-enabled", "continuity.enabled"),
            ("continuity-clipboard", "continuity.clipboard"),
            ("continuity-notifications", "continuity.notifications"),
            ("continuity-wifi-settings", "continuity.wifi_settings"),
            ("continuity-media", "continuity.media"),
            ("continuity-display-input", "continuity.display_input"),
            ("continuity-file-transfer", "continuity.file_transfer"),
            (
                "continuity-realtime-sessions",
                "continuity.realtime_sessions",
            ),
            ("continuity-typed-gamepad", "continuity.typed_gamepad"),
            (
                "continuity-app-continuations",
                "continuity.app_continuations",
            ),
        ];

        for (id, service) in targets {
            let spec = devices
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing Continuity action {id}"));
            assert_eq!(spec.kind, OperationKind::Effect);
            assert_eq!(
                spec.operation_type.as_deref(),
                Some(DEVICE_MESH_OPERATION_TYPE)
            );
            let args =
                serde_json::Map::from_iter([("value".to_string(), serde_json::Value::Bool(true))]);
            let encoded = spec.build_json(&args).expect("typed boolean value");
            let decoded: DeviceMeshOperation =
                serde_json::from_slice(&encoded.payload).expect("device-local payload");
            assert_eq!(
                decoded,
                DeviceMeshOperation::SetService {
                    service: service.into(),
                    enabled: true,
                }
            );
        }

        let mut doc = AutoCommit::new();
        assert!(
            set_text_action("device_mesh.role", "Always-on server")
                .apply(&mut doc)
                .is_err(),
            "device trust policy must not enter the replayable document action namespace"
        );
        let role = devices
            .actions
            .iter()
            .find(|action| action.id == "role")
            .unwrap();
        assert_eq!(role.contract.risk, tilo_actions::RiskLevel::Destructive);
    }

    #[test]
    fn agent_provider_configuration_is_a_typed_immediate_local_effect() {
        let resources = provide_resources(&AutoCommit::new());
        let models = find(&resources, &pane_resource_id(Category::AgentModels));
        let add = models
            .actions
            .iter()
            .find(|action| action.id == "add-openai-compatible")
            .expect("provider creation is agent-addressable");
        assert_eq!(add.kind, OperationKind::Effect);
        assert_eq!(add.locus, ExecutionLocus::ClientLocal);
        assert_eq!(
            add.operation_type.as_deref(),
            Some(AGENT_PROVIDER_OPERATION_TYPE)
        );
        assert!(matches!(add.contract.execution, ExecutionMode::Immediate));
        assert!(matches!(add.contract.result, ResultSpec::None));
        assert!(add.contract.tags.iter().any(|tag| tag == "device-local"));
    }

    #[test]
    fn per_device_natural_scroll_actions_are_refused_from_replay() {
        let mut a = AutoCommit::new();
        a.put(
            &ROOT,
            panes::mouse_keyboard::TOUCHPAD_NATURAL_SCROLL_KEY,
            ScalarValue::Boolean(false),
        )
        .expect("seed touchpad preference");
        a.commit();
        let action = set_bool_action(panes::mouse_keyboard::TOUCHPAD_NATURAL_SCROLL_KEY, true);
        assert!(action.apply(&mut a).is_err());

        assert_eq!(
            read_bool(
                &a,
                &ROOT,
                panes::mouse_keyboard::TOUCHPAD_NATURAL_SCROLL_KEY
            ),
            Some(false),
            "the prior document value is not overwritten by a local hardware action"
        );
    }

    /// Writing the value a key already holds is a no-op: the document's heads are
    /// unchanged (so a replayed / stale envelope can't churn a peer).
    #[test]
    fn setting_to_current_value_is_a_no_op() {
        let mut doc = AutoCommit::new();
        let _ = doc.put(
            &ROOT,
            panes::notifications::DND_KEY,
            ScalarValue::Boolean(true),
        );
        doc.commit();
        let before = doc.get_heads();
        let action = set_bool_action(panes::notifications::DND_KEY, true);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
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

    /// Display scale is a device-local typed effect rather than a replicated
    /// document write. The operation still carries the exact picker label so
    /// agents and the visible control share one validated contract.
    #[test]
    fn display_scale_resource_uses_the_dropdowns_durable_label() {
        let resources = provide_resources(&AutoCommit::new());
        let displays = find(&resources, &pane_resource_id(Category::Displays));
        let scale = displays
            .actions
            .iter()
            .find(|action| action.id == "scale")
            .expect("Displays exports a canonical scale action");
        let args = serde_json::Map::from_iter([(
            "choice".to_owned(),
            serde_json::Value::String("150%".to_owned()),
        )]);
        let encoded = scale
            .build_json(&args)
            .expect("the visible display scale is accepted");
        assert_eq!(encoded.type_id, DISPLAY_OPERATION_TYPE);
        assert_eq!(
            serde_json::from_slice::<DisplayOperation>(&encoded.payload)
                .expect("display operation decodes"),
            DisplayOperation::SetScale {
                label: "150%".into()
            }
        );
        let mut doc = AutoCommit::new();
        assert!(matches!(
            set_text_action(panes::display::DISPLAY_SCALE_KEY, "300%").apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
    }

    #[test]
    fn default_app_handlers_are_bounded_canonical_child_resources() {
        use desktop_policy::{AppCandidate, ResourceKind};

        let pdf = ResourceKind::extension("pdf").expect("valid extension");
        let browser = AppCandidate {
            package_id: "org.tilo.Reader".into(),
            display_name: "Reader".into(),
            resources: [pdf.clone()].into(),
        };
        let alternate = AppCandidate {
            package_id: "org.tilo.Archive".into(),
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
            serde_json::Value::String("Reader — org.tilo.Reader".to_owned()),
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
            read_str_or_text(&doc, &ROOT, "default_apps.ext.pdf").as_deref(),
            Some("Reader — org.tilo.Reader")
        );
        let invalid = serde_json::Map::from_iter([(
            "choice".to_owned(),
            serde_json::Value::String("Unknown — org.tilo.Unknown".to_owned()),
        )]);
        assert!(
            action.build_json(&invalid).is_err(),
            "a stale or undiscovered handler is refused before dispatch"
        );
    }

    #[test]
    fn every_published_action_has_an_explicit_collaboration_scope() {
        fn visit(resource: &Resource) {
            for action in &resource.actions {
                let scope = classify_action(action).unwrap_or_else(|| {
                    panic!(
                        "{:?}#{} has no explicit collaboration scope",
                        resource.id, action.id
                    )
                });
                match scope {
                    SettingScope::PortableDocument => {
                        assert_eq!(action.kind, OperationKind::Document);
                        assert_eq!(action.locus, ExecutionLocus::ClientLocal);
                    }
                    SettingScope::DeviceLocalEffect => {
                        assert_eq!(action.kind, OperationKind::Effect);
                        assert!(
                            action.contract.tags.iter().any(|tag| tag == "device-local"),
                            "{:?}#{} must state its device-local policy",
                            resource.id,
                            action.id
                        );
                    }
                    SettingScope::Presentation => {
                        assert_eq!(action.kind, OperationKind::View);
                    }
                }
            }
            for child in &resource.children {
                visit(child);
            }
        }

        for resource in &provide_resources(&AutoCommit::new()) {
            visit(resource);
        }
    }

    #[test]
    fn hardware_preference_keys_are_never_replayable_document_targets() {
        let local_keys = [
            "network.wifi_enabled",
            "bluetooth.enabled",
            "audio.volume",
            "audio.muted",
            "audio.input_volume",
            "audio.balance",
            "audio.mono",
            "display.brightness",
            "display.auto_brightness",
            "display.scale",
            "power.profile",
            "power.auto_suspend",
            "power.blank",
            "power.suspend",
            "power.lid",
            "power.power_key",
            "power.critical",
            "updates.auto",
            "time.auto",
            "time.timezone",
            "time.h24",
            "input.touchpad_natural_scroll",
            "input.mouse_natural_scroll",
            "input.tap_to_click",
            "input.touchpad_pointer_speed",
            "input.mouse_pointer_speed",
            "input.key_repeat",
            "input.accel_profile",
            "input.compose_key",
            "input.keyboard_layout",
        ];
        for key in local_keys {
            for value in [
                SettingValue::Bool(true),
                SettingValue::Float(0.5),
                SettingValue::Text("value".into()),
            ] {
                assert!(
                    matches!(
                        SettingsAction::SetSetting {
                            key: key.into(),
                            value,
                        }
                        .apply(&mut AutoCommit::new()),
                        Err(ActionError::Refused(_))
                    ),
                    "device-local key {key} accepted a replayable SettingsAction"
                );
            }
        }
    }
}
