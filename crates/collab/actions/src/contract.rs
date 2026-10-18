//! Machine-readable operational semantics for exported actions.
//!
//! Labels explain an action to a person. This module explains it to callers
//! that must plan safely without seeing the application: whether it is
//! currently available, what confirmation it needs, whether a failed call may
//! be retried, how it completes, and what it returns.

use serde::{Deserialize, Serialize};
pub use tilo_action_policy::ActionKind;

use crate::action::OperationKind;

/// Whether an action can be invoked in the resource's current state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "state", rename_all = "snake_case")]
pub enum Availability {
    #[default]
    Available,
    Unavailable {
        /// Stable programmatic reason such as `offline` or `selection_empty`.
        code: String,
        /// Localised explanation suitable for a person or an agent transcript.
        reason: String,
    },
}

read_through_json_value!(Availability);

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Consequence class used by policy, confirmation UI, and audit logs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Presentation-only or otherwise observational.
    None,
    /// A reversible edit or ordinary external effect.
    #[default]
    Low,
    /// Deletes, overwrites, publishes, purchases, or otherwise causes a
    /// consequential change that is not cheaply reversible.
    Destructive,
    /// Crosses an administrative or security boundary.
    Privileged,
    /// Reads or exports private user data. It needs an explicit confirmation,
    /// but unlike deletion it does not itself alter the source resource.
    ///
    /// This is deliberately appended: `RiskLevel` is also encoded by compact
    /// serde formats, where reordering variants would reinterpret old audit
    /// records and envelopes.
    Sensitive,
}

impl RiskLevel {
    /// Whether a change of this class must be explicitly confirmed before it
    /// runs: deletions and overwrites, crossings of a security boundary, and
    /// exports of private data.
    pub fn requires_confirmation(self) -> bool {
        matches!(self, Self::Destructive | Self::Privileged | Self::Sensitive)
    }

    /// The level's name, as it is written outside a typed message (a
    /// question a host relays, an activity facet).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Destructive => "destructive",
            Self::Privileged => "privileged",
            Self::Sensitive => "sensitive",
        }
    }

    /// The level [`Self::name`] names; `None` for any other word.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::None,
            Self::Low,
            Self::Destructive,
            Self::Privileged,
            Self::Sensitive,
        ]
        .into_iter()
        .find(|level| level.name() == name)
    }
}

/// [`ActionContract::tags`] entry of an action any member of a room may ask
/// the room's host to run for themselves on a record of the room (linking
/// their own account elsewhere). The host runs it as that member, under
/// their own standing, so the action's door and its own checks decide what
/// it may do; its answer goes back to the asking device alone.
pub const MEMBER_OWN_TAG: &str = "host:member-own";

/// [`ActionContract::tags`] entry of an action offered only to programmatic
/// callers — agents, scripts and the controls or surfaces that submit it —
/// and never listed for a person to pick (the action palette, menus).
pub const PROGRAMMATIC_TAG: &str = "surface:programmatic";

/// [`ActionContract::tags`] entry of an action whose result carries content
/// from outside the user's own data (a web page's text, its logs): data for
/// the caller to reason about, never instructions to follow.
pub const UNTRUSTED_CONTENT_TAG: &str = "content:untrusted";

/// [`ActionContract::tags`] entry of an action whose risk, and therefore
/// whether it needs confirmation, depends on the arguments it is invoked
/// with (a generic edit that may delete). The publishing host classifies each
/// invocation; a caller that cannot classify arguments treats the action as
/// requiring confirmation (see [`ActionContract::advertised_confirmation`]).
pub const ARGUMENT_RISK_TAG: &str = "confirmation:by-argument";

/// The parameter of a generic field edit that carries the edit.
pub const FIELD_EDIT_PARAM: &str = "edit";

