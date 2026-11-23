//! The messages one enrollment carries, sealed for the device they are for.
//!
//! The same bytes travel over the local network and over iroh: the request a
//! machine seals to the device that issued its token, the answer that device
//! seals back to the key in that request, the machine's acknowledgement, and
//! the approver's Finalized. Only the way they are carried differs.

use anyhow::{Context, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_user_identity::canonical::signing_digest;
use tilo_user_identity::{DeviceCert, StatementSigner};

use crate::root::RootSigner;

/// Well-known UDP port every nearby-pairing listener binds and every
/// soliciting device broadcasts to. Broadcast discovery has no way to
/// advertise a port before the first datagram, so the rendezvous must be
/// fixed. It stays below Linux's ephemeral range (32768-60999) so an outgoing
/// socket can never be auto-assigned it and block a listener, and inside the
/// 1025-65535 range the desktop firewall zone accepts. IANA leaves it
/// unassigned and no common service uses it.
pub const NEARBY_PORT: u16 = 23_831;
/// The largest datagram one enrollment carries on the local network.
pub const MAX_PACKET: usize = 60 * 1024;
/// The domain a response's signature is over, before the payload.
pub const RESPONSE_DOMAIN: &[u8] = b"tilo-enrollment-response-v2\0";
/// The domain an acknowledgement's signature is over, before the payload.
pub const ACK_DOMAIN: &[u8] = b"tilo-enrollment-ack-v1\0";
/// The domain a Finalized's signature is over, before the payload.
pub const FINALIZED_DOMAIN: &[u8] = b"tilo-enrollment-finalized-v1\0";
/// The domain a nearby pair offer is sealed under.
pub const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";
/// Solicitation magic: the device reads HPKE pair offers.
pub const PAIR_MAGIC: [u8; 8] = *b"TLOPAIR2";

/// What a machine enrolls as: a home server of the person's, or one of their
/// personal devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrollmentKind {
    HomePeer,
    PersonalDevice,
}

/// A machine asking to enroll: the token it was handed, its own keys, and what
/// it calls itself.
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    pub device_public_key: [u8; 32],
    pub server_node_id: [u8; 32],
    pub label: String,
    /// What the machine is, in its own words, for the person deciding whether
    /// to admit it ([`crate::hardware_summary`]).
    #[serde(default)]
    pub hardware: String,
    pub kind: EnrollmentKind,
}

/// Plaintext of a v2 request: the request plus the key its response is
/// sealed to (only the requester holds the matching secret).
#[derive(Serialize)]
pub struct RequestV2<'a> {
    pub request: &'a Request,
    pub response_public_key: [u8; 32],
}

#[derive(Deserialize)]
pub struct RequestV2Owned {
    pub request: Request,
    pub response_public_key: [u8; 32],
}

/// Where an enrollment response is sealed to: the response key inside the
/// requester's sealed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseRoute {
    pub response_public_key: [u8; 32],
}

/// What a device of the person answers a machine with.
#[derive(Serialize, Deserialize)]
pub struct ResponsePayload {
    pub nonce: [u8; 16],
    pub recipient_device_public_key: [u8; 32],
    pub server_node_id: [u8; 32],
    pub cert: DeviceCert,
    /// A public-only runtime identity file. It deliberately contains no
    /// root signing share; collaboration is authenticated by the separately
    /// certified device key installed in `identity_root`.
    pub public_identity_json: Option<Vec<u8>>,
    /// Complete bounded fleet projection after this device was admitted.
    pub fleet_json: Option<Vec<u8>>,
    /// The device the joining one asks into the person's workspace: the
    /// approving device itself, which is one of the person's.
    pub introducer: Option<tilo_server_protocol::servers::Introducer>,
}

/// The approving device's answer when it holds the identity's key: signed by
/// that key, the answer every build understands.
#[derive(Serialize, Deserialize)]
pub struct Response {
    pub payload: ResponsePayload,
    pub signature: Vec<u8>,
}

/// The approving device's answer when it holds none of the identity's key and
/// introduces a device the person's key holders certified: signed by its own
/// certified device key, the device the token named as its issuer.
#[derive(Serialize, Deserialize)]
pub struct IntroducedResponse {
    pub payload: ResponsePayload,
    pub issuer: DeviceCert,
    pub signature: Vec<u8>,
}

