//! Pure helpers that convert between capnp reader/builder views and
//! the local `wire::{CollabRequest, CollabEvent}` JSON types. Kept
//! side-effect-free so both the service impl and the round-trip test
//! can call them.

use uuid::Uuid;

use crate::wire::{CollabRequest, PresenceKey, PresencePayload};

/// Namespace UUID used to derive a stable `cluster_id` from an
/// opaque identity blob. The launcher-side identity is postcard-
/// encoded and the bridge has no schema to parse it; we hash it into
/// a v5 UUID so two `joinChannel` calls with the same identity land
/// in the same cluster on the shell side (which uses `cluster_id`
/// as a libp2p routing key).
///
/// The specific namespace UUID is arbitrary — it just needs to be
/// stable so identical inputs map to identical outputs across
/// process restarts. We use the standard OID namespace.
pub const CLUSTER_ID_NAMESPACE: Uuid = Uuid::NAMESPACE_OID;

/// Derive the `cluster_id` field that goes on the upstream `Hello`
/// frame. Behaviour:
///
/// - Empty identity → `Uuid::nil()`. The launcher hasn't provisioned
///   an identity yet; the SDK is running "outside a cluster" and the
///   shell broker treats a nil id as the local-only cluster.
/// - Non-empty identity → v5 UUID derived from those bytes. Stable
///   across bridge restarts.
pub fn cluster_id_from_identity(identity: &[u8]) -> Uuid {
    if identity.is_empty() {
        Uuid::nil()
    } else {
        Uuid::new_v5(&CLUSTER_ID_NAMESPACE, identity)
    }
}

/// Build the upstream `Hello` request for a `joinChannel` call.
///
/// The mapping:
/// - `channel_id` → `app_id` (the shell broker uses `app_id` as its
///   peer-attribution key on outbound fanout)
/// - `identity`   → derives `cluster_id` (see
///   [`cluster_id_from_identity`])
pub fn hello_from_join(channel_id: &str, identity: &[u8]) -> CollabRequest {
    CollabRequest::Hello {
        app_id: channel_id.to_owned(),
        cluster_id: cluster_id_from_identity(identity),
        record: None,
        handle: None,
    }
}

/// Try to decode `keyBytes` (postcard) into a local `PresenceKey`
/// mirror. On decode failure returns `None`; the caller logs and
/// drops the frame.
pub fn decode_key(bytes: &[u8]) -> Option<PresenceKey> {
    postcard::from_bytes::<PresenceKey>(bytes).ok()
}

/// Build a `Presence` request from a keyBytes + payload pair.
///
/// The bridge decodes the postcard-key so the outbound JSON frame
/// carries a typed `PresenceKey` variant. If the key fails to decode
/// this returns `None`, matching the "log + drop" behaviour required
/// by the bridge spec.
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
