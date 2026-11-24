//! Authenticated handshake between a server and a peer.
//!
//! As soon as a connection opens, both sides perform this challenge/response:
//!
//! ```text
//! client → server:   Hello { protocol: u32, cert: DeviceCert, nonce: [u8;32] }
//! server checks the protocol version (another one is answered with the
//! server's own, alone, and the connection ends) and the cert; if the client
//! is admissible under the trust policy, replies:
//! server → client:   HelloAck {
//!     protocol: u32,
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
//! client's connection and forward its cert to the real server.
//!
//! **A refusal that says why.** A server that refuses a hello closes the
//! stream. Where one thing alone refused the device, and the device may know
//! it, the server says so first ([`HelloRefusal`]): a [`HelloRefused`] in the
//! ack's place, signed as an ack is, behind an empty frame
//! (`tilo_server_client::frame::write_refusal`). A device built before this
//! refuses the empty frame and reads what it always read, a hello nobody
//! answered. A device built since believes the refusal exactly where it would
//! have believed that server's ack, and takes one it cannot check, or a
//! reason it does not know, as a refusal with nothing said.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use tilo_user_identity::{
    DeviceCert, DeviceCertError, RevocationEntry, RevocationList, verify_device_cert_at,
};

use crate::policy::TrustPolicy;

/// The device ↔ device interface's one version
/// ([`neighbour_wire::PEER_PROTOCOL_VERSION`]), which the handshake checks.
///
/// [`Hello`] and [`HelloAck`] start with it, so every version reads it
/// whatever follows ([`decode_handshake`]). A server answers a hello at
/// another version with a frame holding only its own ([`version_answer`])
/// and closes; neither side reads anything else from the other.
pub use neighbour_wire::PEER_PROTOCOL_VERSION;

/// Domain tag bound into every mutual-authentication challenge signature.
/// Domain tag under which the *server* signs the client's challenge.
///
/// The two handshake directions sign under different tags. With one shared
/// tag, a server that signs any `client_nonce` it is handed is a signing
/// oracle for the `HelloAck2` another server expects from the holder of the
/// same device key: present the victim's cert, feed the victim's
/// `server_nonce` back as a `client_nonce`, and the victim signs its own
/// challenge. A tag per direction makes a server-side signature unusable as a
/// client-side proof and vice versa.
const TILO_AUTH_SERVER_TAG: &[u8] = b"tilo-server-auth-v2/server\0";
/// Domain tag under which the *client* signs the server's challenge.
const TILO_AUTH_CLIENT_TAG: &[u8] = b"tilo-server-auth-v2/client\0";
/// Domain tag under which the *server* signs why it refused a hello.
///
/// A tag of its own, as each direction has: a server acks whatever
/// `client_nonce` a device it admits hands it, so under the server's
/// challenge tag such a device could have it sign a refusal of another
/// device's hello.
const TILO_AUTH_REFUSAL_TAG: &[u8] = b"tilo-server-auth-v2/refusal\0";

/// First message: client → server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// The client's [`PEER_PROTOCOL_VERSION`]. First, and never moved.
    pub protocol: u32,
    pub cert: DeviceCert,
    /// Fresh 32-byte challenge nonce the client wants the server to
    /// sign.
    pub client_nonce: [u8; 32],
    /// The workspace a device the host has not admitted yet asks to enter
    /// through its entry, and the device's proof ([`crate::entering`]). Only
    /// the host's session endpoint takes it; every other endpoint refuses a
    /// hello that carries one.
    pub entering: Option<crate::entering::Entry>,
}

/// Second message: server → client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    /// The server's [`PEER_PROTOCOL_VERSION`]. First, and never moved.
    pub protocol: u32,
    /// Server's device cert (the server's cert).
    pub cert: DeviceCert,
    /// Fresh 32-byte challenge nonce the server wants the client
    /// to sign.
    pub server_nonce: [u8; 32],
    /// Ed25519 signature by the server's *device* signing key over
    /// [`server_challenge_digest`]`(client_nonce)`. Proves the server
    /// holds the private key matching the cert.
    #[serde(with = "serde_bytes")]
    pub sig_over_client_nonce: [u8; 64],
}

impl Hello {
    /// This device's hello: its certificate and a fresh `client_nonce`, at
    /// this build's [`PEER_PROTOCOL_VERSION`].
    #[must_use]
    pub fn new(cert: DeviceCert, client_nonce: [u8; 32]) -> Self {
        Self {
            protocol: PEER_PROTOCOL_VERSION,
            cert,
            client_nonce,
            entering: None,
        }
    }

    /// The hello of a device the host has not admitted yet, asking to come
    /// in as `entry` says.
    #[must_use]
    pub fn entering(
        cert: DeviceCert,
        client_nonce: [u8; 32],
        entry: crate::entering::Entry,
    ) -> Self {
        Self {
            entering: Some(entry),
            ..Self::new(cert, client_nonce)
        }
    }
}

/// A handshake frame that is not one this device reads.
#[derive(Debug, Error)]
pub enum HandshakeFrameError {
    /// The other side speaks a [`PEER_PROTOCOL_VERSION`] this one does not
    /// work with ([`neighbour_wire::works_with_peer_version`]): `theirs`.
    #[error("the other device speaks peer protocol {theirs}, this one {PEER_PROTOCOL_VERSION}")]
    Version {
        /// The version the frame starts with.
        theirs: u32,
    },
    /// The frame does not decode at this device's version.
    #[error("handshake frame does not decode: {0}")]
    Codec(#[from] postcard::Error),
}

impl HandshakeFrameError {
    /// The other side's version, when that is why the frame was refused.
    #[must_use]
    pub const fn peer_version(&self) -> Option<u32> {
        match self {
            Self::Version { theirs } => Some(*theirs),
            Self::Codec(_) => None,
        }
    }
}

/// Which side of a version mismatch is behind.
#[must_use]
pub const fn this_device_is_behind(theirs: u32) -> bool {
    theirs > PEER_PROTOCOL_VERSION
}

/// Decode a [`Hello`] or [`HelloAck`] frame: its leading version first, so
/// a frame from a version this one does not work with is refused as such,
/// whatever its shape.
///
/// # Errors
///
/// [`HandshakeFrameError::Version`] when the frame starts with a
/// [`PEER_PROTOCOL_VERSION`] outside this one's window
/// ([`neighbour_wire::works_with_peer_version`]),
/// [`HandshakeFrameError::Codec`] when it does not decode.
pub fn decode_handshake<T: serde::de::DeserializeOwned>(
    frame: &[u8],
) -> Result<T, HandshakeFrameError> {
    let (theirs, _) = postcard::take_from_bytes::<u32>(frame)?;
    if !neighbour_wire::works_with_peer_version(theirs) {
        return Err(HandshakeFrameError::Version { theirs });
    }
    Ok(postcard::from_bytes(frame)?)
}

/// What a server answers a hello at another version with: its own
/// version, alone, before it closes.
pub fn version_answer() -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(&PEER_PROTOCOL_VERSION)
}

/// Third message: client → server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloAck2 {
    /// Ed25519 signature by the client's device signing key over
    /// [`client_challenge_digest`]`(server_nonce)`.
    #[serde(with = "serde_bytes")]
    pub sig_over_server_nonce: [u8; 64],
}

