//! Machine-readable operational semantics for exported actions.
//!
//! Labels explain an action to a person. This module explains it to callers
//! that must plan safely without seeing the application: whether it is
//! currently available, what confirmation it needs, whether a failed call may
//! be retried, how it completes, and what it returns.

use serde::{Deserialize, Serialize};

/// Whether an action can be invoked in the resource's current state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable {
        /// Stable programmatic reason such as `offline` or `selection_empty`.
        code: String,
        /// Localised explanation suitable for a person or an agent transcript.
        reason: String,
    },
}

impl Default for Availability {
    fn default() -> Self {
        Self::Available
    }
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
}

/// Minimum authority required to see and invoke an action. Capability-denied
/// actions remain absent; this value lets every allowed catalogue use the same
/// gate instead of inferring authority inconsistently from `OperationKind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRequirement {
    None,
    Read,
    Comment,
    #[default]
    Write,
    Invite,
    Admin,
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

/// Where an action remains discoverable when no live window publishes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryScope {
    #[default]
    Live,
    /// Installed-app catalogue can launch the app and deliver the invocation.
    Launchable,
    /// Safe summary may remain in the permission-filtered durable index.
    Durable,
}

/// Shape of a successful result. Schemas use JSON Schema so non-Rust callers
/// do not need a parallel qOS-only type system.
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
    pub fn validate(&self, output: &crate::ActionOutput) -> Result<(), String> {
        match self {
            Self::None if output.value.is_none() && output.resources.is_empty() => Ok(()),
            Self::None => Err("action declared no result but returned one".into()),
            Self::Json { schema } => {
                let value = output
                    .value
                    .as_ref()
                    .ok_or_else(|| "action did not return its declared JSON result".to_string())?;
                crate::params::validate_schema(value, schema)
            }
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
            Self::Job if output.value.is_none() && output.resources.is_empty() => Ok(()),
            Self::Job => Err("job action returned an undeclared terminal value".into()),
        }
    }
}

/// Complete operational contract copied into every inert descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionContract {
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub permission: PermissionRequirement,
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
    pub discovery: DiscoveryScope,
    #[serde(default)]
    pub result: ResultSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Default for ActionContract {
    fn default() -> Self {
        Self {
            availability: Availability::Available,
            permission: PermissionRequirement::Write,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: RetryPolicy::AtMostOnce,
            reversible: false,
            execution: ExecutionMode::Immediate,
            discovery: DiscoveryScope::Live,
            result: ResultSpec::None,
            tags: Vec::new(),
        }
    }
}

impl ActionContract {
    /// Safe defaults for a presentation-only operation.
    pub fn view() -> Self {
        Self {
            risk: RiskLevel::None,
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            ..Self::default()
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
        if matches!(self.risk, RiskLevel::Destructive | RiskLevel::Privileged)
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

    #[test]
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
}
