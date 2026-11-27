//! Inert current-declaration evidence for deferred no-argument invocations. No builder, registry, or
//! executable is retained in this value.
use tilo_actions::{ActionInput, ActionSpec, ApiError, ContinuationEvidence, ContinuationQuery};

#[derive(Clone, PartialEq)]
pub struct DeferredIntent {
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
    pub fn capture(spec: &ActionSpec) -> Result<Self, ApiError> {
        let ActionInput::Deferred { type_id } = spec.input() else {
            return Err(Self::stale());
        };
        spec.validate_json(&serde_json::Map::new())
            .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
        if !spec.contract.availability.is_available() {
            return Err(Self::stale());
        }
        let Some(draft @ ContinuationEvidence::Draft { .. }) =
            spec.continuation_identity(ContinuationQuery::Draft)
        else {
            return Err(ApiError::Unavailable {
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
    pub fn validate(&self, spec: &ActionSpec) -> Result<(), ApiError> {
        (Self::capture(spec)? == *self)
            .then_some(())
            .ok_or_else(Self::stale)
    }
    pub(crate) fn draft(&self) -> &ContinuationEvidence {
        &self.draft
    }
    fn stale() -> ApiError {
        ApiError::Unavailable {
            code: "stale_deferred_intent".into(),
            reason: "The retained creation intent is no longer current".into(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum PreparedActionIntent {
    Encoded(tilo_actions::EncodedAction),
    Deferred(DeferredIntent),
}
impl PreparedActionIntent {
    pub fn encoded(&self) -> Option<&tilo_actions::EncodedAction> {
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
) -> Result<(), ApiError> {
    if text.is_some() && json.is_none() {
        return Err(ApiError::InvalidArguments(
            "Deferred actions accept no argument".into(),
        ));
    }
    spec.validate_json(json.unwrap_or(&serde_json::Map::new()))
        .map_err(|error| ApiError::InvalidArguments(error.to_string()))
}