/// Why a server refused a device's hello, where it says so
/// ([`HelloRefusal::said_of`]).
///
/// New reasons are added at the end. A device that does not know a reason
/// cannot read the [`HelloRefused`] that carries it, and takes the refusal as
/// one with nothing said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelloRefusal {
    /// The device's person removed it from their devices.
    DeviceRevoked,
    /// The certificate the hello showed did not verify, or had expired: a
    /// device whose kept certificate lapsed, or one a device of its person
    /// signed with a certificate this server cannot take.
    CertificateRejected,
    /// The device's certificate is valid, and the server admits nobody of its
    /// person: no workspace it serves has them as a member any more. A device
    /// that kept a copy of one of them lets it go.
    NotAdmitted,
}

impl HelloRefusal {
    /// The reason's name, which never changes: the server's signature is over
    /// it, and a session that stops for this reason fails with it.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::DeviceRevoked => "device-revoked",
            Self::CertificateRejected => "certificate-rejected",
            Self::NotAdmitted => "not-admitted",
        }
    }

    /// What a server says of `error`, its refusal of the hello that showed
    /// `cert` at an endpoint that asks `trust`: `None` where it says nothing.
    ///
    /// A server says no more than its refusal gives away. A removal is said
    /// when it alone refused the device: the certificate is valid, or the
    /// removal would not have been looked at, and `trust` admits its person.
    /// Whoever shows that certificate and is refused can work out as much,
    /// because nothing else refuses it. A removed device of a person the
    /// endpoint does not admit hears nothing, as that person's other devices
    /// hear nothing. Who removed the device and which other devices its
    /// person has are never said; when and why only to a device its person
    /// retired ([`HelloRefused::revocation`]).
    ///
    /// A person an endpoint of the workspaces' members ([`HandshakeTrust::Live`])
    /// does not admit hears only that: whoever is turned
    /// away works it out from the closed link anyway, and a copy they kept of
    /// a workspace that removed them learns to let it go. Their removed
    /// devices hear the same, so the answer tells no removed device from the
    /// others.
    ///
    /// A certificate the server cannot verify is the device's own: it may
    /// certify again, and it learns nothing about the people this server
    /// serves.
    #[must_use]
    pub fn said_of(
        error: &AuthError,
        cert: &DeviceCert,
        own_user_public_key: &[u8; 32],
        trust: HandshakeTrust<'_>,
    ) -> Option<Self> {
        match error {
            AuthError::Revoked if trust.admits(cert, own_user_public_key) => {
                Some(Self::DeviceRevoked)
            }
            AuthError::Revoked | AuthError::PolicyReject
                if matches!(trust, HandshakeTrust::Live(_)) =>
            {
                Some(Self::NotAdmitted)
            }
            AuthError::Cert(_) => Some(Self::CertificateRejected),
            _ => None,
        }
    }
}

impl std::fmt::Display for HelloRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceRevoked => f.write_str("its person removed this device"),
            Self::CertificateRejected => f.write_str("the certificate it showed was refused"),
            Self::NotAdmitted => f.write_str("no workspace it serves has this person"),
        }
    }
}

/// What a server answers a hello it refuses and says why: it stands in the
/// place of a [`HelloAck`] and is signed as one is, so a device checks it as
/// it checks an ack ([`ServerProof`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloRefused {
    /// The server's [`PEER_PROTOCOL_VERSION`]. First, and never moved.
    pub protocol: u32,
    /// Server's device cert.
    pub cert: DeviceCert,
    /// Why the server refused the hello.
    pub refusal: HelloRefusal,
    /// Ed25519 signature by the server's *device* signing key over
    /// [`refusal_digest`]`(client_nonce, refusal)`: this server refused this
    /// hello, for this reason.
    #[serde(with = "serde_bytes")]
    pub sig_over_refusal: [u8; 64],
    /// The revocation of the refused device's key its root signed, when the
    /// device was retired: a server that was away while its person retired
    /// it learns so from the first device that refuses it, and lets go of
    /// what it holds. The root's signature is its own proof. A device lost,
    /// stolen or compromised is told only that it was removed.
    pub revocation: Option<RevocationEntry>,
}

/// A server's signed answer to a hello: its [`HelloAck`], or its
/// [`HelloRefused`]. Every check a device makes of its server's ack
/// ([`verify_hello_ack`] and the ones beside it) takes either, so a device
/// believes a refusal exactly where it would have believed that server's ack.
pub trait ServerProof {
    /// The certificate the server answers under.
    fn cert(&self) -> &DeviceCert;
    /// The server's signature over [`Self::digest`].
    fn signature(&self) -> &[u8; 64];
    /// What the server signed, in answer to the hello that carried
    /// `client_nonce`.
    fn digest(&self, client_nonce: &[u8; 32]) -> [u8; 32];
}

impl ServerProof for HelloAck {
    fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    fn signature(&self) -> &[u8; 64] {
        &self.sig_over_client_nonce
    }

    fn digest(&self, client_nonce: &[u8; 32]) -> [u8; 32] {
        server_challenge_digest(client_nonce)
    }
}

impl ServerProof for HelloRefused {
    fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    fn signature(&self) -> &[u8; 64] {
        &self.sig_over_refusal
    }

    fn digest(&self, client_nonce: &[u8; 32]) -> [u8; 32] {
        refusal_digest(client_nonce, self.refusal)
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
    /// An endpoint-bound certificate names a device other than the
    /// transport endpoint that presented it.
    #[error("certificate does not match the authenticated endpoint")]
    EndpointMismatch,
}

/// Who a server endpoint admits at the handshake, by name.
///
/// Every server endpoint states its requirement here rather than passing a
/// [`TrustPolicy`] literal, so a reviewer reads the admission rule at the
/// call site and a new endpoint has to choose one.
#[derive(Clone, Copy, Debug)]
pub enum HandshakeTrust<'a> {
    /// The server's live trust policy: its own user and every user it
    /// currently admits (the main server ALPN, content-endpoint discovery,
    /// compute and browser sessions).
    Live(&'a TrustPolicy),
    /// [`Self::Live`], and the devices the server's workspaces name among their
    /// holders: a holder is no member and no trusted person, and only the one
    /// device a naming names is admitted, to keep and carry the workspace.
    LiveHolders(&'a TrustPolicy, &'a std::collections::HashSet<[u8; 32]>),
    /// Only devices of the server's own user (home-peer management).
    OwnerDevicesOnly,
    /// Only devices of the server's own user that may admit devices
    /// (device-certification approvers).
    OwnerDeviceAdmitters,
    /// Any device whose certificate is valid and unrevoked, of any user.
    /// Authentication only: the endpoint must authorize every request
    /// itself (workspace administration, whose owner-only verbs check the
    /// server's user and whose members' receipts check workspace membership).
    AnyCertifiedDevice,
}

impl HandshakeTrust<'_> {
    fn admits(self, cert: &DeviceCert, own_user_public_key: &[u8; 32]) -> bool {
        let owner = || TrustPolicy::AllowMatchingUser.admits(cert, own_user_public_key);
        match self {
            Self::Live(policy) => policy.admits(cert, own_user_public_key),
            Self::LiveHolders(policy, holders) => {
                policy.admits(cert, own_user_public_key)
                    || holders.contains(&cert.device_public_key)
            }
            Self::OwnerDevicesOnly => owner(),
            Self::OwnerDeviceAdmitters => owner() && cert.capabilities.can_admit_devices,
            Self::AnyCertifiedDevice => true,
        }
    }
}

