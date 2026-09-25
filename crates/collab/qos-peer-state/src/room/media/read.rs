//! Shared media data parsing; successful decoding confers no authority.
use super::{MediaPolicyError, RoomMediaV1};
use crate::resources::{Held, OuterLimits, PeerResourceError};
use crate::room::read::{CopyValue, ReadError, RoomRead, RoomValue, Selection};
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Policy(MediaPolicyError),
    Resource(NativeResourceError),
    Decode(PeerResourceError),
}
pub(crate) trait PolicyValue {
    fn policy(&self) -> &RoomMediaV1;
}
impl PolicyValue for RoomMediaV1 {
    fn policy(&self) -> &RoomMediaV1 {
        self
    }
}
pub(crate) struct BoundedPolicy {
    held: Held<RoomMediaV1>,
}
impl PolicyValue for BoundedPolicy {
    fn policy(&self) -> &RoomMediaV1 {
        &self.held.value
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
pub(crate) struct BoundedDecode<'a> {
    pub(crate) turn: &'a NativeWorkTurn,
    pub(crate) limits: OuterLimits,
}
impl PolicyDecode for BoundedDecode<'_> {
    type Value = BoundedPolicy;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Value, Error> {
        Ok(BoundedPolicy {
            held: Held::decode_in_turn(bytes, self.limits, self.turn).map_err(
                |error| match error {
                    PeerResourceError::Malformed => Error::Policy(MediaPolicyError::Malformed),
                    PeerResourceError::Capacity => Error::Decode(error),
                },
            )?,
        })
    }
}
// Closed custom visitors: calls/denied allocate only fresh BTreeMap nodes
// after each guarded key/value seed; bounded_string allocates exactly the
// visited UTF-8 bytes. Validation and canonical comparison below allocate none.
impl crate::resources::outer::EncodeSchema for RoomMediaV1 {}
impl<'de> crate::resources::outer::Schema<'de> for RoomMediaV1 {}

fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => Error::Policy(MediaPolicyError::Malformed),
        ReadError::Resource(error) => Error::Resource(error),
        ReadError::Visitor(error) => error,
    }
}
pub(crate) fn parse<R: RoomRead, C: CopyValue, D: PolicyDecode>(
    room: &R,
    copy: &C,
    decoder: &D,
) -> Result<Option<D::Value>, Error> {
    let mut original: Option<C::Bytes> = None;
    room.visit_property(&room.root(), super::FIELD, Selection::All, |value| {
        let RoomValue::Bytes(bytes) = value else {
            return Err(Error::Policy(MediaPolicyError::Malformed));
        };
        if bytes.len() > super::MAX_POLICY_BYTES {
            return Err(Error::Policy(MediaPolicyError::Capacity));
        }
        if let Some(prior) = &original {
            if prior.as_ref() != bytes {
                return Err(Error::Policy(MediaPolicyError::Conflict));
            }
        } else {
            original = Some(copy.bytes(bytes).map_err(Error::Resource)?);
        }
        Ok(())
    })
    .map_err(read_error)?;
    let Some(original) = original else {
        return Ok(None);
    };
    let bytes = original.as_ref();
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
    struct Compare<'a>(&'a [u8]);
    impl postcard::ser_flavors::Flavor for Compare<'_> {
        type Output = bool;
        fn try_push(&mut self, value: u8) -> postcard::Result<()> {
            self.try_extend(&[value])
        }
        fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
            self.0 = self
                .0
                .strip_prefix(bytes)
                .ok_or(postcard::Error::SerializeBufferFull)?;
            Ok(())
        }
        fn finalize(self) -> postcard::Result<bool> {
            Ok(self.0.is_empty())
        }
    }
    postcard::serialize_with_flavor(policy, Compare(bytes)).unwrap_or(false)
}

pub(super) fn legacy_error(error: Error) -> MediaPolicyError {
    match error {
        Error::Policy(error) => error,
        Error::Resource(_) | Error::Decode(PeerResourceError::Capacity) => {
            MediaPolicyError::Capacity
        }
        Error::Decode(PeerResourceError::Malformed) => MediaPolicyError::Malformed,
    }
}

#[cfg(test)]
mod tests;
