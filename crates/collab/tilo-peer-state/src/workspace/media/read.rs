//! Shared media data parsing; successful decoding confers no authority.
use super::{MediaPolicyError, WorkspaceMediaV1};
use crate::workspace::read::{Selection, WorkspaceRead, WorkspaceValue, policy_error};

pub(crate) fn parse(
    workspace: &impl WorkspaceRead,
) -> Result<Option<WorkspaceMediaV1>, MediaPolicyError> {
    let mut original: Option<Vec<u8>> = None;
    workspace
        .visit_property(&automerge::ROOT, super::FIELD, Selection::All, |value| {
            let WorkspaceValue::Bytes(bytes) = value else {
                return Err(MediaPolicyError::Malformed);
            };
            if bytes.len() > super::MAX_POLICY_BYTES {
                return Err(MediaPolicyError::Capacity);
            }
            if let Some(prior) = &original {
                if prior.as_slice() != bytes {
                    return Err(MediaPolicyError::Conflict);
                }
            } else {
                original = Some(bytes.to_vec());
            }
            Ok(())
        })
        .map_err(policy_error)?;
    let Some(original) = original else {
        return Ok(None);
    };
    let bytes = original.as_slice();
    // Scan all conflicts before decoding: malformed first values must not
    // obscure the historical Conflict result of distinct conflicting bytes.
    super::preflight_policy(bytes)?;
    let policy: WorkspaceMediaV1 = tilo_user_identity::canonical::from_canonical_bytes(bytes)
        .map_err(|_| MediaPolicyError::Malformed)?;
    super::validate(&policy)?;
    if !crate::postcard_sink::encodes_as(&policy, bytes) {
        return Err(MediaPolicyError::Malformed);
    }
    let mut buffer = [0; 36];
    let uuid = uuid::Uuid::from_bytes(policy.workspace);
    let expected = uuid.hyphenated().encode_lower(&mut buffer);
    let count = workspace
        .visit_property(&automerge::ROOT, "workspace", Selection::All, |value| {
            if matches!(value, WorkspaceValue::Str(workspace) if workspace == expected) {
                Ok(())
            } else {
                Err(MediaPolicyError::Workspace)
            }
        })
        .map_err(policy_error)?;
    if count == 0 {
        return Err(MediaPolicyError::Workspace);
    }
    Ok(Some(policy))
}