/// What signs the approving side of a pairing.
#[derive(Clone, Copy)]
pub enum Approver<'a> {
    /// The identity's key, held here.
    Root(&'a dyn RootSigner),
    /// This device's certified key, introducing a device the person's key
    /// holders certified.
    Introducer {
        device: &'a ed25519_dalek::SigningKey,
        issuer: &'a DeviceCert,
    },
}

impl Approver<'_> {
    /// Sign `digest`, a statement's domain-separated digest, as this side of
    /// the pairing.
    ///
    /// # Errors
    ///
    /// The identity's key refuses, or cannot sign.
    pub fn sign(&self, digest: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
        Ok(match self {
            Self::Root(root) => root.sign_digest(digest)?.to_vec(),
            Self::Introducer { device, .. } => device.sign_digest(digest)?.to_vec(),
        })
    }
}

/// A machine that shows a code, as it announces itself to the device of the
/// person that dials it.
#[derive(Clone, Serialize, Deserialize)]
pub struct PairSolicitation {
    pub magic: [u8; 8],
    pub device_public_key: [u8; 32],
    pub server_node_id: [u8; 32],
    pub label: String,
    /// What the machine is, in its own words ([`crate::hardware_summary`]).
    pub hardware: String,
    pub kind: EnrollmentKind,
}

#[derive(Serialize, Deserialize)]
pub struct AckPayload {
    pub nonce: [u8; 16],
    pub device_public_key: [u8; 32],
    pub response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
pub struct Ack {
    pub payload: AckPayload,
    pub signature: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct FinalizedPayload {
    pub nonce: [u8; 16],
    pub device_public_key: [u8; 32],
    pub response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
pub struct Finalized {
    pub payload: FinalizedPayload,
    pub signature: Vec<u8>,
}

pub(crate) const ENVELOPE_MAGIC: &[u8; 7] = b"TLOENRL";
pub(crate) const ENVELOPE_V2: u8 = 2;

pub(crate) fn v2_header() -> [u8; 8] {
    let [m0, m1, m2, m3, m4, m5, m6] = *ENVELOPE_MAGIC;
    [m0, m1, m2, m3, m4, m5, m6, ENVELOPE_V2]
}

/// The sealed box inside `bytes`, or `None` without the envelope prefix.
pub fn v2_body(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(&v2_header()[..])
}

pub(crate) fn wrap_v2(sealed: &tilo_seal::SealedBox) -> Vec<u8> {
    let mut out = v2_header().to_vec();
    out.extend(sealed.to_bytes());
    out
}

/// A device's X25519 sealing secret, derived from its Ed25519 key.
fn device_sealing_secret(device: &ed25519_dalek::SigningKey) -> tilo_seal::SecretKey {
    tilo_seal::SecretKey::from_bytes(device.to_scalar_bytes())
}

/// A device's X25519 sealing public key, derived from its Ed25519 key.
pub(crate) fn device_sealing_public(
    device_public_key: [u8; 32],
) -> anyhow::Result<tilo_seal::PublicKey> {
    Ok(tilo_seal::PublicKey::from_bytes(
        VerifyingKey::from_bytes(&device_public_key)?
            .to_montgomery()
            .to_bytes(),
    ))
}

fn pair_offer_aad(recipient: [u8; 32]) -> Vec<u8> {
    [PAIR_DOMAIN, &recipient].concat()
}

/// Seal an enrollment token to the soliciting device.
pub fn encrypt_pair_offer(token: &str, recipient: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &device_sealing_public(recipient)?,
        tilo_seal::Label::ServerEnrollOffer,
        &pair_offer_aad(recipient),
        token.as_bytes(),
    )?))
}

/// Open the token a device of the person sealed to this machine.
pub fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let recipient_public = recipient.verifying_key().to_bytes();
    let secret = device_sealing_secret(recipient);
    let aad = pair_offer_aad(recipient_public);
    let plaintext = v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(&secret, tilo_seal::Label::ServerEnrollOffer, &aad, body)
                .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext.to_vec()).context("nearby pairing offer is not text")
}

fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

