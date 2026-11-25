//! The serialisable shortcut program.
//!
//! Everything here is inert data: it can be stored, synced, diffed and shown
//! in an editor without a host. Meaning comes from [`check`](fn@crate::check),
//! which types it against live descriptors, and [`run`](fn@crate::invocation::run), which
//! executes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_action_policy::Rule;
use tilo_actions::ActionKey;
use tilo_actions::api::ActionAddress;
use tilo_actions::params::{ActionParam, ParamType};

use crate::{
    AGENT_APP_ID, AGENT_CLASSIFY_ACTION, AGENT_EXTRACT_ACTION, AGENT_FREEFORM_ACTION,
    AGENT_SUMMARISE_ACTION, AGENT_TASK_ACTION, AGENT_TRANSFORM_KIND,
};

/// One saved shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Shortcut {
    /// Stable id; also the tail of the resource id the shortcut is published
    /// under ([`crate::shortcut_resource_id`]).
    pub id: String,
    /// The person's name for it. Carried into history as the purpose of every
    /// action the shortcut takes.
    pub name: String,
    /// What an invocation is started with, in the same typed vocabulary actions use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionParam>,
    pub steps: Vec<Step>,
    /// How long one step may take before the invocation stops waiting for it, in
    /// seconds. Absent, [`DEFAULT_STEP_TIMEOUT`](crate::invocation::DEFAULT_STEP_TIMEOUT)
    /// applies. An invocation nobody is present at needs a bound: a step whose action
    /// never answers would otherwise hold the invocation — and the one on its heels
    /// — for ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout_secs: Option<u64>,
    /// The value a finished invocation reports back to whoever started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Binding>,
    /// The most the shortcut may do, shown when it is imported. An invocation acts
    /// with the stricter of this and the authority of the person who started
    /// it: a step outside the grant is refused here, a step outside the
    /// person's authority is refused by the host. An invocation nobody is present at
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
    /// The saver's device signing this shortcut, as saved in one workspace
    /// ([`Self::saved_signing_digest`]). Who saved a shortcut is authority (its
    /// triggers fire on that person's behalf), so `saved_by` counts only with
    /// a proof that verifies under that person's root. A shortcut without one
    /// runs by hand but never arms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_proof: Option<SavedProof>,
    /// Whether it is paused: a paused shortcut fires nothing, and what comes while it is paused
    /// is never answered later. It is no part of what its saver signed
    /// ([`Self::saved_signing_digest`]), so pausing and resuming it keeps its saver's proof, and
    /// a build that does not know it reads the same proof.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
}

/// A device's signature over one saved shortcut in one workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedProof {
    /// The signing device's certificate, as its identity layer encodes it.
    pub device_cert: serde_json::Value,
    /// The device's Ed25519 signature over [`Shortcut::saved_signing_digest`],
    /// lowercase hex.
    pub signature: String,
}

/// Domain of [`Shortcut::saved_signing_digest`].
pub const SAVED_SHORTCUT_DOMAIN: &str = "tilo-shortcut-saved-v1";

impl Shortcut {
    /// The address that runs this shortcut through the action API.
    #[must_use]
    pub fn action_address(&self) -> ActionAddress {
        crate::shortcut_run_address(&self.id)
    }

    /// What the saver's device signs: the workspace the shortcut is saved in
    /// and the shortcut itself, `saved_by` included and the proof left out,
    /// so a proof neither moves to another workspace nor covers a changed
    /// shortcut.
    #[must_use]
    pub fn saved_signing_digest(&self, workspace: &str) -> [u8; 32] {
        let mut unsigned = self.clone();
        unsigned.saved_proof = None;
        let mut hasher = blake3::Hasher::new_derive_key(SAVED_SHORTCUT_DOMAIN);
        hasher.update(&(workspace.len() as u64).to_le_bytes());
        hasher.update(workspace.as_bytes());
        // Writing a shortcut to a Vec cannot fail.
        hasher.update(&serde_json::to_vec(&unsigned).unwrap_or_default());
        *hasher.finalize().as_bytes()
    }
}

