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
    ResultSpec, RetryPolicy,
};

use crate::categories::Category;
use crate::prefs::SystemPrefs;

mod portable;
pub use portable::{PORTABLE_PREFERENCES, PortablePrefType, PortablePreference};

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
/// Speaker groups across the person's devices. The compositor owns groups,
/// consent and calibration; Settings only asks, so these never replicate.
pub const SPEAKER_OPERATION_TYPE: &str = "settings.speaker-operation";

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
    SPEAKER_OPERATION_TYPE,
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
    InstallCodecs,
    RemoveCodecs,
    RefreshCodecs,
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
    SetTouchpadNaturalScroll {
        enabled: bool,
    },
    SetMouseNaturalScroll {
        enabled: bool,
    },
    SetTapToClick {
        enabled: bool,
    },
    SetTouchpadPointerSpeed {
        value: f64,
    },
    SetMousePointerSpeed {
        value: f64,
    },
    SetKeyRepeat {
        value: f64,
    },
    SetTapDrag {
        enabled: bool,
    },
    SetTapDragLock {
        enabled: bool,
    },
    SetDisableWhileTyping {
        enabled: bool,
    },
    SetMiddleEmulation {
        enabled: bool,
    },
    SetLeftHanded {
        enabled: bool,
    },
    SetTouchpadGestures {
        enabled: bool,
    },
    SetTouchpadEnabled {
        enabled: bool,
    },
    SetTouchpadDisabledOnExternalMouse {
        enabled: bool,
    },
    SetTouchpadClickMethod {
        method: String,
    },
    SetTouchpadScrollMethod {
        method: String,
    },
    SetScrollSpeed {
        value: f64,
    },
    SetAccelerationProfile {
        profile: String,
    },
    SetComposeKey {
        key: String,
    },
    SetCapsLock {
        behavior: String,
    },
    SetActiveSource {
        label: String,
    },
    /// Map the tablet `device` onto a monitor, or `whole desktop`.
    SetTabletOutput {
        device: String,
        output: String,
    },
    SetTabletKeepAspect {
        device: String,
        enabled: bool,
    },
    SetTabletLeftHanded {
        device: String,
        enabled: bool,
    },
    /// Soft (`0.0`) … firm (`1.0`) pen pressure.
    SetTabletTipFeel {
        device: String,
        value: f64,
    },
    /// `button` is `lower`, `upper` or `third`; `action` a stylus button
    /// choice id.
    SetTabletButton {
        device: String,
        button: String,
        action: String,
    },
    /// `action` is an eraser choice id.
    SetTabletEraser {
        device: String,
        action: String,
    },
    /// `action` is a pad button choice id.
    SetPadButton {
        device: String,
        button: u32,
        action: String,
    },
    /// What pad `device`'s ring or strip (`dial` is `ring` or `strip`)
    /// number `index` does in `mode`; `action` is a dial choice id.
    SetPadDial {
        device: String,
        dial: String,
        index: u32,
        mode: u32,
        action: String,
    },
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
    SetEchoCancellation {
        enabled: bool,
    },
    SetSystemSounds {
        enabled: bool,
    },
    SetSystemSoundVolume {
        value: f64,
    },
    /// A sound theme id from the installed themes.
    SetSoundTheme {
        theme: String,
    },
    /// Whether one system event makes a sound; `event` is its settings key.
    SetEventSound {
        event: String,
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
    SetNightLightSchedule {
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
    /// One monitor's brightness, `0.0..=1.0`: the external monitor on
    /// `connector`, over DDC/CI.
    SetOutputBrightness {
        connector: String,
        value: f64,
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
    /// How bright SDR content is while `connector` runs in HDR, as a
    /// fraction `0.0..=1.0` of the offered range.
    SetSdrBrightness {
        connector: String,
        value: f64,
    },
    SetVrr {
        connector: String,
        enabled: bool,
    },
    SetColorProfile {
        connector: String,
        profile: Option<String>,
    },
    /// Place an output's top-left corner at an exact point of the shared
    /// desktop, as dragging it in the arrangement canvas does.
    MoveOutput {
        connector: String,
        x: i32,
        y: i32,
    },
    /// Paint every display's number and name on it for a few seconds.
    Identify,
    Apply,
    Confirm,
    Revert,
    /// Ask again before keeping display changes from this layout tool.
    ForgetDisplayTool {
        executable: String,
    },
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

/// Typed speaker commands. Agents and the Speakers pane share this door;
/// flow steps (which screen is showing) are presentation and are not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeakerOperation {
    Create {
        name: String,
        members: Vec<ui::collaboration::SpeakerMemberSpec>,
    },
    Identify {
        device: String,
        output: String,
    },
    /// Measure from the microphone of `listening` (where the person sits),
    /// with every other offered microphone except those of `excluded`.
    Calibrate {
        group: String,
        excluded: Vec<String>,
        listening: Option<String>,
    },
    CancelCalibration {
        group: String,
    },
    ApplyCalibration {
        group: String,
    },
    RevertCalibration {
        group: String,
    },
    DismissCalibration {
        group: String,
    },
    Delete {
        group: String,
    },
}

impl SpeakerOperation {
    /// The shell command this operation asks for.
    #[must_use]
    pub fn command(&self) -> ui::collaboration::SpeakerCommand {
        use ui::collaboration::SpeakerCommand as Command;
        match self.clone() {
            Self::Create { name, members } => Command::Create { name, members },
            Self::Identify { device, output } => Command::Identify { device, output },
            Self::Calibrate {
                group,
                excluded,
                listening,
            } => Command::StartCalibration {
                group,
                excluded,
                listening,
            },
            Self::CancelCalibration { group } => Command::CancelCalibration { group },
            Self::ApplyCalibration { group } => Command::ApplyCalibration { group },
            Self::RevertCalibration { group } => Command::RevertCalibration { group },
            Self::DismissCalibration { group } => Command::DismissCalibration { group },
            Self::Delete { group } => Command::Delete { group },
        }
    }
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
        SPEAKER_OPERATION_TYPE => {
            let operation: SpeakerOperation =
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
    pub(crate) fn scalar(&self) -> ScalarValue {
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
        InputOperation::SetTapDragLock { .. } => InputOperation::SetTapDragLock { enabled },
        InputOperation::SetDisableWhileTyping { .. } => {
            InputOperation::SetDisableWhileTyping { enabled }
        }
        InputOperation::SetMiddleEmulation { .. } => InputOperation::SetMiddleEmulation { enabled },
        InputOperation::SetLeftHanded { .. } => InputOperation::SetLeftHanded { enabled },
        InputOperation::SetTouchpadGestures { .. } => {
            InputOperation::SetTouchpadGestures { enabled }
        }
        InputOperation::SetTouchpadEnabled { .. } => InputOperation::SetTouchpadEnabled { enabled },
        InputOperation::SetTouchpadDisabledOnExternalMouse { .. } => {
            InputOperation::SetTouchpadDisabledOnExternalMouse { enabled }
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
        InputOperation::SetScrollSpeed { .. } => InputOperation::SetScrollSpeed { value },
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
    let allowed = if let Some(pref_type) = portable::portable_pref_type(key) {
        match pref_type {
            PortablePrefType::Bool => boolean(),
            PortablePrefType::Text => text(),
        }
    } else if key.starts_with("default_apps.") && !presentation_key {
        text()
    } else if valid_notification_app_key {
        true
    } else {
        // Trust, service and residency policy is deliberately absent: those
        // settings use typed device-local effect operations and must never
        // enter the replayable SettingsAction namespace.
        // Keyboard shortcut chords are likewise absent: the shell's
        // `keyboard-shortcuts` resource is their one write path.
        false
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

pub(super) fn bool_flip_spec(id: &str, key: &str, on_now: bool, noun: &str) -> ActionSpec {
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
pub(super) fn bool_set_spec(id: &str, label: &str, key: &'static str) -> ActionSpec {
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

/// Largest desktop coordinate the arrangement accepts; far beyond any real
/// multi-display desk, small enough that every sum stays in range.
pub const MAX_DISPLAY_COORDINATE: i32 = 65_536;

fn local_point_spec(
    id: impl Into<String>,
    label: impl Into<String>,
    type_id: &'static str,
    tags: &'static [&'static str],
    build: impl Fn(i32, i32) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    let bound = i64::from(MAX_DISPLAY_COORDINATE);
    let coordinate = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Integer, description)
            .with_schema(serde_json::json!({ "minimum": -bound, "maximum": bound }))
    };
    ActionSpec::typed_operation(
        id,
        label,
        "A point on the shared desktop, in desktop pixels",
        OperationKind::Effect,
        type_id,
        vec![coordinate("x", "Left edge"), coordinate("y", "Top edge")],
        move |args| {
            let axis = |name: &str| {
                i32::try_from(args.integer(name).unwrap_or(0).clamp(-bound, bound))
                    .unwrap_or_default()
            };
            build(axis("x"), axis("y"))
        },
    )
    .with_contract(local_operation_contract(tags))
}

fn encode_local_operation<T: Serialize>(type_id: &str, operation: &T) -> EncodedAction {
    EncodedAction::encode_operation(type_id, operation).unwrap_or_else(|_| EncodedAction {
        type_id: type_id.into(),
        payload: Vec::new(),
    })
}

pub(super) fn choice_prompt_spec(
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

/// Categories whose preference door is owned by [`tilo_resource::publish`]:
/// portable [`SettingsAction`] rows (and open-only About / Permissions). Live
/// device inventories and hardware effects stay on the hosted widget.
pub(crate) fn is_document_door_category(category: Category) -> bool {
    matches!(
        category,
        Category::Appearance
            | Category::Accessibility
            | Category::Notifications
            | Category::RegionFormats
            | Category::KeyboardShortcuts
            | Category::About
            | Category::Permissions
    )
}

/// Section children for [`tilo_resource::publish`]: `SettingsAction` preference
/// panes plus open-only navigation. Per-app notification rows, default-app
/// handlers, accounts, and hardware sections are not shared document state.
pub(crate) fn document_children(state: &crate::resource::SettingsState) -> Vec<Resource> {
    let doc = state.to_doc();
    // Flip targets fall back to this machine's durable defaults when the CRDT
    // has not seeded a key yet — same policy the full tree used before.
    let prefs = SystemPrefs::load();
    Category::ALL
        .iter()
        .copied()
        .filter(|category| is_document_door_category(*category))
        .map(|category| document_section_resource(category, &doc, &prefs))
        .collect()
}

#[allow(
    clippy::unreachable,
    reason = "caller filters to document-door categories only"
)]
fn document_section_resource(
    category: Category,
    doc: &AutoCommit,
    prefs: &SystemPrefs,
) -> Resource {
    let mut pane = match category {
        Category::Appearance => appearance_section(doc, prefs),
        Category::Accessibility => accessibility_section(doc, prefs),
        Category::Notifications => notifications_portable_section(doc, prefs),
        Category::RegionFormats => region_section(doc, prefs),
        Category::KeyboardShortcuts => shortcuts_section(doc),
        Category::About | Category::Permissions => Resource::new(
            pane_resource_id(category),
            "settings.section",
            category.label(),
        ),
        other => {
            unreachable!(
                "document_section_resource only builds document-door categories, got {other:?}"
            )
        }
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

/// Hosted-only extras that are not on the framework resource: hardware and
/// service sections, account inventory, default-app handlers, and per-app
/// notification rows from local durable prefs.
pub(crate) fn live_device_resources(
    doc: &AutoCommit,
    account_inventory: &[accounts::Account],
) -> Vec<Resource> {
    let prefs = SystemPrefs::load();
    let mut extras: Vec<Resource> = Category::ALL
        .iter()
        .copied()
        .filter(|category| !is_document_door_category(*category))
        .map(|category| section_resource(category, doc, &prefs, account_inventory))
        .collect();
    extras.extend(notification_app_resources(doc, &prefs));
    extras
}

/// Build the resource tree Settings publishes: the configuration root, a child
/// per section, each exporting the settings it owns as the same actions its
/// on-screen controls dispatch. Current values are read from `doc` (falling
/// back to the shipped defaults for a section whose pane hasn't seeded its keys
/// yet), so a bool's flip target and an enum's fallback reflect what is set.
///
/// Standalone / `--shots` / unit tests: [`tilo_resource::publish`] owns the
/// `SettingsAction` door; device-local sections and live inventories are merged
/// into the same root so the palette shape stays one tree.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    provide_resources_with_accounts(doc, &[])
}

pub fn provide_resources_with_accounts(
    doc: &AutoCommit,
    account_inventory: &[accounts::Account],
) -> Vec<Resource> {
    let state = crate::resource::SettingsState::from_doc(doc);
    let mut root = ui::tilo_resource::publish::<crate::resource::Settings>(
        "settings.root",
        "Settings",
        &state,
    );
    let prefs = SystemPrefs::load();
    if let Some(section) = root
        .children
        .iter_mut()
        .find(|child| child.id.as_str() == pane_resource_id(Category::Notifications))
    {
        // Portable notification preferences stay the published children.
        // Per-app rows are this machine's inventory beside them.
        section
            .children
            .extend(notification_app_resources(doc, &prefs));
    }
    let mut published: std::collections::HashMap<String, Resource> = root
        .children
        .drain(..)
        .map(|child| (child.id.as_str().to_owned(), child))
        .collect();
    for category in Category::ALL.iter().copied() {
        let id = pane_resource_id(category);
        if let Some(section) = published.remove(&id) {
            root = root.with_child(section);
        } else {
            root = root.with_child(section_resource(category, doc, &prefs, account_inventory));
        }
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
        Category::Speakers => speakers_section(),
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
        ("settings.speaker-group:", Category::Speakers),
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
        ("google-drive", "Connect Google Drive"),
        ("onedrive", "Connect OneDrive"),
        ("dropbox", "Connect Dropbox"),
        ("icloud-drive", "Connect iCloud Drive"),
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

fn notifications_portable_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    let sec = Resource::new(
        "settings.section:notifications",
        "settings.section",
        "Notifications",
    );
    portable::attach_portable_preferences(sec, Category::Notifications, doc, d)
}

/// Per-app notification rows from local durable prefs — not shared document
/// state, so hosted surfaces publish them beside the framework resource.
fn notification_app_resources(doc: &AutoCommit, d: &SystemPrefs) -> Vec<Resource> {
    let b = |key: &str, default: bool| read_bool(doc, &ROOT, key).unwrap_or(default);
    crate::panes::notifications::resource_notification_apps(&d.notifications)
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
        .map(|(app_id, app_name)| {
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
            resource.with_action(choice_prompt_spec(
                "mute",
                "Mute notifications temporarily",
                key,
                "Not muted, For 1 hour, For 8 hours, For 24 hours",
                &["Not muted", "For 1 hour", "For 8 hours", "For 24 hours"],
                read_str_or_text(doc, &ROOT, format!("notifications.app.{app_id}.mute"))
                    .unwrap_or_else(|| "Not muted".into()),
            ))
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered resource declaration mirrors the Notifications hierarchy"
)]
fn notifications_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    let mut sec = notifications_portable_section(doc, d);
    for resource in notification_app_resources(doc, d) {
        sec = sec.with_child(resource);
    }
    sec
}

fn appearance_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    let sec = Resource::new(
        "settings.section:appearance",
        "settings.section",
        "Appearance",
    );
    portable::attach_portable_preferences(sec, Category::Appearance, doc, d)
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
    sec = sec.with_action(echo_cancellation_spec());
    sec = sec.with_action(system_sounds_spec());
    sec = sec.with_action(system_sound_volume_spec());
    for event in settings_store::system_sounds::SystemSoundEvent::ALL {
        sec = sec.with_action(event_sound_spec(event));
    }
    let themes = media::event_sound::installed_themes();
    if let Some(action) = sound_theme_spec(
        themes
            .into_iter()
            .map(|theme| (theme.name, theme.id))
            .collect(),
    ) {
        sec = sec.with_action(action);
    }
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

fn echo_cancellation_spec() -> ActionSpec {
    local_bool_spec(
        "echo-cancellation",
        "Reduce echo in calls",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "session"],
        |enabled| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetEchoCancellation { enabled },
            )
        },
    )
}

fn system_sounds_spec() -> ActionSpec {
    local_bool_spec(
        "system-sounds",
        "Turn system sounds on or off",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "sounds"],
        |enabled| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetSystemSounds { enabled },
            )
        },
    )
}

fn system_sound_volume_spec() -> ActionSpec {
    local_number_spec(
        "system-sound-volume",
        "Set system sound volume",
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "sounds"],
        |value| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetSystemSoundVolume { value },
            )
        },
    )
}

