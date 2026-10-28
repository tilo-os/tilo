//! Inert current-declaration evidence for deferred no-argument invocations.
//! No builder, registry, or executable is retained in this value.
use tilo_actions::{ActionInput, ActionSpec, ContinuationEvidence, ContinuationQuery, DoorError};

#[derive(Clone, PartialEq)]
pub(crate) struct DeferredIntent {
    id: String,
    label: String,
    description: Option<String>,
    locus: tilo_actions::ExecutionLocus,
    kind: tilo_actions::OperationKind,
    operation_type: Option<String>,
    contract: tilo_actions::ActionContract,
    type_id: String,
    draft: ContinuationEvidence,
}

impl DeferredIntent {
    pub(crate) fn capture(spec: &ActionSpec) -> Result<Self, DoorError> {
        let ActionInput::Deferred { type_id } = spec.input() else {
            return Err(Self::stale());
        };
        spec.validate_json(&serde_json::Map::new())
            .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
        if !spec.contract.availability.is_available() {
            return Err(Self::stale());
        }
        let Some(draft @ ContinuationEvidence::Draft { .. }) =
            spec.continuation_identity(ContinuationQuery::Draft)
        else {
            return Err(DoorError::Unavailable {
                code: "unsupported_deferred_intent".into(),
                reason: "The action provider cannot retain this creation intent".into(),
            });
        };
        Ok(Self {
            id: spec.id.clone(),
            label: spec.label.clone(),
            description: spec.description.clone(),
            locus: spec.locus,
            kind: spec.kind,
            operation_type: spec.operation_type.clone(),
            contract: spec.contract.clone(),
            type_id: type_id.to_owned(),
            draft,
        })
    }
    pub(crate) fn validate(&self, spec: &ActionSpec) -> Result<(), DoorError> {
        (Self::capture(spec)? == *self)
            .then_some(())
            .ok_or_else(Self::stale)
    }
    pub(crate) fn draft(&self) -> &ContinuationEvidence {
        &self.draft
    }
    fn stale() -> DoorError {
        DoorError::Unavailable {
            code: "stale_deferred_intent".into(),
            reason: "The retained creation intent is no longer current".into(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub(crate) enum PreparedActionIntent {
    Encoded(tilo_actions::EncodedAction),
    Deferred(DeferredIntent),
}
impl PreparedActionIntent {
    pub(crate) fn encoded(&self) -> Option<&tilo_actions::EncodedAction> {
        match self {
            Self::Encoded(value) => Some(value),
            Self::Deferred(_) => None,
        }
    }
}

pub(crate) fn validate_deferred_arguments(
    spec: &ActionSpec,
    json: Option<&serde_json::Map<String, serde_json::Value>>,
    text: Option<&str>,
) -> Result<(), DoorError> {
    if text.is_some() && json.is_none() {
        return Err(DoorError::InvalidArguments(
            "Deferred actions accept no argument".into(),
        ));
    }
    spec.validate_json(json.unwrap_or(&serde_json::Map::new()))
        .map_err(|error| DoorError::InvalidArguments(error.to_string()))
}
