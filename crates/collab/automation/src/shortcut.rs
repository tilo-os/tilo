//! The serialisable shortcut program.
//!
//! Everything here is inert data: it can be stored, synced, diffed and shown
//! in an editor without a host. Meaning comes from [`check`](crate::check),
//! which types it against live descriptors, and [`run`](crate::run), which
//! executes it.

use std::collections::BTreeMap;

use collab_types::PermissionProfileRule;
use serde::{Deserialize, Serialize};
use tilo_actions::door::ActionAddress;
use tilo_actions::params::{ActionParam, ParamType};

use crate::{
    AGENT_CLASSIFY_ACTION, AGENT_EXTRACT_ACTION, AGENT_FREEFORM_ACTION, AGENT_SUMMARISE_ACTION,
    SHORTCUT_RUN_ACTION, shortcut_resource_id,
};

/// One saved shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    /// Stable id; also the tail of the resource id the shortcut is published
    /// under ([`shortcut_resource_id`]).
    pub id: String,
    /// The person's name for it. Carried into history as the purpose of every
    /// action the shortcut takes.
    pub name: String,
    /// What a run is started with, in the same typed vocabulary actions use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionParam>,
    pub steps: Vec<Step>,
    /// The value a finished run reports back to whoever started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Binding>,
    /// The most the shortcut may do. A run acts with the intersection of this
    /// and the authority of the person who started it: a step outside the
    /// grant is refused here, a step outside the person's authority is refused
    /// by the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grant: Vec<PermissionProfileRule>,
    /// When the system may start it without being asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerSpec>,
    /// Confirmations given in advance for runs nobody is present at: a
    /// triggered run may take exactly these actions although their contract
    /// asks for confirmation. They are the saving person's, so a host honours
    /// them only in runs that person started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consents: Vec<UnattendedConsent>,
    /// Policy id of the person who saved it. Set by the host that stored it,
    /// whatever the saved JSON claimed; its triggers fire on that person's
    /// behalf only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_by: Option<String>,
}

impl Shortcut {
    /// The address that runs this shortcut through the action door.
    #[must_use]
    pub fn action_address(&self) -> ActionAddress {
        ActionAddress::new(shortcut_resource_id(&self.id), SHORTCUT_RUN_ACTION)
    }
}

/// Advance confirmation of one exact action: `action` on resource
/// `resource` of application `app_id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnattendedConsent {
    pub app_id: String,
    pub resource: String,
    pub action: String,
}

impl UnattendedConsent {
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            resource: resource.into(),
            action: action.into(),
        }
    }

    /// Whether this consent names `address` on `app_id`.
    #[must_use]
    pub fn covers(&self, app_id: &str, address: &ActionAddress) -> bool {
        self.app_id == app_id && self.resource == address.resource && self.action == address.action
    }
}

/// One step, named so later steps can refer to its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Unique within the shortcut; ASCII letters, digits, `-` and `_`.
    pub id: String,
    #[serde(flatten)]
    pub kind: StepKind,
}

impl Step {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: StepKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }
}

/// What a step does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepKind {
    /// Invoke one published action.
    Invoke {
        app_id: String,
        resource: Selector,
        action: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        args: BTreeMap<String, Binding>,
    },
    /// Run one of two blocks.
    If {
        cond: Condition,
        then: Vec<Step>,
        #[serde(default, rename = "else", skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<Step>,
    },
    /// Run `body` once per element of an array, with the element bound to
    /// the variable `item`.
    ForEach {
        items: Binding,
        #[serde(default = "default_item")]
        item: String,
        body: Vec<Step>,
    },
    /// Bind a variable for later steps.
    SetVar { name: String, value: Binding },
    /// Ask the person running the shortcut for one value. The answer is this
    /// step's output. Unattended runs cannot ask and fail here.
    Ask { param: ActionParam },
    /// Transform a value with the agent. Lowered to an invocation of the
    /// agent's transform action for `transform`.
    Ai {
        transform: AiTransform,
        input: Binding,
    },
}

fn default_item() -> String {
    "item".into()
}

