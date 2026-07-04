//! Peer identity — the shell-side wiring for user-scoped identities.
//!
//! Today, [`PeerId`] is a 64-bit hash of the peer's libp2p pubkey. It
//! is stable across restarts and adequate to route messages, but it
//! carries no statement about *who* the peer is — every device is
//! anonymous until an out-of-band invite ties it to a workspace.
//!
//! This module layers user-scoped identity on top of that hash. When
//! two devices first exchange messages (either at libp2p dial time
//! or over an existing gossipsub topic), each side presents:
//!
//! - its **device public key** (32-byte Ed25519),
//! - a signed **[`DeviceCert`]** binding that key to the user's
//!   root public key with the labels + capabilities the user
//!   consented to at enrollment time.
//!
//! From those two, the shell reconstructs a [`PeerIdentity`] that
//! subsequent CRDT-op validation, share-session admission, and
//! revocation checks read from. Any op signed by a device whose
//! cert either (a) does not verify under the workspace owner's
//! root key, (b) has expired, or (c) has been revoked, is dropped
//! before it reaches the workspace tree.
//!
//! # Non-goals in this module
//!
//! - Persistence. Loading `/var/lib/qos/user-identity/*` at boot
//!   lives in the shell's firstboot / login flow, not here.
//! - Transport. The wire that carries [`DeviceCert`]s between peers
//!   is a separate concern (SP2); this module just holds the
//!   verified-once, keyed-by-hash lookup table.
//! - CRDT-op validation itself lives in
//!   `network::replica` — this module exposes the primitives
//!   ([`PeerIdentityStore::verify_signature_from`]) that the
//!   validator calls.
//!
//! Everything wire-facing (the `DeviceCert` byte layout, the
//! signing preimage, the revocation entry) lives in
//! `qos-user-identity`. The shell should never invent its own
//! variant of those types.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ed25519_dalek::Verifier;

use qos_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceCertError, RevocationList, verify_device_cert,
    verify_device_cert_at,
};

use super::protocol::PeerId;

/// Everything the shell knows about a peer's identity.
///
/// Constructed at connect time from the peer's presented cert +
/// device pubkey. Stored in a [`PeerIdentityStore`] keyed by the
/// hashed [`PeerId`] so `replica` and share-session code can look
/// it up in O(1) when validating a CRDT op or admitting a share.
#[derive(Clone, Debug)]
pub struct PeerIdentity {
    /// Peer's device Ed25519 public key. 32 bytes.
    pub device_pubkey: [u8; 32],
    /// User root Ed25519 public key that certified this device.
    /// Multiple peers may share the same value — they are all
    /// devices of the same user. When it matches the workspace
    /// owner's root key, the peer is one of the user's own
    /// devices and gets owner permissions automatically.
    pub user_pubkey: [u8; 32],
    /// The full signed device cert. Retained so downstream
    /// validation can re-verify without a second lookup, and so
    /// the peer's [`DeviceCapabilities`] survive a later cache
    /// eviction of the pubkey-only view.
    pub device_cert: DeviceCert,
    /// Convenience copy of `device_cert.capabilities`. What the
    /// peer may do on behalf of its user.
    pub capabilities: DeviceCapabilities,
}

impl PeerIdentity {
    /// Build a `PeerIdentity` from a peer's presented cert.
    ///
    /// Verifies the cert's signature under the embedded
    /// `user_public_key`, checks it has not expired at `now_ns`,
    /// and mirrors the `capabilities` field for cheap lookup.
    /// Does NOT check the revocation list — that check lives with
    /// the [`PeerIdentityStore`] because the list is shared
    /// across peers.
    pub fn from_cert(cert: DeviceCert, now_ns: u64) -> Result<Self, DeviceCertError> {
        verify_device_cert_at(&cert, now_ns)?;
        Ok(Self {
            device_pubkey: cert.device_public_key,
            user_pubkey: cert.user_public_key,
            capabilities: cert.capabilities.clone(),
            device_cert: cert,
        })
    }

    /// True if this peer's user identity matches the given user
    /// root key — i.e. the peer is one of the user's own devices.
    pub fn is_same_user(&self, user_pubkey: &[u8; 32]) -> bool {
        &self.user_pubkey == user_pubkey
    }
}