/// The operation and path of a field edit in its JSON form
/// (`{path: [{field: name} | {item: id} | {key: k} | {cell: ..}], op, ..}`).
fn edit_parts(
    edit: &serde_json::Value,
) -> Option<(&str, Vec<tilo_action_policy::edits::EditStep<'_>>)> {
    use tilo_action_policy::edits::EditStep;
    let op = edit.get("op")?.as_str()?;
    let path = edit
        .get("path")?
        .as_array()?
        .iter()
        .map(|step| {
            let (kind, value) = step.as_object()?.iter().next()?;
            Some(match kind.as_str() {
                "field" => EditStep::Field(value.as_str()?),
                _ => EditStep::Item,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((op, path))
}

/// [`ActionContract::tags`] entry of the read-only action that answers, when
/// invoked with no arguments, the first page of the document an app has
/// open (a word processor's text, a sheet's used cells, a deck's outline).
/// A caller that just opened a file reads it through this action without
/// knowing the app.
pub const DOCUMENT_READ_TAG: &str = "read:document";

/// [`ActionContract::tags`] entry of the action that opens the file its
/// `path` parameter names in this app instance, reading it inside the app's
/// sandbox. A caller that launched the app with a file opens it through
/// this action, and only through an action that declares it: one taking a
/// `path` for another purpose (importing an installer image, exporting)
/// is never guessed from its name.
pub const OPENS_PATH_TAG: &str = "open:path";

/// Prefix of the [`ActionContract::tags`] entries naming the file
/// extensions (lowercase, without the dot) an [`OPENS_PATH_TAG`] action
/// opens: `open:path-ext:csv`. An opener naming none opens any file.
pub const OPENS_PATH_EXTENSION_PREFIX: &str = "open:path-ext:";

/// Facet the executor writes on a change made by a verb its app marks
/// `#[action(incoming_message)]`: a message someone received. The
/// `IncomingMessage` trigger fires on it without knowing the app's schema.
pub const INCOMING_MESSAGE_FACET: &str = "incoming-message";
/// Contract tag on the declaration of such a verb, so a surface and a
/// catalogue show it.
pub const INCOMING_MESSAGE_TAG: &str = "trigger:incoming-message";

/// [`ActionContract::tags`] entry of an action that reaches outside its
/// document through a queue (a file rename the device carries out) and whose
/// undo queues the compensating change. It is still an
/// [`Outside`](ActionKind::Outside) action for the grants deciding it, but it
/// can be taken back: a person reviewing it is told it changes their work,
/// not that it sends something away.
pub const COMPENSATED_UNDO_TAG: &str = "undo:compensated";

/// Contract tag on the declaration of a housekeeping verb: a change about the
/// record rather than to it (`#[action(housekeeping)]`, such as marking a
/// channel read). An app does these on its own, so nobody is asked to review
/// or confirm one: one an app emits while History shows a past version is
/// dropped, and the app does it again once the person is back.
pub const HOUSEKEEPING_TAG: &str = "change:housekeeping";

/// What one invocation of an action is, for the grants deciding it: its kind,
/// the one field of its resource it writes when it writes only one (a reply
/// writes the `comments`), which places it below that field in the tree, and
/// whether it is privileged ([`RiskLevel::Privileged`]): it exercises
/// authority over the room or others in it, so the person's own grants must
/// allow it (`tilo_action_policy::Verdict::privileged`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionNeed {
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub privileged: bool,
}

/// The node of the action `action` on the resource `resource` (of kind
/// `resource_kind`) the app `app` publishes, below `base` (a room, or the
/// root for a path relative to one), and below the one `field` it writes if
/// any: where every door decides it.
#[must_use]
pub fn action_node(
    base: &tilo_action_policy::TreePath,
    app: &str,
    resource_kind: &str,
    resource: &str,
    field: Option<&str>,
    action: &str,
) -> tilo_action_policy::TreePath {
    let node = base.clone().app(app).kind(resource_kind).resource(resource);
    match field {
        Some(field) => node.field(field),
        None => node,
    }
    .action(action)
}

impl ActionNeed {
    /// An action of `kind` on the resource as a whole.
    #[must_use]
    pub const fn of(kind: ActionKind) -> Self {
        Self {
            kind,
            field: None,
            privileged: false,
        }
    }

    /// An undoable change of the resource's comments: what someone who may
    /// only comment may do.
    #[must_use]
    pub fn comments() -> Self {
        Self {
            kind: ActionKind::Undoable,
            field: Some(tilo_action_policy::roles::COMMENTS_FIELD.to_owned()),
            privileged: false,
        }
    }

    /// An undoable change under the resource's `moderation` node: acting on
    /// other people's content, which Moderators and the owner may do.
    #[must_use]
    pub fn moderation() -> Self {
        Self {
            kind: ActionKind::Undoable,
            field: Some(tilo_action_policy::roles::MODERATION_FIELD.to_owned()),
            privileged: false,
        }
    }
}

const fn undoable() -> ActionKind {
    ActionKind::Undoable
}

/// Confirmation required after argument binding and immediately before run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "mode", rename_all = "snake_case")]
pub enum ConfirmationPolicy {
    #[default]
    None,
    Confirm,
    /// Requires an exact phrase, useful for irreversible bulk operations.
    Phrase {
        phrase: String,
    },
}

read_through_json_value!(ConfirmationPolicy);

/// What a caller may safely do after losing the completion response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    /// Repeating the call has the same externally observable result.
    Idempotent,
    /// The implementation deduplicates calls by invocation id.
    RetryWithInvocationId,
    /// Never automatically repeat; the first call may have completed.
    #[default]
    AtMostOnce,
}

/// Completion behavior advertised before invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "mode", rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Expected to finish in the request/response window.
    #[default]
    Immediate,
    /// Returns a durable job handle and may report progress.
    Background { cancellable: bool },
    /// Produces ordered values, each checked against `item_schema`, until a
    /// terminal result.
    Streaming {
        cancellable: bool,
        item_schema: serde_json::Value,
    },
}

read_through_json_value!(ExecutionMode);

/// Shape of a successful result. Schemas use JSON Schema so non-Rust callers
/// do not need a parallel Tilo-only type system.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "type", rename_all = "snake_case")]
pub enum ResultSpec {
    #[default]
    None,
    Json {
        schema: serde_json::Value,
    },
    Resource {
        resource_kind: String,
    },
    Job,
}

read_through_json_value!(ResultSpec);

impl ResultSpec {
    /// Check that a completed action returned the shape it advertised.
    ///
    /// # Errors
    ///
    /// Returns a human-readable contract violation when `output` does not
    /// match this result specification.
    pub fn validate(&self, output: &crate::ActionOutput) -> Result<(), String> {
        match self {
            // An action with no result may still name what it created.
            Self::None | Self::Job if output.value.is_none() => Ok(()),
            Self::None => Err("action declared no result but returned one".into()),
            // An absent value is `null`: a result its schema lets be null may be
            // left out by an executor with nothing to report.
            Self::Json { schema } => match output.value.as_ref() {
                Some(value) => crate::params::validate_schema(value, schema),
                None => crate::params::validate_schema(&serde_json::Value::Null, schema)
                    .map_err(|_| "action did not return its declared JSON result".to_string()),
            },
            Self::Resource { resource_kind }
                if !output.resources.is_empty()
                    && output.resources.iter().all(|address| {
                        address == resource_kind
                            || address.strip_prefix(resource_kind).is_some_and(|suffix| {
                                suffix.starts_with(':') || suffix.starts_with('/')
                            })
                    }) =>
            {
                Ok(())
            }
            Self::Resource { resource_kind } => Err(format!(
                "action did not return a `{resource_kind}` resource address"
            )),
            // The broker assigns and returns the durable invocation id at
            // admission. Applications must not mint a second, unrelated job
            // identity merely to satisfy the result validator.
            Self::Job => Err("job action returned an undeclared terminal value".into()),
        }
    }
}

/// The contract tag of an action that reaches files, hardware or services
/// of the device it runs on: the door runs it only for the person at that
/// device, never for a collaborator or a remote participant.
pub const LOCAL_DEVICE_ONLY: &str = "tilo.local-device-only";

/// Complete operational contract copied into every inert descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionContract {
    #[serde(default)]
    pub availability: Availability,
    /// What the action is able to do: read, change only the document, or
    /// reach outside it. Grants are given by kind, so every door decides by
    /// this and the action's place in the published tree.
    #[serde(default = "undoable")]
    pub kind: ActionKind,
    /// The one field of its resource the action writes, when it writes only
    /// one: the action sits below that field in the tree, so a grant on the
    /// field (commenting is writing `comments`) reaches it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default)]
    pub risk: RiskLevel,
    #[serde(default)]
    pub confirmation: ConfirmationPolicy,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default)]
    pub reversible: bool,
    #[serde(default)]
    pub execution: ExecutionMode,
    #[serde(default)]
    pub result: ResultSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The action is heavy enough to run on another of the invoker's devices,
    /// and how: see [`Offload`]. `None` (the default) runs it where it is
    /// invoked, always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload: Option<Offload>,
}