/// The server's own identity, as the server side of a handshake needs it.
#[derive(Clone, Copy)]
pub struct ServerIdentity<'a> {
    /// The server's user root public key.
    pub own_user: &'a [u8; 32],
    /// The server's device certificate, sent in [`HelloAck`].
    pub cert: &'a DeviceCert,
    /// The server's device signing key, which signs the client's nonce.
    pub key: &'a ed25519_dalek::SigningKey,
}

/// A server handshake that sent its [`HelloAck`] and waits for the client's
/// [`HelloAck2`].
#[derive(Clone, Debug)]
pub struct PendingServer {
    peer: AuthenticatedPeer,
    server_nonce: [u8; 32],
}

impl PendingServer {
    /// Second half: check the client's signature over the server nonce.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::BadChallengeSig`] if the signature does not
    /// verify under the hello certificate's device key.
    pub fn finish(self, ack2: &HelloAck2) -> Result<AuthenticatedPeer, AuthError> {
        verify_hello_ack2(ack2, &self.peer.device_public_key, &self.server_nonce)?;
        Ok(self.peer)
    }
}

/// First half of the server handshake, without I/O: verify `hello` under
/// `trust` and `revocations`, and return the [`HelloAck`] to send with the
/// state [`PendingServer::finish`] completes.
///
/// `server_nonce` must be fresh randomness for every handshake.
///
/// # Errors
///
/// Returns [`AuthError::Cert`] if the client's cert doesn't verify or is
/// expired, [`AuthError::Revoked`] if its device is revoked, or
/// [`AuthError::PolicyReject`] if `trust` does not admit it.
pub fn server_begin(
    identity: &ServerIdentity<'_>,
    trust: HandshakeTrust<'_>,
    revocations: &RevocationList,
    hello: &Hello,
    now_ns: u64,
    server_nonce: [u8; 32],
) -> Result<(PendingServer, HelloAck), AuthError> {
    let peer = admit_cert(&hello.cert, identity.own_user, trust, revocations, now_ns)?;
    let ack = build_hello_ack(
        identity.cert.clone(),
        identity.key,
        server_nonce,
        &hello.client_nonce,
    );
    Ok((PendingServer { peer, server_nonce }, ack))
}

/// The server side of an entry ([`crate::entering`]): check the entering
/// device's certificate as any device's (valid and unrevoked, of any person)
/// and its proof under `binding`, the keying material this end exported from
/// the connection carrying the hello, and return the peer with the
/// [`HelloAck`] that proves this server to it. No `HelloAck2` follows: the
/// entry's proof is the device's proof of its key on this connection.
///
/// `server_nonce` must be fresh randomness for every handshake.
///
/// # Errors
///
/// [`AuthError::BadChallengeSig`] for a hello that carries no entry or whose
/// proof does not verify; then as [`server_begin`] for the certificate, so a
/// certificate is refused only to a device that holds its key.
pub fn server_enter(
    identity: &ServerIdentity<'_>,
    revocations: &RevocationList,
    hello: &Hello,
    binding: &[u8; 32],
    now_ns: u64,
    server_nonce: [u8; 32],
) -> Result<(AuthenticatedPeer, HelloAck), AuthError> {
    let entry = hello.entering.as_ref().ok_or(AuthError::BadChallengeSig)?;
    // The proof first: what is then said of the certificate
    // ([`HelloRefusal::said_of`]) is said to a device that holds its key.
    let device = &hello.cert.device_public_key;
    let digest = crate::entering::entry_digest(binding, device, &entry.entering);
    ed25519_dalek::VerifyingKey::from_bytes(device)
        .map_err(|_| AuthError::BadChallengeSig)?
        .verify(&digest, &ed25519_dalek::Signature::from_bytes(&entry.proof))
        .map_err(|_| AuthError::BadChallengeSig)?;
    let peer = admit_cert(
        &hello.cert,
        identity.own_user,
        HandshakeTrust::AnyCertifiedDevice,
        revocations,
        now_ns,
    )?;
    let ack = build_hello_ack(
        identity.cert.clone(),
        identity.key,
        server_nonce,
        &hello.client_nonce,
    );
    Ok((peer, ack))
}

/// Authenticate a certificate offered over a transport that already proved
/// possession of `remote`, the device key of its endpoint (an Iroh
/// connection's remote id). Binds the certificate to that key and applies
/// the handshake's validity, revocation and trust checks.
///
/// # Errors
///
/// Returns [`AuthError::EndpointMismatch`] if the certificate names another
/// device, otherwise as [`server_begin`].
pub fn authenticate_endpoint_bound(
    cert: &DeviceCert,
    remote: &[u8; 32],
    own_user_public_key: &[u8; 32],
    trust: HandshakeTrust<'_>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<AuthenticatedPeer, AuthError> {
    if &cert.device_public_key != remote {
        return Err(AuthError::EndpointMismatch);
    }
    admit_cert(cert, own_user_public_key, trust, revocations, now_ns)
}

fn admit_cert(
    cert: &DeviceCert,
    own_user_public_key: &[u8; 32],
    trust: HandshakeTrust<'_>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<AuthenticatedPeer, AuthError> {
    verify_device_cert_at(cert, now_ns)?;
    if revocations.refuses_at(cert, now_ns) {
        return Err(AuthError::Revoked);
    }
    if !trust.admits(cert, own_user_public_key) {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        cert: cert.clone(),
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
    })
}

/// The digest the *server* signs in `HelloAck`: the client's challenge under
/// the server-direction tag.
pub fn server_challenge_digest(client_nonce: &[u8; 32]) -> [u8; 32] {
    challenge_digest_under(TILO_AUTH_SERVER_TAG, client_nonce)
}

/// The digest the *client* signs in `HelloAck2`: the server's challenge under
/// the client-direction tag.
pub fn client_challenge_digest(server_nonce: &[u8; 32]) -> [u8; 32] {
    challenge_digest_under(TILO_AUTH_CLIENT_TAG, server_nonce)
}

fn challenge_digest_under(tag: &[u8], nonce: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(nonce);
    *hasher.finalize().as_bytes()
}

/// The digest the *server* signs in [`HelloRefused`]: the client's challenge
/// and the reason's name, under the refusal tag.
pub fn refusal_digest(client_nonce: &[u8; 32], refusal: HelloRefusal) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TILO_AUTH_REFUSAL_TAG);
    hasher.update(client_nonce);
    hasher.update(refusal.code().as_bytes());
    *hasher.finalize().as_bytes()
}

/// Server-side: verify the client's `Hello` against `own_user_pk` +
/// `policy` + `revocations`. Returns the authenticated peer state
/// on success.
///
/// This is the hello check alone. A server endpoint runs the whole
/// handshake through [`server_begin`] and [`PendingServer::finish`].
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
    admit_cert(
        &hello.cert,
        own_user_public_key,
        HandshakeTrust::Live(policy),
        revocations,
        now_ns,
    )
}

/// Server-side: build the `HelloAck` reply — signs the client's
/// nonce with the server's device signing key.
pub fn build_hello_ack(
    own_cert: DeviceCert,
    own_signing_key: &ed25519_dalek::SigningKey,
    server_nonce: [u8; 32],
    client_nonce: &[u8; 32],
) -> HelloAck {
    let digest = server_challenge_digest(client_nonce);
    let sig = own_signing_key.sign(&digest).to_bytes();
    HelloAck {
        protocol: PEER_PROTOCOL_VERSION,
        cert: own_cert,
        server_nonce,
        sig_over_client_nonce: sig,
    }
}