/// Which resource an invocation acts on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// Exactly this stable resource id.
    Fixed(String),
    /// The resource address a binding evaluates to at run time.
    FromBinding(Binding),
    /// The first published resource of this kind (or a sub-kind of it) that
    /// offers the action.
    FirstOfKind(String),
}

/// Where a value comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// A constant.
    Literal(serde_json::Value),
    /// One of the shortcut's inputs.
    Input(String),
    /// A variable set by [`StepKind::SetVar`] or a loop's item.
    Var(String),
    /// Part of an earlier step's output, addressed by an RFC 6901 JSON
    /// pointer (empty for the whole value).
    StepOutput {
        step: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pointer: String,
    },
    /// Text with `{{input.NAME}}`, `{{var.NAME}}` and `{{step.ID}}` or
    /// `{{step.ID/json/pointer}}` placeholders. Strings are inserted as they
    /// are; other values as compact JSON.
    Template(String),
}

impl Binding {
    #[must_use]
    pub fn step(step: impl Into<String>, pointer: impl Into<String>) -> Self {
        Self::StepOutput {
            step: step.into(),
            pointer: pointer.into(),
        }
    }
}

/// A branch condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Neither null, false, zero, nor an empty string, array or object.
    Truthy(Binding),
    /// Both sides evaluate to equal JSON values.
    Equals {
        left: Binding,
        right: Binding,
    },
    Not(Box<Condition>),
    All(Vec<Condition>),
    Any(Vec<Condition>),
}

/// What an [`Ai`](StepKind::Ai) step asks of the agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiTransform {
    Summarise,
    /// Returns a JSON value that satisfies `schema`.
    Extract {
        schema: serde_json::Value,
    },
    /// Returns exactly one of `choices`.
    Classify {
        choices: Vec<String>,
    },
    /// Returns text answering `prompt`, itself a [`Binding::Template`].
    Freeform {
        prompt: String,
    },
}

impl AiTransform {
    /// The agent transform action this lowers to.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Summarise => AGENT_SUMMARISE_ACTION,
            Self::Extract { .. } => AGENT_EXTRACT_ACTION,
            Self::Classify { .. } => AGENT_CLASSIFY_ACTION,
            Self::Freeform { .. } => AGENT_FREEFORM_ACTION,
        }
    }

    /// The arguments of the lowered invocation.
    #[must_use]
    pub fn arguments(&self, input: &Binding) -> BTreeMap<String, Binding> {
        let mut args = BTreeMap::new();
        args.insert("input".to_string(), input.clone());
        match self {
            Self::Summarise => {}
            Self::Extract { schema } => {
                args.insert("schema".into(), Binding::Literal(schema.clone()));
            }
            Self::Classify { choices } => {
                args.insert(
                    "choices".into(),
                    Binding::Literal(serde_json::json!(choices)),
                );
            }
            Self::Freeform { prompt } => {
                args.insert("prompt".into(), Binding::Template(prompt.clone()));
            }
        }
        args
    }
}

/// What may start a shortcut. Data only: the session decides how to watch
/// for each and starts an unattended run when one fires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "trigger", rename_all = "snake_case")]
pub enum TriggerSpec {
    /// A key chord in the shell's own notation, e.g. `Super+Shift+N`.
    Keyboard {
        chord: String,
    },
    Schedule {
        schedule: Schedule,
        #[serde(default)]
        catch_up: CatchUp,
    },
    AppOpened {
        app_id: String,
    },
    /// Joining a workspace; `None` for any.
    WorkspaceJoined {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room: Option<String>,
    },
    /// A file created, changed or removed inside the folder a capability of
    /// the shortcut's workspace shares with its saver, named by the
    /// capability's id. The trigger carries that authority, never a path:
    /// revoking the capability, or the saver losing read access to it,
    /// disarms the trigger.
    FileChanged {
        capability: String,
    },
    /// Do Not Disturb ([`FOCUS_DO_NOT_DISTURB`]) or the focus schedule
    /// ([`FOCUS_SCHEDULE`]) switching on or off.
    FocusMode {
        mode: String,
        transition: FocusTransition,
    },
    /// The network changed; each set field narrows which changes count.
    NetworkChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connected: Option<bool>,
    },
    /// A brokered device class appeared (`camera`, `display`, `storage`, …).
    DeviceConnected {
        device_class: String,
    },
    IncomingMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// A known peer device came within reach; `None` for any.
    NearbyDevice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
}

