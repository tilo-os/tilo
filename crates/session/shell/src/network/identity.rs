//! Peer identity — the shell-side wiring for user-scoped identities.
//!
//! A peer is named by its [`PeerOrigin`] (the complete iroh endpoint id,
//! as the transport authenticated it) — stable across restarts, but
//! silent about *who* the peer is until an out-of-band invite ties it to a workspace.
//!
//! The name is carried whole rather than folded to a `u64`, because this
//! table decides whether a signed CRDT op is accepted: under a 64-bit
//! key, an attacker who lands on a trusted peer's digest replaces that
//! peer's entry, and their own ops then verify *as* the trusted peer —
//! 64 bits is within reach of grinding, the full peer id is not.
//!
//! When two devices first exchange messages, each presents its device
//! public key (32-byte Ed25519) and a signed [`DeviceCert`] binding it to
//! the user's root public key. The shell reconstructs a [`PeerIdentity`]
//! from those for CRDT-op validation, share-session admission, and
//! revocation checks — an op whose cert fails to verify, has expired, or
//! has been revoked is dropped before it reaches the workspace tree.
//!
//! Persistence, transport, and CRDT-op validation itself live elsewhere;
//! this module only holds the verified-once, keyed-by-hash lookup table
//! ([`PeerIdentityStore::verify_signature_from`] is what `network::replica`
//! calls). The `DeviceCert` byte layout and revocation entry live in `tilo-user-identity`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ed25519_dalek::Verifier;

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceCertError, RevocationList, verify_device_cert,
    verify_device_cert_at,
};

use crate::shell::state::PeerOrigin;
use tilo_lock::RwLockRecover as _;

/// Everything the shell knows about a peer's identity.
///
/// Constructed at connect time from the peer's presented cert + device
/// pubkey; stored in a [`PeerIdentityStore`] keyed by [`PeerOrigin`] so `replica` and share-session code can look it up in O(1).
#[derive(Clone, Debug)]
pub struct PeerIdentity {
    /// Peer's device Ed25519 public key. 32 bytes.
    pub device_pubkey: [u8; 32],
    /// User root Ed25519 public key that certified this device. Multiple
    /// peers may share it (devices of the same user); matching the workspace owner's root key gives the peer owner permissions.
    pub user_pubkey: [u8; 32],
    /// The full signed device cert, retained so downstream validation can re-verify without a second lookup.
    pub device_cert: DeviceCert,
    /// Convenience copy of `device_cert.capabilities`.
    pub capabilities: DeviceCapabilities,
}

impl PeerIdentity {
    /// Build a `PeerIdentity` from a peer's presented cert: verifies the
    /// signature under the embedded `user_public_key`, checks it hasn't
    /// expired at `now_ns`. Does NOT check the revocation list — that lives with the [`PeerIdentityStore`], shared across peers.
    ///
    /// # Errors
    ///
    /// Returns `DeviceCertError` if the certificate signature does not verify,
    /// the embedded public key is invalid, or the certificate has expired.
    pub fn from_cert(cert: DeviceCert, now_ns: u64) -> Result<Self, DeviceCertError> {
        verify_device_cert_at(&cert, now_ns)?;
        Ok(Self {
            device_pubkey: cert.device_public_key,
            user_pubkey: cert.user_public_key,
            capabilities: cert.capabilities.clone(),
            device_cert: cert,
        })
    }

    /// True if this peer's user identity matches the given user root key.
    pub fn is_same_user(&self, user_pubkey: &[u8; 32]) -> bool {
        &self.user_pubkey == user_pubkey
    }
}

/// A thread-safe map from [`PeerOrigin`] to verified [`PeerIdentity`],
/// plus one [`RevocationList`] per user identity (keyed by root pubkey, since different peers can belong to different users).
#[derive(Clone, Default)]
pub struct PeerIdentityStore {
    inner: Arc<RwLock<PeerIdentityStoreInner>>,
}

#[derive(Default)]
struct PeerIdentityStoreInner {
    by_peer: HashMap<PeerOrigin, PeerIdentity>,
    /// Per-user-identity revocation lists — stable across cert cache churn, since revocations are additive.
    revocations: HashMap<[u8; 32], RevocationList>,
    /// Verified root statements naming who governs in place of a revoked
    /// device; additive like the revocations they carry.
    successions: Vec<tilo_user_identity::GovernorSuccession>,
}

