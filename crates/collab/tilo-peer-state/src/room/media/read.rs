//! Shared media data parsing; successful decoding confers no authority.
use super::{MediaPolicyError, RoomMediaV1};
use crate::room::read::{ReadError, RoomRead, RoomValue, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Policy(MediaPolicyError),
    Capacity,
}
pub(crate) trait PolicyValue {
    fn policy(&self) -> &RoomMediaV1;
}
impl PolicyValue for RoomMediaV1 {
    fn policy(&self) -> &RoomMediaV1 {
        self
    }
}
pub(crate) trait PolicyDecode {
    type Value: PolicyValue;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Value, Error>;
}
pub(crate) struct LegacyPolicy;
impl PolicyDecode for LegacyPolicy {
    type Value = RoomMediaV1;
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
pub(crate) fn parse<R: RoomRead, D: PolicyDecode>(
    room: &R,
    decoder: &D,
) -> Result<Option<D::Value>, Error> {
    let mut original: Option<Vec<u8>> = None;
    room.visit_property(&room.root(), super::FIELD, Selection::All, |value| {
        let RoomValue::Bytes(bytes) = value else {
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
    let uuid = uuid::Uuid::from_bytes(policy.room);
    let expected = uuid.hyphenated().encode_lower(&mut buffer);
    let count = room
        .visit_property(&room.root(), "room", Selection::All, |value| {
            if matches!(value, RoomValue::Str(room) if room == expected) {
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

fn canonical_matches(policy: &RoomMediaV1, bytes: &[u8]) -> bool {
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