/// Action id of one event's sound toggle, e.g. `event-sound-low-battery`.
#[must_use]
pub fn event_sound_action_id(event: settings_store::system_sounds::SystemSoundEvent) -> String {
    format!("event-sound-{}", event.key().replace('_', "-"))
}

fn event_sound_spec(event: settings_store::system_sounds::SystemSoundEvent) -> ActionSpec {
    local_bool_spec(
        event_sound_action_id(event),
        format!("Play a sound for {}", event.label().to_lowercase()),
        AUDIO_OPERATION_TYPE,
        &["device-local", "audio", "sounds"],
        move |enabled| {
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetEventSound {
                    event: event.key().to_string(),
                    enabled,
                },
            )
        },
    )
}

/// `themes` is `(display name, theme id)`.
fn sound_theme_spec(themes: Vec<(String, String)>) -> Option<ActionSpec> {
    if themes.is_empty() || themes.len() > MAX_DYNAMIC_CHOICES {
        return None;
    }
    let choices = themes.iter().map(|(name, _)| name.clone()).collect();
    Some(local_choice_spec(
        "sound-theme",
        "Set sound theme",
        AUDIO_OPERATION_TYPE,
        choices,
        &["device-local", "audio", "sounds"],
        move |choice| {
            let theme = themes
                .iter()
                .find(|(name, _)| name == &choice)
                .map(|(_, id)| id.clone())
                .unwrap_or_default();
            encode_local_operation(
                AUDIO_OPERATION_TYPE,
                &AudioOperation::SetSoundTheme { theme },
            )
        },
    ))
}