impl PeerIdentityStore {
    /// Fresh empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a peer's identity. `Err` if the cert's signature
    /// doesn't verify or the cert has expired at `now_ns`.
    ///
    /// `peer` must be the origin the *transport* authenticated, never one a
    /// payload claimed: an entry inserted under someone else's name is an entry that verifies that someone else's ops.
    ///
    /// # Errors
    ///
    /// Returns `DeviceCertError` if the certificate signature does not verify,
    /// the embedded public key is invalid, or the certificate has expired.
    pub fn insert(
        &self,
        peer: PeerOrigin,
        cert: DeviceCert,
        now_ns: u64,
    ) -> Result<PeerIdentity, DeviceCertError> {
        let identity = PeerIdentity::from_cert(cert, now_ns)?;
        let mut inner = self.inner.write_recover();
        let changed = inner
            .by_peer
            .insert(peer, identity.clone())
            .is_none_or(|previous| previous.device_cert != identity.device_cert);
        drop(inner);
        // Channel stewards read the devices the session authenticated; a
        // handshake that repeats what is known changes nothing they read.
        if changed {
            tilo_identity_host::policy_change::announce();
        }
        Ok(identity)
    }

    /// Drop a peer's cached identity — used on disconnect. Idempotent.
    pub fn remove(&self, peer: &PeerOrigin) {
        let mut inner = self.inner.write_recover();
        let removed = inner.by_peer.remove(peer).is_some();
        drop(inner);
        if removed {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// The first instant after `now_ns` at which [`Self::devices_of`] can
    /// answer differently with nothing written: a certificate expiring or a
    /// revocation dated ahead taking effect. `None` while none is ahead.
    pub fn next_change_after(&self, now_ns: u64) -> Option<u64> {
        let inner = self.inner.read_recover();
        let expiries = inner
            .by_peer
            .values()
            .filter_map(|identity| identity.device_cert.expires_at_ns);
        let revocations = inner
            .revocations
            .values()
            .flat_map(RevocationList::iter)
            .map(|entry| entry.revoked_at_ns);
        let next = expiries.chain(revocations).filter(|at| *at > now_ns).min();
        drop(inner);
        next
    }

    /// Look up a peer by its [`PeerOrigin`]: `None` once its certificate
    /// expired or its root revoked it, even if the revocation arrived after
    /// the peer authenticated.
    pub fn get(&self, peer: &PeerOrigin) -> Option<PeerIdentity> {
        let now_ns = tilo_clock::unix_now_ns();
        let inner = self.inner.read_recover();
        inner
            .by_peer
            .get(peer)
            .filter(|identity| !identity.device_cert.is_expired_at(now_ns))
            .filter(|identity| {
                !inner
                    .revocations
                    .get(&identity.user_pubkey)
                    .is_some_and(|list| list.is_revoked_at(&identity.device_pubkey, now_ns))
            })
            .cloned()
    }

    /// Keep the verified `successions` not yet held.
    pub fn add_successions(&self, successions: &[tilo_user_identity::GovernorSuccession]) {
        let mut inner = self.inner.write_recover();
        let mut changed = false;
        for succession in successions {
            if !inner.successions.contains(succession) {
                inner.successions.push(succession.clone());
                changed = true;
            }
        }
        drop(inner);
        if changed {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// Every verified governor succession held.
    pub fn successions(&self) -> Vec<tilo_user_identity::GovernorSuccession> {
        self.inner.read_recover().successions.clone()
    }

    /// The certificates of `user_pubkey`'s devices the transport authenticated
    /// and that are valid and not revoked at `now_ns`.
    pub fn devices_of(&self, user_pubkey: &[u8; 32], now_ns: u64) -> Vec<DeviceCert> {
        let inner = self.inner.read_recover();
        let revoked = |device: &[u8; 32]| {
            inner
                .revocations
                .get(user_pubkey)
                .is_some_and(|list| list.is_revoked_at(device, now_ns))
        };
        inner
            .by_peer
            .values()
            .filter(|identity| identity.user_pubkey == *user_pubkey)
            .filter(|identity| !revoked(&identity.device_pubkey))
            .filter(|identity| !identity.device_cert.is_expired_at(now_ns))
            .map(|identity| identity.device_cert.clone())
            .collect()
    }

    /// The person (user root key) behind every authenticated peer whose
    /// certificate is still valid and not revoked at `now_ns`.
    pub fn authenticated_persons(
        &self,
        now_ns: u64,
    ) -> std::collections::HashMap<PeerOrigin, [u8; 32]> {
        let inner = self.inner.read_recover();
        inner
            .by_peer
            .iter()
            .filter(|(_, identity)| !identity.device_cert.is_expired_at(now_ns))
            .filter(|(_, identity)| {
                !inner
                    .revocations
                    .get(&identity.user_pubkey)
                    .is_some_and(|list| list.is_revoked_at(&identity.device_pubkey, now_ns))
            })
            .map(|(peer, identity)| (peer.clone(), identity.user_pubkey))
            .collect()
    }

    /// Merge a user's revocation list — additive, see [`RevocationList::insert`].
    pub fn set_revocations(&self, user_pubkey: [u8; 32], list: &RevocationList) {
        let mut inner = self.inner.write_recover();
        let current = inner.revocations.entry(user_pubkey).or_default();
        let mut changed = false;
        for entry in list.iter().cloned() {
            // Never let a later handshake erase or postpone a revocation that
            // this device has already verified.
            changed |= current.insert(entry).unwrap_or(false);
        }
        drop(inner);
        if changed {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// True if `device_pubkey` has been revoked under `user_pubkey` at or before `at_ns`.
    pub fn is_revoked(&self, user_pubkey: &[u8; 32], device_pubkey: &[u8; 32], at_ns: u64) -> bool {
        let inner = self.inner.read_recover();
        inner
            .revocations
            .get(user_pubkey)
            .is_some_and(|list| list.is_revoked_at(device_pubkey, at_ns))
    }

    /// Verify an Ed25519 signature made by `peer`'s device against a
    /// message. Full identity-chain check: peer registered, cert still
    /// verifies under the embedded user root key, cert not expired at
    /// `now_ns`, device not revoked at or before `signed_at_ns`, signature verifies under the device pubkey. `Ok(PeerIdentity)` on all-pass.
    ///
    /// # Errors
    ///
    /// Returns `PeerIdentityError` if:
    /// - The peer is not in the identity store (`UnknownPeer`).
    /// - The peer's certificate signature does not verify (`Cert`).
    /// - The peer's certificate has expired at `now_ns` (`CertExpired`).
    /// - The peer's device has been revoked at or before `signed_at_ns` (`Revoked`).
    /// - The device public key is invalid (`BadDevicePubkey`).
    /// - The Ed25519 signature does not verify (`BadSignature`).
    ///
    /// # Panics
    ///
    /// Panics if the internal `RwLock` has been poisoned.
    pub fn verify_signature_from(
        &self,
        peer: &PeerOrigin,
        message: &[u8],
        signature: &[u8; 64],
        signed_at_ns: u64,
        now_ns: u64,
    ) -> Result<PeerIdentity, PeerIdentityError> {
        // Not `get`: that drops a peer revoked by the wall clock, while a
        // signature is judged at `signed_at_ns` (history authored before the
        // cut-off stays valid) and expiry at the caller's `now_ns`.
        let identity = self
            .inner
            .read_recover()
            .by_peer
            .get(peer)
            .cloned()
            .ok_or(PeerIdentityError::UnknownPeer)?;
        // Re-verify the cert — defence in depth against a corrupted in-memory copy.
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
    /// No cached identity for that peer — caller should either re-request the cert or drop the message.
    #[error("no cached identity for peer")]
    UnknownPeer,
    /// Cert-level error (bad signature, invalid pubkey, expired, etc.).
    #[error("device cert invalid: {0}")]
    Cert(#[from] DeviceCertError),
    /// The cert has expired.
    #[error("device cert expired")]
    CertExpired,
    /// The peer's device has been revoked at or before the message's `signed_at_ns` timestamp.
    #[error("peer device is revoked")]
    Revoked,
    /// The peer's device pubkey doesn't decode to a valid Ed25519 point — reachable only for a cert tampered with post-verification.
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
    use tilo_user_identity::{DeviceIdentity, UserIdentity, sign_device_cert};

    /// Build a fresh (user root sk, device sk, cert) for tests.
    fn fresh_identity(caps: DeviceCapabilities) -> (SigningKey, SigningKey, DeviceCert) {
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

    /// A peer, named the way the transport names it.
    fn peer(n: u8) -> PeerOrigin {
        PeerOrigin::peer(&iroh::SecretKey::from_bytes(&[n; 32]).public())
    }

    #[test]
    fn identity_store_keys_entries_per_peer_and_rejects_invalid_ones() {
        // Phase `insert_and_lookup`.
        {
            let (_, _, cert) = fresh_identity(DeviceCapabilities::primary());
            let store = PeerIdentityStore::new();
            let who = peer(7);
            let ident = store.insert(who.clone(), cert.clone(), 500).unwrap();
            assert_eq!(ident.device_pubkey, cert.device_public_key);
            assert!(ident.capabilities.can_admit_devices);
            let looked_up = store.get(&who).unwrap();
            assert_eq!(looked_up.device_pubkey, cert.device_public_key);
        }
        // Phase `distinct_peers_never_share_an_entry`.
        // The store is keyed by the whole peer id, so two peers a 64-bit fold
        // could collide keep separate entries (see the module doc's digest-collision attack).
        {
            let (_, dev_sk_a, cert_a) = fresh_identity(DeviceCapabilities::primary());
            let (_, _, cert_b) = fresh_identity(DeviceCapabilities::member());
            let store = PeerIdentityStore::new();
            let a = peer(1);
            let b = peer(2);
            store.insert(a.clone(), cert_a.clone(), 500).unwrap();
            store.insert(b.clone(), cert_b.clone(), 500).unwrap();

            assert_ne!(a, b, "two peers must not share a name");
            assert_eq!(
                store.get(&a).unwrap().device_pubkey,
                cert_a.device_public_key,
                "registering b must not have displaced a"
            );
            assert_eq!(
                store.get(&b).unwrap().device_pubkey,
                cert_b.device_public_key
            );

            // And a's signature does not verify when attributed to b.
            let msg = b"authored by a";
            let sig = dev_sk_a.sign(msg).to_bytes();
            store
                .verify_signature_from(&a, msg, &sig, 600, 700)
                .unwrap();
            assert_eq!(
                store
                    .verify_signature_from(&b, msg, &sig, 600, 700)
                    .unwrap_err(),
                PeerIdentityError::BadSignature
            );
        }
        // Phase `hub_origin_does_not_alias_a_peer_origin`.
        // A hub subscriber id can never be mistaken for a peer id — the namespacing `PeerOrigin` applies is what the store keys on.
        {
            let (_, _, cert) = fresh_identity(DeviceCapabilities::primary());
            let store = PeerIdentityStore::new();
            store.insert(PeerOrigin::hub(1), cert, 500).unwrap();
            assert!(store.get(&peer(1)).is_none());
            assert!(store.get(&PeerOrigin::hub(1)).is_some());
        }
        // Phase `insert_rejects_expired`.
        {
            let (root_sk, _, _) = fresh_identity(DeviceCapabilities::member());
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
                store.insert(peer(1), cert, 500).unwrap_err(),
                DeviceCertError::Expired
            );
        }
        // Phase `insert_rejects_bad_signature`.
        {
            let (_, _, mut cert) = fresh_identity(DeviceCapabilities::primary());
            cert.signature[0] ^= 0x01;
            let store = PeerIdentityStore::new();
            assert!(store.insert(peer(1), cert, 500).is_err());
        }
    }

    #[test]
    fn signatures_verify_only_for_the_signing_peer_and_user() {
        // Phase `verify_signature_end_to_end`.
        {
            let (_, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
            let store = PeerIdentityStore::new();
            store.insert(peer(1), cert, 500).unwrap();

            let msg = b"hello world";
            let sig = dev_sk.sign(msg).to_bytes();
            let identity = store
                .verify_signature_from(&peer(1), msg, &sig, 600, 700)
                .unwrap();
            assert!(identity.capabilities.can_own_workspaces);
        }
        // Phase `verify_signature_rejects_tampered`.
        {
            let (_, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
            let store = PeerIdentityStore::new();
            store.insert(peer(1), cert, 500).unwrap();

            let msg = b"hello world";
            let mut sig = dev_sk.sign(msg).to_bytes();
            sig[0] ^= 0x01;
            assert_eq!(
                store
                    .verify_signature_from(&peer(1), msg, &sig, 600, 700)
                    .unwrap_err(),
                PeerIdentityError::BadSignature
            );
        }
        // Phase `verify_signature_rejects_wrong_peer`.
        {
            let (_, _, cert) = fresh_identity(DeviceCapabilities::primary());
            let store = PeerIdentityStore::new();
            store.insert(peer(1), cert, 500).unwrap();

            // A peer id that was never inserted returns UnknownPeer.
            assert_eq!(
                store
                    .verify_signature_from(&peer(99), b"x", &[0u8; 64], 600, 700)
                    .unwrap_err(),
                PeerIdentityError::UnknownPeer
            );
        }
        // Phase `same_user_check`.
        {
            let (_, _, cert) = fresh_identity(DeviceCapabilities::primary());
            let user_pk = cert.user_public_key;
            let ident = PeerIdentity::from_cert(cert, 500).unwrap();
            assert!(ident.is_same_user(&user_pk));
            assert!(!ident.is_same_user(&[0u8; 32]));
        }
    }

    #[test]
    fn revocation_is_per_user_and_blocks_future_ops() {
        // Phase `revocation_kills_future_ops`.
        {
            use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

            let (root_sk, dev_sk, cert) = fresh_identity(DeviceCapabilities::primary());
            let user_pk = cert.user_public_key;
            let device_pk = cert.device_public_key;
            let store = PeerIdentityStore::new();
            store.insert(peer(1), cert, 500).unwrap();

            // Sig BEFORE the revocation cut-off — accepted.
            let msg = b"before";
            let sig = dev_sk.sign(msg).to_bytes();
            store
                .verify_signature_from(&peer(1), msg, &sig, 400, 500)
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
            store.set_revocations(user_pk, &list);
            // A later peer advertising an empty/stale set cannot erase what we
            // already learned.
            store.set_revocations(user_pk, &RevocationList::new());

            // Signed AT the cut-off — rejected.
            let msg = b"at";
            let sig = dev_sk.sign(msg).to_bytes();
            assert_eq!(
                store
                    .verify_signature_from(&peer(1), msg, &sig, 600, 700)
                    .unwrap_err(),
                PeerIdentityError::Revoked
            );

            // Old ops (signed before cut-off) still accepted — grace for legitimately-authored history.
            let msg = b"still-old";
            let sig = dev_sk.sign(msg).to_bytes();
            store
                .verify_signature_from(&peer(1), msg, &sig, 500, 700)
                .unwrap();
        }
        // Phase `different_users_get_separate_revocation_lists`.
        {
            use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

            let (_, _, alice_cert) = fresh_identity(DeviceCapabilities::primary());
            let (bob_root, _, bob_cert) = fresh_identity(DeviceCapabilities::primary());
            let alice_user_pk = alice_cert.user_public_key;
            let bob_user_pk = bob_cert.user_public_key;
            let bob_device_pk = bob_cert.device_public_key;

            let store = PeerIdentityStore::new();
            store.insert(peer(1), alice_cert, 500).unwrap();
            store.insert(peer(2), bob_cert, 500).unwrap();

            // Revoke Bob's device.
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
            store.set_revocations(bob_user_pk, &list);

            // Bob's device is revoked …
            assert!(store.is_revoked(&bob_user_pk, &bob_device_pk, 700));
            // … but not under Alice's identity space.
            assert!(!store.is_revoked(&alice_user_pk, &bob_device_pk, 700));
        }
    }

    /// #4022: channel stewards read the authenticated devices without a
    /// clock, so the store announces each change and names the instant its
    /// answer next changes by time alone.
    #[test]
    fn authenticated_devices_announce_changes_and_name_their_next_expiry() {
        use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
        let (root, _, mut cert) = fresh_identity(DeviceCapabilities::primary());
        cert.expires_at_ns = Some(5_000);
        sign_device_cert(&mut cert, &root).unwrap();
        let user = cert.user_public_key;
        let device = cert.device_public_key;
        let store = PeerIdentityStore::new();
        assert_eq!(store.next_change_after(500), None);

        let epoch = tilo_identity_host::policy_change::epoch();
        store.insert(peer(1), cert, 500).unwrap();
        assert_ne!(
            tilo_identity_host::policy_change::epoch(),
            epoch,
            "a newly authenticated device is announced"
        );
        assert_eq!(store.next_change_after(500), Some(5_000), "its expiry");

        // A revocation dated ahead of the certificate's end takes effect
        // first.
        let mut revocation = RevocationEntry {
            user_public_key: user,
            revoked_device_public_key: device,
            revoked_at_ns: 2_000,
            reason: RevocationReason::Compromised,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        let mut list = RevocationList::new();
        list.insert(revocation).unwrap();
        let epoch = tilo_identity_host::policy_change::epoch();
        store.set_revocations(user, &list);
        assert_ne!(
            tilo_identity_host::policy_change::epoch(),
            epoch,
            "a new revocation is announced"
        );
        assert_eq!(store.next_change_after(500), Some(2_000));
        assert_eq!(store.next_change_after(2_000), Some(5_000));
        assert_eq!(store.next_change_after(5_000), None);
        assert_eq!(store.devices_of(&user, 1_999).len(), 1);
        assert!(store.devices_of(&user, 2_000).is_empty());

        let epoch = tilo_identity_host::policy_change::epoch();
        store.remove(&peer(1));
        assert_ne!(
            tilo_identity_host::policy_change::epoch(),
            epoch,
            "a disconnected device is announced"
        );
    }
}
