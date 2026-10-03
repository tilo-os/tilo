//! Authenticated handshake between a relay and a peer.
//!
//! As soon as a connection opens, both sides perform this challenge/response:
//!
//! ```text
//! client → server:   Hello { cert: DeviceCert, nonce: [u8;32] }
//! server checks cert; if the client is admissible under the trust
//! policy, replies:
//! server → client:   HelloAck {
//!     cert: DeviceCert,
//!     nonce: [u8;32],
//!     sig_over_client_nonce: [u8;64],
//! }
//! client checks the server's cert + verifies
//! sig_over_client_nonce with the server's device pubkey; replies:
//! client → server:   HelloAck2 { sig_over_server_nonce: [u8;64] }
//! ```
//!
//! The signatures cover a BLAKE3 digest of a fixed domain tag
//! concatenated with the challenge nonce, so a signature under one
//! nonce cannot be replayed as a signature under any other nonce or
//! protocol.
//!
//! **Why nonces both ways.** A pure "prove you hold the private key"
//! flow only needs one nonce. We do two so the client also
//! authenticates the server — otherwise a MITM could accept a
//! client's connection and forward its cert to the real relay.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

use crate::policy::TrustPolicy;

/// Domain tag bound into every mutual-authentication challenge signature.
const TILO_AUTH_NONCE_TAG: &[u8] = b"tilo-relay-auth-v1\0";

/// First message: client → server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// Client's device cert.
    pub cert: DeviceCert,
    /// Fresh 32-byte challenge nonce the client wants the server to
    /// sign.
    pub client_nonce: [u8; 32],
}

/// Second message: server → client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    /// Server's device cert (the relay's cert).
    pub cert: DeviceCert,
    /// Fresh 32-byte challenge nonce the server wants the client
    /// to sign.
    pub server_nonce: [u8; 32],
    /// Ed25519 signature by the server's *device* signing key over
    /// `blake3(AUTH_NONCE_TAG || client_nonce)`. Proves the server
    /// holds the private key matching the cert.
    #[serde(with = "crate::sig64")]
    pub sig_over_client_nonce: [u8; 64],
}

/// Third message: client → server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloAck2 {
    /// Ed25519 signature by the client's device signing key over
    /// `blake3(AUTH_NONCE_TAG || server_nonce)`.
    #[serde(with = "crate::sig64")]
    pub sig_over_server_nonce: [u8; 64],
}

/// What kind of peer sits on the other end of an authenticated
/// stream. Set at handshake time from the device cert's capabilities
/// and threaded through the peer session so fan-out can distinguish
/// "another user device" from "another relay in a federated mesh".
///
/// The important consequence: a peer relay's messages must NOT be
/// re-forwarded to other peer relays (both live and store-and-
/// forward), because the shared automerge doc would echo forever
/// otherwise. See the daemon's `WorkspaceRuntime::fanout_delta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerRole {
    /// A user device (laptop / phone / desktop) — subscribes to
    /// receive workspace ops.
    Device,
    /// Another relay hosting one or more of the same workspaces —
    /// federated peer.
    PeerRelay,
}

impl PeerRole {
    /// Derive the role from a device cert's capabilities. A cert
    /// with `can_act_as_host = true` is treated as a peer relay when
    /// it dials another host; every other cert is a plain device.
    pub fn from_cert(cert: &DeviceCert) -> Self {
        if cert.capabilities.can_act_as_host {
            Self::PeerRelay
        } else {
            Self::Device
        }
    }
}

/// Authenticated peer state that survives past the handshake — used
/// by the daemon to route subsequent messages.
#[derive(Clone, Debug)]
pub struct AuthenticatedPeer {
    /// Verified device cert.
    pub cert: DeviceCert,
    /// The user root pubkey this peer chains to. Copied out for
    /// cheap comparisons.
    pub user_public_key: [u8; 32],
    /// The peer's device pubkey. Copied out for the same reason.
    pub device_public_key: [u8; 32],
    /// Which side of the federation this peer represents. Derived
    /// from `cert.capabilities.can_act_as_host` — a device with the
    /// host capability that dials in is federating, not subscribing.
    pub role: PeerRole,
}