/// Server-side: build the [`HelloRefused`] that answers the hello carrying
/// `client_nonce` with `refusal`, signed with the server's device signing key.
pub fn build_hello_refused(
    own_cert: DeviceCert,
    own_signing_key: &ed25519_dalek::SigningKey,
    client_nonce: &[u8; 32],
    refusal: HelloRefusal,
    revocation: Option<RevocationEntry>,
) -> HelloRefused {
    let sig = own_signing_key
        .sign(&refusal_digest(client_nonce, refusal))
        .to_bytes();
    HelloRefused {
        protocol: PEER_PROTOCOL_VERSION,
        cert: own_cert,
        refusal,
        sig_over_refusal: sig,
        revocation,
    }
}

/// Server-side: what answers `hello`, refused with `error` at an endpoint that
/// asks `trust`, where the server says why ([`HelloRefusal::said_of`]), with
/// the root's entry from `revocations` that retired the device, if one did.
#[must_use]
pub fn server_refusal(
    identity: &ServerIdentity<'_>,
    trust: HandshakeTrust<'_>,
    revocations: &RevocationList,
    hello: &Hello,
    error: &AuthError,
) -> Option<HelloRefused> {
    let refusal = HelloRefusal::said_of(error, &hello.cert, identity.own_user, trust)?;
    let retired = (refusal == HelloRefusal::DeviceRevoked)
        .then(|| revocations.retired(&hello.cert.user_public_key, &hello.cert.device_public_key))
        .flatten()
        .cloned();
    Some(build_hello_refused(
        identity.cert.clone(),
        identity.key,
        &hello.client_nonce,
        refusal,
        retired,
    ))
}

/// Client-side: verify the server's `HelloAck` — checks the cert
/// chain, expiry, policy admission (from *client's* point of view:
/// the server must chain to our own user identity), revocation,
/// and the nonce signature.
///
/// # Errors
///
/// Returns [`AuthError::Cert`] if the server's cert doesn't verify or
/// is expired, [`AuthError::PolicyReject`] if it is no person's device or
/// doesn't chain to `own_user_public_key`, [`AuthError::Revoked`] if
/// its device is revoked, or [`AuthError::BadChallengeSig`] if the
/// nonce signature doesn't verify.
pub fn verify_hello_ack(
    ack: &impl ServerProof,
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
/// is expired, [`AuthError::PolicyReject`] if it is no person's device,
/// [`AuthError::Revoked`] if its device is revoked, or
/// [`AuthError::BadChallengeSig`] if the nonce signature doesn't
/// verify.
pub fn verify_pinned_host_ack(
    ack: &impl ServerProof,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_inner(ack, None, revocations, now_ns, challenge_client_nonce)
}

fn verify_hello_ack_inner(
    ack: &impl ServerProof,
    expected_user_public_key: Option<&[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_cryptographic(ack, revocations, now_ns, challenge_client_nonce)?;
    if !ack.cert().capabilities.can_own_workspaces {
        return Err(AuthError::PolicyReject);
    }
    // A same-owner client pins the expected user root. A hosted-service
    // client instead pins the Iroh EndpointAddr and accepts any valid server
    // root, while still requiring proof of the certified device key.
    if let Some(expected) = expected_user_public_key
        && &ack.cert().user_public_key != expected
    {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        cert: ack.cert().clone(),
        user_public_key: ack.cert().user_public_key,
        device_public_key: ack.cert().device_public_key,
    })
}

/// Verify the exact primary device authorized by a signed, workspace-scoped
/// interactive-owner hint. This exception does not grant host capability and
/// cannot be reached from the generic pinned-host API.
///
/// # Errors
///
/// Returns [`AuthError`] unless the ACK certificate is the exact expected
/// governor, is current and non-revoked, can govern workspaces, and proves the
/// challenge with its certified device key.
pub fn verify_interactive_owner_ack(
    ack: &impl ServerProof,
    expected_governor: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_cryptographic(ack, revocations, now_ns, challenge_client_nonce)?;
    if ack.cert() != expected_governor || !ack.cert().capabilities.can_own_workspaces {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        cert: ack.cert().clone(),
        user_public_key: ack.cert().user_public_key,
        device_public_key: ack.cert().device_public_key,
    })
}

