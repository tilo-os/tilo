//! The serialisable shortcut program.
//!
//! Everything here is inert data: it can be stored, synced, diffed and shown
//! in an editor without a host. Meaning comes from [`check`](fn@crate::check),
//! which types it against live descriptors, and [`run`](fn@crate::run), which
//! executes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_action_policy::Rule;
use tilo_actions::ActionKey;
use tilo_actions::door::ActionAddress;
use tilo_actions::params::{ActionParam, ParamType};

use crate::expression::Expr;
use crate::{
    AGENT_APP_ID, AGENT_CLASSIFY_ACTION, AGENT_EXTRACT_ACTION, AGENT_FREEFORM_ACTION,
    AGENT_SUMMARISE_ACTION, AGENT_TASK_ACTION, AGENT_TRANSFORM_KIND,
};

/// One saved shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    /// Stable id; also the tail of the resource id the shortcut is published
    /// under ([`crate::shortcut_resource_id`]).
    pub id: String,
    /// The person's name for it. Carried into history as the purpose of every
    /// action the shortcut takes.
    pub name: String,
    /// What a run is started with, in the same typed vocabulary actions use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionParam>,
    pub steps: Vec<Step>,
    /// How long one step may take before the run stops waiting for it, in
    /// seconds. Absent, [`DEFAULT_STEP_TIMEOUT`](crate::run::DEFAULT_STEP_TIMEOUT)
    /// applies. A run nobody is present at needs a bound: a step whose action
    /// never answers would otherwise hold the run — and the one on its heels
    /// — for ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout_secs: Option<u64>,
    /// The value a finished run reports back to whoever started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Binding>,
    /// The most the shortcut may do, shown when it is imported. A run acts
    /// with the stricter of this and the authority of the person who started
    /// it: a step outside the grant is refused here, a step outside the
    /// person's authority is refused by the host. A run nobody is present at
    /// refuses what the grant would ask about, and passes a confirmation only
    /// for an action a rule names exactly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grant: Vec<Rule>,
    /// When the system may start it without being asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerSpec>,
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
        crate::shortcut_run_address(&self.id)
    }
}

/// One step, named so later steps can refer to its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Unique within the shortcut; ASCII letters, digits, `-` and `_`.
    pub id: String,
    /// What the run does when the step does not succeed. Absent, the step
    /// must succeed and the run stops at it
    /// ([`StepPolicy::Fail`](crate::StepPolicy::Fail)).
    #[serde(default, skip_serializing_if = "StepPolicy::is_fail")]
    pub on_error: StepPolicy,
    #[serde(flatten)]
    pub kind: StepKind,
}

impl Step {
    /// The step must succeed; when it does not, the run stops at it.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: StepKind) -> Self {
        Self {
            id: id.into(),
            on_error: StepPolicy::Fail,
            kind,
        }
    }

    /// The step, under `on_error` instead of the default policy.
    #[must_use]
    pub fn with_policy(mut self, on_error: StepPolicy) -> Self {
        self.on_error = on_error;
        self
    }
}

/// What a run does with a step that did not succeed.
///
/// Whatever the policy, a failure is recorded with the step and shown: a
/// step that is retried, run into a fallback or passed over is never the
/// same as one that worked.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "on_error", rename_all = "snake_case")]
pub enum StepPolicy {
    /// Stop the run here.
    #[default]
    Fail,
    /// Try the step again `attempts` times in all, waiting `backoff_ms`
    /// before the second attempt and doubling the wait each time after it,
    /// and stop the run if the last one fails too. A retry sends the same
    /// [`InvocationId`](tilo_actions::outcome::InvocationId), so an action is
    /// never applied twice.
    Retry {
        #[serde(default = "one")]
        attempts: u32,
        #[serde(default = "default_backoff_ms")]
        backoff_ms: u64,
    },
    /// Run `steps` in this step's place, after it ran, instead of stopping
    /// when it fails. The failed step's output stays unset, and the steps
    /// that run here are recorded where they ran.
    Fallback { steps: Vec<Step> },
    /// Carry on with `value` as the step's output, if nothing it changed
    /// can be taken back by simply not using it, and mark the run degraded.
    Default { value: Binding },
}

impl StepPolicy {
    #[must_use]
    pub const fn is_fail(&self) -> bool {
        matches!(self, Self::Fail)
    }

    /// How long the run waits before attempt `attempt` (the second one
    /// being 2): `backoff_ms`, doubling each time, at most
    /// [`MAX_BACKOFF_MS`](crate::run::MAX_BACKOFF_MS).
    #[must_use]
    pub fn wait_before(&self, attempt: u32) -> std::time::Duration {
        match self {
            Self::Retry { backoff_ms, .. } => {
                let wait = backoff_ms
                    .saturating_mul(1_u64 << attempt.saturating_sub(2).min(32))
                    .clamp(1, crate::run::MAX_BACKOFF_MS);
                std::time::Duration::from_millis(wait)
            }
            _ => std::time::Duration::ZERO,
        }
    }
}