/// Failure modes for the handshake.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The peer's cert did not verify or was expired.
    #[error("peer cert invalid: {0}")]
    Cert(#[from] DeviceCertError),
    /// The peer's device is on the revocation list.
    #[error("peer device is revoked")]
    Revoked,
    /// The peer's cert is not admissible under the local trust
    /// policy (wrong user identity).
    #[error("peer not admissible under trust policy")]
    PolicyReject,
    /// The nonce-signature did not verify. Almost always a MITM,
    /// a replay, or a bug in the peer's implementation.
    #[error("challenge signature did not verify")]
    BadChallengeSig,
}

/// Compute the domain-bound challenge digest both sides sign.
pub fn challenge_digest(nonce: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TILO_AUTH_NONCE_TAG);
    hasher.update(nonce);
    *hasher.finalize().as_bytes()
}

/// Server-side: verify the client's `Hello` against `own_user_pk` +
/// `policy` + `revocations`. Returns the authenticated peer state
/// on success.
///
/// # Errors
///
/// Returns [`AuthError::Cert`] if the client's cert doesn't verify or
/// is expired, [`AuthError::Revoked`] if its device is revoked, or
/// [`AuthError::PolicyReject`] if `policy` does not admit it.
pub fn accept_hello(
    hello: &Hello,
    own_user_public_key: &[u8; 32],
    policy: &TrustPolicy,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<AuthenticatedPeer, AuthError> {
    verify_device_cert_at(&hello.cert, now_ns)?;
    if revocations.is_revoked_at(&hello.cert.device_public_key, now_ns) {
        return Err(AuthError::Revoked);
    }
    if !policy.admits(&hello.cert, own_user_public_key) {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        role: PeerRole::from_cert(&hello.cert),
        cert: hello.cert.clone(),
        user_public_key: hello.cert.user_public_key,
        device_public_key: hello.cert.device_public_key,
    })
}

/// Server-side: build the `HelloAck` reply — signs the client's
/// nonce with the relay's device signing key.
pub fn build_hello_ack(
    own_cert: DeviceCert,
    own_signing_key: &ed25519_dalek::SigningKey,
    server_nonce: [u8; 32],
    client_nonce: &[u8; 32],
) -> HelloAck {
    let digest = challenge_digest(client_nonce);
    let sig = own_signing_key.sign(&digest).to_bytes();
    HelloAck {
        cert: own_cert,
        server_nonce,
        sig_over_client_nonce: sig,
    }
}

/// Client-side: verify the server's `HelloAck` — checks the cert
/// chain, expiry, policy admission (from *client's* point of view:
/// the server must chain to our own user identity), revocation,
/// and the nonce signature.
///
/// # Errors
///
/// Returns [`AuthError::Cert`] if the server's cert doesn't verify or
/// is expired, [`AuthError::PolicyReject`] if it isn't host-capable or
/// doesn't chain to `own_user_public_key`, [`AuthError::Revoked`] if
/// its device is revoked, or [`AuthError::BadChallengeSig`] if the
/// nonce signature doesn't verify.
pub fn verify_hello_ack(
    ack: &HelloAck,
    own_user_public_key: &[u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_inner(
        ack,
        Some(own_user_public_key),
        revocations,
        now_ns,
        challenge_client_nonce,
    )
}

/// Verify a server reached through an already-pinned Iroh endpoint without
/// requiring it to share the client's user root. This is the hosted-service
/// path: Iroh authenticates the endpoint from `EndpointAddr`, while this
/// handshake verifies that the server owns the device key in its valid cert.
///
/// # Errors
///
/// Returns [`AuthError::Cert`] if the server's cert doesn't verify or
/// is expired, [`AuthError::PolicyReject`] if it isn't host-capable,
/// [`AuthError::Revoked`] if its device is revoked, or
/// [`AuthError::BadChallengeSig`] if the nonce signature doesn't
/// verify.
pub fn verify_pinned_host_ack(
    ack: &HelloAck,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_inner(ack, None, revocations, now_ns, challenge_client_nonce)
}

fn verify_hello_ack_inner(
    ack: &HelloAck,
    expected_user_public_key: Option<&[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_cryptographic(ack, revocations, now_ns, challenge_client_nonce)?;
    if !ack.cert.capabilities.can_act_as_host {
        return Err(AuthError::PolicyReject);
    }
    // A same-owner client pins the expected user root. A hosted-service
    // client instead pins the Iroh EndpointAddr and accepts any valid server
    // root, while still requiring proof of the certified device key.
    if let Some(expected) = expected_user_public_key
        && &ack.cert.user_public_key != expected
    {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        role: PeerRole::from_cert(&ack.cert),
        cert: ack.cert.clone(),
        user_public_key: ack.cert.user_public_key,
        device_public_key: ack.cert.device_public_key,
    })
}

/// Verify the exact primary device authorized by a signed, room-scoped
/// interactive-owner hint. This exception does not grant host capability and
/// cannot be reached from the generic pinned-host API.
///
/// # Errors
///
/// Returns [`AuthError`] unless the ACK certificate is the exact expected
/// governor, is current and non-revoked, can govern workspaces, and proves the
/// challenge with its certified device key.
pub fn verify_interactive_owner_ack(
    ack: &HelloAck,
    expected_governor: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_cryptographic(ack, revocations, now_ns, challenge_client_nonce)?;
    if &ack.cert != expected_governor || !ack.cert.capabilities.can_own_workspaces {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        role: PeerRole::from_cert(&ack.cert),
        cert: ack.cert.clone(),
        user_public_key: ack.cert.user_public_key,
        device_public_key: ack.cert.device_public_key,
    })
}

