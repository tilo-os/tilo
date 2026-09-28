//! Untrusted local resume metadata. These bounds are storage bounds, not a wire
//! schema or proof of authority. Only the authenticated delivery owner may use
//! a stored token as a resume candidate; decoding it never permits replay skips.
use serde::{Deserialize, Deserializer, Serialize, de};

pub(crate) const MAX_CURSOR_BYTES: usize = 4096;
pub(crate) const MAX_CURSOR_ID_BYTES: usize = 256;
pub(crate) const MAX_CURSOR_ROOM_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorIdentity {
    pub workspace: u64,
    #[serde(deserialize_with = "bounded_room")]
    pub room: String,
    pub workspace_root: [u8; 32],
    pub device: [u8; 32],
    pub device_root: [u8; 32],
    pub host: [u8; 32],
    pub host_root: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredDeliveryCursor {
    #[serde(deserialize_with = "cursor_version")]
    pub version: u8,
    pub identity: CursorIdentity,
    #[serde(deserialize_with = "bounded_bytes::<_, MAX_CURSOR_ID_BYTES>")]
    pub consumer: Vec<u8>,
    #[serde(deserialize_with = "bounded_bytes::<_, MAX_CURSOR_ID_BYTES>")]
    pub authorization_view: Vec<u8>,
    #[serde(deserialize_with = "bounded_bytes::<_, MAX_CURSOR_ID_BYTES>")]
    pub store_generation: Vec<u8>,
    #[serde(deserialize_with = "bounded_bytes::<_, MAX_CURSOR_BYTES>")]
    pub token: Vec<u8>,
}

fn valid_room(room: &str) -> bool {
    !room.trim().is_empty()
        && room.len() <= MAX_CURSOR_ROOM_BYTES
        && !room.chars().any(char::is_control)
}
impl StoredDeliveryCursor {
    pub(crate) fn validate_storage_shape(&self) -> Result<(), String> {
        if self.version != 1
            || !valid_room(&self.identity.room)
            || [
                &self.consumer,
                &self.authorization_view,
                &self.store_generation,
            ]
            .iter()
            .any(|bytes| bytes.is_empty() || bytes.len() > MAX_CURSOR_ID_BYTES)
            || self.token.is_empty()
            || self.token.len() > MAX_CURSOR_BYTES
        {
            return Err("invalid stored delivery cursor shape".into());
        }
        Ok(())
    }

    pub(crate) fn matches_identity(&self, expected: &CursorIdentity) -> bool {
        self.identity == *expected
    }
}

fn cursor_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    match u8::deserialize(deserializer)? {
        1 => Ok(1),
        _ => Err(de::Error::custom("unsupported stored cursor version")),
    }
}
fn bounded_room<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    struct Room;
    impl de::Visitor<'_> for Room {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a nonempty room of at most 256 bytes")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            if !valid_room(value) {
                return Err(E::custom("invalid stored cursor room"));
            }
            Ok(value.to_owned())
        }
    }
    deserializer.deserialize_str(Room)
}
fn bounded_bytes<'de, D: Deserializer<'de>, const MAX: usize>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    struct Bytes<const MAX: usize>;
    impl<const MAX: usize> de::Visitor<'_> for Bytes<MAX> {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "1 to {MAX} opaque bytes")
        }
        fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Vec<u8>, E> {
            if value.is_empty() || value.len() > MAX {
                return Err(E::custom("stored cursor field exceeds bounds"));
            }
            // Postcard lends its input slice: validate before allocating owned bytes.
            Ok(value.to_vec())
        }
    }
    deserializer.deserialize_bytes(Bytes::<MAX>)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn cursor(token: u8) -> StoredDeliveryCursor {
        StoredDeliveryCursor {
            version: 1,
            identity: CursorIdentity {
                workspace: 9_007_199_254_740_993,
                room: "room".into(),
                workspace_root: [1; 32],
                device: [2; 32],
                device_root: [3; 32],
                host: [4; 32],
                host_root: [5; 32],
            },
            consumer: vec![6],
            authorization_view: vec![7],
            store_generation: vec![8],
            token: vec![token],
        }
    }
    #[test]
    fn bounded_decode_preserves_opaque_scope_without_claiming_authority() {
        let value = cursor(9);
        let bytes = postcard::to_allocvec(&value).unwrap();
        let decoded: StoredDeliveryCursor = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.identity.workspace, 9_007_199_254_740_993);
        assert!(decoded.matches_identity(&value.identity));
        for field in 0..7 {
            let mut other = value.identity.clone();
            match field {
                0 => other.workspace += 1,
                1 => other.room.push('x'),
                2 => other.workspace_root[0] ^= 1,
                3 => other.device[0] ^= 1,
                4 => other.device_root[0] ^= 1,
                5 => other.host[0] ^= 1,
                _ => other.host_root[0] ^= 1,
            }
            assert!(!decoded.matches_identity(&other));
        }
    }
    #[test]
    fn malformed_cursor_fields_fail_before_owned_token_allocation() {
        for field in 0..5 {
            for size in [0, if field == 4 { 4097 } else { 257 }] {
                let mut value = cursor(9);
                match field {
                    0 => value.identity.room = "r".repeat(size),
                    1 => value.consumer = vec![1; size],
                    2 => value.authorization_view = vec![1; size],
                    3 => value.store_generation = vec![1; size],
                    _ => value.token = vec![1; size],
                }
                assert!(value.validate_storage_shape().is_err());
                let bytes = postcard::to_allocvec(&value).unwrap();
                assert!(postcard::from_bytes::<StoredDeliveryCursor>(&bytes).is_err());
            }
        }
        let mut value = cursor(9);
        value.version = 2;
        assert!(
            postcard::from_bytes::<StoredDeliveryCursor>(&postcard::to_allocvec(&value).unwrap())
                .is_err()
        );
        value.version = 1;
        value.identity.room = "\n".into();
        assert!(
            postcard::from_bytes::<StoredDeliveryCursor>(&postcard::to_allocvec(&value).unwrap())
                .is_err()
        );
    }
    #[test]
    fn maximum_fields_decode_without_loss_and_truncation_is_rejected() {
        let mut value = cursor(9);
        value.identity.room = "r".repeat(256);
        value.consumer = vec![1; 256];
        value.authorization_view = vec![2; 256];
        value.store_generation = vec![3; 256];
        value.token = vec![4; 4096];
        let bytes = postcard::to_allocvec(&value).unwrap();
        assert_eq!(
            postcard::from_bytes::<StoredDeliveryCursor>(&bytes).unwrap(),
            value
        );
        assert!(postcard::from_bytes::<StoredDeliveryCursor>(&bytes[..bytes.len() - 1]).is_err());
    }
}