/// A shortcut to store, or remove, in one workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutChange {
    Save(Box<Shortcut>),
    /// Remove the saved shortcut with this id.
    Delete(String),
}

/// One step, named so later steps can refer to its output.
///
/// Written as one flat map: the step's own fields and its kind's. A
/// `#[serde(flatten)]` would write the same JSON, but it opens a map whose
/// length it cannot state, which a non-self-describing format such as the
/// host's postcard cannot encode; a shortcut is hashed in that form when a
/// device commits its save. [`Step`] therefore writes the map itself, with
/// its length known, and reads the flat form back through the fields.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(remote = "Self")]
pub struct Step {
    /// Unique within the shortcut; ASCII letters, digits, `-` and `_`.
    pub id: String,
    /// What the invocation does when the step does not succeed. Absent, the step
    /// must succeed and the invocation stops at it
    /// ([`StepPolicy::Fail`]).
    #[serde(default, skip_serializing_if = "StepPolicy::is_fail")]
    pub on_error: StepPolicy,
    #[serde(flatten)]
    pub kind: StepKind,
}

impl Serialize for Step {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::{Error as _, SerializeMap as _};
        let kind = serde_json::to_value(&self.kind).map_err(S::Error::custom)?;
        let fields: std::collections::BTreeMap<String, serde_json::Value> = kind
            .as_object()
            .ok_or_else(|| S::Error::custom("a step's kind is written as a map"))?
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let policy = !StepPolicy::is_fail(&self.on_error);
        let fields_count = fields
            .len()
            .saturating_add(1)
            .saturating_add(usize::from(policy));
        let mut map = serializer.serialize_map(Some(fields_count))?;
        map.serialize_entry("id", &self.id)?;
        if policy {
            map.serialize_entry("on_error", &self.on_error)?;
        }
        // Sorted by name, so a proof over the bytes does not depend on how
        // serde_json's map keeps its order in the build that writes it.
        for (name, value) in fields {
            map.serialize_entry(&name, &value)?;
        }
        map.end()
    }
}

impl Step {
    /// The step must succeed; when it does not, the invocation stops at it.
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

/// What an invocation does with a step that did not succeed.
///
/// Whatever the policy, a failure is recorded with the step and shown: a
/// step that is retried, sent into a fallback or passed over is never the
/// same as one that worked.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "on_error", rename_all = "snake_case")]
pub enum StepPolicy {
    /// Stop the invocation here.
    #[default]
    Fail,
    /// Try the step again `attempts` times in all, waiting `backoff_ms`
    /// before the second attempt and doubling the wait each time after it,
    /// and stop the invocation if the last one fails too. A retry sends the same
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
    /// can be taken back by simply not using it, and mark the invocation degraded.
    Default { value: Binding },
}

impl StepPolicy {
    #[must_use]
    pub const fn is_fail(&self) -> bool {
        matches!(self, Self::Fail)
    }

