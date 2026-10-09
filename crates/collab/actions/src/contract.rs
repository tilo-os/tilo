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
#[serde(tag = "state", rename_all = "snake_case")]
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
}

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

/// [`ActionContract::tags`] entry of the read-only action that answers, when
/// invoked with no arguments, the first page of the document an app has
/// open (a word processor's text, a sheet's used cells, a deck's outline).
/// A caller that just opened a file reads it through this action without
/// knowing the app.
pub const DOCUMENT_READ_TAG: &str = "read:document";

/// What one invocation of an action is, for the grants deciding it: its kind,
/// and the one field of its resource it writes when it writes only one (a
/// reply writes the `comments`), which places it below that field in the
/// tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionNeed {
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
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
        Self { kind, field: None }
    }

    /// An undoable change of the resource's comments: what someone who may
    /// only comment may do.
    #[must_use]
    pub fn comments() -> Self {
        Self {
            kind: ActionKind::Undoable,
            field: Some(tilo_action_policy::roles::COMMENTS_FIELD.to_owned()),
        }
    }
}

const fn undoable() -> ActionKind {
    ActionKind::Undoable
}

/// Confirmation required after argument binding and immediately before run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConfirmationPolicy {
    #[default]
    None,
    Confirm,
    /// Requires an exact phrase, useful for irreversible bulk operations.
    Phrase {
        phrase: String,
    },
}

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
#[serde(tag = "mode", rename_all = "snake_case")]
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

/// Shape of a successful result. Schemas use JSON Schema so non-Rust callers
/// do not need a parallel Tilo-only type system.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
}