/// [`TriggerSpec::FocusMode`] mode: the person's Do Not Disturb switch.
pub const FOCUS_DO_NOT_DISTURB: &str = "do_not_disturb";
/// [`TriggerSpec::FocusMode`] mode: the person's recurring focus schedule.
pub const FOCUS_SCHEDULE: &str = "focus";

static APP_PAYLOAD: [ActionParam; 1] = [ActionParam::required(
    "app_id",
    ParamType::Text,
    "The application that opened",
)];
static WORKSPACE_PAYLOAD: [ActionParam; 2] = [
    ActionParam::required(
        "workspace",
        ParamType::Text,
        "The id of the workspace joined",
    ),
    ActionParam::required(
        "workspace_name",
        ParamType::Text,
        "The name of the workspace joined",
    ),
];
static FILE_PAYLOAD: [ActionParam; 2] = [
    ActionParam::required(
        "file",
        ParamType::Text,
        "The path of the file, inside the watched folder",
    ),
    ActionParam::required(
        "change",
        ParamType::Text,
        "`created`, `modified` or `removed`",
    ),
];
static FOCUS_PAYLOAD: [ActionParam; 2] = [
    ActionParam::required("mode", ParamType::Text, "`do_not_disturb` or `focus`"),
    ActionParam::required("active", ParamType::Boolean, "Whether the mode switched on"),
];

impl TriggerSpec {
    /// What a firing of this trigger tells the run it starts. Each value is
    /// bound to the shortcut's input of the same name, when it declares one
    /// of the same type.
    #[must_use]
    pub fn payload(&self) -> &'static [ActionParam] {
        match self {
            Self::AppOpened { .. } => &APP_PAYLOAD,
            Self::WorkspaceJoined { .. } => &WORKSPACE_PAYLOAD,
            Self::FileChanged { .. } => &FILE_PAYLOAD,
            Self::FocusMode { .. } => &FOCUS_PAYLOAD,
            _ => &[],
        }
    }

    /// Stable name of the trigger's kind, recorded with every run it starts.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Keyboard { .. } => "keyboard",
            Self::Schedule { .. } => "schedule",
            Self::AppOpened { .. } => "app_opened",
            Self::WorkspaceJoined { .. } => "workspace_joined",
            Self::FileChanged { .. } => "file_changed",
            Self::FocusMode { .. } => "focus_mode",
            Self::NetworkChanged { .. } => "network_changed",
            Self::DeviceConnected { .. } => "device_connected",
            Self::IncomingMessage { .. } => "incoming_message",
            Self::NearbyDevice { .. } => "nearby_device",
        }
    }
}

/// When a scheduled trigger fires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum Schedule {
    /// A fixed period, counted from the Unix epoch so every device the
    /// shortcut reaches agrees on its fire times.
    Interval { seconds: u64 },
    /// A local wall-clock time on the listed weekdays (every day when empty).
    Calendar {
        at: TimeOfDay,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        weekdays: Vec<Weekday>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// What to do about fire times missed while the device was off or asleep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchUp {
    /// Forget them; wait for the next one.
    #[default]
    Skip,
    /// Run once for however many were missed.
    RunOnce,
}

/// One of the saver's devices taking one fire time of a schedule, so the
/// saver's other devices leave it. Kept with the workspace's shortcuts, one
/// per schedule: a claim on a later fire time replaces an earlier one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleClaim {
    pub shortcut: String,
    /// Index of the schedule among the shortcut's triggers.
    pub trigger: usize,
    /// The fire time taken, in milliseconds since the Unix epoch.
    pub due_ms: i64,
    /// The device that runs it.
    pub device: String,
}

impl ScheduleClaim {
    /// The schedule this claims a fire time of: `shortcut/trigger`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.shortcut, self.trigger)
    }

    /// Whether this claim supersedes `other` for the same schedule.
    #[must_use]
    pub fn supersedes(&self, other: &Self) -> bool {
        (self.due_ms, other.device.as_str()) > (other.due_ms, self.device.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusTransition {
    Enter,
    Leave,
}