    /// How long the invocation waits before attempt `attempt` (the second one
    /// being 2): `backoff_ms`, doubling each time, at most
    /// [`MAX_BACKOFF_MS`](crate::invocation::MAX_BACKOFF_MS).
    #[must_use]
    pub fn wait_before(&self, attempt: u32) -> std::time::Duration {
        match self {
            Self::Retry { backoff_ms, .. } => {
                let wait = backoff_ms
                    .saturating_mul(1_u64 << attempt.saturating_sub(2).min(32))
                    .clamp(1, crate::invocation::MAX_BACKOFF_MS);
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
    crate::invocation::DEFAULT_BACKOFF_MS
}

/// What a step does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "step", rename_all = "snake_case")]
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
        /// Discord user's message under their name. Only a bot's action API
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
    /// Ask the person running the shortcut for one value. The answer is this
    /// step's output. Unattended invocations cannot ask and fail here.
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
    /// Start a task with the agent — a model-driven session of the actions the
    /// person is offered — and wait for its typed summary.
    Task {
        /// What the task is to do, in the person's words.
        instruction: String,
        /// What the person can run it with: each one narrows the task's
        /// authority below the invocation's, and never widens it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<Rule>,
        /// The JSON Schema the summary must satisfy; absent for text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
    /// Read one record of the workspace as it is now, through the record
    /// API every reader reads records through: `record` of `app` (empty for
    /// the app's record of the workspace; a chain's head for its newest
    /// part), or with `joined` the chain of parts whose base it is. `record`
    /// may name it from the invocation with `{{...}}` placeholders, as a template
    /// does: the record an event changed. The output is the API's answer,
    /// `{revision, records: [{record, value | absent | arriving}]}`. A read
    /// changes nothing; what the invocation does with it, it does through actions.
    Read {
        app: String,
        #[serde(default)]
        record: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        joined: bool,
    },
}

/// The binding a read step's `record` is, when it names its record from the
/// run with `{{...}}` placeholders ([`StepKind::Read`]).
#[must_use]
pub fn read_record_binding(record: &str) -> Option<Binding> {
    record
        .contains("{{")
        .then(|| Binding::Template(record.to_owned()))
}

fn default_item() -> String {
    "item".into()
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
    /// The resource address a binding evaluates to when the step runs.
    FromBinding(Binding),
    /// The first published resource of this kind (or a sub-kind of it) that
    /// offers the action.
    FirstOfKind(String),
}

/// Where a value comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
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
#[serde(remote = "Self", rename_all = "snake_case")]
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
    AiTransform,
    TriggerSpec,
    StepPolicy
);

/// What may start a shortcut. Data only: the session decides how to watch
/// for each and starts an unattended invocation when one fires.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "trigger", rename_all = "snake_case")]
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
    /// One moment, in milliseconds since the Unix epoch. The trigger fires
    /// once when the moment has come, however late whoever keeps the time
    /// gets to it (a host that was down then fires it when it is up again),
    /// and never a second time.
    Once {
        at_ms: i64,
    },
    AppOpened {
        app_id: String,
    },
    /// Joining a workspace; `None` for any.
    WorkspaceJoined {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
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
    /// A committed change to a record of `kind` in the workspace. Empty
    /// `actions` and `fields` match any change; otherwise a change matches
    /// when it ran one of the verbs (public ids) or edited a field whose
    /// path matches one of the patterns (`channels.*.name`). `filter` then
    /// narrows it by what the change holds ([`ChangeFilter`]): who made it,
    /// whose change it copies, the verb's arguments and the changed item's
    /// fields. Who made a change does not decide on its own whether it
    /// counts: a change written for someone who is no member (a message a
    /// service carries in) matches like any other.
    ResourceChanged {
        kind: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<String>,
        #[serde(default, skip_serializing_if = "ActorFilter::is_anyone")]
        by: ActorFilter,
        #[serde(default, skip_serializing_if = "ChangeFilter::is_empty")]
        filter: ChangeFilter,
    },
    /// An event from a connector's declared trigger source, reported by the
    /// device that runs the connector: the app the connector belongs to, the
    /// source it named, and equality on top-level payload fields to narrow
    /// which events fire it.
    Connector {
        app_id: String,
        source: String,
        /// Equality on top-level payload fields.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        filter: serde_json::Map<String, serde_json::Value>,
    },
    /// An event an application declared next to its actions, e.g.
    /// `mail.message`: published as a change to a record of kind
    /// `<app_id>.<kind>.event`, whose arguments are the event's payload.
    /// The payload is the typed input.
    AppEvent {
        app_id: String,
        kind: String,
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
    /// A moment before, at or after a time a resource holds: the field
    /// `field` of each item of a resource of `kind`, and every occurrence of
    /// an entry that repeats (`start` of the Calendar app's entries,
    /// [`crate::resource_time::CALENDAR_EVENTS`]). `offset_secs` is counted
    /// from that time, negative for before it. `filter` is equality on the
    /// item's own fields ([`crate::resource_time::matches`]). The trigger
    /// fires once for each time held, again when that time moves, and never
    /// for a time that is no longer held ([`crate::resource_time::due`]).
    ResourceTime {
        kind: String,
        field: String,
        #[serde(default)]
        offset_secs: i64,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        filter: serde_json::Map<String, serde_json::Value>,
    },
    /// Something new in a source: what it holds differs from what it held
    /// when whoever keeps the automation last looked
    /// ([`crate::source_change`]). `kind` says what sort of source it is
    /// ([`crate::source_change::WEB_FEED`]) and `at` which one of that
    /// sort, empty for every one of them. `filter` is equality on the fields
    /// of what is new ([`crate::resource_time::matches`]). The trigger fires
    /// once for each new thing, never for what the source held before the
    /// trigger was first seen, and what is new goes with the invocation as
    /// its event. A source that tells nobody of what is new is looked at on a
    /// clock ([`crate::source_change::looked_at`]), every `every_secs`
    /// seconds, or as often as whoever looks keeps to when it names none.
    SourceChanged {
        kind: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        every_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        filter: serde_json::Map<String, serde_json::Value>,
    },
    /// An account's webhook: its workspace's events posted to `url` from the device that keeps
    /// the workspace, signed by that device's key. The shortcut is the webhook: saved as its own
    /// record, armed here, and its run posts one event ([`WEBHOOK_POST`](crate::WEBHOOK_POST)).
    /// `events` names the kinds posted ([`WEBHOOK_EVENTS`](crate::WEBHOOK_EVENTS)); empty posts
    /// every one. `path` narrows a change to one record. `owner` is the key that registered it,
    /// by its device key in hex: listing and deleting are that key's alone.
    Webhook {
        url: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        owner: String,
        /// Events before this moment, ms since the epoch, are not posted: what was there when
        /// the webhook was made is not news.
        #[serde(default)]
        since_ms: i64,
    },
}

/// What a [`TriggerSpec::ResourceChanged`] asks of a change beyond its kind, its verbs and its
/// fields: every test must hold. Each names a field of what the change holds by a dotted path
/// ([`ChangeFilter::subject`]): `by`, the member who made it (`agent:<id>`, a person's id,
/// `app:<id>`); `origin`, the member whose change this one copies, when a writer copied one
/// ([`tilo_actions::ORIGIN_FACET`]); `arguments.<name>`, what the verb was given; `item.<name>`,
/// the changed item's fields after the change; `kind`, the record's kind;
/// `incoming_message`, whether the change brings a message the workspace received, as a store's
/// own outside-origin write says it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeFilter(pub Vec<FieldTest>);

/// One test of a [`ChangeFilter`]: the field at `field` and what it must be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldTest {
    /// The dotted path of the field in [`ChangeFilter::subject`].
    pub field: String,
    #[serde(flatten)]
    pub test: Test,
}

/// What a field must be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Test {
    /// Equal to the value.
    Is(serde_json::Value),
    /// Missing, or anything but the value.
    IsNot(serde_json::Value),
    /// A list holding the value, a map with it as a key, or text containing it, anywhere within
    /// the field.
    Has(serde_json::Value),
    /// Missing, or holding the value nowhere within it.
    Lacks(serde_json::Value),
}

impl ChangeFilter {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A filter of `tests`.
    #[must_use]
    pub fn of(tests: impl IntoIterator<Item = (impl Into<String>, Test)>) -> Self {
        Self(
            tests
                .into_iter()
                .map(|(field, test)| FieldTest {
                    field: field.into(),
                    test,
                })
                .collect(),
        )
    }