fn one() -> u32 {
    1
}

const fn default_backoff_ms() -> u64 {
    crate::run::DEFAULT_BACKOFF_MS
}

/// What a step does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepKind {
    /// Invoke one published action: the app, the resource kind that
    /// publishes it and its public id, on the resource `resource` selects.
    Invoke {
        action: ActionKey,
        resource: Selector,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        args: BTreeMap<String, Binding>,
        /// The name and avatar a bot shows for this one action
        /// ([`tilo_actions::Appearance`]), such as a bridge posting a
        /// Discord user's message under their name. Only a bot's door
        /// admits one; it never changes who the author is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        appearance: Option<Binding>,
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
    /// Answer an expression and keep its value as this step's output, so a
    /// later step can name it as it names any other step's. Pure: nothing is
    /// invoked and nothing is recorded as an action.
    Compute { expr: Expr },
    /// Run `body` once per whole number from 0 below `count` (at most
    /// [`MAX_REPEAT`](crate::run::MAX_REPEAT) times), with the iteration
    /// number bound to the variable `index`.
    Repeat {
        count: Binding,
        #[serde(default = "default_index")]
        index: String,
        body: Vec<Step>,
    },
    /// Run `body` while `cond` holds, at most `max` times (at most
    /// [`MAX_REPEAT`](crate::run::MAX_REPEAT)). A `max` of zero repeats
    /// nothing: no loop in a shortcut is unbounded.
    While {
        cond: Condition,
        max: u64,
        body: Vec<Step>,
    },
    /// Stop the run there, reporting `value` as its output: a step inside a
    /// branch or a loop can finish a run that has nothing left to do.
    Return {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Binding>,
    },
    /// Ask the person running the shortcut for one value. The answer is this
    /// step's output. Unattended runs cannot ask and fail here.
    Ask { param: ActionParam },
    /// Answer `prompt` with the agent in one model call, as a value of
    /// `output`. Lowered to an invocation of the agent's `freeform`
    /// transform, whose result is held to the declared shape.
    Ai {
        prompt: String,
        /// The JSON Schema the answer must satisfy; absent for text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
        /// What the agent reads: text, a content address, or the description
        /// of a resource it may open itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Binding>,
    },
    /// Start a task with the agent — a model-driven run of the actions the
    /// person is offered — and wait for its typed summary.
    Task {
        /// What the task is to do, in the person's words.
        instruction: String,
        /// What the person can run it with: each one narrows the task's
        /// authority below the run's, and never widens it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<Rule>,
        /// The JSON Schema the summary must satisfy; absent for text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
}

fn default_item() -> String {
    "item".into()
}

fn default_index() -> String {
    "index".into()
}

impl StepKind {
    /// The agent transform an AI or task step lowers to; `None` for a step
    /// that does not ask the agent anything.
    #[must_use]
    pub fn ai_transform(&self) -> Option<AiTransform> {
        Some(match self {
            Self::Ai { prompt, output, .. } => AiTransform::Freeform {
                prompt: prompt.clone(),
                output: output.clone(),
            },
            Self::Task {
                instruction,
                allow,
                output,
            } => AiTransform::Task {
                instruction: instruction.clone(),
                allow: allow.clone(),
                output: output.clone(),
            },
            _ => return None,
        })
    }
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
    /// One of the shortcut's inputs, or part of it: `NAME/json/pointer`
    /// addresses into an object input (`event/after/text`).
    Input(String),
    /// A variable set by [`StepKind::SetVar`] or a loop's item, or part of
    /// it, addressed as an input is (`item/op`).
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
    /// A JSON object of bound values, a member left out when its binding
    /// has no value. A value built of parts shows its shape to whoever reads
    /// the program: a field edit's path names the field it changes, so its
    /// grant reaches that field only ([`crate::grant::edit_reach`]).
    Object(BTreeMap<String, Binding>),
    /// A JSON array of bound values, `null` where a binding has no value.
    List(Vec<Binding>),
    /// The value of an expression: arithmetic, a comparison, a text, list or
    /// dictionary operation ([`Expr`]). Boxed because an expression may hold
    /// bindings of its own.
    #[serde(rename = "expr")]
    Expression(Box<Expr>),
}

impl Binding {
    /// The input or variable an [`Input`](Self::Input) or [`Var`](Self::Var)
    /// name refers to, and the RFC 6901 pointer into its value (empty for
    /// the whole value).
    #[must_use]
    pub fn named_part(name: &str) -> (&str, String) {
        match name.split_once('/') {
            Some((base, rest)) => (base, format!("/{rest}")),
            None => (name, String::new()),
        }
    }

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
    /// An expression that answers a truth value: an ordering comparison, a
    /// bare boolean, or a value judged by its truthiness. A comparison is
    /// refused here before anything runs when its two sides cannot be
    /// compared.
    Holds(Expr),
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
    /// Answers `prompt`, as text or as a value of `output`.
    Freeform {
        prompt: String,
        /// The JSON Schema the answer must satisfy; absent for text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
    /// Runs a whole task ([`StepKind::Task`]) and returns its summary.
    Task {
        instruction: String,
        /// The task's authority, as the grant node of each action it may take.
        allow: Vec<Rule>,
        /// The JSON Schema the summary must satisfy; absent for text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
}

impl AiTransform {
    /// The agent transform action this lowers to.
    #[must_use]
    pub fn key(&self) -> ActionKey {
        ActionKey::new(AGENT_APP_ID, AGENT_TRANSFORM_KIND, self.action())
    }

    /// The public id of the agent transform action this lowers to.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Summarise => AGENT_SUMMARISE_ACTION,
            Self::Extract { .. } => AGENT_EXTRACT_ACTION,
            Self::Classify { .. } => AGENT_CLASSIFY_ACTION,
            Self::Freeform { .. } => AGENT_FREEFORM_ACTION,
            Self::Task { .. } => AGENT_TASK_ACTION,
        }
    }

    /// The JSON Schema the answer must satisfy, or `None` for text.
    #[must_use]
    pub const fn output(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Extract { schema } => Some(schema),
            Self::Task { output, .. } | Self::Freeform { output, .. } => output.as_ref(),
            Self::Summarise | Self::Classify { .. } => None,
        }
    }

    /// The arguments of the lowered invocation. `input` is what the step
    /// reads, when it reads anything.
    #[must_use]
    pub fn arguments(&self, input: Option<&Binding>) -> BTreeMap<String, Binding> {
        // The declared shape is what the agent validates the answer against,
        // so it travels as the step declared it.
        let mut args = BTreeMap::new();
        if let Some(output) = self.output() {
            args.insert("schema".into(), Binding::Literal(output.clone()));
        }
        let text = |value: &str| Binding::Literal(serde_json::json!(value));
        match self {
            Self::Summarise | Self::Extract { .. } => {}
            Self::Classify { choices } => {
                args.insert(
                    "choices".into(),
                    Binding::Literal(serde_json::json!(choices)),
                );
            }
            Self::Freeform { prompt, .. } => {
                args.insert("prompt".into(), text(prompt));
            }
            Self::Task {
                instruction, allow, ..
            } => {
                args.insert("instruction".into(), text(instruction));
                args.insert("allow".into(), Binding::Literal(serde_json::json!(allow)));
            }
        }
        if let Some(input) = input {
            args.insert("input".into(), input.clone());
        }
        args
    }
}