/// Largest CPU time an [`Offload`] may declare: a compute job's ceiling.
pub const MAX_OFFLOAD_CPU_TIME_MS: u64 = 5 * 60 * 1_000;
/// Largest memory an [`Offload`] may declare: a compute job's ceiling.
pub const MAX_OFFLOAD_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Smallest memory an [`Offload`] may declare: a compute job's floor.
pub const MIN_OFFLOAD_MEMORY_BYTES: u64 = 16 * 1024 * 1024;
/// Largest output an [`Offload`] may declare: a compute job's ceiling.
pub const MAX_OFFLOAD_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

/// How an offloadable action runs away from the device it was invoked on.
///
/// An offloadable action is a pure content transform: its inputs are
/// content (files the invoker can read), its outputs are content, and the
/// outputs are a function of the inputs alone, byte for byte, whichever
/// device computes them. That is what lets the system run it on the
/// invoker's best-placed device — placed by the workspace's relay, run as
/// the signed package's catalogue entrypoint under that device's launcher
/// sandbox — and hand back exactly what a local run returns. When no device
/// is better placed, or the offload fails for any reason other than the
/// action's own, it runs locally.
///
/// The declaration names a package and entrypoint and bounds the work. It
/// grants nothing: the worker reads only the inputs the invoker hands it,
/// and is admitted by the same rules as any compute job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offload {
    /// The signed package whose catalogue entrypoint performs the transform.
    pub app: String,
    /// The package's catalogue (CLI) command that performs it.
    pub entrypoint: String,
    /// Whether it needs a GPU.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu: bool,
    /// CPU time ceiling, in milliseconds.
    pub cpu_time_ms: u64,
    /// Memory ceiling, in bytes. The inputs count against it.
    pub memory_bytes: u64,
    /// Ceiling on all outputs together, in bytes.
    pub output_bytes: u64,
}