    /// What the tests read of a change: who made it, whose change it copies, the verb's
    /// arguments, the changed item, the record's kind and whether it brings a received message.
    #[must_use]
    pub fn subject(event: &crate::dispatch::DocumentEvent) -> serde_json::Value {
        serde_json::json!({
            "by": String::from_utf8_lossy(&event.actor.by.actor_bytes()),
            "origin": event.origin,
            "kind": event.kind,
            "arguments": event.arguments,
            "item": event.after,
            "incoming_message": event.incoming_message,
        })
    }

    /// Whether every test holds of `subject` ([`Self::subject`]).
    #[must_use]
    pub fn holds(&self, subject: &serde_json::Value) -> bool {
        self.0.iter().all(|test| {
            let field = test
                .field
                .split('.')
                .try_fold(subject, |value, step| match value {
                    serde_json::Value::Object(fields) => fields.get(step),
                    serde_json::Value::Array(items) => {
                        step.parse::<usize>().ok().and_then(|at| items.get(at))
                    }
                    _ => None,
                })
                .filter(|value| !value.is_null());
            match &test.test {
                Test::Is(wanted) => field == Some(wanted),
                Test::IsNot(wanted) => field != Some(wanted),
                Test::Has(wanted) => field.is_some_and(|field| holds_within(field, wanted)),
                Test::Lacks(wanted) => !field.is_some_and(|field| holds_within(field, wanted)),
            }
        })
    }
}

/// Whether `wanted` is in `value`: an element or key of it or of anything it holds, or a part of
/// a text it holds.
fn holds_within(value: &serde_json::Value, wanted: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => {
            wanted.as_str().is_some_and(|wanted| text.contains(wanted))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| item == wanted || holds_within(item, wanted)),
        serde_json::Value::Object(fields) => fields.iter().any(|(key, item)| {
            wanted.as_str() == Some(key.as_str()) || item == wanted || holds_within(item, wanted)
        }),
        _ => value == wanted,
    }
}

/// Whose changes a [`TriggerSpec::ResourceChanged`] counts.
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
    "What fired the invocation: the change, the connector event or the fire time",
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
    /// What a firing of this trigger tells the invocation it starts. Each value is
    /// bound to the shortcut's input of the same name, when it declares one
    /// of the same type.
    #[must_use]
    pub fn payload(&self) -> &'static [ActionParam] {
        match self {
            Self::AppOpened { .. } => &APP_PAYLOAD,
            Self::WorkspaceJoined { .. } => &WORKSPACE_PAYLOAD,
            Self::FileChanged { .. } => &FILE_PAYLOAD,
            Self::FocusMode { .. } => &FOCUS_PAYLOAD,
            Self::ResourceChanged { .. }
            | Self::Connector { .. }
            | Self::IncomingMessage { .. }
            | Self::AppEvent { .. }
            | Self::Schedule { .. }
            | Self::Once { .. }
            | Self::ResourceTime { .. }
            | Self::SourceChanged { .. }
            | Self::Webhook { .. } => &EVENT_PAYLOAD,
            _ => &[],
        }
    }

    /// Whether this trigger is an account's webhook ([`Self::Webhook`]).
    #[must_use]
    pub const fn is_webhook(&self) -> bool {
        matches!(self, Self::Webhook { .. })
    }

    /// Stable name of the trigger's kind, recorded with every invocation it starts.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Keyboard { .. } => "keyboard",
            Self::Schedule { .. } => "schedule",
            Self::Once { .. } => "once",
            Self::AppOpened { .. } => "app_opened",
            Self::WorkspaceJoined { .. } => "workspace_joined",
            Self::FileChanged { .. } => "file_changed",
            Self::FocusMode { .. } => "focus_mode",
            Self::NetworkChanged { .. } => "network_changed",
            Self::DeviceConnected { .. } => "device_connected",
            Self::ResourceChanged { .. } => "resource_changed",
            Self::Connector { .. } => "connector",
            Self::AppEvent { .. } => "app_event",
            Self::IncomingMessage { .. } => "incoming_message",
            Self::NearbyDevice { .. } => "nearby_device",
            Self::ResourceTime { .. } => "resource_time",
            Self::SourceChanged { .. } => "source_changed",
            Self::Webhook { .. } => "webhook",
        }
    }
}

/// When a scheduled trigger fires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "every", rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusTransition {
    Enter,
    Leave,
}

// Read and written as JSON through one `Value`, by code compiled once here, so
// an app that keeps shortcuts compiles none of these types' serde code; the
// host's binary encoding keeps the derived code.
tilo_actions::readable_through_json_value!(Shortcut);
tilo_actions::readable_through_json_value!(Step, read);
tilo_actions::readable_through_json_value!(StepPolicy);
tilo_actions::readable_through_json_value!(StepKind);
tilo_actions::readable_through_json_value!(Binding);
tilo_actions::readable_through_json_value!(Condition);
tilo_actions::readable_through_json_value!(TriggerSpec);
tilo_actions::readable_through_json_value!(Schedule);