/// Seal an enrollment request to the issuing device. Returns
/// the envelope and the secret its response will be sealed to.
pub fn encrypt_request(
    request: &Request,
    issuer_device_public_key: [u8; 32],
) -> anyhow::Result<(Vec<u8>, tilo_seal::SecretKey)> {
    let response_secret = tilo_seal::SecretKey::generate()?;
    let plaintext = zeroize::Zeroizing::new(postcard::to_stdvec(&RequestV2 {
        request,
        response_public_key: response_secret.public_key()?.to_bytes(),
    })?);
    let sealed = tilo_seal::hpke::seal(
        &device_sealing_public(issuer_device_public_key)?,
        tilo_seal::Label::ServerEnrollRequest,
        &v2_header(),
        &plaintext,
    )?;
    Ok((wrap_v2(&sealed), response_secret))
}

/// Open an enrollment request, returning where its response must be sealed
/// to.
pub fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, ResponseRoute)> {
    let secret = device_sealing_secret(issuer_device);
    let not_for_us = || anyhow::anyhow!("enrollment request was not encrypted for this issuer");
    let body = v2_body(bytes).ok_or_else(not_for_us)?;
    let plaintext = tilo_seal::hpke::open_bytes(
        &secret,
        tilo_seal::Label::ServerEnrollRequest,
        &v2_header(),
        body,
    )
    .map_err(|_| not_for_us())?;
    let decoded: RequestV2Owned = postcard::from_bytes(&plaintext)?;
    Ok((
        decoded.request,
        ResponseRoute {
            response_public_key: decoded.response_public_key,
        },
    ))
}

/// Seal the signed response to the requester.
pub fn encrypt_response(
    plaintext: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    route: ResponseRoute,
) -> anyhow::Result<Vec<u8>> {
    let aad = response_aad(token_nonce, recipient);
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &tilo_seal::PublicKey::from_bytes(route.response_public_key),
        tilo_seal::Label::ServerEnrollResponse,
        &aad,
        plaintext,
    )?))
}

/// Open the response a device of the person sealed to this machine.
pub fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<zeroize::Zeroizing<Vec<u8>>> {
    let aad = response_aad(token_nonce, recipient);
    v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(
                response_secret,
                tilo_seal::Label::ServerEnrollResponse,
                &aad,
                body,
            )
            .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("enrollment response was not encrypted for this device"))
}

fn response_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn ack_digest(payload: &AckPayload) -> anyhow::Result<[u8; 32]> {
    signing_digest(ACK_DOMAIN, payload).map_err(|error| anyhow::anyhow!("{error}"))
}

fn finalized_digest(payload: &FinalizedPayload) -> anyhow::Result<[u8; 32]> {
    signing_digest(FINALIZED_DOMAIN, payload).map_err(|error| anyhow::anyhow!("{error}"))
}

/// The machine's acknowledgement of its signed configuration.
pub fn make_ack(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = AckPayload {
        nonce,
        device_public_key: device.verifying_key().to_bytes(),
        response_digest: response_digest(response_bytes),
    };
    let ack = Ack {
        signature: device.sign_digest(&ack_digest(&payload)?)?.to_vec(),
        payload,
    };
    Ok(postcard::to_stdvec(&ack)?)
}

/// Check the machine acknowledged the configuration this device sent it.
pub fn verify_ack(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let ack: Ack = postcard::from_bytes(bytes)?;
    if ack.payload.nonce != nonce
        || ack.payload.device_public_key != device_public_key
        || ack.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment acknowledgement did not match the response");
    }
    let signature: [u8; 64] = ack
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("acknowledgement signature has the wrong length"))?;
    VerifyingKey::from_bytes(&device_public_key)?.verify(
        &ack_digest(&ack.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

/// The approver's word that the machine is installed, signed by the key that
/// admitted it.
pub fn make_finalized(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver: Approver<'_>,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let signature = approver.sign(&finalized_digest(&payload)?)?;
    Ok(postcard::to_stdvec(&Finalized { payload, signature })?)
}

/// Check the approver's Finalized matches this exchange and its signature.
pub fn verify_finalized(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let finalized: Finalized = postcard::from_bytes(bytes)?;
    if finalized.payload.nonce != nonce
        || finalized.payload.device_public_key != device_public_key
        || finalized.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment completion did not match this exchange");
    }
    crate::verify_signature(
        approver_public_key,
        &finalized_digest(&finalized.payload)?,
        &finalized.signature,
    )
}