/// The sound-theme picker's binding; `themes` is `(display name, theme id)`.
pub fn audio_sound_theme_binding(
    themes: Vec<(String, String)>,
) -> Option<tilo_actions::ActionBinding<String>> {
    let resource = audio_control_resource(sound_theme_spec(themes)?);
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |choice| {
        serde_json::Map::from_iter([("choice".to_string(), serde_json::Value::String(choice))])
    })
    .ok()
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
        "echo-cancellation" => echo_cancellation_spec(),
        "system-sounds" => system_sounds_spec(),
        other => settings_store::system_sounds::SystemSoundEvent::ALL
            .into_iter()
            .find(|event| event_sound_action_id(*event) == other)
            .map(event_sound_spec)?,
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
        "system-sound-volume" => system_sound_volume_spec(),
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
pub(crate) fn displays_section(_doc: &AutoCommit, _d: &SystemPrefs) -> Resource {
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
    sec = sec.with_action(local_bool_spec(
        "night_light_schedule",
        "Set Night Light schedule",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        |enabled| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetNightLightSchedule { enabled },
            )
        },
    ));
    sec = sec.with_action(local_choice_spec(
        "night_light_temperature",
        "Set Night Light warmth",
        DISPLAY_OPERATION_TYPE,
        vec!["Subtle (4500 K)", "Warm (3700 K)", "Very warm (2800 K)"]
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
            resource = resource.with_action(output_brightness_spec(connector.clone()));
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
            let operation_connector = connector.clone();
            resource = resource.with_action(local_point_spec(
                "move",
                "Move display",
                DISPLAY_OPERATION_TYPE,
                &["device-local", "display", "live-output"],
                move |x, y| {
                    encode_local_operation(
                        DISPLAY_OPERATION_TYPE,
                        &DisplayOperation::MoveOutput {
                            connector: operation_connector.clone(),
                            x,
                            y,
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
                resource = resource.with_action(sdr_brightness_spec(connector.clone()));
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
                            .map(|name| format!("{name} ({id})"))
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
        (
            "identify",
            "Show each display's number on it",
            DisplayOperation::Identify,
        ),
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
    for tool in settings_store::display_tools::TrustedDisplayTools::load()
        .tools
        .iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        sec = sec.with_child(display_tool_resource(tool));
    }
    sec
}

/// The resource for one trusted display tool, carrying its `forget` action.
fn display_tool_resource(tool: &settings_store::display_tools::DisplayTool) -> Resource {
    let resource = Resource::new(
        format!("settings.display-tool:{}", tool.executable),
        "settings.display-tool",
        tool.name().to_string(),
    );
    match local_operation_spec(
        "forget",
        "Ask before keeping this tool's display changes",
        OperationKind::Effect,
        DISPLAY_OPERATION_TYPE,
        &DisplayOperation::ForgetDisplayTool {
            executable: tool.executable.clone(),
        },
        &["device-local", "display", "trusted-tool"],
    ) {
        Some(action) => resource.with_action(action),
        None => resource,
    }
}

/// Binding for a trusted tool's "Ask again" button: the same `forget`
/// action an agent or script invokes.
pub fn display_tool_forget_binding(
    tool: &settings_store::display_tools::DisplayTool,
) -> Option<tilo_actions::ActionBinding<()>> {
    tilo_actions::ActionBinding::unit_for(&display_tool_resource(tool), "forget").ok()
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
        "night_light_schedule" => local_bool_spec(
            "night_light_schedule",
            "Set Night Light schedule",
            DISPLAY_OPERATION_TYPE,
            &["device-local", "display", "hardware"],
            |enabled| {
                encode_local_operation(
                    DISPLAY_OPERATION_TYPE,
                    &DisplayOperation::SetNightLightSchedule { enabled },
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
            vec!["Subtle (4500 K)", "Warm (3700 K)", "Very warm (2800 K)"]
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

/// Binding for the arrangement canvas: a drop becomes the output's `move`
/// action, the same one an agent or script invokes.
pub fn display_output_move_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<(i32, i32)>> {
    let operation_connector = connector.clone();
    let action = local_point_spec(
        "move",
        "Move display",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "live-output"],
        move |x, y| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::MoveOutput {
                    connector: operation_connector.clone(),
                    x,
                    y,
                },
            )
        },
    );
    let resource = display_output_control_resource(&connector, action.clone());
    tilo_actions::ActionBinding::new(&resource, &action, |(x, y): (i32, i32)| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("x".into(), serde_json::Value::from(x));
        arguments.insert("y".into(), serde_json::Value::from(y));
        arguments
    })
    .ok()
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

fn output_brightness_spec(connector: String) -> ActionSpec {
    local_number_spec(
        "brightness",
        "Set monitor brightness",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "hardware"],
        move |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetOutputBrightness {
                    connector: connector.clone(),
                    value,
                },
            )
        },
    )
}

fn sdr_brightness_spec(connector: String) -> ActionSpec {
    local_number_spec(
        "sdr-brightness",
        "Set SDR content brightness in HDR",
        DISPLAY_OPERATION_TYPE,
        &["device-local", "display", "live-output"],
        move |value| {
            encode_local_operation(
                DISPLAY_OPERATION_TYPE,
                &DisplayOperation::SetSdrBrightness {
                    connector: connector.clone(),
                    value,
                },
            )
        },
    )
}

/// The SDR content brightness slider of an HDR-capable monitor's card.
pub fn display_output_sdr_brightness_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<f64>> {
    let resource =
        display_output_control_resource(&connector, sdr_brightness_spec(connector.clone()));
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
}

/// The brightness slider of one monitor's card.
pub fn display_output_brightness_binding(
    connector: String,
) -> Option<tilo_actions::ActionBinding<f64>> {
    let resource =
        display_output_control_resource(&connector, output_brightness_spec(connector.clone()));
    let spec = resource.actions.first()?;
    tilo_actions::ActionBinding::new(&resource, spec, |value| {
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(value))])
    })
    .ok()
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
    let mut sec = Resource::new(
        pane_resource_id(Category::MouseKeyboard),
        "settings.section",
        "Mouse & Keyboard",
    );
    for (id, label, operation) in [
        (
            "touchpad-enabled",
            "Turn the touchpad on or off",
            InputOperation::SetTouchpadEnabled { enabled: false },
        ),
        (
            "touchpad-disabled-with-mouse",
            "Set disable touchpad while a mouse is connected",
            InputOperation::SetTouchpadDisabledOnExternalMouse { enabled: false },
        ),
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
            "tap-drag-lock",
            "Set tap-and-drag lock",
            InputOperation::SetTapDragLock { enabled: false },
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
        (
            "scroll-speed",
            "Set scroll speed",
            InputOperation::SetScrollSpeed { value: 0.0 },
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
        "touchpad-click-method",
        "Set touchpad click method",
        &crate::panes::mouse_keyboard::CLICK_METHOD_CHOICES
            .iter()
            .map(|(id, _, _)| (*id).to_string())
            .collect::<Vec<_>>(),
        |choice| InputOperation::SetTouchpadClickMethod { method: choice },
    ));
    sec = sec.with_action(input_choice_spec(
        "touchpad-scroll-method",
        "Set touchpad scroll method",
        &crate::panes::mouse_keyboard::SCROLL_METHOD_CHOICES
            .iter()
            .map(|(id, _, _)| (*id).to_string())
            .collect::<Vec<_>>(),
        |choice| InputOperation::SetTouchpadScrollMethod { method: choice },
    ));
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
    sec = sec.with_action(input_choice_spec(
        "caps-lock",
        "Set what Caps Lock does",
        &crate::panes::mouse_keyboard::CAPS_LOCK_CHOICES
            .iter()
            .map(|(label, _)| (*label).to_string())
            .collect::<Vec<_>>(),
        |choice| InputOperation::SetCapsLock { behavior: choice },
    ));
    // Text assistance is portable user text preference rather than a hardware
    // command, and therefore intentionally remains in SettingsAction.
    sec = portable::attach_portable_preferences(sec, Category::MouseKeyboard, doc, d);
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
        .filter(|entry| {
            !sources.contains(&entry.id) && crate::input_sources::has_room_for(&sources, &entry.id)
        })
        .map(|entry| entry.label.clone())
        .collect();
    add_choices.sort();
    if !add_choices.is_empty() {
        let Some(fallback) = add_choices.first().cloned() else {
            return sec;
        };
        let choices = add_choices;
        sec = sec.with_action(
            ActionSpec::typed_operation(
                "add-source",
                "Add keyboard input source",
                "Choose an installed keyboard layout or input method",
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
    for child in tablet_resources() {
        sec = sec.with_child(child);
    }
    sec
}

/// One resource per attached drawing tablet and per tablet pad, carrying
/// the same per-device choices as their cards.
fn tablet_resources() -> Vec<Resource> {
    use crate::panes::mouse_keyboard as pane;
    use settings_store::hardware::{InputDeviceKind, InputHardwareSnapshot};
    const TAGS: &[&str] = &["device-local", "input", "tablet"];
    let Some(devices) = InputHardwareSnapshot::load() else {
        return Vec::new();
    };
    let ids = |choices: Vec<(String, String)>| choices.into_iter().map(|(id, _)| id).collect();
    let mut resources = Vec::new();
    for tablet in devices
        .of_kind(InputDeviceKind::Tablet)
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let device = tablet.key.clone();
        let op = move |operation: InputOperation| {
            encode_local_operation(INPUT_OPERATION_TYPE, &operation)
        };
        let mut resource = Resource::new(
            pane::tablet_resource_id(&tablet.key),
            "settings.tablet",
            tablet.name.clone(),
        );
        resource = resource.with_action(local_choice_spec(
            "map-to",
            "Map the tablet to a display",
            INPUT_OPERATION_TYPE,
            ids(pane::tablet_output_choices()),
            TAGS,
            {
                let device = device.clone();
                move |output| {
                    op(InputOperation::SetTabletOutput {
                        device: device.clone(),
                        output,
                    })
                }
            },
        ));
        resource = resource.with_action(local_bool_spec(
            "keep-aspect",
            "Keep the tablet's aspect ratio",
            INPUT_OPERATION_TYPE,
            TAGS,
            {
                let device = device.clone();
                move |enabled| {
                    op(InputOperation::SetTabletKeepAspect {
                        device: device.clone(),
                        enabled,
                    })
                }
            },
        ));
        resource = resource.with_action(local_bool_spec(
            "left-handed",
            "Turn the tablet for left-handed use",
            INPUT_OPERATION_TYPE,
            TAGS,
            {
                let device = device.clone();
                move |enabled| {
                    op(InputOperation::SetTabletLeftHanded {
                        device: device.clone(),
                        enabled,
                    })
                }
            },
        ));
        resource = resource.with_action(local_number_spec(
            "tip-feel",
            "Set pen tip feel, soft to firm",
            INPUT_OPERATION_TYPE,
            TAGS,
            {
                let device = device.clone();
                move |value| {
                    op(InputOperation::SetTabletTipFeel {
                        device: device.clone(),
                        value: value.clamp(0.0, 1.0),
                    })
                }
            },
        ));
        for (id, label, button) in [
            ("lower-button", "Set the lower pen button", "lower"),
            ("upper-button", "Set the upper pen button", "upper"),
            ("third-button", "Set the third pen button", "third"),
        ] {
            let device = device.clone();
            resource = resource.with_action(local_choice_spec(
                id,
                label,
                INPUT_OPERATION_TYPE,
                ids(pane::stylus_button_choices()),
                TAGS,
                move |action| {
                    op(InputOperation::SetTabletButton {
                        device: device.clone(),
                        button: button.into(),
                        action,
                    })
                },
            ));
        }
        resource = resource.with_action(local_choice_spec(
            "eraser",
            "Set what the eraser does",
            INPUT_OPERATION_TYPE,
            ids(pane::eraser_choices()),
            TAGS,
            {
                let device = device.clone();
                move |action| {
                    op(InputOperation::SetTabletEraser {
                        device: device.clone(),
                        action,
                    })
                }
            },
        ));
        resources.push(resource);
    }
    for pad in devices
        .of_kind(InputDeviceKind::TabletPad)
        .into_iter()
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let mut resource = Resource::new(
            pane::pad_resource_id(&pad.key),
            "settings.tablet-pad",
            pad.name.clone(),
        );
        for button in 0..pad.pad_buttons.min(pane::MAX_PAD_BUTTONS) {
            let device = pad.key.clone();
            resource = resource.with_action(local_choice_spec(
                format!("button-{button}"),
                format!("Set pad button {}", button + 1),
                INPUT_OPERATION_TYPE,
                ids(pane::pad_button_choices()),
                TAGS,
                move |action| {
                    encode_local_operation(
                        INPUT_OPERATION_TYPE,
                        &InputOperation::SetPadButton {
                            device: device.clone(),
                            button,
                            action,
                        },
                    )
                },
            ));
        }
        for (dial, modes) in pane::pad_dials(pad) {
            let (kind, index, noun) = match dial {
                settings_store::hardware::PadDial::Ring(index) => ("ring", index, "ring"),
                settings_store::hardware::PadDial::Strip(index) => ("strip", index, "strip"),
            };
            for mode in 0..modes {
                let device = pad.key.clone();
                resource = resource.with_action(local_choice_spec(
                    pane::dial_action_id(dial, mode),
                    format!(
                        "Set what pad {noun} {} does in mode {}",
                        index + 1,
                        mode + 1
                    ),
                    INPUT_OPERATION_TYPE,
                    ids(pane::dial_choices()),
                    TAGS,
                    move |action| {
                        encode_local_operation(
                            INPUT_OPERATION_TYPE,
                            &InputOperation::SetPadDial {
                                device: device.clone(),
                                dial: kind.into(),
                                index,
                                mode,
                                action,
                            },
                        )
                    },
                ));
            }
        }
        resources.push(resource);
    }
    resources
}

/// Bind a control on a tablet or pad card to its device resource's action.
fn tablet_binding<T: 'static>(
    resource_id: &str,
    action_id: &str,
    parameter: &'static str,
    to_value: fn(T) -> serde_json::Value,
) -> Option<tilo_actions::ActionBinding<T>> {
    let resource = tablet_resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == resource_id)?;
    let spec = resource.actions.iter().find(|spec| spec.id == action_id)?;
    tilo_actions::ActionBinding::new(&resource, spec, move |value| {
        serde_json::Map::from_iter([(parameter.into(), to_value(value))])
    })
    .ok()
}