// A Shortcuts draft stores a step's selector, bindings, condition and
// transform and a shortcut's triggers as their JSON forms, as maps, lists and
// scalars: a condition is made of conditions, which a derived model cannot
// describe, and each is written whole by the editor.
#[cfg(feature = "model")]
tilo_resource::value_model!(
    Selector,
    Binding,
    Condition,
    Expr,
    AiTransform,
    TriggerSpec,
    StepPolicy
);

/// What may start a shortcut. Data only: the session decides how to watch
/// for each and starts an unattended run when one fires.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// A committed change to a record of `kind` in the room. Empty
    /// `actions` and `fields` match any change; otherwise a change matches
    /// when it ran one of the verbs (public ids) or edited a field whose
    /// path matches one of the patterns (`channels.*.name`).
    RecordChanged {
        kind: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<String>,
        #[serde(default, skip_serializing_if = "ActorFilter::is_anyone")]
        by: ActorFilter,
    },
    /// An event from a connector's declared trigger source on the room's
    /// host. Only a bot can carry one: a person's devices see no connector.
    Connector {
        app_id: String,
        source: String,
        /// Equality on top-level payload fields.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        filter: serde_json::Map<String, serde_json::Value>,
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

/// Whose changes a [`TriggerSpec::RecordChanged`] counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorFilter {
    #[default]
    Anyone,
    /// Only people acting at their own devices.
    People,
    /// People, agents and Shortcuts, never a bot.
    NotBots,
}

impl ActorFilter {
    #[must_use]
    pub const fn is_anyone(&self) -> bool {
        matches!(self, Self::Anyone)
    }
}

/// The input a record, connector or incoming-message firing binds its
/// event to.
pub const EVENT_INPUT: &str = "event";

static EVENT_PAYLOAD: [ActionParam; 1] = [ActionParam::required(
    EVENT_INPUT,
    ParamType::Object,
    "What fired the run: the change or the connector event",
)];

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
            Self::RecordChanged { .. } | Self::Connector { .. } | Self::IncomingMessage { .. } => {
                &EVENT_PAYLOAD
            }
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
            Self::RecordChanged { .. } => "record_changed",
            Self::Connector { .. } => "connector",
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
    pub device: String,
}

impl ScheduleClaim {
    /// The schedule this claims a fire time of: `shortcut/trigger`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.shortcut, self.trigger)
    }

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
