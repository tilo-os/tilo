//! Pure helpers converting between capnp reader/builder views and the
//! local `wire::{CollabRequest, CollabEvent}` JSON types; side-effect-free
//! so both the service impl and the round-trip test can call them.

use uuid::Uuid;

use crate::wire::{CollabRequest, PresenceKey, PresencePayload};

/// Namespace UUID for deriving a stable `cluster_id` from an opaque
/// identity blob. The launcher-side identity is postcard-encoded and the
/// bridge has no schema to parse it, so it's hashed into a v5 UUID —
/// same identity means same cluster on the shell side, which uses
/// `cluster_id` as a libp2p routing key. The namespace itself is
/// arbitrary (the standard OID namespace); it only needs to be stable.
pub const CLUSTER_ID_NAMESPACE: Uuid = Uuid::NAMESPACE_OID;

/// Derives the `cluster_id` for the upstream `Hello` frame: empty
/// identity → `Uuid::nil()` (unprovisioned launcher identity; the shell
/// broker treats nil as the local-only cluster), non-empty → a stable v5 UUID.
pub fn cluster_id_from_identity(identity: &[u8]) -> Uuid {
    if identity.is_empty() {
        Uuid::nil()
    } else {
        Uuid::new_v5(&CLUSTER_ID_NAMESPACE, identity)
    }
}

/// Builds the upstream `Hello` request for a `joinChannel` call:
/// `channel_id` → `app_id` (the shell broker's peer-attribution key on
/// outbound fanout), `identity` → `cluster_id` (see [`cluster_id_from_identity`]).
pub fn hello_from_join(channel_id: &str, identity: &[u8]) -> CollabRequest {
    CollabRequest::Hello {
        app_id: channel_id.to_owned(),
        cluster_id: cluster_id_from_identity(identity),
        record: None,
        handle: None,
    }
}

/// Tries decoding `keyBytes` (postcard) into a local `PresenceKey`; `None` on failure, for the caller to log and drop.
pub fn decode_key(bytes: &[u8]) -> Option<PresenceKey> {
    postcard::from_bytes::<PresenceKey>(bytes).ok()
}

/// Builds a `Presence` request from a keyBytes + payload pair, decoding
/// the postcard key into a typed variant; `None` if the key fails to
/// decode, so the caller logs and drops the frame.
pub fn presence_request(
    key_bytes: &[u8],
    payload: PresencePayload,
) -> Option<CollabRequest> {
    let key = decode_key(key_bytes)?;
    Some(CollabRequest::Presence { key, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_id_stable_for_same_identity() {
        let a = cluster_id_from_identity(b"identity-1");
        let b = cluster_id_from_identity(b"identity-1");
        let c = cluster_id_from_identity(b"identity-2");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn empty_identity_is_nil() {
        assert_eq!(cluster_id_from_identity(&[]), Uuid::nil());
    }

    #[test]
    fn hello_pairs_channel_id_and_identity() {
        let req = hello_from_join("chan-42", b"i");
        match req {
            CollabRequest::Hello { app_id, cluster_id, .. } => {
                assert_eq!(app_id, "chan-42");
                assert_eq!(cluster_id, cluster_id_from_identity(b"i"));
            }
            _ => panic!("expected Hello"),
        }
    }

    #[test]
    fn decode_key_round_trips_tab_variant() {
        let k = PresenceKey::Tab { id: "t-1".into() };
        let bytes = postcard::to_stdvec(&k).unwrap();
        let back = decode_key(&bytes).unwrap();
        assert_eq!(back, k);
    }

    #[test]
    fn decode_key_returns_none_on_garbage() {
        assert!(decode_key(b"\xff\xff\xff\xff not postcard").is_none());
    }
}