fn verify_hello_ack_cryptographic(
    ack: &HelloAck,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<(), AuthError> {
    verify_device_cert_at(&ack.cert, now_ns)?;
    if revocations.is_revoked_at(&ack.cert.device_public_key, now_ns) {
        return Err(AuthError::Revoked);
    }
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&ack.cert.device_public_key)
        .map_err(|_| AuthError::BadChallengeSig)?;
    let digest = challenge_digest(challenge_client_nonce);
    let sig = ed25519_dalek::Signature::from_bytes(&ack.sig_over_client_nonce);
    vk.verify(&digest, &sig)
        .map_err(|_| AuthError::BadChallengeSig)
}

/// Server-side: verify the client's `HelloAck2` — its signature
/// over our nonce.
///
/// # Errors
///
/// Returns [`AuthError::BadChallengeSig`] if `client_device_pubkey`
/// is not a valid Ed25519 key or the signature over `server_nonce`
/// doesn't verify.
pub fn verify_hello_ack2(
    ack: &HelloAck2,
    client_device_pubkey: &[u8; 32],
    server_nonce: &[u8; 32],
) -> Result<(), AuthError> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(client_device_pubkey)
        .map_err(|_| AuthError::BadChallengeSig)?;
    let digest = challenge_digest(server_nonce);
    let sig = ed25519_dalek::Signature::from_bytes(&ack.sig_over_server_nonce);
    vk.verify(&digest, &sig)
        .map_err(|_| AuthError::BadChallengeSig)?;
    Ok(())
}