/// Verify the exact device of the client's own person that hosts workspaces
/// itself: the ACK certificate chains to `own_user_public_key`, is the
/// device `expected_device`, is current and unrevoked, and proves the
/// challenge with its certified key. No host capability is asked of it: a
/// person's device serves the workspaces placed on it, and the session it opens
/// is the person's own.
///
/// # Errors
///
/// Returns [`AuthError`] unless every condition above holds.
pub fn verify_own_device_ack(
    ack: &impl ServerProof,
    own_user_public_key: &[u8; 32],
    expected_device: &[u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<AuthenticatedPeer, AuthError> {
    verify_hello_ack_cryptographic(ack, revocations, now_ns, challenge_client_nonce)?;
    if &ack.cert().user_public_key != own_user_public_key
        || &ack.cert().device_public_key != expected_device
    {
        return Err(AuthError::PolicyReject);
    }
    Ok(AuthenticatedPeer {
        cert: ack.cert().clone(),
        user_public_key: ack.cert().user_public_key,
        device_public_key: ack.cert().device_public_key,
    })
}

fn verify_hello_ack_cryptographic(
    ack: &impl ServerProof,
    revocations: &RevocationList,
    now_ns: u64,
    challenge_client_nonce: &[u8; 32],
) -> Result<(), AuthError> {
    verify_device_cert_at(ack.cert(), now_ns)?;
    if revocations.refuses_at(ack.cert(), now_ns) {
        return Err(AuthError::Revoked);
    }
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&ack.cert().device_public_key)
        .map_err(|_| AuthError::BadChallengeSig)?;
    let digest = ack.digest(challenge_client_nonce);
    let sig = ed25519_dalek::Signature::from_bytes(ack.signature());
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
    let digest = client_challenge_digest(server_nonce);
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
    let digest = client_challenge_digest(server_nonce);
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
            issuer: None,
        };
        sign_device_cert(&mut cert, root_sk).unwrap();
        Party { cert, sk: dev_sk }
    }

    tilo_test_env::scenario! {
        fn an_ack_proves_only_the_device_and_direction_it_was_signed_for;

        phase an_own_device_proves_itself_only_as_the_pinned_device;
        {
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let laptop = enrol(
                &root_sk,
                user.public_key,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let phone = enrol(
                &root_sk,
                user.public_key,
                "phone",
                DeviceCapabilities::primary(),
            );
            let (other_root, other) = UserIdentity::generate("Bob", 1);
            let stranger = enrol(
                &other_root,
                other.public_key,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let revs = RevocationList::new();
            let nonce = [5u8; 32];
            let ack = build_hello_ack(laptop.cert.clone(), &laptop.sk, [6u8; 32], &nonce);
            let peer = verify_own_device_ack(
                &ack,
                &user.public_key,
                &laptop.cert.device_public_key,
                &revs,
                1000,
                &nonce,
            )
            .unwrap();
            assert_eq!(peer.device_public_key, laptop.cert.device_public_key);
            // Another of the person's devices is not the one pinned.
            assert!(matches!(
                verify_own_device_ack(
                    &ack,
                    &user.public_key,
                    &phone.cert.device_public_key,
                    &revs,
                    1000,
                    &nonce,
                ),
                Err(AuthError::PolicyReject)
            ));
            // Another person's device is never the person's own.
            let foreign = build_hello_ack(stranger.cert.clone(), &stranger.sk, [6u8; 32], &nonce);
            assert!(matches!(
                verify_own_device_ack(
                    &foreign,
                    &user.public_key,
                    &stranger.cert.device_public_key,
                    &revs,
                    1000,
                    &nonce,
                ),
                Err(AuthError::PolicyReject)
            ));
            // A proof over another challenge is no proof.
            assert!(matches!(
                verify_own_device_ack(
                    &ack,
                    &user.public_key,
                    &laptop.cert.device_public_key,
                    &revs,
                    1000,
                    &[7u8; 32],
                ),
                Err(AuthError::BadChallengeSig)
            ));
        }

        phase a_signature_from_one_direction_is_never_a_proof_in_the_other;
        {
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let server = enrol(
                &root_sk,
                user.public_key,
                "server",
                DeviceCapabilities::member(),
            );
            let client = enrol(
                &root_sk,
                user.public_key,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let revs = RevocationList::new();
            // A server that signs whatever `client_nonce` it is
            // handed must not thereby produce the `HelloAck2` another server
            // expects from the same key. Feed a victim's server nonce in as a
            // client nonce; the resulting server signature is not a client proof.
            let victim_server_nonce = [9u8; 32];
            let oracle = build_hello_ack(
                server.cert.clone(),
                &server.sk,
                [3u8; 32],
                &victim_server_nonce,
            );
            let reflected = HelloAck2 {
                sig_over_server_nonce: oracle.sig_over_client_nonce,
            };
            assert!(
                matches!(
                    verify_hello_ack2(
                        &reflected,
                        &server.cert.device_public_key,
                        &victim_server_nonce
                    ),
                    Err(AuthError::BadChallengeSig)
                ),
                "a server-direction signature must never verify as a client proof"
            );
            // And the other way round: a client's proof is not a server's.
            let client_proof = build_hello_ack2(&client.sk, &victim_server_nonce);
            let reflected_ack = HelloAck {
                protocol: PEER_PROTOCOL_VERSION,
                cert: client.cert,
                server_nonce: [4u8; 32],
                sig_over_client_nonce: client_proof.sig_over_server_nonce,
            };
            assert!(
                matches!(
                    verify_hello_ack(
                        &reflected_ack,
                        &user.public_key,
                        &revs,
                        1000,
                        &victim_server_nonce
                    ),
                    Err(AuthError::BadChallengeSig)
                ),
                "a client-direction signature must never verify as a server proof"
            );
        }

        phase client_rejects_acks_from_strangers_non_hosts_and_bad_signatures;
        {
            let (root_sk, alice) = UserIdentity::generate("Alice", 1);
            let server = enrol(
                &root_sk,
                alice.public_key,
                "server",
                DeviceCapabilities::member(),
            );
            let revs = RevocationList::new();

            // Bob's device answers Alice's client: the signature is valid but the
            // wrong-user policy rejects it.
            let (bob_root, bob) = UserIdentity::generate("Bob", 1);
            let bob_dev = enrol(
                &bob_root,
                bob.public_key,
                "attacker",
                DeviceCapabilities::member(),
            );
            let client_nonce = [7u8; 32];
            let ack = build_hello_ack(bob_dev.cert.clone(), &bob_dev.sk, [0u8; 32], &client_nonce);
            assert!(matches!(
                verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &client_nonce).unwrap_err(),
                AuthError::PolicyReject
            ));

            // A pinned device must be a person's device, not an agent's grant.
            let (provider_root, provider) = UserIdentity::generate("Provider", 1);
            let ordinary_device = enrol(
                &provider_root,
                provider.public_key,
                "agent grant",
                DeviceCapabilities::nothing(),
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

            let client_nonce = [4u8; 32];
            let mut ack = build_hello_ack(server.cert.clone(), &server.sk, [0u8; 32], &client_nonce);
            ack.sig_over_client_nonce[0] ^= 0x01;
            assert!(matches!(
                verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &client_nonce).unwrap_err(),
                AuthError::BadChallengeSig
            ));

            let ack = build_hello_ack(server.cert.clone(), &server.sk, [0u8; 32], &[9u8; 32]);
            assert!(matches!(
                verify_hello_ack(&ack, &alice.public_key, &revs, 1000, &[8u8; 32]).unwrap_err(),
                AuthError::BadChallengeSig
            ));
        }
    }

    tilo_test_env::scenario! {
        fn the_server_admits_only_the_devices_its_policy_trusts;

        phase server_accepts_only_trusted_unrevoked_devices_and_completes_mutual_auth;
        {
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let server = enrol(
                &root_sk,
                user.public_key,
                "server",
                DeviceCapabilities::member(),
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
            let hello = Hello::new(client.cert.clone(), client_nonce);
            let server_peer = accept_hello(
                &hello,
                &user.public_key,
                &TrustPolicy::default(),
                &revs,
                1000,
            )
            .unwrap();
            assert_eq!(server_peer.device_public_key, client.cert.device_public_key);
            let ack = build_hello_ack(server.cert.clone(), &server.sk, server_nonce, &client_nonce);
            let client_peer =
                verify_hello_ack(&ack, &user.public_key, &revs, 1000, &client_nonce).unwrap();
            assert_eq!(client_peer.device_public_key, server.cert.device_public_key);
            let ack2 = build_hello_ack2(&client.sk, &server_nonce);
            verify_hello_ack2(&ack2, &client.cert.device_public_key, &server_nonce).unwrap();

            // Wrong user: the server is Alice's; the client presents Bob's cert.
            let (bob_root, bob) = UserIdentity::generate("Bob", 1);
            let bob_client = enrol(
                &bob_root,
                bob.public_key,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let hello = Hello::new(bob_client.cert, [0u8; 32]);
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
            let hello = Hello::new(client.cert, [0u8; 32]);
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

        phase server_begin_finish_round_trip;
        {
            let (server, laptop, _, alice, _) = alice_relay_and_bob();
            let revocations = RevocationList::new();
            let policy = TrustPolicy::default();
            let (pending, ack) = server_begin(
                &identity(&server, &alice),
                HandshakeTrust::Live(&policy),
                &revocations,
                &hello(&laptop),
                1000,
                [5; 32],
            )
            .unwrap();
            verify_hello_ack(&ack, &alice, &revocations, 1000, &[3; 32]).unwrap();
            assert_eq!(ack.server_nonce, [5; 32]);
            let peer = pending
                .finish(&build_hello_ack2(&laptop.sk, &ack.server_nonce))
                .unwrap();
            let expected = accept_hello(&hello(&laptop), &alice, &policy, &revocations, 1000).unwrap();
            assert_eq!(peer.cert, expected.cert);
            assert_eq!(peer.user_public_key, expected.user_public_key);
            assert_eq!(peer.device_public_key, expected.device_public_key);
        }

        phase owner_devices_only_rejects_admitted_member;
        {
            let (server, laptop, member, alice, _) = alice_relay_and_bob();
            let revocations = RevocationList::new();
            let live = TrustPolicy::AllowList {
                users: vec![
                    hex::encode(alice),
                    hex::encode(member.cert.user_public_key),
                ],
            };
            let begin = |trust, party: &Party| {
                server_begin(
                    &identity(&server, &alice),
                    trust,
                    &revocations,
                    &hello(party),
                    1000,
                    [5; 32],
                )
            };
            // The live policy admits the member; the owner-only rules do not.
            begin(HandshakeTrust::Live(&live), &member).unwrap();
            for trust in [
                HandshakeTrust::OwnerDevicesOnly,
                HandshakeTrust::OwnerDeviceAdmitters,
            ] {
                assert!(matches!(
                    begin(trust, &member).unwrap_err(),
                    AuthError::PolicyReject
                ));
                begin(trust, &laptop).unwrap();
            }
            begin(HandshakeTrust::AnyCertifiedDevice, &member).unwrap();
        }

        phase owner_device_admitters_requires_can_admit_devices;
        {
            let (server, _, _, alice, alice_root) = alice_relay_and_bob();
            let member = enrol(&alice_root, alice, "member", DeviceCapabilities::member());
            assert!(!member.cert.capabilities.can_admit_devices);
            let revocations = RevocationList::new();
            let begin = |trust| {
                server_begin(
                    &identity(&server, &alice),
                    trust,
                    &revocations,
                    &hello(&member),
                    1000,
                    [5; 32],
                )
            };
            begin(HandshakeTrust::OwnerDevicesOnly).unwrap();
            assert!(matches!(
                begin(HandshakeTrust::OwnerDeviceAdmitters).unwrap_err(),
                AuthError::PolicyReject
            ));
        }

        phase revoked_device_rejected_in_begin;
        {
            let (server, laptop, _, alice, alice_root) = alice_relay_and_bob();
            let mut entry = RevocationEntry {
                user_public_key: alice,
                revoked_device_public_key: laptop.cert.device_public_key,
                revoked_at_ns: 500,
                reason: RevocationReason::Compromised,
                signature: [0u8; 64],
            };
            sign_revocation_entry(&mut entry, &alice_root).unwrap();
            let mut revocations = RevocationList::new();
            revocations.insert(entry).unwrap();
            for trust in [
                HandshakeTrust::OwnerDevicesOnly,
                HandshakeTrust::AnyCertifiedDevice,
            ] {
                assert!(matches!(
                    server_begin(
                        &identity(&server, &alice),
                        trust,
                        &revocations,
                        &hello(&laptop),
                        1000,
                        [5; 32],
                    )
                    .unwrap_err(),
                    AuthError::Revoked
                ));
                assert!(matches!(
                    authenticate_endpoint_bound(
                        &laptop.cert,
                        &laptop.cert.device_public_key,
                        &alice,
                        trust,
                        &revocations,
                        1000,
                    )
                    .unwrap_err(),
                    AuthError::Revoked
                ));
            }
        }

        phase wrong_ack2_signature_rejected_in_finish;
        {
            let (server, laptop, member, alice, _) = alice_relay_and_bob();
            let start = || {
                server_begin(
                    &identity(&server, &alice),
                    HandshakeTrust::OwnerDevicesOnly,
                    &RevocationList::new(),
                    &hello(&laptop),
                    1000,
                    [5; 32],
                )
                .unwrap()
                .0
            };
            for ack2 in [
                build_hello_ack2(&member.sk, &[5; 32]),
                build_hello_ack2(&laptop.sk, &[6; 32]),
                {
                    let mut ack2 = build_hello_ack2(&laptop.sk, &[5; 32]);
                    ack2.sig_over_server_nonce[0] ^= 1;
                    ack2
                },
            ] {
                assert!(matches!(
                    start().finish(&ack2).unwrap_err(),
                    AuthError::BadChallengeSig
                ));
            }
            start()
                .finish(&build_hello_ack2(&laptop.sk, &[5; 32]))
                .unwrap();
        }

        phase endpoint_bound_rejects_mismatched_remote;
        {
            let (_, laptop, member, alice, _) = alice_relay_and_bob();
            let revocations = RevocationList::new();
            let bind = |remote: &[u8; 32], trust| {
                authenticate_endpoint_bound(&laptop.cert, remote, &alice, trust, &revocations, 1000)
            };
            assert!(matches!(
                bind(
                    &member.cert.device_public_key,
                    HandshakeTrust::AnyCertifiedDevice
                )
                .unwrap_err(),
                AuthError::EndpointMismatch
            ));
            let peer = bind(
                &laptop.cert.device_public_key,
                HandshakeTrust::AnyCertifiedDevice,
            )
            .unwrap();
            assert_eq!(peer.device_public_key, laptop.cert.device_public_key);
            assert!(matches!(
                authenticate_endpoint_bound(
                    &member.cert,
                    &member.cert.device_public_key,
                    &alice,
                    HandshakeTrust::OwnerDevicesOnly,
                    &revocations,
                    1000,
                )
                .unwrap_err(),
                AuthError::PolicyReject
            ));
            let mut forged = laptop.cert.clone();
            forged.capabilities.foreign = !forged.capabilities.foreign;
            assert!(matches!(
                authenticate_endpoint_bound(
                    &forged,
                    &forged.device_public_key,
                    &alice,
                    HandshakeTrust::AnyCertifiedDevice,
                    &revocations,
                    1000,
                )
                .unwrap_err(),
                AuthError::Cert(_)
            ));
        }
    }

    fn alice_relay_and_bob() -> (Party, Party, Party, [u8; 32], SigningKey) {
        alice_and_bob()
    }

    /// Alice's server and her laptop, Bob's laptop, Alice's root key and the secret of it.
    fn alice_and_bob() -> (Party, Party, Party, [u8; 32], SigningKey) {
        let (alice_root, alice) = UserIdentity::generate("Alice", 1);
        let server = enrol(
            &alice_root,
            alice.public_key,
            "server",
            DeviceCapabilities::member(),
        );
        let laptop = enrol(
            &alice_root,
            alice.public_key,
            "laptop",
            DeviceCapabilities::primary(),
        );
        let (bob_root, bob) = UserIdentity::generate("Bob", 1);
        let member = enrol(
            &bob_root,
            bob.public_key,
            "bob-laptop",
            DeviceCapabilities::primary(),
        );
        (server, laptop, member, alice.public_key, alice_root)
    }

    fn identity<'a>(server: &'a Party, own_user: &'a [u8; 32]) -> ServerIdentity<'a> {
        ServerIdentity {
            own_user,
            cert: &server.cert,
            key: &server.sk,
        }
    }

    fn hello(party: &Party) -> Hello {
        Hello::new(party.cert.clone(), [3; 32])
    }

    fn server_accepts(frame: &[u8], user: &[u8; 32]) -> Option<AuthenticatedPeer> {
        let hello: Hello = decode_handshake(frame).ok()?;
        let policy = TrustPolicy::default();
        accept_hello(&hello, user, &policy, &RevocationList::new(), 1000).ok()
    }

    #[test]
    fn devices_on_another_peer_version_are_told_which_and_read_nothing_else() {
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let foreign = enrol(
            &root_sk,
            user.public_key,
            "mac",
            DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::primary()
            },
        );
        let frame = |protocol: u32| {
            let mut hello = Hello::new(foreign.cert.clone(), [1; 32]);
            hello.protocol = protocol;
            postcard::to_stdvec(&hello).unwrap()
        };

        // The same version: the server admits it, with the marker the workspace's
        // role cap reads.
        let peer = server_accepts(&frame(PEER_PROTOCOL_VERSION), &user.public_key).unwrap();
        assert!(peer.cert.capabilities.foreign);

        // The next version works with this one while a rollout goes on, and
        // so does the one before while this build still speaks it.
        assert!(server_accepts(&frame(PEER_PROTOCOL_VERSION + 1), &user.public_key).is_some());
        assert_eq!(
            server_accepts(&frame(PEER_PROTOCOL_VERSION - 1), &user.public_key).is_some(),
            neighbour_wire::OLDEST_PEER_PROTOCOL_VERSION < PEER_PROTOCOL_VERSION
        );

        // A version further away, older or newer: refused as such, whatever
        // follows.
        for theirs in [
            neighbour_wire::OLDEST_PEER_PROTOCOL_VERSION - 1,
            PEER_PROTOCOL_VERSION + 2,
        ] {
            let refused = decode_handshake::<Hello>(&frame(theirs)).unwrap_err();
            assert_eq!(refused.peer_version(), Some(theirs));
            assert!(server_accepts(&frame(theirs), &user.public_key).is_none());
        }
        let mut newer = postcard::to_stdvec(&(PEER_PROTOCOL_VERSION + 2)).unwrap();
        newer.extend_from_slice(b"a shape this build has never seen");
        assert_eq!(
            decode_handshake::<Hello>(&newer)
                .unwrap_err()
                .peer_version(),
            Some(PEER_PROTOCOL_VERSION + 2)
        );
        assert!(this_device_is_behind(PEER_PROTOCOL_VERSION + 1));
        assert!(!this_device_is_behind(PEER_PROTOCOL_VERSION - 1));

        // The server answers another version with its own, alone: what a
        // client at another version reads in place of an ack.
        let answer = version_answer().unwrap();
        assert_eq!(
            postcard::take_from_bytes::<u32>(&answer).unwrap(),
            (PEER_PROTOCOL_VERSION, &[][..])
        );
    }

    /// A [`HelloRefused`] with its reason by number, as a build that knows more reasons than
    /// this one writes it.
    #[derive(Serialize)]
    struct Later<'a> {
        protocol: u32,
        cert: &'a DeviceCert,
        refusal: u32,
        #[serde(with = "serde_bytes")]
        sig_over_refusal: [u8; 64],
        revocation: Option<RevocationEntry>,
    }

    /// `root`'s removal of `device`, in a list of its own.
    fn removed(root: &SigningKey, device: &Party) -> RevocationList {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device.cert.device_public_key,
            revoked_at_ns: 500,
            reason: RevocationReason::Retired,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        let mut revocations = RevocationList::new();
        revocations.insert(entry).unwrap();
        revocations
    }

    tilo_test_env::scenario! {
        fn a_refusal_says_why_only_where_the_refusal_already_tells;

        phase a_removal_is_said_when_it_alone_refused_the_device;
        {
            let (server, laptop, member, alice, alice_root) = alice_and_bob();
            let revocations = removed(&alice_root, &laptop);
            let refused = |trust, party: &Party, revocations: &RevocationList| {
                let error = server_begin(
                    &identity(&server, &alice),
                    trust,
                    revocations,
                    &hello(party),
                    1000,
                    [5; 32],
                )
                .unwrap_err();
                let said = server_refusal(
                    &identity(&server, &alice),
                    trust,
                    revocations,
                    &hello(party),
                    &error,
                );
                (error, said.map(|refused| refused.refusal))
            };
            // The person's own device, removed: every endpoint that would have admitted it
            // says so.
            let own_only = TrustPolicy::default();
            for trust in [
                HandshakeTrust::OwnerDevicesOnly,
                HandshakeTrust::AnyCertifiedDevice,
                HandshakeTrust::Live(&own_only),
            ] {
                let (error, said) = refused(trust, &laptop, &revocations);
                assert!(matches!(error, AuthError::Revoked));
                assert_eq!(said, Some(HelloRefusal::DeviceRevoked));
            }
            // A device its person retired is handed its root's entry, which proves itself; one
            // lost, stolen or compromised is told only that it was removed.
            let handed = |revocations: &RevocationList| {
                let trust = HandshakeTrust::OwnerDevicesOnly;
                let error = server_begin(
                    &identity(&server, &alice),
                    trust,
                    revocations,
                    &hello(&laptop),
                    1000,
                    [5; 32],
                )
                .unwrap_err();
                server_refusal(&identity(&server, &alice), trust, revocations, &hello(&laptop), &error)
                    .and_then(|refused| refused.revocation)
            };
            let retired = revocations.iter().next().cloned().unwrap();
            assert_eq!(handed(&revocations), Some(retired.clone()));
            let mut lost = RevocationEntry {
                reason: RevocationReason::Lost,
                ..retired
            };
            sign_revocation_entry(&mut lost, &alice_root).unwrap();
            let mut losing = RevocationList::new();
            losing.insert(lost).unwrap();
            assert_eq!(handed(&losing), None);
            // A device nothing removed, of a person the endpoint does not admit, hears nothing:
            // nor does one whose removal the server knows, since that person's other devices
            // hear nothing here either.
            let (error, said) = refused(HandshakeTrust::OwnerDevicesOnly, &member, &revocations);
            assert!(matches!(error, AuthError::PolicyReject));
            assert_eq!(said, None);
            let (bob_root, _) = UserIdentity::generate("Bob", 1);
            let stranger = enrol(
                &bob_root,
                bob_root.verifying_key().to_bytes(),
                "bob-phone",
                DeviceCapabilities::primary(),
            );
            let strangers = removed(&bob_root, &stranger);
            let (error, said) = refused(HandshakeTrust::OwnerDevicesOnly, &stranger, &strangers);
            assert!(matches!(error, AuthError::Revoked));
            assert_eq!(said, None);
            // An endpoint of the workspaces' members tells a person it admits no more only that,
            // their removed device and the others alike: a copy they kept learns to let it go.
            let nobody = TrustPolicy::AllowList { users: Vec::new() };
            let (error, said) = refused(HandshakeTrust::Live(&nobody), &member, &revocations);
            assert!(matches!(error, AuthError::PolicyReject));
            assert_eq!(said, Some(HelloRefusal::NotAdmitted));
            let (error, said) = refused(HandshakeTrust::Live(&nobody), &stranger, &strangers);
            assert!(matches!(error, AuthError::Revoked));
            assert_eq!(said, Some(HelloRefusal::NotAdmitted));
            // Where that person is admitted, the removal alone refuses the device, and it is told.
            let admitting = TrustPolicy::AllowList {
                users: vec![hex::encode(stranger.cert.user_public_key)],
            };
            let (error, said) = refused(HandshakeTrust::Live(&admitting), &stranger, &strangers);
            assert!(matches!(error, AuthError::Revoked));
            assert_eq!(said, Some(HelloRefusal::DeviceRevoked));
            // A certificate that does not verify is refused before a removal is looked at, and
            // the device is told its certificate was refused: that is its own to know, and to
            // certify again.
            let mut forged = Party {
                cert: laptop.cert.clone(),
                sk: laptop.sk.clone(),
            };
            forged.cert.capabilities.foreign = !forged.cert.capabilities.foreign;
            let (error, said) = refused(HandshakeTrust::OwnerDevicesOnly, &forged, &revocations);
            assert!(matches!(error, AuthError::Cert(_)));
            assert_eq!(said, Some(HelloRefusal::CertificateRejected));
            // No other refusal says anything, whatever the endpoint admits.
            for error in [
                AuthError::PolicyReject,
                AuthError::BadChallengeSig,
                AuthError::EndpointMismatch,
            ] {
                assert_eq!(
                    HelloRefusal::said_of(
                        &error,
                        &laptop.cert,
                        &alice,
                        HandshakeTrust::AnyCertifiedDevice
                    ),
                    None
                );
            }
        }

        phase a_device_believes_a_refusal_where_it_would_believe_the_ack;
        {
            let (server, _, member, alice, _) = alice_and_bob();
            let revs = RevocationList::new();
            let nonce = [3u8; 32];
            let refused = build_hello_refused(
                server.cert.clone(),
                &server.sk,
                &nonce,
                HelloRefusal::DeviceRevoked,
                None,
            );
            assert_eq!(refused.protocol, PEER_PROTOCOL_VERSION);
            // Its server's, for this hello: believed by each check an ack passes.
            let peer = verify_hello_ack(&refused, &alice, &revs, 1000, &nonce).unwrap();
            assert_eq!(peer.device_public_key, server.cert.device_public_key);
            verify_own_device_ack(
                &refused,
                &alice,
                &server.cert.device_public_key,
                &revs,
                1000,
                &nonce,
            )
            .unwrap();
            // A refusal of another hello is none of this one.
            assert!(matches!(
                verify_hello_ack(&refused, &alice, &revs, 1000, &[4u8; 32]),
                Err(AuthError::BadChallengeSig)
            ));
            // Another person's device refuses nobody in this server's name.
            let foreign = build_hello_refused(
                member.cert.clone(),
                &member.sk,
                &nonce,
                HelloRefusal::DeviceRevoked,
                None,
            );
            assert!(matches!(
                verify_hello_ack(&foreign, &alice, &revs, 1000, &nonce),
                Err(AuthError::PolicyReject)
            ));
            assert!(matches!(
                verify_own_device_ack(
                    &foreign,
                    &alice,
                    &server.cert.device_public_key,
                    &revs,
                    1000,
                    &nonce
                ),
                Err(AuthError::PolicyReject)
            ));
            // Nor does a key under this server's certificate that is not its own.
            let mut forged = foreign;
            forged.cert = server.cert;
            assert!(matches!(
                verify_hello_ack(&forged, &alice, &revs, 1000, &nonce),
                Err(AuthError::BadChallengeSig)
            ));
        }

        phase an_ack_is_no_refusal_and_a_refusal_no_ack;
        {
            let (server, _, _, alice, _) = alice_and_bob();
            let revs = RevocationList::new();
            let victim_nonce = [9u8; 32];
            // A server acks whatever nonce a device it admits hands it. Neither the ack of the
            // victim's own nonce, nor the ack of a nonce picked to be the refusal's digest, is a
            // refusal of the victim's hello.
            for asked in [
                victim_nonce,
                refusal_digest(&victim_nonce, HelloRefusal::DeviceRevoked),
            ] {
                let ack = build_hello_ack(server.cert.clone(), &server.sk, [3u8; 32], &asked);
                let forged = HelloRefused {
                    protocol: PEER_PROTOCOL_VERSION,
                    cert: server.cert.clone(),
                    refusal: HelloRefusal::DeviceRevoked,
                    sig_over_refusal: ack.sig_over_client_nonce,
                    revocation: None,
                };
                assert!(
                    matches!(
                        verify_hello_ack(&forged, &alice, &revs, 1000, &victim_nonce),
                        Err(AuthError::BadChallengeSig)
                    ),
                    "an ack's signature must never verify as a refusal"
                );
            }
            // And a refusal lets nobody in: its signature is no ack of the same hello.
            let refused = build_hello_refused(
                server.cert.clone(),
                &server.sk,
                &victim_nonce,
                HelloRefusal::DeviceRevoked,
                None,
            );
            let forged = HelloAck {
                protocol: PEER_PROTOCOL_VERSION,
                cert: server.cert,
                server_nonce: [4u8; 32],
                sig_over_client_nonce: refused.sig_over_refusal,
            };
            assert!(
                matches!(
                    verify_hello_ack(&forged, &alice, &revs, 1000, &victim_nonce),
                    Err(AuthError::BadChallengeSig)
                ),
                "a refusal's signature must never verify as an ack"
            );
        }

        phase it_travels_whole_and_a_reason_nobody_knows_yet_does_not_read;
        {
            let (server, _, _, _, _) = alice_and_bob();
            let refused = build_hello_refused(
                server.cert.clone(),
                &server.sk,
                &[3u8; 32],
                HelloRefusal::DeviceRevoked,
                None,
            );
            let frame = postcard::to_stdvec(&refused).unwrap();
            let read: HelloRefused = decode_handshake(&frame).unwrap();
            assert_eq!(read.refusal, HelloRefusal::DeviceRevoked);
            assert_eq!(read.cert, refused.cert);
            assert_eq!(read.sig_over_refusal, refused.sig_over_refusal);
            assert_eq!(HelloRefusal::DeviceRevoked.code(), "device-revoked");

            // The same frame with a reason added after this build: it does not decode, so a
            // device takes it as a refusal with nothing said.
            let later = postcard::to_stdvec(&Later {
                protocol: PEER_PROTOCOL_VERSION,
                cert: &server.cert,
                refusal: 0,
                sig_over_refusal: refused.sig_over_refusal,
                revocation: None,
            })
            .unwrap();
            assert_eq!(later, frame, "the stand-in is the frame, reason by number");
            let later = postcard::to_stdvec(&Later {
                protocol: PEER_PROTOCOL_VERSION,
                cert: &server.cert,
                refusal: 3,
                sig_over_refusal: refused.sig_over_refusal,
                revocation: None,
            })
            .unwrap();
            assert!(matches!(
                decode_handshake::<HelloRefused>(&later),
                Err(HandshakeFrameError::Codec(_))
            ));
        }
    }

    #[test]
    fn an_entry_proves_its_key_before_its_certificate_is_judged() {
        use crate::entering::{Entering, Entry, Through, entry_digest};
        let (server, laptop, _, alice, alice_root) = alice_and_bob();
        let revocations = removed(&alice_root, &laptop);
        let binding = [7u8; 32];
        let entering = Entering {
            workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_owned(),
            name: "Alice".to_owned(),
            request_id: [2; 32],
            through: Through::Entry,
        };
        let entry = |proof| {
            Hello::entering(
                laptop.cert.clone(),
                [3; 32],
                Entry {
                    entering: entering.clone(),
                    access: None,
                    proof,
                },
            )
        };
        let enter = |hello: &Hello, revocations: &RevocationList| {
            server_enter(
                &identity(&server, &alice),
                revocations,
                hello,
                &binding,
                1000,
                [5; 32],
            )
        };
        let proof = laptop
            .sk
            .sign(&entry_digest(
                &binding,
                &laptop.cert.device_public_key,
                &entering,
            ))
            .to_bytes();
        // Proved and not removed: in.
        enter(&entry(proof), &RevocationList::new()).unwrap();
        // Proved and removed: refused as removed, which the server may then say.
        let error = enter(&entry(proof), &revocations).unwrap_err();
        assert!(matches!(error, AuthError::Revoked));
        assert_eq!(
            HelloRefusal::said_of(
                &error,
                &laptop.cert,
                &alice,
                HandshakeTrust::AnyCertifiedDevice
            ),
            Some(HelloRefusal::DeviceRevoked)
        );
        // Not proved: refused for that, removed or not, so nothing of the certificate is said
        // to whoever only shows it.
        for revocations in [&RevocationList::new(), &revocations] {
            assert!(matches!(
                enter(&entry([0; 64]), revocations).unwrap_err(),
                AuthError::BadChallengeSig
            ));
        }
    }
}