/// A thread-safe map from hashed [`PeerId`] to verified
/// [`PeerIdentity`], plus the shared revocation list the identity
/// belongs to.
///
/// The store holds one [`RevocationList`] per user identity —
/// keyed by the user's root pubkey — because different peers can
/// belong to different users (a workspace-share invitee is not
/// necessarily one of the owner's devices).
#[derive(Clone, Default)]
pub struct PeerIdentityStore {
    inner: Arc<RwLock<PeerIdentityStoreInner>>,
}

#[derive(Default)]
struct PeerIdentityStoreInner {
    by_peer: HashMap<PeerId, PeerIdentity>,
    /// Per-user-identity revocation lists. The value stays stable
    /// across cert cache churn — revocations are additive.
    revocations: HashMap<[u8; 32], RevocationList>,
}

impl PeerIdentityStore {
    /// Fresh empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a peer's identity.
    ///
    /// Returns `Err` if the cert's signature does not verify OR
    /// the cert has expired at `now_ns`. On success the peer is
    /// addressable by its `PeerId` until [`Self::remove`] or a
    /// later [`Self::insert`] replaces it.
    pub fn insert(
        &self,
        peer_id: PeerId,
        cert: DeviceCert,
        now_ns: u64,
    ) -> Result<PeerIdentity, DeviceCertError> {
        let identity = PeerIdentity::from_cert(cert, now_ns)?;
        let mut inner = self
            .inner
            .write()
            .expect("PeerIdentityStore lock poisoned");
        inner.by_peer.insert(peer_id, identity.clone());
        Ok(identity)
    }

    /// Drop a peer's cached identity — used when the peer
    /// disconnects. Idempotent: removing a missing peer is a
    /// no-op.
    pub fn remove(&self, peer_id: PeerId) {
        let mut inner = self
            .inner
            .write()
            .expect("PeerIdentityStore lock poisoned");
        inner.by_peer.remove(&peer_id);
    }

    /// Look up a peer by its hashed [`PeerId`].
    pub fn get(&self, peer_id: PeerId) -> Option<PeerIdentity> {
        let inner = self.inner.read().expect("PeerIdentityStore lock poisoned");
        inner.by_peer.get(&peer_id).cloned()
    }

    /// Merge a user's revocation list. Additive — later inserts
    /// only overwrite older entries with earlier `revoked_at_ns`
    /// timestamps (see [`RevocationList::insert`]).
    pub fn set_revocations(&self, user_pubkey: [u8; 32], list: RevocationList) {
        let mut inner = self
            .inner
            .write()
            .expect("PeerIdentityStore lock poisoned");
        inner.revocations.insert(user_pubkey, list);
    }

    /// True if `device_pubkey` has been revoked under `user_pubkey`
    /// at or before `at_ns`.
    pub fn is_revoked(&self, user_pubkey: &[u8; 32], device_pubkey: &[u8; 32], at_ns: u64) -> bool {
        let inner = self.inner.read().expect("PeerIdentityStore lock poisoned");
        inner
            .revocations
            .get(user_pubkey)
            .is_some_and(|list| list.is_revoked_at(device_pubkey, at_ns))
    }

    /// Verify an Ed25519 signature made by `peer_id`'s device
    /// against a message.
    ///
    /// Runs the full identity-chain check:
    ///
    /// 1. The peer must be registered in the store.
    /// 2. Their cert must (still) verify under the embedded user
    ///    root key.
    /// 3. Their cert must not have expired at `now_ns`.
    /// 4. Their device must not be revoked under their user
    ///    identity at or before `signed_at_ns`.
    /// 5. The Ed25519 signature must verify under the device
    ///    pubkey.
    ///
    /// Returns `Ok(PeerIdentity)` on all-pass. Callers can then
    /// use the returned identity's `capabilities` /
    /// `user_pubkey` for authorization decisions.
    pub fn verify_signature_from(
        &self,
        peer_id: PeerId,
        message: &[u8],
        signature: &[u8; 64],
        signed_at_ns: u64,
        now_ns: u64,
    ) -> Result<PeerIdentity, PeerIdentityError> {
        let identity = self.get(peer_id).ok_or(PeerIdentityError::UnknownPeer)?;
        // Re-verify the cert (defence in depth against a corrupted
        // in-memory copy).
        verify_device_cert(&identity.device_cert)?;
        if identity.device_cert.is_expired_at(now_ns) {
            return Err(PeerIdentityError::CertExpired);
        }
        if self.is_revoked(&identity.user_pubkey, &identity.device_pubkey, signed_at_ns) {
            return Err(PeerIdentityError::Revoked);
        }
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&identity.device_pubkey)
            .map_err(|_| PeerIdentityError::BadDevicePubkey)?;
        let sig = ed25519_dalek::Signature::from_bytes(signature);
        vk.verify(message, &sig)
            .map_err(|_| PeerIdentityError::BadSignature)?;
        Ok(identity)
    }
}

