//! Shared media data parsing; successful decoding confers no authority.
use super::{MediaPolicyError, WorkspaceMediaV1};
use crate::workspace::read::{ReadError, Selection, WorkspaceRead, WorkspaceValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Policy(MediaPolicyError),
    Capacity,
}
pub(crate) trait PolicyValue {
    fn policy(&self) -> &WorkspaceMediaV1;
}
impl PolicyValue for WorkspaceMediaV1 {
    fn policy(&self) -> &WorkspaceMediaV1 {
        self
    }
}
pub(crate) trait PolicyDecode {
    type Value: PolicyValue;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Value, Error>;
}
pub(crate) struct LegacyPolicy;
impl PolicyDecode for LegacyPolicy {
    type Value = WorkspaceMediaV1;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Value, Error> {
        tilo_user_identity::canonical::from_canonical_bytes(bytes)
            .map_err(|_| Error::Policy(MediaPolicyError::Malformed))
    }
}
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => Error::Policy(MediaPolicyError::Malformed),
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => error,
    }
}
pub(crate) fn parse<R: WorkspaceRead, D: PolicyDecode>(
    workspace: &R,
    decoder: &D,
) -> Result<Option<D::Value>, Error> {
    let mut original: Option<Vec<u8>> = None;
    workspace
        .visit_property(&workspace.root(), super::FIELD, Selection::All, |value| {
            let WorkspaceValue::Bytes(bytes) = value else {
                return Err(Error::Policy(MediaPolicyError::Malformed));
            };
            if bytes.len() > super::MAX_POLICY_BYTES {
                return Err(Error::Policy(MediaPolicyError::Capacity));
            }
            if let Some(prior) = &original {
                if prior.as_slice() != bytes {
                    return Err(Error::Policy(MediaPolicyError::Conflict));
                }
            } else {
                original = Some(bytes.to_vec());
            }
            Ok(())
        })
        .map_err(read_error)?;
    let Some(original) = original else {
        return Ok(None);
    };
    let bytes = original.as_slice();
    // Scan all conflicts before decoding: malformed first values must not
    // obscure the historical Conflict result of distinct conflicting bytes.
    super::preflight_policy(bytes).map_err(Error::Policy)?;
    let value = decoder.decode(bytes)?;
    let policy = value.policy();
    super::validate(policy).map_err(Error::Policy)?;
    if !canonical_matches(policy, bytes) {
        return Err(Error::Policy(MediaPolicyError::Malformed));
    }
    let mut buffer = [0; 36];
    let uuid = uuid::Uuid::from_bytes(policy.workspace);
    let expected = uuid.hyphenated().encode_lower(&mut buffer);
    let count = workspace
        .visit_property(&workspace.root(), "room", Selection::All, |value| {
            if matches!(value, WorkspaceValue::Str(workspace) if workspace == expected) {
                Ok(())
            } else {
                Err(Error::Policy(MediaPolicyError::Binding))
            }
        })
        .map_err(read_error)?;
    if count == 0 {
        return Err(Error::Policy(MediaPolicyError::Binding));
    }
    Ok(Some(value))
}

fn canonical_matches(policy: &WorkspaceMediaV1, bytes: &[u8]) -> bool {
    crate::postcard_sink::encodes_as(policy, bytes)
}

pub(super) fn legacy_error(error: Error) -> MediaPolicyError {
    match error {
        Error::Policy(error) => error,
        Error::Capacity => MediaPolicyError::Capacity,
    }
}

#[cfg(test)]
mod tests;