impl Offload {
    /// Why this declaration could never be placed as a compute job, if it
    /// could not.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let identifier = |value: &str, max: usize| {
            !value.is_empty()
                && value.len() <= max
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/')
                })
        };
        let mut errors = Vec::new();
        if !identifier(&self.app, 256) {
            errors.push("offload app must be a package identifier".into());
        }
        if !identifier(&self.entrypoint, 128) {
            errors.push("offload entrypoint must be a catalogue command".into());
        }
        if self.cpu_time_ms == 0 || self.cpu_time_ms > MAX_OFFLOAD_CPU_TIME_MS {
            errors.push("offload cpu time is outside the compute bounds".into());
        }
        if !(MIN_OFFLOAD_MEMORY_BYTES..=MAX_OFFLOAD_MEMORY_BYTES).contains(&self.memory_bytes) {
            errors.push("offload memory is outside the compute bounds".into());
        }
        if self.output_bytes == 0 || self.output_bytes > MAX_OFFLOAD_OUTPUT_BYTES {
            errors.push("offload output is outside the compute bounds".into());
        }
        errors
    }
}

impl Default for ActionContract {
    fn default() -> Self {
        Self {
            availability: Availability::Available,
            kind: ActionKind::Undoable,
            field: None,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: RetryPolicy::AtMostOnce,
            reversible: false,
            execution: ExecutionMode::Immediate,
            result: ResultSpec::None,
            tags: Vec::new(),
            offload: None,
        }
    }
}

