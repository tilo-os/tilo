//! Inert app reader registration. Selection confers no document or caller authority.
//!
//! The current RecordStore supplies an actual local loan, but no qualified native
//! committed read view. Admission therefore stays closed before any projection or
//! head getter. The positive bridge must retain its private stamp through the
//! terminal queue and recheck the original caller at the actual owner disclosure.
use automerge::AutoCommit;
use std::convert::Infallible;
use tilo_actions::{ActionSpec, EncodedAction, Resource};

pub mod retirement;

pub const MAX_REQUEST_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticReadError {
    InvalidRegistration,
    InvalidContract,
    InvalidRequest,
    Limit,
    InvalidOwner,
    MissingQualifiedOwner,
}

/// App-owned code paired with a closed request decoder and exact result schema.
/// Only the actual resolved Widget's table participates in SDK selection. This
/// public inert registration cannot create a frame, response stamp, or permission.
pub struct RegisteredSemanticReader {
    operation_type: &'static str,
    schema: fn() -> &'static serde_json::Value,
    validate: fn(&Resource, &[u8]) -> Result<(), SemanticReadError>,
}

impl RegisteredSemanticReader {
    pub(crate) fn operation_type(&self) -> &'static str {
        self.operation_type
    }

    pub const fn new(
        operation_type: &'static str,
        schema: fn() -> &'static serde_json::Value,
        validate: fn(&Resource, &[u8]) -> Result<(), SemanticReadError>,
    ) -> Self {
        Self {
            operation_type,
            schema,
            validate,
        }
    }
}

/// A borrowed selection from the actual app table and exact current resource.
/// It owns no copied client contract and exposes no constructor outside the SDK.
pub(crate) struct SelectedSemanticRead<'a> {
    registration: &'a RegisteredSemanticReader,
    resource: &'a Resource,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
}

pub(crate) fn select<'a>(
    table: &'a [RegisteredSemanticReader],
    resource: &'a Resource,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
) -> Result<Option<SelectedSemanticRead<'a>>, SemanticReadError> {
    let mut matching = table.iter().filter(|entry| {
        Some(entry.operation_type) == spec.operation_type.as_deref()
            || entry.operation_type == encoded.type_id
    });
    let Some(registration) = matching.next() else {
        return Ok(None);
    };
    if registration.operation_type.is_empty() || matching.next().is_some() {
        return Err(SemanticReadError::InvalidRegistration);
    }
    let selected = SelectedSemanticRead {
        registration,
        resource,
        spec,
        encoded,
    };
    selected.check_contract()?;
    if encoded.payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    (registration.validate)(resource, &encoded.payload)?;
    Ok(Some(selected))
}

impl SelectedSemanticRead<'_> {
    fn check_contract(&self) -> Result<(), SemanticReadError> {
        use tilo_actions::{ExecutionMode, OperationKind, PermissionRequirement, ResultSpec};
        if !self
            .resource
            .actions
            .iter()
            .any(|actual| std::ptr::eq(actual, self.spec))
            || self.spec.kind != OperationKind::Effect
            || self.spec.operation_type.as_deref() != Some(self.registration.operation_type)
            || self.encoded.type_id != self.registration.operation_type
            || self.spec.contract.permission != PermissionRequirement::Read
            || self.spec.contract.execution != ExecutionMode::Immediate
            || !matches!(&self.spec.contract.result, ResultSpec::Json { schema } if schema == (self.registration.schema)())
        {
            return Err(SemanticReadError::InvalidContract);
        }
        Ok(())
    }

    pub(crate) fn admit_frame(
        &self,
        scope: &crate::invocation_scope::ResourceInvocationScope,
        document: &AutoCommit,
    ) -> Result<Infallible, SemanticReadError> {
        self.check_contract()?;
        scope
            .check_read_loan(document)
            .map_err(|_| SemanticReadError::InvalidOwner)?;
        Err(SemanticReadError::MissingQualifiedOwner)
    }
}

#[cfg(test)]
mod tests;