/// Client-side: build the final `HelloAck2` — signs the server's
/// nonce with our device signing key.
pub fn build_hello_ack2(
    own_signing_key: &ed25519_dalek::SigningKey,
    server_nonce: &[u8; 32],
) -> HelloAck2 {
    let digest = challenge_digest(server_nonce);
    let sig = own_signing_key.sign(&digest).to_bytes();
    HelloAck2 {
        sig_over_server_nonce: sig,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceIdentity, RevocationEntry, RevocationList, RevocationReason,
        UserIdentity, sign_device_cert, sign_revocation_entry,
    };

    struct Party {
        cert: DeviceCert,
        sk: SigningKey,
    }

    fn enrol(
        root_sk: &SigningKey,
        user_pk: [u8; 32],
        label: &str,
        caps: DeviceCapabilities,
    ) -> Party {
        let (dev_sk, dev) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: user_pk,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, root_sk).unwrap();
        Party { cert, sk: dev_sk }
    }

    #[test]
    fn server_accepts_only_trusted_unrevoked_devices_and_completes_mutual_auth() {
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let relay = enrol(
            &root_sk,
            user.public_key,
            "server",
            DeviceCapabilities::host(),
        );
        let client = enrol(
            &root_sk,
            user.public_key,
            "laptop",
            DeviceCapabilities::primary(),
        );

        // Happy path: both directions verify.
        let revs = RevocationList::new();
        let client_nonce = [1u8; 32];
        let server_nonce = [2u8; 32];
        let hello = Hello {
            cert: client.cert.clone(),
            client_nonce,
        };
        let server_peer = accept_hello(
            &hello,
            &user.public_key,
            &TrustPolicy::default(),
            &revs,
            1000,
        )
        .unwrap();
        assert_eq!(server_peer.device_public_key, client.cert.device_public_key);
        let ack = build_hello_ack(relay.cert.clone(), &relay.sk, server_nonce, &client_nonce);
        let client_peer =
            verify_hello_ack(&ack, &user.public_key, &revs, 1000, &client_nonce).unwrap();
        assert_eq!(client_peer.device_public_key, relay.cert.device_public_key);
        let ack2 = build_hello_ack2(&client.sk, &server_nonce);
        verify_hello_ack2(&ack2, &client.cert.device_public_key, &server_nonce).unwrap();

        // Wrong user: the relay is Alice's; the client presents Bob's cert.
        let (bob_root, bob) = UserIdentity::generate("Bob", 1);
        let bob_client = enrol(
            &bob_root,
            bob.public_key,
            "laptop",
            DeviceCapabilities::primary(),
        );
        let hello = Hello {
            cert: bob_client.cert,
            client_nonce: [0u8; 32],
        };
        assert!(matches!(
            accept_hello(
                &hello,
                &user.public_key,
                &TrustPolicy::default(),
                &revs,
                1000
            )
            .unwrap_err(),
            AuthError::PolicyReject
        ));

        // Revoked: Alice's own laptop, revoked at 500, connecting at 1000.
        let mut rev = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: client.cert.device_public_key,
            revoked_at_ns: 500,
            reason: RevocationReason::Compromised,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut rev, &root_sk).unwrap();
        let mut revoked = RevocationList::new();
        revoked.insert(rev).unwrap();
        let hello = Hello {
            cert: client.cert,
            client_nonce: [0u8; 32],
        };
        assert!(matches!(
            accept_hello(
                &hello,
                &user.public_key,
                &TrustPolicy::default(),
                &revoked,
                1000
            )
            .unwrap_err(),
            AuthError::Revoked
        ));
    }

    /// `Hello` as a build from before the foreign-device marker encodes it.
    mod v1 {
        #[allow(
            clippy::struct_excessive_bools,
            reason = "a frozen copy of the v1 wire type"
        )]
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCapabilities {
            pub can_own_workspaces: bool,
            pub can_admit_devices: bool,
            pub can_act_as_host: bool,
            pub can_read_all_workspaces: bool,
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCert {
            pub user_public_key: [u8; 32],
            pub device_public_key: [u8; 32],
            pub device_label: String,
            pub capabilities: DeviceCapabilities,
            pub issued_at_ns: u64,
            pub expires_at_ns: Option<u64>,
            #[serde(with = "crate::sig64")]
            pub signature: [u8; 64],
        }

        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct Hello {
            pub cert: DeviceCert,
            pub client_nonce: [u8; 32],
        }
    }

    /// An old relay decodes the frame with its own types; for what it can
    /// decode, verification is the unchanged v1 path.
    fn old_relay_accepts(frame: &[u8], user: &[u8; 32]) -> bool {
        postcard::from_bytes::<v1::Hello>(frame).is_ok() && new_relay_accepts(frame, user).is_some()
    }

    fn new_relay_accepts(frame: &[u8], user: &[u8; 32]) -> Option<AuthenticatedPeer> {
        let hello: Hello = postcard::from_bytes(frame).ok()?;
        let policy = TrustPolicy::default();
        accept_hello(&hello, user, &policy, &RevocationList::new(), 1000).ok()
    }

    #[test]
    fn relay_handshakes_across_certificate_versions() {
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let native = enrol(
            &root_sk,
            user.public_key,
            "laptop",
            DeviceCapabilities::primary(),
        );
        let foreign = enrol(
            &root_sk,
            user.public_key,
            "mac",
            DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::primary()
            },
        );
        let hello = |cert: &DeviceCert| {
            postcard::to_stdvec(&Hello {
                cert: cert.clone(),
                client_nonce: [1; 32],
            })
            .unwrap()
        };

        // Old client, new relay: the old client's frame is the native frame.
        let caps = &native.cert.capabilities;
        let old_frame = postcard::to_stdvec(&v1::Hello {
            cert: v1::DeviceCert {
                user_public_key: native.cert.user_public_key,
                device_public_key: native.cert.device_public_key,
                device_label: native.cert.device_label.clone(),
                capabilities: v1::DeviceCapabilities {
                    can_own_workspaces: caps.can_own_workspaces,
                    can_admit_devices: caps.can_admit_devices,
                    can_act_as_host: caps.can_act_as_host,
                    can_read_all_workspaces: caps.can_read_all_workspaces,
                },
                issued_at_ns: native.cert.issued_at_ns,
                expires_at_ns: native.cert.expires_at_ns,
                signature: native.cert.signature,
            },
            client_nonce: [1; 32],
        })
        .unwrap();
        assert_eq!(hello(&native.cert), old_frame);
        assert!(new_relay_accepts(&old_frame, &user.public_key).is_some());

        // New native client, old relay.
        assert!(old_relay_accepts(&hello(&native.cert), &user.public_key));

        // New foreign client: an old relay cannot decode it; a new relay
        // admits it with the marker the room's role cap reads.
        let foreign_frame = hello(&foreign.cert);
        assert!(!old_relay_accepts(&foreign_frame, &user.public_key));
        let peer = new_relay_accepts(&foreign_frame, &user.public_key).unwrap();
        assert!(peer.cert.capabilities.foreign);
        assert_eq!(peer.role, PeerRole::Device);
    }

    #[test]
    fn client_rejects_acks_from_strangers_non_hosts_and_bad_signatures() {
        let (root_sk, alice) = UserIdentity::generate("Alice", 1);
        let relay = enrol(
            &root_sk,
            alice.public_key,
            "server",
            DeviceCapabilities::host(),
        );
        let revs = RevocationList::new();

        // Bob's device answers Alice's client: the signature is valid but the
        // wrong-user policy rejects it.
        let (bob_root, bob) = UserIdentity::generate("Bob", 1);
        let bob_dev = enrol(
            &bob_root,
            bob.public_key,
            "attacker",
            DeviceCapabilities::host(),
        );
        let client_nonce = [7u8; 32];
        let ack = build_hello_ack(bob_dev.cert.clone(), &bob_dev.sk, [0u8; 32], &client_nonce);
        assert!(matches!(
            verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &client_nonce).unwrap_err(),
            AuthError::PolicyReject
        ));

        // A pinned host must hold the host capability.
        let (provider_root, provider) = UserIdentity::generate("Provider", 1);
        let ordinary_device = enrol(
            &provider_root,
            provider.public_key,
            "ordinary device",
            DeviceCapabilities::member(),
        );
        let client_nonce = [8u8; 32];
        let ack = build_hello_ack(
            ordinary_device.cert,
            &ordinary_device.sk,
            [9u8; 32],
            &client_nonce,
        );
        assert!(matches!(
            verify_pinned_host_ack(&ack, &revs, 1000, &client_nonce).unwrap_err(),
            AuthError::PolicyReject
        ));

        // A tampered challenge signature.
        let client_nonce = [4u8; 32];
        let mut ack = build_hello_ack(relay.cert.clone(), &relay.sk, [0u8; 32], &client_nonce);
        ack.sig_over_client_nonce[0] ^= 0x01;
        assert!(matches!(
            verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &client_nonce).unwrap_err(),
            AuthError::BadChallengeSig
        ));

        // A signature replayed under a different client nonce.
        let ack = build_hello_ack(relay.cert.clone(), &relay.sk, [0u8; 32], &[9u8; 32]);
        assert!(matches!(
            verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &[8u8; 32]).unwrap_err(),
            AuthError::BadChallengeSig
        ));
    }
}