/// Failure modes for peer identity operations.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PeerIdentityError {
    /// No cached identity for that `PeerId`. Caller should either
    /// re-request the cert from the peer or drop the message.
    #[error("no cached identity for peer")]
    UnknownPeer,
    /// Cert-level error (bad signature, invalid pubkey, expired,
    /// etc.).
    #[error("device cert invalid: {0}")]
    Cert(#[from] DeviceCertError),
    /// The cert has expired.
    #[error("device cert expired")]
    CertExpired,
    /// The peer's device has been revoked at or before the
    /// message's `signed_at_ns` timestamp.
    #[error("peer device is revoked")]
    Revoked,
    /// The peer's device pubkey does not decode to a valid Ed25519
    /// point. Reachable only for a cert whose pubkey bytes were
    /// tampered with post-verification (i.e. never in practice).
    #[error("device pubkey invalid")]
    BadDevicePubkey,
    /// The Ed25519 signature over the message did not verify.
    #[error("signature did not verify")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use qos_user_identity::{DeviceIdentity, UserIdentity, sign_device_cert};

    /// Build a fresh (user root sk, device sk, cert) for tests.
    fn fresh_identity(
        caps: DeviceCapabilities,
    ) -> (SigningKey, SigningKey, DeviceCert) {
        let (root_sk, user) = UserIdentity::generate("Alice", 100);
        let (dev_sk, dev) = DeviceIdentity::generate("Framework 16", 200);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 300,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        (root_sk, dev_sk, cert)
    }

    #[test]
    fn insert_and_lookup() {
        let (_root_sk, _dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let store = PeerIdentityStore::new();
        let peer_id: PeerId = 0xdead_beef;
        let ident = store.insert(peer_id, cert.clone(), 500).unwrap();
        assert_eq!(ident.device_pubkey, cert.device_public_key);
        assert!(ident.capabilities.can_admit_devices);
        let looked_up = store.get(peer_id).unwrap();
        assert_eq!(looked_up.device_pubkey, cert.device_public_key);
    }

    #[test]
    fn insert_rejects_expired() {
        let (root_sk, _dev_sk, _) = fresh_identity(DeviceCapabilities::member());
        let user_pk = root_sk.verifying_key().to_bytes();
        let (_dev_sk2, dev2) = DeviceIdentity::generate("temp", 200);
        let mut cert = DeviceCert {
            user_public_key: user_pk,
            device_public_key: dev2.public_key,
            device_label: dev2.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 300,
            expires_at_ns: Some(400),
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        let store = PeerIdentityStore::new();
        assert_eq!(
            store.insert(1, cert, 500).unwrap_err(),
            DeviceCertError::Expired
        );
    }

    #[test]
    fn insert_rejects_bad_signature() {
        let (_root_sk, _dev_sk, mut cert) = fresh_identity(DeviceCapabilities::primary());
        cert.signature[0] ^= 0x01;
        let store = PeerIdentityStore::new();
        assert!(store.insert(1, cert, 500).is_err());
    }

    #[test]
    fn verify_signature_end_to_end() {
        let (_root_sk, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let store = PeerIdentityStore::new();
        store.insert(1, cert, 500).unwrap();

        let msg = b"hello world";
        let sig = dev_sk.sign(msg).to_bytes();
        let identity = store
            .verify_signature_from(1, msg, &sig, 600, 700)
            .unwrap();
        assert!(identity.capabilities.can_own_workspaces);
    }

    #[test]
    fn verify_signature_rejects_tampered() {
        let (_root_sk, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let store = PeerIdentityStore::new();
        store.insert(1, cert, 500).unwrap();

        let msg = b"hello world";
        let mut sig = dev_sk.sign(msg).to_bytes();
        sig[0] ^= 0x01;
        assert_eq!(
            store
                .verify_signature_from(1, msg, &sig, 600, 700)
                .unwrap_err(),
            PeerIdentityError::BadSignature
        );
    }

    #[test]
    fn verify_signature_rejects_wrong_peer() {
        let (_root_sk, _dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let store = PeerIdentityStore::new();
        store.insert(1, cert, 500).unwrap();

        // A peer id that was never inserted returns UnknownPeer.
        assert_eq!(
            store
                .verify_signature_from(999, b"x", &[0u8; 64], 600, 700)
                .unwrap_err(),
            PeerIdentityError::UnknownPeer
        );
    }

    #[test]
    fn revocation_kills_future_ops() {
        use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let (root_sk, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let user_pk = cert.user_public_key;
        let device_pk = cert.device_public_key;
        let store = PeerIdentityStore::new();
        store.insert(1, cert, 500).unwrap();

        // Sig BEFORE the revocation cut-off — accepted.
        let msg = b"before";
        let sig = dev_sk.sign(msg).to_bytes();
        store
            .verify_signature_from(1, msg, &sig, 400, 500)
            .unwrap();

        // Publish a revocation at t=600.
        let mut rev = RevocationEntry {
            user_public_key: user_pk,
            revoked_device_public_key: device_pk,
            revoked_at_ns: 600,
            reason: RevocationReason::Compromised,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut rev, &root_sk).unwrap();
        let mut list = RevocationList::new();
        list.insert(rev).unwrap();
        store.set_revocations(user_pk, list);

        // Signed AT the cut-off — rejected.
        let msg = b"at";
        let sig = dev_sk.sign(msg).to_bytes();
        assert_eq!(
            store
                .verify_signature_from(1, msg, &sig, 600, 700)
                .unwrap_err(),
            PeerIdentityError::Revoked
        );

        // Old ops (signed before cut-off) still accepted — grace
        // for legitimately-authored history.
        let msg = b"still-old";
        let sig = dev_sk.sign(msg).to_bytes();
        store
            .verify_signature_from(1, msg, &sig, 500, 700)
            .unwrap();
    }

    #[test]
    fn different_users_get_separate_revocation_lists() {
        let (alice_root, _alice_dev, alice_cert) = fresh_identity(DeviceCapabilities::primary());
        let (bob_root, bob_dev, bob_cert) = fresh_identity(DeviceCapabilities::primary());
        let alice_user_pk = alice_cert.user_public_key;
        let bob_user_pk = bob_cert.user_public_key;
        let bob_device_pk = bob_cert.device_public_key;

        let store = PeerIdentityStore::new();
        store.insert(1, alice_cert, 500).unwrap();
        store.insert(2, bob_cert, 500).unwrap();

        // Revoke Bob's device.
        use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
        let mut rev = RevocationEntry {
            user_public_key: bob_user_pk,
            revoked_device_public_key: bob_device_pk,
            revoked_at_ns: 600,
            reason: RevocationReason::Stolen,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut rev, &bob_root).unwrap();
        let mut list = RevocationList::new();
        list.insert(rev).unwrap();
        store.set_revocations(bob_user_pk, list);

        // Bob's device is revoked …
        assert!(store.is_revoked(&bob_user_pk, &bob_device_pk, 700));
        // … but not under Alice's identity space.
        assert!(!store.is_revoked(&alice_user_pk, &bob_device_pk, 700));
        let _ = alice_root;
        let _ = bob_dev;
    }

    #[test]
    fn same_user_check() {
        let (_root_sk, _dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
        let user_pk = cert.user_public_key;
        let ident = PeerIdentity::from_cert(cert, 500).unwrap();
        assert!(ident.is_same_user(&user_pk));
        assert!(!ident.is_same_user(&[0u8; 32]));
    }
}