impl ActionContract {
    /// What an invocation needs when its value does not say otherwise.
    #[must_use]
    pub fn need(&self) -> ActionNeed {
        ActionNeed {
            kind: self.kind,
            field: self.field.clone(),
            privileged: self.risk == RiskLevel::Privileged,
        }
    }

    /// Safe defaults for a presentation-only operation.
    pub fn view() -> Self {
        Self {
            risk: RiskLevel::None,
            kind: ActionKind::Read,
            retry: RetryPolicy::Idempotent,
            ..Self::default()
        }
    }

    /// The defaults for an operation of `operation`'s kind: a view reads, a
    /// document operation changes only the document, an effect reaches
    /// outside it.
    pub fn for_operation(operation: OperationKind) -> Self {
        match operation {
            OperationKind::View => Self::view(),
            OperationKind::Document => Self::default(),
            OperationKind::Effect => Self {
                kind: ActionKind::Outside,
                ..Self::default()
            },
        }
    }

    /// Whether the action is offered only to programmatic callers. See
    /// [`PROGRAMMATIC_TAG`].
    pub fn is_programmatic(&self) -> bool {
        self.tags.iter().any(|tag| tag == PROGRAMMATIC_TAG)
    }

    /// Whether the action reads the open document's first page. See
    /// [`DOCUMENT_READ_TAG`].
    pub fn reads_document(&self) -> bool {
        self.tags.iter().any(|tag| tag == DOCUMENT_READ_TAG)
    }

    /// Whether the action opens the file its `path` names. See
    /// [`OPENS_PATH_TAG`].
    pub fn opens_path(&self) -> bool {
        self.tags.iter().any(|tag| tag == OPENS_PATH_TAG)
    }