pub fn tablet_bool_binding(
    resource_id: &str,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<bool>> {
    tablet_binding(resource_id, action_id, "value", serde_json::Value::Bool)
}

pub fn tablet_number_binding(
    resource_id: &str,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<f64>> {
    tablet_binding(resource_id, action_id, "value", |value| {
        serde_json::json!(value)
    })
}

pub fn tablet_text_binding(
    resource_id: &str,
    action_id: &str,
) -> Option<tilo_actions::ActionBinding<String>> {
    tablet_binding(resource_id, action_id, "choice", serde_json::Value::String)
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
    let sec = Resource::new(
        "settings.section:accessibility",
        "settings.section",
        "Accessibility",
    );
    portable::attach_portable_preferences(sec, Category::Accessibility, doc, d)
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
        (
            "install-codecs",
            "Install additional codecs from RPM Fusion and Cisco",
            UpdateOperation::InstallCodecs,
        ),
        (
            "remove-codecs",
            "Remove additional codecs",
            UpdateOperation::RemoveCodecs,
        ),
        (
            "refresh-codecs",
            "Check again for the codec pack's hardware video driver",
            UpdateOperation::RefreshCodecs,
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
                    // Downloads third-party software under its publishers'
                    // licences: never without the owner's explicit consent.
                    | UpdateOperation::InstallCodecs
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
fn region_section(doc: &AutoCommit, d: &SystemPrefs) -> Resource {
    let section = Resource::new(
        pane_resource_id(Category::RegionFormats),
        "settings.section",
        "Region & Formats",
    );
    portable::attach_portable_preferences(section, Category::RegionFormats, doc, d)
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
    let prefs = SystemPrefs::load();
    let mut section = Resource::new(
        pane_resource_id(Category::KeyboardShortcuts),
        "settings.section",
        "Keyboard Shortcuts",
    );
    section =
        portable::attach_portable_preferences(section, Category::KeyboardShortcuts, doc, &prefs);
    // Chords are changed through the shell's `keyboard-shortcuts` resource,
    // the one write path every caller shares; this section keeps only the
    // Vim navigation preference.
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

fn speaker_spec(
    id: &str,
    label: &str,
    placeholder: &str,
    params: Vec<ActionParam>,
    encode: impl Fn(&tilo_actions::ActionArgs) -> SpeakerOperation + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        placeholder,
        OperationKind::Effect,
        SPEAKER_OPERATION_TYPE,
        params,
        move |args| encode_local_operation(SPEAKER_OPERATION_TYPE, &encode(args)),
    )
    .with_contract(local_operation_contract(&["device-local", "speakers"]))
}

/// Speaker sets: create one, and per set delete, calibrate, apply or revert
/// a calibration. Sets come from the last snapshot the Speakers pane
/// received, so building this tree never waits on the compositor.
fn speakers_section() -> Resource {
    let mut section = Resource::new(
        pane_resource_id(Category::Speakers),
        "settings.section",
        "Speakers",
    );
    section = section.with_action(speaker_spec(
        "create",
        "Create a speaker set",
        "Name and members",
        vec![
            ActionParam::required("name", ParamType::Text, "Name, such as Living room"),
            ActionParam::required(
                "members",
                ParamType::Text,
                "JSON list of {device, output, role}; role is {\"role\":\"left\"}, \"right\", \"center\", \"lfe\", \"surround-left\", \"surround-right\" or {\"role\":\"room\",\"x_mm\":…,\"y_mm\":…,\"z_mm\":…}",
            ),
        ],
        |args| SpeakerOperation::Create {
            name: args.text("name").unwrap_or_default().to_string(),
            members: serde_json::from_str(args.text("members").unwrap_or_default())
                .unwrap_or_default(),
        },
    ));
    section = section.with_action(speaker_spec(
        "identify",
        "Play a sound on a speaker",
        "Device and output",
        vec![
            ActionParam::required("device", ParamType::Text, "Device id"),
            ActionParam::required("output", ParamType::Text, "Output selector"),
        ],
        |args| SpeakerOperation::Identify {
            device: args.text("device").unwrap_or_default().to_string(),
            output: args.text("output").unwrap_or_default().to_string(),
        },
    ));
    let snapshot = crate::speaker_setup::cached_snapshot();
    for group in snapshot
        .groups
        .iter()
        .filter(|group| group.led_here)
        .take(MAX_DYNAMIC_RESOURCES)
    {
        let id = group.id.clone();
        let mut resource = Resource::new(
            format!("settings.speaker-group:{id}"),
            "settings.speaker-group",
            group.name.clone(),
        );
        let delete_id = id.clone();
        resource = resource.with_action(
            speaker_spec(
                "delete",
                "Delete this speaker set",
                "",
                Vec::new(),
                move |_| SpeakerOperation::Delete {
                    group: delete_id.clone(),
                },
            )
            .destructive(),
        );
        let microphones = group
            .microphones
            .iter()
            .filter(|microphone| microphone.status.usable())
            .map(|microphone| microphone.device.clone())
            .take(MAX_DYNAMIC_CHOICES)
            .collect::<Vec<_>>();
        if !microphones.is_empty() {
            let calibrate_id = id.clone();
            resource = resource.with_action(speaker_spec(
                "calibrate",
                "Calibrate from where you sit",
                "Listening device",
                vec![
                    ActionParam::required_owned(
                        "listening",
                        ParamType::Text,
                        "Device whose microphone is where you sit",
                    )
                    .with_choices(microphones.clone()),
                    ActionParam::optional_owned(
                        "exclude",
                        ParamType::Text,
                        "Comma-separated device ids whose microphones stay out; every other offered microphone also listens",
                    ),
                ],
                move |args| SpeakerOperation::Calibrate {
                    group: calibrate_id.clone(),
                    excluded: args
                        .text("exclude")
                        .unwrap_or_default()
                        .split(',')
                        .map(str::trim)
                        .filter(|device| !device.is_empty())
                        .map(str::to_string)
                        .collect(),
                    listening: args.text("listening").map(str::to_string),
                },
            ));
        }
        let cancel_id = id.clone();
        resource = resource.with_action(speaker_spec(
            "cancel-calibration",
            "Stop the running calibration",
            "",
            Vec::new(),
            move |_| SpeakerOperation::CancelCalibration {
                group: cancel_id.clone(),
            },
        ));
        for (action, label, make) in [
            (
                "apply-calibration",
                "Apply the measured calibration",
                (|group| SpeakerOperation::ApplyCalibration { group })
                    as fn(String) -> SpeakerOperation,
            ),
            (
                "revert-calibration",
                "Revert to the previous calibration",
                |group| SpeakerOperation::RevertCalibration { group },
            ),
        ] {
            let group_id = id.clone();
            resource =
                resource.with_action(speaker_spec(action, label, "", Vec::new(), move |_| {
                    make(group_id.clone())
                }));
        }
        section = section.with_child(resource);
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

// Inherent resource publication (was ResourceProvider)
impl crate::SettingsApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources_with_accounts(doc, self.account_resource_inventory())
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "settings.root" {
            // The document door, not a navigation stub and not the live hardware
            // inventory. Preference panes come from `tilo_resource::publish`.
            let state = crate::resource::SettingsState::from_doc(doc);
            return Some(ui::tilo_resource::publish::<crate::resource::Settings>(
                "settings.root",
                "Settings",
                &state,
            ));
        }
        // Notification rows include device-local apps that the portable document
        // pane does not. Every other id already on the published tree is that node.
        let device_notifications = category_for_resource_id(id) == Some(Category::Notifications)
            || id.starts_with("settings.notification-app:");
        if !device_notifications {
            let state = crate::resource::SettingsState::from_doc(doc);
            let root = ui::tilo_resource::publish::<crate::resource::Settings>(
                "settings.root",
                "Settings",
                &state,
            );
            if let Some(found) = find_resource(root, id) {
                return Some(found);
            }
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
    use tilo_actions::LocalTransport;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn find<'a>(sections: &'a [Resource], id: &str) -> &'a Resource {
        sections[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("section {id} present"))
    }

    #[test]
    fn resource_publish_owns_preference_door_and_hosted_keeps_device_extras() {
        let state = crate::resource::SettingsState::default();
        let published = ui::tilo_resource::publish::<crate::resource::Settings>(
            "settings.root",
            "Settings",
            &state,
        );
        let section_ids: Vec<&str> = published
            .children
            .iter()
            .map(|child| child.id.as_str())
            .collect();
        assert!(
            section_ids.contains(&"settings.section:appearance"),
            "{section_ids:?}"
        );
        assert!(
            section_ids.contains(&"settings.section:notifications"),
            "{section_ids:?}"
        );
        assert!(
            section_ids.contains(&"settings.section:accessibility"),
            "{section_ids:?}"
        );
        assert!(
            section_ids.contains(&"settings.section:region_formats"),
            "{section_ids:?}"
        );
        assert!(
            section_ids.contains(&"settings.section:keyboard_shortcuts"),
            "{section_ids:?}"
        );
        assert!(
            !section_ids.contains(&"settings.section:sound"),
            "hardware sections stay off the framework resource: {section_ids:?}"
        );
        assert!(
            !section_ids.contains(&"settings.section:network"),
            "hardware sections stay off the framework resource: {section_ids:?}"
        );

        let appearance = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "settings.section:appearance")
            .expect("appearance section");
        let appearance_ids: Vec<&str> = appearance.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(appearance_ids.contains(&"open"), "{appearance_ids:?}");
        assert!(appearance_ids.contains(&"dark_theme"), "{appearance_ids:?}");
        assert!(appearance_ids.contains(&"ui_scale"), "{appearance_ids:?}");

        let notifications = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "settings.section:notifications")
            .expect("notifications section");
        let notif_ids: Vec<&str> = notifications
            .actions
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert!(notif_ids.contains(&"dnd"), "{notif_ids:?}");
        assert!(
            notifications.children.is_empty(),
            "per-app rows are live extras, not document children"
        );

        let config = crate::prefs::test_env::ScratchConfig::new();
        let mut app = config.app();
        let doc = AutoCommit::new();
        assert!(!app.is_hosted(), "standalone tests stay on the local host");
        let standalone = app.publish_resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "settings.root");
        assert_eq!(standalone[0].children.len(), Category::ALL.len());
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == "settings.section:sound"),
            "standalone merges device-local sections into the one root"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.as_str() == "settings.section:sound"),
            "hosted keeps hardware sections as extras"
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.as_str() == "settings.section:network"),
            "hosted keeps hardware sections as extras"
        );
        assert!(
            !extras
                .iter()
                .any(|resource| resource.id.as_str() == "settings.section:appearance"),
            "hosted must not republish the SettingsAction door: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            !extras
                .iter()
                .any(|resource| resource.kind == "settings.root"),
            "hosted must not republish settings.root beside ResourceRoot"
        );
    }

    /// Ctrl+K tree, stable pane addresses, lookups, bounded inventories and refused stale targets.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn settings_tree_addresses_are_stable_shallow_and_bounded() {
        // Case `ctrl_k_publishes_the_settings_tree`.
        // Ctrl+K on Settings yields the tree: one `settings.root` with exactly one
        // stable `settings.section` child per category. Every pane exports its
        // View operation; panes with mutable settings additionally export the
        // collaborative document actions they own. Drives the app's real hook.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
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
            assert!(ids.contains(&"dark_theme"), "{ids:?}");
            assert!(ids.contains(&"ui_scale"), "{ids:?}");
        }

        // Case `pane_addresses_are_stable_and_unique`.
        {
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

        // Case `exact_root_lookup_is_the_published_document_door`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let doc = AutoCommit::new();
            let root = app
                .publish_resource(&doc, "settings.root")
                .expect("root resolves exactly");
            assert_eq!(root.kind, "settings.root");
            assert!(
                root.actions.iter().any(|action| action.id == "apply"),
                "the root lookup is the published document door"
            );
            assert_eq!(
                root.children.len(),
                Category::ALL
                    .iter()
                    .copied()
                    .filter(|category| is_document_door_category(*category))
                    .count(),
                "hardware panes stay off the exact root lookup"
            );
            assert!(
                root.children.iter().all(|section| {
                    section
                        .children
                        .iter()
                        .all(|child| child.kind != "settings.account")
                }),
                "resolving the root must not enumerate live account inventories"
            );

            let appearance = app
                .publish_resource(&doc, &pane_resource_id(Category::Appearance))
                .expect("one section resolves without rebuilding every other section");
            assert_eq!(appearance.label, Category::Appearance.label());
            assert!(
                appearance
                    .actions
                    .iter()
                    .any(|action| action.id == "dark_theme")
            );
        }

        // Case `adversarial_account_inventory_is_hard_bounded`.
        {
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

        // Case `stale_or_presentation_targets_are_refused`.
        {
            let mut doc = AutoCommit::new();
            for action in [
                set_bool_action("appearance.high_contrast.edit", true),
                set_text_action("settings.select.appearance", "true"),
                set_text_action("shortcuts.no-longer-supported", "Super+Shift+K"),
                set_text_action("shortcuts.search", "Super+J"),
                set_text_action("unknown.preference", "value"),
            ] {
                assert!(matches!(
                    action.apply(&mut doc),
                    Err(ActionError::Refused(_))
                ));
            }
        }
    }

    /// Account removal, storage reclamation and update operations need host confirmation.
    #[test]
    fn destructive_resources_require_host_confirmation() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `account_removal_resource_requires_host_confirmation`.
        {
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

        // Case `storage_reclamation_requires_host_confirmation`.
        {
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

        // Case `update_operations_are_typed_local_effects_with_destructive_guards`.
        {
            let updates = find(&resources, &pane_resource_id(Category::Updates));
            for id in [
                "check",
                "restart",
                "rollback",
                "refresh-firmware",
                "install-firmware",
                "install-codecs",
                "remove-codecs",
                "refresh-codecs",
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
            for id in ["restart", "rollback", "install-firmware", "install-codecs"] {
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
    }

    /// Static portable preferences are declared once in the table and published by section.
    #[test]
    fn portable_preference_table_covers_static_section_actions() {
        let resources = provide_resources(&AutoCommit::new());
        for section in [
            Category::Notifications,
            Category::Appearance,
            Category::Accessibility,
            Category::RegionFormats,
            Category::MouseKeyboard,
            Category::KeyboardShortcuts,
        ] {
            let resource = find(&resources, &pane_resource_id(section));
            let ids: std::collections::HashSet<&str> = resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for action_id in portable::published_action_ids(section) {
                assert!(
                    ids.contains(action_id),
                    "table action {action_id} missing from {} resource tree",
                    section.label()
                );
            }
        }
        // Every static table key accepts its declared type and refuses the other.
        for pref in PORTABLE_PREFERENCES {
            let mut doc = AutoCommit::new();
            let (ok, bad) = match pref.pref_type() {
                PortablePrefType::Bool => {
                    (SettingValue::Bool(true), SettingValue::Text("nope".into()))
                }
                PortablePrefType::Text => {
                    (SettingValue::Text("value".into()), SettingValue::Bool(true))
                }
            };
            assert!(
                SettingsAction::SetSetting {
                    key: pref.key.into(),
                    value: ok,
                }
                .apply(&mut doc)
                .is_ok(),
                "table key {} should accept its type",
                pref.key
            );
            assert!(
                matches!(
                    SettingsAction::SetSetting {
                        key: pref.key.into(),
                        value: bad,
                    }
                    .apply(&mut AutoCommit::new()),
                    Err(ActionError::Refused(_))
                ),
                "table key {} should refuse the wrong type",
                pref.key
            );
        }
    }

    /// Power, notification/input, shortcut, accessibility and default-app resources cover their controls.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn preference_resources_cover_every_visible_control() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `power_resource_covers_every_non_hardware_preference_control`.
        {
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

        // Case `notification_and_input_resources_cover_their_durable_preferences`.
        {
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
                "touchpad-enabled",
                "touchpad-disabled-with-mouse",
                "touchpad-natural-scroll",
                "mouse-natural-scroll",
                "tap-to-click",
                "touchpad-speed",
                "mouse-speed",
                "key-repeat",
                "tap-drag",
                "tap-drag-lock",
                "disable-while-typing",
                "middle-emulation",
                "left-handed",
                "touchpad-gestures",
                "touchpad-click-method",
                "touchpad-scroll-method",
                "spell-checking",
                "auto-correct",
                "acceleration-profile",
                "compose-key",
                "caps-lock",
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

        // Case `the_shortcuts_section_offers_only_vim_mode`.
        {
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

            assert_eq!(
                action_ids,
                std::collections::HashSet::from([OPEN_PANE_ACTION, "vim-mode"]),
                "chords are rebound only through the shell's keyboard-shortcuts resource"
            );
        }

        // Case `accessibility_controls_bind_to_their_exported_typed_actions`.
        {
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

        // Case `default_app_handlers_are_bounded_canonical_child_resources`.
        {
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
    }

    /// Network, Bluetooth, display tokens and display scale are bounded device-local effects.
    #[test]
    fn hardware_controls_are_bounded_typed_local_effects() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `hardware_network_controls_are_bounded_local_effects`.
        {
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

        // Case `bluetooth_discovery_controls_are_bounded_local_effects`.
        {
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

        // Case `display_operation_tokens_are_typed_and_stable`.
        {
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
            assert!(
                local_operation_token(DISPLAY_OPERATION_TYPE, br#"{\"set_mode\":{}}"#).is_err()
            );
        }

        // Case `display_scale_resource_uses_the_dropdowns_durable_label`.
        // Display scale is a device-local typed effect rather than a replicated
        // document write. The operation still carries the exact picker label so
        // agents and the visible control share one validated contract.
        {
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
    }

    /// Input/time and hardware preferences are never replayable document targets.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn device_local_controls_never_enter_action_replay() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `input_and_time_controls_cannot_enter_settings_action_replay`.
        {
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

        // Case `input_and_time_resources_and_controls_share_local_effect_protocols`.
        {
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

        // Case `per_device_natural_scroll_actions_are_refused_from_replay`.
        {
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

        // Case `hardware_preference_keys_are_never_replayable_document_targets`.
        {
            let local_keys = [
                "network.wifi_enabled",
                "bluetooth.enabled",
                "audio.volume",
                "audio.muted",
                "audio.input_volume",
                "audio.balance",
                "audio.mono",
                "audio.echo_cancellation",
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

    /// Accepted local operations, continuity actions and agent provider configuration.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn local_operations_and_continuity_are_typed_effects() {
        // Shared read-only fixture for every case below.
        let resources = provide_resources(&AutoCommit::new());

        // Case `accepted_local_operations_advertise_background_string_results`.
        {
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

        // Case `continuity_actions_are_typed_device_local_effects`.
        {
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
                let args = serde_json::Map::from_iter([(
                    "value".to_string(),
                    serde_json::Value::Bool(true),
                )]);
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

        // Case `agent_provider_configuration_is_a_typed_immediate_local_effect`.
        {
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
    }

    /// No-op writes, rejected theme choices and explicit collaboration scopes.
    #[test]
    fn shared_writes_are_scoped_idempotent_and_validated() {
        // Case `setting_to_current_value_is_a_no_op`.
        // Writing the value a key already holds is a no-op: the document's heads are
        // unchanged (so a replayed / stale envelope can't churn a peer).
        {
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

        // Case `theme_choice_rejects_an_unknown_value`.
        // The shared typed door rejects an unrecognised theme choice before the
        // application's builder can write junk or silently reinterpret it.
        {
            let res = provide_resources(&AutoCommit::new());
            let appearance = find(&res, "settings.section:appearance");
            let theme = appearance
                .actions
                .iter()
                .find(|a| a.id == "dark_theme")
                .unwrap();
            let typo = serde_json::Map::from_iter([(
                "choice".to_owned(),
                serde_json::Value::String("not-a-theme".to_owned()),
            )]);
            let error = theme
                .build_json(&typo)
                .expect_err("unknown theme is invalid");
            assert!(error.to_string().contains("must be one of"), "{error}");
        }

        // Case `every_published_action_has_an_explicit_collaboration_scope`.
        {
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
    }

    /// A seed written through the executor, as the panes write: Do Not Disturb
    /// off and the Dark theme chosen, so every bool flip and choice has a real
    /// current value to change.
    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        for action in [
            set_bool_action(panes::notifications::DND_KEY, false),
            set_text_action(panes::appearance::DARK_THEME_KEY, "Dark"),
        ] {
            tilo_actions::apply_local(&mut doc, &action, encode(&action)).unwrap();
        }
        doc
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| provide_resources(doc),
        palette: ["Network — Open Network", "Turn on Do Not Disturb", "Set dark theme"],
        replay: [
            set_dark_theme => |_| encode(&set_text_action(panes::appearance::DARK_THEME_KEY, "Dark cool")),
        ],
        undo: [
            turn_on_dnd => |_| encode(&set_bool_action(panes::notifications::DND_KEY, true)),
        ],
        encodes: [
            dnd_flips_the_current_value => ("settings.section:notifications", "dnd", |_, _| {
                encode(&set_bool_action(panes::notifications::DND_KEY, true))
            }),
            theme => ("settings.section:appearance", "dark_theme", arg: "Dark soft", |_, _| {
                encode(&set_text_action(panes::appearance::DARK_THEME_KEY, "Dark soft"))
            }),
        ],
    }
}