    /// The file extensions an [`opens_path`](Self::opens_path) action is
    /// declared for; none means any file. See
    /// [`OPENS_PATH_EXTENSION_PREFIX`].
    pub fn opened_extensions(&self) -> impl Iterator<Item = &str> {
        self.tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(OPENS_PATH_EXTENSION_PREFIX))
    }

    /// Whether the action's result carries content from outside the user's
    /// own data. See [`UNTRUSTED_CONTENT_TAG`].
    pub fn carries_untrusted_content(&self) -> bool {
        self.tags.iter().any(|tag| tag == UNTRUSTED_CONTENT_TAG)
    }

    /// Whether the action, once taken, can be undone: the host's undo
    /// reverts a document change, and an outside one declared with its
    /// compensation ([`COMPENSATED_UNDO_TAG`]) is undone by queueing it.
    pub fn can_be_undone(&self) -> bool {
        self.reversible || self.tags.iter().any(|tag| tag == COMPENSATED_UNDO_TAG)
    }

    /// Whether confirmation depends on the invocation's arguments. See
    /// [`ARGUMENT_RISK_TAG`].
    pub fn confirmation_depends_on_arguments(&self) -> bool {
        self.tags.iter().any(|tag| tag == ARGUMENT_RISK_TAG)
    }

    /// The confirmation a caller that cannot classify arguments must assume:
    /// the declared policy, or [`ConfirmationPolicy::Confirm`] for an action
    /// whose risk depends on its arguments. A trust boundary that only sees
    /// this inert contract (the session broker) gates on it; the publishing
    /// host, which can classify, asks
    /// [`ActionSpec::confirmation_for_json`](crate::ActionSpec::confirmation_for_json).
    pub fn advertised_confirmation(&self) -> ConfirmationPolicy {
        if self.confirmation == ConfirmationPolicy::None && self.confirmation_depends_on_arguments()
        {
            ConfirmationPolicy::Confirm
        } else {
            self.confirmation.clone()
        }
    }

    /// The confirmation an invocation with `argument` (the wire argument:
    /// typed JSON behind [`ARGUMENTS_JSON_PREFIX`](crate::ARGUMENTS_JSON_PREFIX))
    /// needs, decided without the publishing app's code: the declared
    /// policy; for a generic field edit, the edit itself read by
    /// [`tilo_action_policy::edits::field_edit_asks`]; for any other action
    /// whose risk depends on its arguments, [`Self::advertised_confirmation`].
    pub fn confirmation_for_argument(&self, argument: Option<&str>) -> ConfirmationPolicy {
        if self.confirmation != ConfirmationPolicy::None
            || !self.confirmation_depends_on_arguments()
            || !self
                .tags
                .iter()
                .any(|tag| tag == tilo_action_policy::edits::FIELD_EDIT_TAG)
        {
            return self.advertised_confirmation();
        }
        let edit = argument
            .and_then(|text| text.strip_prefix(crate::ARGUMENTS_JSON_PREFIX))
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|arguments| arguments.get(FIELD_EDIT_PARAM).cloned());
        let quiet: Vec<&str> = self
            .tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(tilo_action_policy::edits::QUIET_REMOVAL_TAG))
            .collect();
        match edit.as_ref().and_then(edit_parts) {
            Some((op, path)) if !tilo_action_policy::edits::field_edit_asks(op, &path, &quiet) => {
                ConfirmationPolicy::None
            }
            _ => ConfirmationPolicy::Confirm,
        }
    }

    /// Validate the contract as inert protocol data. This is intentionally
    /// usable at both publication ends: a well-behaved app checks before it
    /// sends, and the broker repeats the check at its trust boundary.
    pub fn validate_declaration(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Availability::Unavailable { code, reason } = &self.availability
            && (code.trim().is_empty() || reason.trim().is_empty())
        {
            errors.push("unavailable action needs a code and reason".into());
        }
        if self.risk.requires_confirmation()
            && matches!(self.confirmation, ConfirmationPolicy::None)
        {
            errors.push("consequential action requires confirmation".into());
        }
        if let ConfirmationPolicy::Phrase { phrase } = &self.confirmation
            && phrase.trim().is_empty()
        {
            errors.push("confirmation phrase must not be empty".into());
        }
        if matches!(self.retry, RetryPolicy::RetryWithInvocationId) {
            errors.push(
                "retry_with_invocation_id requires a caller-supplied deduplication key, which this transport does not yet expose".into(),
            );
        }
        if matches!(self.execution, ExecutionMode::Background { .. })
            && matches!(self.result, ResultSpec::None)
        {
            errors.push("background action must declare a result or job".into());
        }
        if matches!(self.execution, ExecutionMode::Immediate)
            && matches!(self.result, ResultSpec::Job)
        {
            errors.push("immediate action cannot declare a background job result".into());
        }
        if let ExecutionMode::Streaming { item_schema, .. } = &self.execution
            && let Err(reason) = crate::params::validate_schema_definition(item_schema)
        {
            errors.push(format!("invalid streaming item {reason}"));
        }
        if let ResultSpec::Json { schema } = &self.result
            && let Err(reason) = crate::params::validate_schema_definition(schema)
        {
            errors.push(format!("invalid result {reason}"));
        }
        if self.tags.iter().any(|tag| tag.trim().is_empty()) {
            errors.push("contract tags must not be empty".into());
        }
        let unique_tags = self.tags.iter().collect::<std::collections::HashSet<_>>();
        if unique_tags.len() != self.tags.len() {
            errors.push("contract tags must be unique".into());
        }
        if let Some(offload) = &self.offload {
            errors.extend(offload.validate());
            // A transform of content: it changes nothing where it runs.
            if self.kind != ActionKind::Read {
                errors.push("an offloadable action must be a read".into());
            }
            if self.tags.iter().any(|tag| tag == LOCAL_DEVICE_ONLY) {
                errors.push("a local-device-only action cannot be offloaded".into());
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Risk levels keep their compact wire discriminants and a broker-owned job identity is not a
    /// second application result.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_contract_keeps_wire_discriminants_and_one_job_identity() {
        risk_level_keeps_existing_compact_wire_discriminants();
        broker_owned_job_identity_is_not_a_second_application_result();
    }

    fn risk_level_keeps_existing_compact_wire_discriminants() {
        // `RiskLevel` appears in recorded action contracts. Postcard encodes
        // enum variants by ordinal, so the old values are a compatibility
        // boundary rather than an implementation detail.
        assert_eq!(postcard::to_allocvec(&RiskLevel::None).unwrap(), vec![0]);
        assert_eq!(postcard::to_allocvec(&RiskLevel::Low).unwrap(), vec![1]);
        assert_eq!(
            postcard::to_allocvec(&RiskLevel::Destructive).unwrap(),
            vec![2]
        );
        assert_eq!(
            postcard::to_allocvec(&RiskLevel::Privileged).unwrap(),
            vec![3]
        );
        assert_eq!(
            postcard::from_bytes::<RiskLevel>(&[2]).unwrap(),
            RiskLevel::Destructive
        );
        assert_eq!(
            postcard::from_bytes::<RiskLevel>(&[3]).unwrap(),
            RiskLevel::Privileged
        );
    }

    fn broker_owned_job_identity_is_not_a_second_application_result() {
        assert!(
            ResultSpec::Job
                .validate(&crate::ActionOutput::default())
                .is_ok()
        );
        assert!(
            ResultSpec::Job
                .validate(&crate::ActionOutput {
                    value: Some(serde_json::json!("a second job id")),
                    resources: Vec::new(),
                })
                .is_err()
        );
    }

    #[test]
    fn only_a_nullable_json_result_may_be_left_out() {
        let nullable = ResultSpec::Json {
            schema: serde_json::json!({"type": ["string", "null"]}),
        };
        let required = ResultSpec::Json {
            schema: serde_json::json!({"type": "string"}),
        };
        let none = crate::ActionOutput::default();
        assert!(nullable.validate(&none).is_ok());
        assert!(required.validate(&none).is_err());
        let text = crate::ActionOutput {
            value: Some(serde_json::json!("changed")),
            resources: Vec::new(),
        };
        assert!(nullable.validate(&text).is_ok());
        assert!(required.validate(&text).is_ok());
    }

    /// An offload declaration is additive on the wire, bounded like a
    /// compute job, and only declarable on a read that may leave the device.
    #[test]
    fn offload_is_an_additive_bounded_declaration_on_reads() {
        let local = ActionContract::view();
        let json = serde_json::to_value(&local).unwrap();
        assert!(json.get("offload").is_none(), "absent stays off the wire");
        let old: ActionContract = serde_json::from_value(json).unwrap();
        assert_eq!(old.offload, None);

        let offload = Offload {
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
            gpu: false,
            cpu_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
        };
        let heavy = ActionContract {
            offload: Some(offload.clone()),
            ..ActionContract::view()
        };
        assert!(heavy.validate_declaration().is_empty());
        let round: ActionContract =
            serde_json::from_value(serde_json::to_value(&heavy).unwrap()).unwrap();
        assert_eq!(round, heavy);

        let unbounded = ActionContract {
            offload: Some(Offload {
                cpu_time_ms: 0,
                memory_bytes: MAX_OFFLOAD_MEMORY_BYTES + 1,
                output_bytes: MAX_OFFLOAD_OUTPUT_BYTES + 1,
                entrypoint: "rm -rf /".into(),
                ..offload.clone()
            }),
            ..ActionContract::view()
        };
        assert_eq!(unbounded.validate_declaration().len(), 4);
        let writes = ActionContract {
            offload: Some(offload.clone()),
            ..ActionContract::default()
        };
        assert_eq!(
            writes.validate_declaration(),
            vec!["an offloadable action must be a read".to_string()]
        );
        let pinned = ActionContract {
            offload: Some(offload),
            tags: vec![LOCAL_DEVICE_ONLY.into()],
            ..ActionContract::view()
        };
        assert_eq!(
            pinned.validate_declaration(),
            vec!["a local-device-only action cannot be offloaded".to_string()]
        );
    }
}
