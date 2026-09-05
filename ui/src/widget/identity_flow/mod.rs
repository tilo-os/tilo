//! The identity ceremony: a state machine of screens for bootstrapping
//! an identity, sharing/verifying recovery words, pairing a second
//! device via DKG, and minting/accepting channel invites, over the
//! `threshold-identity` engine.
//!
//! Screens are boxed and pre-built rather than one inline enum, so a
//! transition is a constant-time `Box::new` swap.
//!
//! Cryptographic logic lives in standalone functions, not on the
//! screens, so tests (including `end_to_end_ceremony_lets_both_devices_co_sign`)
//! drive it directly without ever building a `TaffyTree`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::widget::TaffyTreeExt;
use automerge::{AutoCommit, ObjId};
use collab_types::{
    Attestation, AttestationId, ChannelEvent, ChannelId, Claim, Clearance, Ed25519PublicKey, Role,
    RoleAttestation, Signature, generate_keypair, sign_event, verify_event,
};
use ed25519_dalek::SigningKey as DalekSigningKey;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_bytes as serde_bytes_codec;
use sha2::{Digest, Sha512};
use taffy::{NodeId, Style, TaffyTree};
use threshold_identity::{EnrolmentRound1, EnrolmentRound2, ThresholdError, ThresholdIdentity};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

pub mod add_device_host;
pub mod create_identity;
pub mod invite_accept;
pub mod invite_mint;
pub mod pair_device;
pub mod recovery_share;
pub mod recovery_verify;

/// Disk-format record for a persisted identity.
///
/// Schema 1 contains a legacy plaintext share. Schema 2 contains public
/// metadata plus an opaque secret-store reference; its `secret_bytes` is
/// empty, so ceremonies that need signing must use the runtime broker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IdentityRecord {
    pub display_name: String,
    /// `[r, g, b]` for direct painting.
    pub avatar_color: [u8; 3],
    /// JSON, not postcard: this is a local file a human may hand-edit,
    /// and `ThresholdIdentity` withholds `Serialize` (the secret share
    /// is deliberately non-serializable) — [`SerializableIdentity`] is
    /// the extracted wire view.
    pub threshold_identity: SerializableIdentity,
    pub created_at: String,
    /// Loader rejects unknown versions.
    pub schema_version: u32,
    /// Present in secured schema 2 records. This is an opaque lookup token,
    /// never a host path or key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<IdentitySecretReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IdentitySecretReference {
    pub backend: String,
    pub locator: String,
}

/// Wire view of a `ThresholdIdentity`. `ThresholdIdentity` withholds
/// `Serialize` because the secret share carries invariants a derive
/// can't enforce; this extracts just enough to reconstruct an
/// equivalent identity. `secret_bytes` is either the raw bootstrap seed or a
/// complete opaque FROST device package — treat this struct as secret material.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SerializableIdentity {
    /// Distinguishes a dalek bootstrap seed from an opaque FROST device-share
    /// envelope. Both are device-local secret material.
    pub single_device: bool,
    pub group_public_key: [u8; 32],
    /// `t` in t-of-n (`1` in single-device mode).
    pub threshold: u16,
    /// `n` in t-of-n (`1` in single-device mode).
    pub total_shares: u16,
    pub share_index: u16,
    /// A 32-byte Ed25519 seed in bootstrap mode, or the bounded versioned
    /// opaque blob returned by `ThresholdIdentity::export_device_secret`.
    #[serde(default, with = "serde_bytes_codec")]
    pub secret_bytes: Vec<u8>,
}

impl SerializableIdentity {
    /// # Errors
    ///
    /// Returns an engine error if a threshold device share cannot be exported.
    pub fn from_identity(id: &ThresholdIdentity) -> Result<Self, IdentityFlowError> {
        let (single_device, secret_bytes) = if let Some(seed) = id.single_device_seed() {
            (true, seed.to_vec())
        } else {
            (false, id.export_device_secret()?.to_vec())
        };
        Ok(Self {
            single_device,
            group_public_key: *id.group_public_key().as_bytes(),
            threshold: id.threshold,
            total_shares: id.total_shares,
            share_index: id.this_share_index,
            secret_bytes,
        })
    }
}

impl IdentityRecord {
    /// # Errors
    ///
    /// Exports only an opaque, invariant-checked device-share envelope in
    /// threshold mode; callers must immediately move it into a secret provider.
    pub fn from_threshold_identity(
        identity: &ThresholdIdentity,
        display_name: impl Into<String>,
        avatar_color: [u8; 3],
        created_at: SystemTime,
    ) -> Result<Self, IdentityFlowError> {
        let serialisable = SerializableIdentity::from_identity(identity)?;
        let ts = chrono::DateTime::<chrono::Utc>::from(created_at).to_rfc3339();
        Ok(Self {
            display_name: display_name.into(),
            avatar_color,
            threshold_identity: serialisable,
            created_at: ts,
            schema_version: 1,
            secret_ref: None,
        })
    }

    /// Persisted under `~/.config/qos/identity/primary.json`.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::EncodeFailed`] if serialisation fails.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, IdentityFlowError> {
        serde_json::to_vec_pretty(self).map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))
    }

    /// Accepts legacy schema 1 and secured metadata schema 2; rejects future
    /// layouts so a ceremony cannot reinterpret key material.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `bytes` isn't valid
    /// JSON for this type, or [`IdentityFlowError::UnknownSchemaVersion`]
    /// if the record's `schema_version` is neither `1` nor `2`.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, IdentityFlowError> {
        let rec: Self = serde_json::from_slice(bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(e.to_string()))?;
        if !matches!(rec.schema_version, 1 | 2) {
            return Err(IdentityFlowError::UnknownSchemaVersion(rec.schema_version));
        }
        Ok(rec)
    }

    pub fn public_key(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.threshold_identity.group_public_key)
    }

    /// Checks the key material against every stored public invariant; a
    /// mismatch is refused rather than silently loaded.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if
    /// the stored secret isn't 32 bytes, or
    /// [`IdentityFlowError::PublicKeyMismatch`] if the derived public key
    /// doesn't match the recorded one.
    pub fn into_threshold_identity(self) -> Result<ThresholdIdentity, IdentityFlowError> {
        let ti = self.threshold_identity;
        if !ti.single_device {
            let identity =
                ThresholdIdentity::from_device_secret(&ti.secret_bytes, ti.total_shares)?;
            if identity.group_public_key().as_bytes() != &ti.group_public_key
                || identity.threshold != ti.threshold
                || identity.this_share_index != ti.share_index
            {
                return Err(IdentityFlowError::PublicKeyMismatch);
            }
            return Ok(identity);
        }
        let seed: [u8; 32] = ti.secret_bytes.as_slice().try_into().map_err(|_| {
            IdentityFlowError::DecodeFailed(format!(
                "identity secret must be 32 bytes, got {}",
                ti.secret_bytes.len()
            ))
        })?;
        let identity = ThresholdIdentity::from_single_device_seed(&seed);
        if identity.group_public_key().as_bytes() != &ti.group_public_key {
            return Err(IdentityFlowError::PublicKeyMismatch);
        }
        Ok(identity)
    }
}

/// The shell wires the real disk-backed impl; tests use
/// [`MemoryIdentityStore`] to avoid touching `~/.config`.
pub trait IdentityStore: Send + Sync {
    /// # Errors
    ///
    /// Returns an error if the backing store exists but can't be read or
    /// decoded.
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError>;
    /// # Errors
    ///
    /// Returns an error if `record` can't be encoded or written.
    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError>;
}

/// Survives the process lifetime but never touches disk.
#[derive(Default, Clone)]
pub struct MemoryIdentityStore {
    inner: Arc<Mutex<Option<IdentityRecord>>>,
}

impl MemoryIdentityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdentityStore for MemoryIdentityStore {
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError> {
        // Recover rather than panic on a poisoned lock: a panic here
        // would abort the whole process in a release build.
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record.clone());
        Ok(())
    }
}

/// JSON payload encoded in the host's QR code on screen 3.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingPayload {
    #[serde(rename = "type")]
    pub ty: String,
    /// Also seeds the 6-digit short code.
    pub channel: String,
    pub initiator_pk: String,
    /// Postcard-encoded `EnrolmentRound1`, hex for ASCII-safe JSON.
    pub initiator_dkg_round1: String,
    /// Independently reachable shell broker. Empty means pairing is honestly
    /// unavailable outside an injected test transport.
    #[serde(default)]
    pub broker_endpoint: String,
    /// DER certificate pinned exactly for this short-lived ceremony.
    #[serde(default)]
    pub broker_certificate: String,
}

impl PairingPayload {
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::EncodeFailed`] if `round1` can't be
    /// postcard-encoded.
    pub fn new(
        nonce: [u8; 16],
        initiator_pk: &Ed25519PublicKey,
        round1: &EnrolmentRound1,
    ) -> Result<Self, IdentityFlowError> {
        let round1_bytes = postcard::to_allocvec(round1)
            .map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))?;
        Ok(Self {
            ty: "qos-pairing".to_string(),
            channel: hex::encode(nonce),
            initiator_pk: hex::encode(initiator_pk.as_bytes()),
            initiator_dkg_round1: hex::encode(round1_bytes),
            broker_endpoint: String::new(),
            broker_certificate: String::new(),
        })
    }

    /// Bind a validated reachable socket address and bounded exact DER pin.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid endpoint or empty/oversized certificate.
    pub fn set_broker(
        &mut self,
        endpoint: impl Into<String>,
        certificate_der: &[u8],
    ) -> Result<(), IdentityFlowError> {
        let endpoint = endpoint.into();
        endpoint.parse::<std::net::SocketAddr>().map_err(|error| {
            IdentityFlowError::DecodeFailed(format!("pairing broker endpoint: {error}"))
        })?;
        if certificate_der.is_empty() || certificate_der.len() > 64 * 1024 {
            return Err(IdentityFlowError::DecodeFailed(
                "pairing broker certificate is empty or too large".into(),
            ));
        }
        self.broker_endpoint = endpoint;
        self.broker_certificate = hex::encode(certificate_der);
        Ok(())
    }

    /// Build a client that requires the exact broker certificate carried by
    /// this payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint or certificate encoding is invalid,
    /// absent, or oversized.
    pub fn remote_broker(&self) -> Result<PairingHandshake, IdentityFlowError> {
        let certificate = hex::decode(&self.broker_certificate)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("broker certificate: {e}")))?;
        if self.broker_endpoint.is_empty() || certificate.is_empty() {
            return Err(IdentityFlowError::DecodeFailed(
                "pairing broker unavailable".into(),
            ));
        }
        PairingHandshake::remote(self.broker_endpoint.clone(), certificate)
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::EncodeFailed`] if serialisation fails.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, IdentityFlowError> {
        serde_json::to_vec(self).map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `bytes` isn't valid
    /// JSON for this type, or if the decoded payload's type tag isn't
    /// `"qos-pairing"`.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, IdentityFlowError> {
        if bytes.len() > 256 * 1024 {
            return Err(IdentityFlowError::DecodeFailed(
                "pairing payload exceeds 256 KiB".into(),
            ));
        }
        let p: Self = serde_json::from_slice(bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(e.to_string()))?;
        if p.ty != "qos-pairing" {
            return Err(IdentityFlowError::DecodeFailed(format!(
                "unexpected type tag: {}",
                p.ty
            )));
        }
        p.nonce()?;
        p.initiator_public_key()?;
        p.initiator_round1()?;
        if p.broker_endpoint.is_empty() != p.broker_certificate.is_empty() {
            return Err(IdentityFlowError::DecodeFailed(
                "pairing broker endpoint and certificate must appear together".into(),
            ));
        }
        if !p.broker_endpoint.is_empty() {
            let _ = p.remote_broker()?;
        }
        Ok(p)
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `channel` isn't
    /// valid 16-byte hex.
    pub fn nonce(&self) -> Result<[u8; 16], IdentityFlowError> {
        let bytes = hex::decode(&self.channel)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("nonce hex: {e}")))?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| IdentityFlowError::DecodeFailed("nonce must be 16 bytes".to_string()))
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `initiator_pk` isn't
    /// valid 32-byte hex.
    pub fn initiator_public_key(&self) -> Result<Ed25519PublicKey, IdentityFlowError> {
        let bytes = hex::decode(&self.initiator_pk)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("pk hex: {e}")))?;
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            IdentityFlowError::DecodeFailed("initiator pk must be 32 bytes".to_string())
        })?;
        Ok(Ed25519PublicKey::from_bytes(arr))
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if
    /// `initiator_dkg_round1` isn't valid hex, or isn't a valid postcard
    /// encoding of an [`EnrolmentRound1`].
    pub fn initiator_round1(&self) -> Result<EnrolmentRound1, IdentityFlowError> {
        let bytes = hex::decode(&self.initiator_dkg_round1)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("round1 hex: {e}")))?;
        postcard::from_bytes(&bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("round1 postcard: {e}")))
    }
}

/// `SHA512("qos-pair-code-v1" || nonce)` truncated mod `1_000_000`.
/// ~16M collision space is fine for a short-lived pairing window: the
/// code is the seed, not the authentication — a wrong code just fails
/// the downstream MAC check.
pub fn derive_pairing_code(nonce: &[u8; 16]) -> u32 {
    let mut hasher = Sha512::new();
    hasher.update(b"qos-pair-code-v1");
    hasher.update(nonce);
    let digest = hasher.finalize();
    // `digest` is a 64-byte SHA-512 output, so the first 4 bytes always
    // exist; the fallback is unreachable in practice.
    let truncated = digest
        .first_chunk::<4>()
        .map_or(0, |b| u32::from_be_bytes(*b));
    truncated % 1_000_000
}

pub fn format_pairing_code(code: u32) -> String {
    format!("{code:06}")
}

/// `SHA-512("qos-pair-mac-v1" || nonce)` truncated to 32 bytes — the
/// MAC key for the pairer's round-2 reply.
pub fn derive_pairing_mac_key(nonce: &[u8; 16]) -> [u8; 32] {
    let mut hasher = Sha512::new();
    hasher.update(b"qos-pair-mac-v1");
    hasher.update(nonce);
    let digest = hasher.finalize();
    // `digest` is a 64-byte SHA-512 output, so the first 32 bytes always
    // exist; the fallback is unreachable in practice.
    digest.first_chunk::<32>().copied().unwrap_or([0u8; 32])
}

type HmacSha512 = Hmac<Sha512>;

/// Host and pairer each derive this key from the same nonce
/// independently, so matching codes imply matching keys.
///
/// # Panics
///
/// Never panics in practice: HMAC is defined for a key of any length
/// (RFC 2104), and `Hmac::<Sha512>::new_from_slice` can only fail for a
/// hash with a zero block size, which SHA-512 never is.
#[allow(
    clippy::expect_used,
    reason = "HMAC accepts any key length; new_from_slice cannot fail for SHA-512"
)]
pub fn compute_pairing_mac(key: &[u8; 32], payload: &[u8]) -> [u8; 64] {
    let mut mac = <HmacSha512 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 64];
    out.copy_from_slice(&bytes[..]);
    out
}

/// Verify a MAC; constant-time comparison via `Hmac::verify_slice`.
///
/// # Panics
///
/// Never panics in practice: HMAC is defined for a key of any length
/// (RFC 2104), and `Hmac::<Sha512>::new_from_slice` can only fail for a
/// hash with a zero block size, which SHA-512 never is.
#[allow(
    clippy::expect_used,
    reason = "HMAC accepts any key length; new_from_slice cannot fail for SHA-512"
)]
pub fn verify_pairing_mac(key: &[u8; 32], payload: &[u8], tag: &[u8; 64]) -> bool {
    let mut mac = <HmacSha512 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload);
    mac.verify_slice(tag).is_ok()
}

/// Authenticate a DKG round against the complete pairing transcript.
///
/// # Errors
///
/// Returns an error if the payload nonce is invalid or the transcript cannot
/// be encoded.
pub fn pairing_transcript_mac(
    payload: &PairingPayload,
    direction: &str,
    pairer_round1: &EnrolmentRound1,
    round2: &EnrolmentRound2,
) -> Result<[u8; 64], IdentityFlowError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        direction: &'a str,
        channel: &'a str,
        initiator_pk: &'a str,
        initiator_round1: &'a str,
        broker_endpoint: &'a str,
        broker_certificate: &'a str,
        pairer_round1: &'a EnrolmentRound1,
        round2: &'a EnrolmentRound2,
    }
    let bytes = postcard::to_allocvec(&Transcript {
        domain: "qos-pairing-transcript-v1",
        direction,
        channel: &payload.channel,
        initiator_pk: &payload.initiator_pk,
        initiator_round1: &payload.initiator_dkg_round1,
        broker_endpoint: &payload.broker_endpoint,
        broker_certificate: &payload.broker_certificate,
        pairer_round1,
        round2,
    })
    .map_err(|error| IdentityFlowError::EncodeFailed(error.to_string()))?;
    Ok(compute_pairing_mac(
        &derive_pairing_mac_key(&payload.nonce()?),
        &bytes,
    ))
}

/// Bounded protocol mailbox used behind the shell's pairing-broker authority.
/// Tests run it directly; production transport code must authenticate and
/// expire the session before forwarding frames into this mailbox.
#[derive(Clone)]
pub struct PairingHandshake {
    backend: PairingBackend,
}

#[derive(Clone)]
enum PairingBackend {
    Memory(Arc<Mutex<HashMap<String, PairingChannelState>>>),
    /// A private broker spool. Each frame is published with `create_new` and
    /// consumed by an atomic rename, so independent processes never observe
    /// the same DKG packet twice. The shell owns this directory.
    Durable(Arc<DurablePairingBackend>),
    Remote {
        endpoint: Arc<str>,
        certificate: Arc<[u8]>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PairingBrokerRequest {
    PutPairerRound1([u8; 16], EnrolmentRound1),
    PutPairerRound2([u8; 16], EnrolmentRound2, PairingMac),
    TakePairerExchange([u8; 16]),
    PutHostRound2([u8; 16], EnrolmentRound2, PairingMac),
    TakeHostRound2([u8; 16]),
    RedeemInvite(Vec<u8>),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PairingBrokerResponse {
    Ok,
    PairerExchange(Option<(EnrolmentRound1, EnrolmentRound2, PairingMac)>),
    HostRound2(Option<(EnrolmentRound2, PairingMac)>),
    InviteSecret(Option<Vec<u8>>),
    Denied,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairingMac(#[serde(with = "serde_bytes_codec")] Vec<u8>);

impl PairingMac {
    pub fn from_bytes(value: [u8; 64]) -> Self {
        Self(value.to_vec())
    }
    pub fn into_bytes(self) -> Option<[u8; 64]> {
        self.0.as_slice().try_into().ok()
    }
}

#[derive(Debug)]
struct DurablePairingBackend {
    root: PathBuf,
    expires_unix: u64,
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct PairingChannelState {
    /// Pairer's DKG round-1 broadcast. Host must consume this before round 2;
    /// synthesizing an empty commitment creates no valid FROST ceremony.
    pairer_round1: Option<EnrolmentRound1>,
    /// Host polls this.
    reply: Option<(EnrolmentRound2, PairingMac)>,
    /// Pairer polls this for the host's round-2 contribution.
    host_round2: Option<(EnrolmentRound2, PairingMac)>,
}

impl Default for PairingHandshake {
    fn default() -> Self {
        Self {
            backend: PairingBackend::Memory(Arc::new(Mutex::new(HashMap::new()))),
        }
    }
}

impl PairingHandshake {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_accepting(&self) -> bool {
        match &self.backend {
            PairingBackend::Memory(_) => true,
            PairingBackend::Durable(backend) => {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(u64::MAX, |duration| duration.as_secs());
                now < backend.expires_unix && !backend.root.join("cancelled").exists()
            }
            PairingBackend::Remote { .. } => false,
        }
    }

    /// Create an exact-certificate-pinned remote broker client.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid socket address or empty/oversized pin.
    pub fn remote(
        endpoint: impl Into<String>,
        certificate: Vec<u8>,
    ) -> Result<Self, IdentityFlowError> {
        let endpoint = endpoint.into();
        endpoint.parse::<std::net::SocketAddr>().map_err(|error| {
            IdentityFlowError::DecodeFailed(format!("pairing broker endpoint: {error}"))
        })?;
        if certificate.is_empty() || certificate.len() > 64 * 1024 {
            return Err(IdentityFlowError::DecodeFailed(
                "pairing broker certificate is empty or too large".into(),
            ));
        }
        Ok(Self {
            backend: PairingBackend::Remote {
                endpoint: Arc::from(endpoint),
                certificate: Arc::from(certificate),
            },
        })
    }

    pub fn remote_call(
        endpoint: &str,
        certificate: &[u8],
        request: &PairingBrokerRequest,
    ) -> Option<PairingBrokerResponse> {
        use std::io::{Read, Write};
        let bytes = postcard::to_allocvec(request).ok()?;
        if bytes.len() > 256 * 1024 {
            return None;
        }
        let tcp =
            std::net::TcpStream::connect_timeout(&endpoint.parse().ok()?, Duration::from_secs(2))
                .ok()?;
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        tcp.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
        if certificate.is_empty() || certificate.len() > 64 * 1024 {
            return None;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(ExactCertificateVerifier {
                expected: Arc::from(certificate),
                provider,
            }))
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from("qos-pairing.invalid").ok()?;
        let connection = rustls::ClientConnection::new(Arc::new(config), name).ok()?;
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        let frame_len = u32::try_from(bytes.len()).ok()?;
        stream.write_all(&frame_len.to_be_bytes()).ok()?;
        stream.write_all(&bytes).ok()?;
        let mut len = [0; 4];
        stream.read_exact(&mut len).ok()?;
        let len = u32::from_be_bytes(len) as usize;
        if len > 256 * 1024 {
            return None;
        }
        let mut response = vec![0; len];
        stream.read_exact(&mut response).ok()?;
        postcard::from_bytes(&response).ok()
    }

    /// Open a restart-safe broker spool owned by the shell/runtime. `root`
    /// must already be a private directory; this constructor creates it as
    /// `0700` when absent and rejects symlinks or permissive directories.
    ///
    /// # Errors
    ///
    /// Returns an error if the spool cannot be created securely or its durable
    /// expiry metadata is invalid or cannot be persisted.
    pub fn durable(
        root: impl Into<PathBuf>,
        expires_at: SystemTime,
    ) -> Result<Self, IdentityFlowError> {
        use std::os::unix::fs::PermissionsExt;
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let metadata = std::fs::symlink_metadata(&root)?;
        if !metadata.file_type().is_dir() {
            return Err(IdentityFlowError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "pairing broker spool must be a real directory",
            )));
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let requested_expiry = expires_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let expiry_path = root.join("expires-unix");
        let expires_unix = match std::fs::read_to_string(&expiry_path) {
            Ok(value) => value.trim().parse::<u64>().map_err(|_| {
                IdentityFlowError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "pairing broker expiry is corrupt",
                ))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&expiry_path)?;
                writeln!(file, "{requested_expiry}")?;
                file.sync_all()?;
                requested_expiry
            }
            Err(error) => return Err(IdentityFlowError::Io(error)),
        };
        Ok(Self {
            backend: PairingBackend::Durable(Arc::new(DurablePairingBackend {
                root,
                expires_unix,
            })),
        })
    }

    fn durable_path(backend: &DurablePairingBackend, nonce: &[u8; 16]) -> PathBuf {
        backend
            .root
            .join(format!("{}.postcard", hex::encode(nonce)))
    }

    fn durable_read(
        backend: &DurablePairingBackend,
        nonce: &[u8; 16],
    ) -> Option<PairingChannelState> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(u64::MAX, |duration| duration.as_secs());
        if now >= backend.expires_unix || backend.root.join("cancelled").exists() {
            return None;
        }
        let bytes = std::fs::read(Self::durable_path(backend, nonce)).ok()?;
        if bytes.len() > 256 * 1024 {
            return None;
        }
        postcard::from_bytes(&bytes).ok()
    }

    fn durable_update(
        backend: &DurablePairingBackend,
        nonce: &[u8; 16],
        update: impl FnOnce(&mut PairingChannelState),
    ) -> bool {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(u64::MAX, |duration| duration.as_secs());
        if now >= backend.expires_unix || backend.root.join("cancelled").exists() {
            return false;
        }
        // The lock file is acquired with create_new, which works across
        // processes. Contention is bounded; callers already poll the broker.
        let lock = backend.root.join(format!("{}.lock", hex::encode(nonce)));
        let mut lock_file = None;
        for _ in 0..64 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&lock)
            {
                Ok(file) => {
                    lock_file = Some(file);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::yield_now();
                }
                Err(_) => return false,
            }
        }
        let Some(_lock_file) = lock_file else {
            return false;
        };
        let path = Self::durable_path(backend, nonce);
        let mut state = Self::durable_read(backend, nonce).unwrap_or_default();
        update(&mut state);
        let Ok(bytes) = postcard::to_allocvec(&state) else {
            let _ = std::fs::remove_file(&lock);
            return false;
        };
        let tmp = backend.root.join(format!(
            ".{}.{}.tmp",
            hex::encode(nonce),
            std::process::id()
        ));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut file| {
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, &path))
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .open(&backend.root)?
                    .sync_all()
            })
            .is_ok();
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&lock);
        written
    }

    /// Permanently cancel this durable ceremony. Memory test channels simply
    /// discard all pending frames.
    pub fn cancel(&self) {
        match &self.backend {
            PairingBackend::Memory(inner) => inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear(),
            PairingBackend::Durable(backend) => {
                use std::os::unix::fs::OpenOptionsExt;
                if let Ok(file) = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(backend.root.join("cancelled"))
                {
                    let _ = file.sync_all();
                    let _ = std::fs::OpenOptions::new()
                        .read(true)
                        .open(&backend.root)
                        .and_then(|directory| directory.sync_all());
                }
            }
            PairingBackend::Remote { .. } => {}
        }
    }

    pub fn publish_pairer_round1(&self, nonce: &[u8; 16], round1: EnrolmentRound1) {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => {
                let mut guard = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.entry(key).or_default().pairer_round1 = Some(round1);
            }
            PairingBackend::Durable(backend) => {
                let _ = Self::durable_update(backend, nonce, |state| {
                    if state.pairer_round1.is_none() {
                        state.pairer_round1 = Some(round1);
                    }
                });
            }
            PairingBackend::Remote {
                endpoint,
                certificate,
            } => {
                let _ = Self::remote_call(
                    endpoint,
                    certificate,
                    &PairingBrokerRequest::PutPairerRound1(*nonce, round1),
                );
            }
        }
    }

    pub fn take_pairer_round1(&self, nonce: &[u8; 16]) -> Option<EnrolmentRound1> {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&key)?
                .pairer_round1
                .take(),
            PairingBackend::Durable(backend) => {
                let mut out = None;
                Self::durable_update(backend, nonce, |state| out = state.pairer_round1.take())
                    .then_some(())?;
                out
            }
            PairingBackend::Remote { .. } => None,
        }
    }

    pub fn publish_pairer_reply(&self, nonce: &[u8; 16], reply: EnrolmentRound2, mac: [u8; 64]) {
        let key = hex::encode(nonce);
        // Recover rather than panic on a poisoned lock: a panic here
        // would abort the whole process in a release build.
        match &self.backend {
            PairingBackend::Memory(inner) => {
                inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(key)
                    .or_default()
                    .reply = Some((reply, PairingMac::from_bytes(mac)));
            }
            PairingBackend::Durable(backend) => {
                let _ = Self::durable_update(backend, nonce, |state| {
                    if state.reply.is_none() {
                        state.reply = Some((reply, PairingMac::from_bytes(mac)));
                    }
                });
            }
            PairingBackend::Remote {
                endpoint,
                certificate,
            } => {
                let _ = Self::remote_call(
                    endpoint,
                    certificate,
                    &PairingBrokerRequest::PutPairerRound2(
                        *nonce,
                        reply,
                        PairingMac::from_bytes(mac),
                    ),
                );
            }
        }
    }

    pub fn take_pairer_reply(&self, nonce: &[u8; 16]) -> Option<(EnrolmentRound2, [u8; 64])> {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => {
                let (reply, mac) = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(&key)?
                    .reply
                    .take()?;
                Some((reply, mac.into_bytes()?))
            }
            PairingBackend::Durable(backend) => {
                let mut out = None;
                Self::durable_update(backend, nonce, |state| out = state.reply.take())
                    .then_some(())?;
                out.and_then(|(reply, mac)| Some((reply, mac.into_bytes()?)))
            }
            PairingBackend::Remote { .. } => None,
        }
    }

    /// Atomically take the pairer's complete DKG exchange. Polling before
    /// round 2 arrives leaves round 1 in place instead of losing it.
    pub fn take_pairer_exchange(
        &self,
        nonce: &[u8; 16],
    ) -> Option<(EnrolmentRound1, EnrolmentRound2, [u8; 64])> {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => {
                let (round1, round2, mac) = {
                    let mut guard = inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let state = guard.get_mut(&key)?;
                    if state.pairer_round1.is_none() || state.reply.is_none() {
                        return None;
                    }
                    let round1 = state.pairer_round1.take()?;
                    let (round2, mac) = state.reply.take()?;
                    drop(guard);
                    (round1, round2, mac)
                };
                Some((round1, round2, mac.into_bytes()?))
            }
            PairingBackend::Durable(backend) => {
                let mut out = None;
                Self::durable_update(backend, nonce, |state| {
                    if state.pairer_round1.is_some() && state.reply.is_some() {
                        let round1 = state.pairer_round1.take();
                        let reply = state.reply.take();
                        if let (Some(round1), Some((round2, mac))) = (round1, reply) {
                            out = Some((round1, round2, mac));
                        }
                    }
                })
                .then_some(())?;
                out.and_then(|(round1, round2, mac)| Some((round1, round2, mac.into_bytes()?)))
            }
            PairingBackend::Remote {
                endpoint,
                certificate,
            } => match Self::remote_call(
                endpoint,
                certificate,
                &PairingBrokerRequest::TakePairerExchange(*nonce),
            )? {
                PairingBrokerResponse::PairerExchange(value) => value
                    .and_then(|(round1, round2, mac)| Some((round1, round2, mac.into_bytes()?))),
                _ => None,
            },
        }
    }

    pub fn publish_host_round2(&self, nonce: &[u8; 16], reply: EnrolmentRound2, mac: [u8; 64]) {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => {
                inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(key)
                    .or_default()
                    .host_round2 = Some((reply, PairingMac::from_bytes(mac)));
            }
            PairingBackend::Durable(backend) => {
                let _ = Self::durable_update(backend, nonce, |state| {
                    if state.host_round2.is_none() {
                        state.host_round2 = Some((reply, PairingMac::from_bytes(mac)));
                    }
                });
            }
            PairingBackend::Remote {
                endpoint,
                certificate,
            } => {
                let _ = Self::remote_call(
                    endpoint,
                    certificate,
                    &PairingBrokerRequest::PutHostRound2(
                        *nonce,
                        reply,
                        PairingMac::from_bytes(mac),
                    ),
                );
            }
        }
    }

    pub fn take_host_round2(&self, nonce: &[u8; 16]) -> Option<(EnrolmentRound2, [u8; 64])> {
        let key = hex::encode(nonce);
        match &self.backend {
            PairingBackend::Memory(inner) => {
                let (reply, mac) = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(&key)?
                    .host_round2
                    .take()?;
                Some((reply, mac.into_bytes()?))
            }
            PairingBackend::Durable(backend) => {
                let mut out = None;
                Self::durable_update(backend, nonce, |state| out = state.host_round2.take())
                    .then_some(())?;
                out.and_then(|(reply, mac)| Some((reply, mac.into_bytes()?)))
            }
            PairingBackend::Remote {
                endpoint,
                certificate,
            } => {
                match Self::remote_call(
                    endpoint,
                    certificate,
                    &PairingBrokerRequest::TakeHostRound2(*nonce),
                )? {
                    PairingBrokerResponse::HostRound2(value) => {
                        value.and_then(|(reply, mac)| Some((reply, mac.into_bytes()?)))
                    }
                    _ => None,
                }
            }
        }
    }
}

/// The QR/invite carries the exact DER certificate, not a CA. Treating that
/// certificate as a root would also trust a child certificate it signs; exact
/// comparison keeps the pin's meaning narrow while the provider still checks
/// the handshake signature against the pinned certificate's public key.
#[derive(Debug)]
struct ExactCertificateVerifier {
    expected: Arc<[u8]>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for ExactCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if intermediates.is_empty() && end_entity.as_ref() == self.expected.as_ref() {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "pairing broker certificate does not match the signed pin".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Expiry preset for the channel invite minted in screen 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteExpiry {
    OneHour,
    OneDay,
    OneWeek,
    Never,
}

impl InviteExpiry {
    pub fn resolve(self, now: SystemTime) -> SystemTime {
        let duration = match self {
            InviteExpiry::OneHour => Duration::from_secs(3600),
            InviteExpiry::OneDay => Duration::from_secs(86400),
            InviteExpiry::OneWeek => Duration::from_secs(604_800),
            // `Never` is encoded as +100 years; downstream verifiers
            // treat that as "no practical expiry".
            InviteExpiry::Never => Duration::from_secs(3_153_600_000),
        };
        // `now` is a real wall-clock time, nowhere near the range limit
        // `SystemTime` can represent, so this can't realistically
        // overflow; `checked_add` with a same-value fallback keeps the
        // function panic-free without changing observable behaviour.
        now.checked_add(duration).unwrap_or(now)
    }

    pub fn label(&self) -> &'static str {
        match self {
            InviteExpiry::OneHour => "1 hour",
            InviteExpiry::OneDay => "24 hours",
            InviteExpiry::OneWeek => "7 days",
            InviteExpiry::Never => "Never",
        }
    }
}

/// Channel-invite envelope encoded into a `qos://invite/<base32>` URL.
///
/// Bearer invite containing only an opaque redemption capability and sealed
/// delegation material. Raw Ed25519 secret bytes never appear in a URL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InviteEnvelope {
    pub channel: ChannelId,
    /// Display only — the validator uses `channel`.
    pub channel_name: String,
    pub owner_pubkey: Ed25519PublicKey,
    pub inviter_label: String,
    pub role: Role,
    /// `u64::MAX` means no practical expiry.
    pub expires_unix: u64,
    pub delegation_pubkey: Ed25519PublicKey,
    /// Random one-use bearer capability. The redemption authority consumes
    /// its digest before releasing the sealed delegation operation.
    #[serde(with = "serde_bytes_codec")]
    pub redemption_capability: Vec<u8>,
    #[serde(default)]
    pub redemption_endpoint: String,
    #[serde(default, with = "serde_bytes_codec")]
    pub redemption_certificate: Vec<u8>,
    pub owner_signature: Signature,
}

impl InviteEnvelope {
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::EncodeFailed`] if the canonical view
    /// can't be postcard-encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, IdentityFlowError> {
        #[derive(Serialize)]
        struct View<'a> {
            channel: &'a ChannelId,
            channel_name: &'a str,
            owner_pubkey: &'a Ed25519PublicKey,
            inviter_label: &'a str,
            role: &'a Role,
            expires_unix: u64,
            delegation_pubkey: &'a Ed25519PublicKey,
            redemption_capability: &'a [u8],
            redemption_endpoint: &'a str,
            redemption_certificate: &'a [u8],
        }
        let view = View {
            channel: &self.channel,
            channel_name: &self.channel_name,
            owner_pubkey: &self.owner_pubkey,
            inviter_label: &self.inviter_label,
            role: &self.role,
            expires_unix: self.expires_unix,
            delegation_pubkey: &self.delegation_pubkey,
            redemption_capability: &self.redemption_capability,
            redemption_endpoint: &self.redemption_endpoint,
            redemption_certificate: &self.redemption_certificate,
        };
        postcard::to_allocvec(&view).map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::SignatureInvalid`] if `owner_signature`
    /// doesn't verify against `owner_pubkey`.
    pub fn verify_signature(&self) -> Result<(), IdentityFlowError> {
        let bytes = self.signing_bytes()?;
        verify_event(&bytes, &self.owner_signature, &self.owner_pubkey)
            .map_err(|e| IdentityFlowError::SignatureInvalid(format!("{e:?}")))?;
        Ok(())
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        let now_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        now_unix >= self.expires_unix
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::EncodeFailed`] if postcard encoding
    /// fails.
    pub fn to_url(&self) -> Result<String, IdentityFlowError> {
        let bytes = postcard::to_allocvec(self)
            .map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))?;
        let encoded = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes);
        Ok(format!("qos://invite/{encoded}"))
    }

    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `url` is missing
    /// the `qos://invite/` prefix, isn't valid base32, or doesn't decode
    /// to a valid envelope.
    pub fn from_url(url: &str) -> Result<Self, IdentityFlowError> {
        let prefix = "qos://invite/";
        let suffix = url.strip_prefix(prefix).ok_or_else(|| {
            IdentityFlowError::DecodeFailed("missing qos://invite/ prefix".into())
        })?;
        if suffix.len() > 16 * 1024 {
            return Err(IdentityFlowError::DecodeFailed(
                "invite capability exceeds 16 KiB".into(),
            ));
        }
        let bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, suffix)
            .ok_or_else(|| IdentityFlowError::DecodeFailed("base32 decode failed".into()))?;
        let envelope: Self = postcard::from_bytes(&bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("postcard: {e}")))?;
        if envelope.redemption_capability.len() != 32
            || envelope.redemption_certificate.len() > 64 * 1024
            || envelope.redemption_endpoint.is_empty() != envelope.redemption_certificate.is_empty()
        {
            return Err(IdentityFlowError::DecodeFailed(
                "invite redemption locator is malformed".into(),
            ));
        }
        if !envelope.redemption_endpoint.is_empty() {
            envelope
                .redemption_endpoint
                .parse::<std::net::SocketAddr>()
                .map_err(|error| {
                    IdentityFlowError::DecodeFailed(format!("invite broker endpoint: {error}"))
                })?;
        }
        #[cfg(not(test))]
        if envelope.redemption_endpoint.is_empty() {
            return Err(IdentityFlowError::DecodeFailed(
                "invite redemption broker is missing".into(),
            ));
        }
        Ok(envelope)
    }
}

pub struct InviteRedemptionAuthority {
    root: PathBuf,
}

#[derive(Clone)]
struct InviteBrokerLocator {
    endpoint: String,
    certificate: Vec<u8>,
    authority_root: PathBuf,
}

static INVITE_BROKER_LOCATOR: std::sync::OnceLock<InviteBrokerLocator> = std::sync::OnceLock::new();

/// Install the shell's long-lived redemption endpoint and private authority
/// location before any invite UI is shown.
///
/// # Errors
///
/// Returns an error for invalid endpoint/certificate data, unsafe authority
/// storage, or an attempt to replace an already configured trust boundary.
pub fn configure_invite_broker(
    endpoint: impl Into<String>,
    certificate: Vec<u8>,
    authority_root: impl Into<PathBuf>,
) -> Result<(), IdentityFlowError> {
    let endpoint = endpoint.into();
    endpoint.parse::<std::net::SocketAddr>().map_err(|error| {
        IdentityFlowError::DecodeFailed(format!("invite broker endpoint: {error}"))
    })?;
    if certificate.is_empty() || certificate.len() > 64 * 1024 {
        return Err(IdentityFlowError::DecodeFailed(
            "invite broker certificate is empty or too large".into(),
        ));
    }
    let authority_root = authority_root.into();
    InviteRedemptionAuthority::at(&authority_root)?;
    INVITE_BROKER_LOCATOR
        .set(InviteBrokerLocator {
            endpoint,
            certificate,
            authority_root,
        })
        .map_err(|_| IdentityFlowError::DecodeFailed("invite broker is already configured".into()))
}

impl InviteRedemptionAuthority {
    /// Open the authority explicitly configured by the trusted shell.
    ///
    /// # Errors
    ///
    /// Returns an error when no production authority is configured or its
    /// private directory cannot be opened safely.
    pub fn open() -> Result<Self, IdentityFlowError> {
        if let Some(locator) = INVITE_BROKER_LOCATOR.get() {
            return Self::at(&locator.authority_root);
        }
        #[cfg(test)]
        {
            let base = std::env::var_os("QOS_IDENTITY_ROOT").map_or_else(
                || std::env::temp_dir().join(format!("qos-ui-test-{}", std::process::id())),
                PathBuf::from,
            );
            Self::at(base.join("invite-redemption-authority"))
        }
        #[cfg(not(test))]
        Err(IdentityFlowError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "invite redemption authority is not configured by the shell",
        )))
    }

    /// Open an isolated authority rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns an error if the root cannot be created as a private,
    /// user-owned real directory.
    pub fn at(root: impl Into<PathBuf>) -> Result<Self, IdentityFlowError> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let metadata = std::fs::symlink_metadata(&root)?;
        if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(IdentityFlowError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "invite authority must be a user-owned real directory",
            )));
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
    }

    fn path(&self, capability: &[u8; 32]) -> PathBuf {
        self.root.join(hex::encode(Sha512::digest(capability)))
    }

    fn issue_lock(&self) -> Result<std::fs::File, IdentityFlowError> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.root.join("issue.lock"))?;
        for _ in 0..250 {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Err(IdentityFlowError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "invite authority is busy",
        )))
    }

    fn master_key(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, IdentityFlowError> {
        use std::io::{Read, Write};
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let path = self.root.join("authority-key");
        let read_existing = || -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, IdentityFlowError> {
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let metadata = file.metadata()?;
            if !metadata.file_type().is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.len() != 32
            {
                return Err(IdentityFlowError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "invite authority key must be a private 32-byte user-owned regular file",
                )));
            }
            let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(32));
            file.take(33).read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        };
        let bytes = if let Some(bytes) = read_existing()? {
            bytes
        } else {
            let mut key = zeroize::Zeroizing::new([0; 32]);
            OsRng.fill_bytes(&mut *key);
            let mut random = [0; 16];
            OsRng.fill_bytes(&mut random);
            let temporary = self
                .root
                .join(format!(".authority-key-{}.tmp", hex::encode(random)));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&*key)?;
            file.sync_all()?;
            let published = std::fs::hard_link(&temporary, &path);
            let _ = std::fs::remove_file(&temporary);
            match published {
                Ok(()) => {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .open(&self.root)?
                        .sync_all()?;
                    zeroize::Zeroizing::new(key.to_vec())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_existing()?
                    .ok_or_else(|| {
                        IdentityFlowError::Io(std::io::Error::other(
                            "published invite authority key disappeared",
                        ))
                    })?,
                Err(error) => return Err(error.into()),
            }
        };
        let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            IdentityFlowError::DecodeFailed("invite authority key is corrupt".into())
        })?;
        Ok(zeroize::Zeroizing::new(key))
    }

    /// Encrypt and durably publish one bounded bearer-capability record.
    ///
    /// # Errors
    ///
    /// Returns an error on unsafe/corrupt storage, encryption failure,
    /// duplicate capability, or when the 256-record live quota is full.
    pub fn issue(
        &self,
        capability: [u8; 32],
        secret: [u8; 32],
        expires_unix: u64,
    ) -> Result<(), IdentityFlowError> {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        // Serialize cleanup, quota accounting, and create-new publication
        // across processes. `File`'s OS lock is released automatically after
        // a crash, unlike a sentinel lockfile.
        let _issue_lock = self.issue_lock()?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |value| value.as_secs());
        for entry in std::fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .take(512)
        {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.len() == 128)
                && let Ok(mut file) = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)
                && let Ok(metadata) = file.metadata()
                && metadata.file_type().is_file()
                && metadata.len() == 80
            {
                let mut header = [0; 8];
                if file.read_exact(&mut header).is_ok() && u64::from_be_bytes(header) <= now {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let entries = std::fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.len() == 128)
            })
            .take(257)
            .count();
        if entries >= 256 {
            return Err(IdentityFlowError::Io(std::io::Error::new(
                std::io::ErrorKind::QuotaExceeded,
                "invite redemption authority is full",
            )));
        }
        let secret = zeroize::Zeroizing::new(secret);
        let key = self.master_key()?;
        let cipher = chacha20poly1305::XChaCha20Poly1305::new((&*key).into());
        let mut nonce = [0; 24];
        OsRng.fill_bytes(&mut nonce);
        let digest = Sha512::digest(capability);
        let mut aad = Vec::with_capacity(72);
        aad.extend_from_slice(&digest);
        aad.extend_from_slice(&expires_unix.to_be_bytes());
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &*secret,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                IdentityFlowError::SigningFailed("invite authority encryption failed".into())
            })?;
        let record_capacity = 32usize.checked_add(ciphertext.len()).ok_or_else(|| {
            IdentityFlowError::EncodeFailed("invite authority record is too large".into())
        })?;
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(record_capacity));
        bytes.extend_from_slice(&expires_unix.to_be_bytes());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path(&capability))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::OpenOptions::new()
            .read(true)
            .open(&self.root)?
            .sync_all()?;
        Ok(())
    }

    /// Atomically and durably consume a capability before releasing its secret.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown, replayed, expired, unsafe, corrupt, or
    /// unauthentic records and for durable-storage failures.
    pub fn redeem(
        &self,
        capability: &[u8],
        now_unix: u64,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, IdentityFlowError> {
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let capability: [u8; 32] = capability.try_into().map_err(|_| {
            IdentityFlowError::DecodeFailed("redemption capability must be 32 bytes".into())
        })?;
        let path = self.path(&capability);
        let claimed = self.root.join(format!(
            ".claimed-{}-{}",
            std::process::id(),
            hex::encode(&capability[..8])
        ));
        std::fs::rename(&path, &claimed).map_err(|_| {
            IdentityFlowError::DecodeFailed(
                "invite is expired, revoked, unknown, or already redeemed".into(),
            )
        })?;
        // Durably consume the capability before decrypting or returning the
        // secret. After power loss, replay must remain impossible even if the
        // process died between release and cleanup.
        std::fs::OpenOptions::new()
            .read(true)
            .open(&self.root)?
            .sync_all()?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&claimed)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() != 80
        {
            let _ = std::fs::remove_file(&claimed);
            return Err(IdentityFlowError::DecodeFailed(
                "redemption record is unsafe or corrupt".into(),
            ));
        }
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(80));
        file.take(81).read_to_end(&mut bytes)?;
        let _ = std::fs::remove_file(&claimed);
        if bytes.len() != 80 {
            return Err(IdentityFlowError::DecodeFailed(
                "redemption record is corrupt".into(),
            ));
        }
        let expiry = u64::from_be_bytes(
            bytes
                .get(..8)
                .ok_or_else(|| {
                    IdentityFlowError::DecodeFailed("redemption record is corrupt".into())
                })?
                .try_into()
                .map_err(|_| {
                    IdentityFlowError::DecodeFailed("redemption record is corrupt".into())
                })?,
        );
        if expiry <= now_unix {
            return Err(IdentityFlowError::InviteExpired);
        }
        let key = self.master_key()?;
        let cipher = chacha20poly1305::XChaCha20Poly1305::new((&*key).into());
        let digest = Sha512::digest(capability);
        let mut aad = Vec::with_capacity(72);
        aad.extend_from_slice(&digest);
        aad.extend_from_slice(&expiry.to_be_bytes());
        let plaintext = cipher
            .decrypt(
                bytes
                    .get(8..32)
                    .ok_or_else(|| {
                        IdentityFlowError::DecodeFailed("redemption record is corrupt".into())
                    })?
                    .into(),
                Payload {
                    msg: bytes.get(32..).ok_or_else(|| {
                        IdentityFlowError::DecodeFailed("redemption record is corrupt".into())
                    })?,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                IdentityFlowError::DecodeFailed("redemption record authentication failed".into())
            })?;
        Ok(zeroize::Zeroizing::new(plaintext))
    }
}

#[cfg(test)]
fn invite_redemption_authority() -> Result<InviteRedemptionAuthority, IdentityFlowError> {
    InviteRedemptionAuthority::open()
}

/// Generates the one-time delegation keypair internally; signs the
/// canonical envelope view with the owner's key.
///
/// # Errors
///
/// Returns [`IdentityFlowError::EncodeFailed`] if the envelope can't be
/// encoded, or [`IdentityFlowError::SigningFailed`] if `owner_signing_key`
/// fails to sign it.
pub fn mint_invite(
    channel: ChannelId,
    channel_name: impl Into<String>,
    owner_pubkey: Ed25519PublicKey,
    owner_signing_key: &DalekSigningKey,
    inviter_label: impl Into<String>,
    role: Role,
    expires_at: SystemTime,
) -> Result<InviteEnvelope, IdentityFlowError> {
    let channel_name = channel_name.into();
    let inviter_label = inviter_label.into();
    if let Some(locator) = INVITE_BROKER_LOCATOR.get() {
        let authority = InviteRedemptionAuthority::at(&locator.authority_root)?;
        return mint_invite_at(
            channel,
            channel_name,
            owner_pubkey,
            owner_signing_key,
            inviter_label,
            role,
            expires_at,
            locator.endpoint.clone(),
            locator.certificate.clone(),
            &authority,
        );
    }
    #[cfg(test)]
    {
        let authority = invite_redemption_authority()?;
        mint_invite_at(
            channel,
            channel_name,
            owner_pubkey,
            owner_signing_key,
            inviter_label,
            role,
            expires_at,
            String::new(),
            Vec::new(),
            &authority,
        )
    }
    #[cfg(not(test))]
    Err(IdentityFlowError::DecodeFailed(
        "invite redemption broker unavailable".into(),
    ))
}

/// Explicitly mint for a shell-owned broker. This is also the integration-test
/// seam: tests can use an isolated authority without mutating the process-wide
/// production locator.
///
/// # Errors
///
/// Returns an error for an invalid endpoint/certificate, unsafe authority
/// storage, encryption/signing failure, or an exhausted authority quota.
#[allow(clippy::too_many_arguments)]
pub fn mint_invite_for_broker(
    channel: ChannelId,
    channel_name: impl Into<String>,
    owner_pubkey: Ed25519PublicKey,
    owner_signing_key: &DalekSigningKey,
    inviter_label: impl Into<String>,
    role: Role,
    expires_at: SystemTime,
    redemption_endpoint: impl Into<String>,
    redemption_certificate: Vec<u8>,
    authority_root: impl Into<PathBuf>,
) -> Result<InviteEnvelope, IdentityFlowError> {
    let redemption_endpoint = redemption_endpoint.into();
    redemption_endpoint
        .parse::<std::net::SocketAddr>()
        .map_err(|error| {
            IdentityFlowError::DecodeFailed(format!("invite broker endpoint: {error}"))
        })?;
    if redemption_certificate.is_empty() || redemption_certificate.len() > 64 * 1024 {
        return Err(IdentityFlowError::DecodeFailed(
            "invite broker certificate is empty or too large".into(),
        ));
    }
    let authority = InviteRedemptionAuthority::at(authority_root)?;
    mint_invite_at(
        channel,
        channel_name.into(),
        owner_pubkey,
        owner_signing_key,
        inviter_label.into(),
        role,
        expires_at,
        redemption_endpoint,
        redemption_certificate,
        &authority,
    )
}

#[allow(clippy::too_many_arguments)]
fn mint_invite_at(
    channel: ChannelId,
    channel_name: String,
    owner_pubkey: Ed25519PublicKey,
    owner_signing_key: &DalekSigningKey,
    inviter_label: String,
    role: Role,
    expires_at: SystemTime,
    redemption_endpoint: String,
    redemption_certificate: Vec<u8>,
    authority: &InviteRedemptionAuthority,
) -> Result<InviteEnvelope, IdentityFlowError> {
    let (delegation_secret_full, delegation_pubkey) = generate_keypair();
    let mut capability = [0u8; 32];
    OsRng.fill_bytes(&mut capability);
    let expires_unix = expires_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs());
    let mut env = InviteEnvelope {
        channel,
        channel_name,
        owner_pubkey,
        inviter_label,
        role,
        expires_unix,
        delegation_pubkey,
        redemption_capability: capability.to_vec(),
        redemption_endpoint,
        redemption_certificate,
        owner_signature: Signature::from_bytes([0; 64]),
    };
    let bytes = env.signing_bytes()?;
    let sig = sign_event(&bytes, owner_signing_key)
        .map_err(|e| IdentityFlowError::SigningFailed(format!("{e:?}")))?;
    env.owner_signature = sig;
    authority.issue(capability, delegation_secret_full.to_bytes(), expires_unix)?;
    Ok(env)
}

/// Builds an `AttestationIssued` event signed by the embedded
/// delegation key (which the owner has pre-signed an authorisation for).
///
/// # Errors
///
/// Returns [`IdentityFlowError::SignatureInvalid`] if the envelope's
/// owner signature doesn't verify, [`IdentityFlowError::InviteExpired`]
/// if it has expired, [`IdentityFlowError::DecodeFailed`] if the embedded
/// delegation secret isn't 32 bytes, or
/// [`IdentityFlowError::SigningFailed`] if the delegation key fails to
/// sign the attestation.
pub fn build_attestation_from_invite(
    envelope: &InviteEnvelope,
    invitee_pubkey: Ed25519PublicKey,
    now: SystemTime,
) -> Result<ChannelEvent, IdentityFlowError> {
    envelope.verify_signature()?;
    if envelope.is_expired(now) {
        return Err(IdentityFlowError::InviteExpired);
    }
    #[cfg(test)]
    let now_unix = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let secret_plain = if envelope.redemption_endpoint.is_empty() {
        #[cfg(not(test))]
        return Err(IdentityFlowError::DecodeFailed(
            "invite redemption broker unavailable".into(),
        ));
        #[cfg(test)]
        invite_redemption_authority()?.redeem(&envelope.redemption_capability, now_unix)?
    } else {
        let response = PairingHandshake::remote_call(
            &envelope.redemption_endpoint,
            &envelope.redemption_certificate,
            &PairingBrokerRequest::RedeemInvite(envelope.redemption_capability.clone()),
        )
        .ok_or_else(|| IdentityFlowError::DecodeFailed("invite broker unavailable".into()))?;
        let PairingBrokerResponse::InviteSecret(Some(secret)) = response else {
            return Err(IdentityFlowError::DecodeFailed(
                "invite is expired, revoked, unknown, or already redeemed".into(),
            ));
        };
        zeroize::Zeroizing::new(secret)
    };
    let secret: [u8; 32] = secret_plain.as_slice().try_into().map_err(|_| {
        IdentityFlowError::DecodeFailed("sealed delegation secret must be 32 bytes".into())
    })?;
    let delegation_sk = DalekSigningKey::from_bytes(&secret);
    // `now` is a real wall-clock time, nowhere near the range limit
    // `SystemTime` can represent, so this can't realistically overflow;
    // `checked_add` with a same-value fallback keeps the function
    // panic-free without changing observable behaviour.
    let valid_until = now
        .checked_add(Duration::from_secs(31_536_000))
        .unwrap_or(now);
    let mut attestation = Attestation {
        id: AttestationId::new(),
        issued_by: envelope.delegation_pubkey,
        subject: invitee_pubkey,
        claims: vec![Claim::ClearedAt(Clearance::PUBLIC)],
        valid_from: now,
        valid_until,
        signature: Signature::from_bytes([0; 64]),
    };
    let bytes = attestation
        .signing_bytes()
        .map_err(|e| IdentityFlowError::EncodeFailed(format!("attestation: {e}")))?;
    let sig = sign_event(&bytes, &delegation_sk)
        .map_err(|e| IdentityFlowError::SigningFailed(format!("{e:?}")))?;
    attestation.signature = sig;
    let ra = RoleAttestation {
        channel: envelope.channel,
        role: envelope.role,
        attestation,
    };
    Ok(ChannelEvent::AttestationIssued(ra))
}

/// Recovery-verify challenge: pick three random word positions from a
/// 24-word mnemonic and store the expected answers.
#[derive(Clone, Debug)]
pub struct RecoveryChallenge {
    /// 1-indexed positions the user must type back.
    pub positions: [usize; 3],
    expected: [String; 3],
}

/// Looks up the (1-indexed) mnemonic word at `position`, without a raw
/// index expression: guards the underflow on `position - 1` and the
/// out-of-range lookup as two explicit `Result` paths instead.
fn word_at(words: &[&str], position: usize) -> Result<String, IdentityFlowError> {
    let idx = position.checked_sub(1).ok_or_else(|| {
        IdentityFlowError::DecodeFailed(format!("recovery position must be >= 1, got {position}"))
    })?;
    words.get(idx).map(|w| (*w).to_string()).ok_or_else(|| {
        IdentityFlowError::DecodeFailed(format!("recovery position {position} out of range"))
    })
}

impl RecoveryChallenge {
    /// Picks three distinct positions in 1..=24.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `mnemonic` isn't
    /// exactly 24 words.
    pub fn from_mnemonic(mnemonic: &str) -> Result<Self, IdentityFlowError> {
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        if words.len() != 24 {
            return Err(IdentityFlowError::DecodeFailed(format!(
                "mnemonic must be exactly 24 words, got {}",
                words.len()
            )));
        }
        // Fisher–Yates directly on RngCore — avoids rand 0.8's SliceRandom.
        let mut positions: Vec<usize> = (1..=24).collect();
        let mut rng = OsRng;
        for i in (1..positions.len()).rev() {
            let j = (rng.next_u32() as usize)
                .checked_rem(i.saturating_add(1))
                .unwrap_or(0);
            positions.swap(i, j);
        }
        let picked: [usize; 3] = match positions.get(0..3) {
            Some([a, b, c]) => [*a, *b, *c],
            _ => {
                return Err(IdentityFlowError::DecodeFailed(
                    "failed to select recovery positions".to_string(),
                ));
            }
        };
        let expected = [
            word_at(&words, picked[0])?,
            word_at(&words, picked[1])?,
            word_at(&words, picked[2])?,
        ];
        Ok(Self {
            positions: picked,
            expected,
        })
    }

    /// Deterministic constructor for tests.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if `mnemonic` isn't
    /// exactly 24 words, or if any of `positions` is out of range.
    pub fn at_positions(mnemonic: &str, positions: [usize; 3]) -> Result<Self, IdentityFlowError> {
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        if words.len() != 24 {
            return Err(IdentityFlowError::DecodeFailed(format!(
                "mnemonic must be exactly 24 words, got {}",
                words.len()
            )));
        }
        let expected = [
            word_at(&words, positions[0])?,
            word_at(&words, positions[1])?,
            word_at(&words, positions[2])?,
        ];
        Ok(Self {
            positions,
            expected,
        })
    }

    /// Drives the verify screen's correct-word border colour.
    pub fn expected_word(&self, slot: usize) -> String {
        self.expected.get(slot).cloned().unwrap_or_default()
    }

    pub fn check(&self, answers: &[&str; 3]) -> bool {
        self.expected
            .iter()
            .zip(answers.iter())
            .all(|(expected, answer)| answer.trim().eq_ignore_ascii_case(expected))
    }
}

/// Single-line text edit state shared by the identity-flow inline
/// inputs; screens render the caret at `cursor` and any
/// `selection_range()`.
#[derive(Clone, Debug, Default)]
pub struct LineEditState {
    pub text: String,
    /// Byte index into `text`. Always on a char boundary.
    pub cursor: usize,
    /// Anchor end of the selection. `None` when nothing is selected.
    pub selection_anchor: Option<usize>,
}

impl LineEditState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            selection_anchor: None,
        }
    }

    /// `(lo, hi)` byte-index pair, both on char boundaries.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
        }
    }

    pub fn selected_text(&self) -> &str {
        match self.selection_range() {
            // `selection_range` always returns char-boundary offsets, so
            // `get` succeeds; the fallback only guards against that
            // invariant somehow not holding.
            Some((lo, hi)) => self.text.get(lo..hi).unwrap_or(""),
            None => "",
        }
    }

    /// Clamped to bounds and snapped to a char boundary; `extend`
    /// keeps the anchor.
    pub fn move_cursor(&mut self, pos: usize, extend: bool) {
        let mut pos = pos.min(self.text.len());
        while pos > 0 && !self.text.is_char_boundary(pos) {
            pos = pos.saturating_sub(1);
        }
        if extend {
            self.selection_anchor.get_or_insert(self.cursor);
        } else {
            self.selection_anchor = None;
        }
        self.cursor = pos;
    }

    /// Returns `true` if anything was removed.
    pub fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection_range() else {
            return false;
        };
        self.text.replace_range(lo..hi, "");
        self.cursor = lo;
        self.selection_anchor = None;
        true
    }

    pub fn insert_str(&mut self, s: &str) {
        let _ = self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor = self.cursor.saturating_add(s.len());
        self.selection_anchor = None;
    }

    fn prev_char_boundary(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut p = self.cursor.saturating_sub(1);
        while p > 0 && !self.text.is_char_boundary(p) {
            p = p.saturating_sub(1);
        }
        p
    }

    fn next_char_boundary(&self) -> usize {
        if self.cursor >= self.text.len() {
            return self.text.len();
        }
        let mut p = self.cursor.saturating_add(1);
        while p < self.text.len() && !self.text.is_char_boundary(p) {
            p = p.saturating_add(1);
        }
        p
    }

    /// The usual editor definition: scans left to where non-alphanumeric
    /// meets alphanumeric.
    fn prev_word_boundary(&self) -> usize {
        let bytes = self.text.as_bytes();
        let is_alnum_before = |p: usize| {
            bytes
                .get(p.saturating_sub(1))
                .is_some_and(u8::is_ascii_alphanumeric)
        };
        let mut p = self.cursor;
        while p > 0 && !is_alnum_before(p) {
            p = p.saturating_sub(1);
        }
        while p > 0 && is_alnum_before(p) {
            p = p.saturating_sub(1);
        }
        p
    }

    fn next_word_boundary(&self) -> usize {
        let bytes = self.text.as_bytes();
        let len = bytes.len();
        let is_alnum_at = |p: usize| bytes.get(p).is_some_and(u8::is_ascii_alphanumeric);
        let mut p = self.cursor;
        while p < len && is_alnum_at(p) {
            p = p.saturating_add(1);
        }
        while p < len && !is_alnum_at(p) {
            p = p.saturating_add(1);
        }
        p
    }

    /// `text_input` is `KeyEvent::text` — the OS's printable text for
    /// this key, if any.
    pub fn apply_key(
        &mut self,
        key: &winit::keyboard::Key,
        modifiers: &winit::event::Modifiers,
        text_input: Option<&str>,
    ) -> bool {
        use winit::event::Modifiers;
        use winit::keyboard::{Key, NamedKey};
        let ctrl = modifiers.state().control_key();
        let shift = modifiers.state().shift_key();
        let _ = Modifiers::default(); // silence unused-import lint in some compositions

        match key {
            Key::Named(NamedKey::ArrowLeft) => {
                let target = if ctrl {
                    self.prev_word_boundary()
                } else {
                    self.prev_char_boundary()
                };
                self.move_cursor(target, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let target = if ctrl {
                    self.next_word_boundary()
                } else {
                    self.next_char_boundary()
                };
                self.move_cursor(target, shift);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.move_cursor(0, shift);
                true
            }
            Key::Named(NamedKey::End) => {
                self.move_cursor(self.text.len(), shift);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if self.delete_selection() {
                    return true;
                }
                if self.cursor == 0 {
                    return false;
                }
                let target = if ctrl {
                    self.prev_word_boundary()
                } else {
                    self.prev_char_boundary()
                };
                self.text.replace_range(target..self.cursor, "");
                self.cursor = target;
                true
            }
            Key::Named(NamedKey::Delete) => {
                if self.delete_selection() {
                    return true;
                }
                if self.cursor >= self.text.len() {
                    return false;
                }
                let target = if ctrl {
                    self.next_word_boundary()
                } else {
                    self.next_char_boundary()
                };
                self.text.replace_range(self.cursor..target, "");
                true
            }
            Key::Character(s) if ctrl => self.apply_ctrl_shortcut(s),
            _ => {
                // Skip control codes: they arrive as zero-width chars on some platforms.
                if let Some(t) = text_input {
                    let cleaned: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !cleaned.is_empty() {
                        self.insert_str(&cleaned);
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Handles Ctrl+A/C/X/V; `s` is the character text from the key
    /// event. Returns `true` if the shortcut was recognised and applied.
    fn apply_ctrl_shortcut(&mut self, s: &str) -> bool {
        match s.to_lowercase().as_str() {
            "a" => {
                self.selection_anchor = Some(0);
                self.cursor = self.text.len();
                true
            }
            "c" => {
                let sel = self.selected_text();
                let bytes = if sel.is_empty() {
                    self.text.clone()
                } else {
                    sel.to_string()
                };
                crate::text_selection::clipboard_copy(&bytes);
                true
            }
            "x" => {
                if self.selection_range().is_some() {
                    let sel = self.selected_text().to_string();
                    crate::text_selection::clipboard_copy(&sel);
                    self.delete_selection();
                    true
                } else {
                    false
                }
            }
            "v" => {
                if let Some(pasted) = crate::text_selection::clipboard_paste() {
                    let cleaned: String = pasted.chars().filter(|c| !c.is_control()).collect();
                    self.insert_str(&cleaned);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Terminal "Done" success card; falls back to a generic message if
/// the store has no record.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the `transform * Affine::translate(..)` calls below compose 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
)]
#[allow(
    clippy::similar_names,
    reason = "cx/cy is the standard center-x/center-y coordinate naming convention; distinguishing them further would reduce clarity"
)]
fn draw_done_card(
    store: &dyn IdentityStore,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
) {
    use vello::kurbo::{RoundedRect, Stroke};
    use vello::peniko::{Color, Fill};
    let backdrop = Color::from_rgba8(0, 0, 0, 180);
    scene.fill(Fill::NonZero, transform, backdrop, None, &viewport);
    let card_w = 480.0;
    let card_h = 280.0;
    let card_x = viewport.x0 + (viewport.width() - card_w) / 2.0;
    let card_y = viewport.y0 + (viewport.height() - card_h) / 2.0;
    let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
    let radius = 12.0;
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &RoundedRect::from_rect(card, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &RoundedRect::from_rect(card, radius),
    );
    let pad = 32.0;
    let (display_name, avatar) = match store.load() {
        Ok(Some(rec)) => (rec.display_name.clone(), rec.avatar_color),
        _ => ("Your identity".to_string(), [120, 120, 200]),
    };
    let avatar_d = 56.0;
    let avatar_cx = card.x0 + pad + avatar_d / 2.0;
    let avatar_cy = card.y0 + pad + avatar_d / 2.0;
    let avatar_color = Color::from_rgb8(avatar[0], avatar[1], avatar[2]);
    scene.fill(
        Fill::NonZero,
        transform,
        avatar_color,
        None,
        &vello::kurbo::Circle::new((avatar_cx, avatar_cy), avatar_d / 2.0),
    );
    let initial: String = display_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let (iw, _) = text_renderer.measure(&initial, 28.0, None);
    text_renderer.draw_text(
        scene,
        &initial,
        28.0,
        Color::from_rgb8(255, 255, 255),
        transform * Affine::translate((avatar_cx - f64::from(iw) / 2.0, avatar_cy - 16.0)),
        None,
    );
    text_renderer.draw_text(
        scene,
        "You're all set",
        22.0,
        theme.tokens.colors.text_primary,
        transform * Affine::translate((card.x0 + pad + avatar_d + 16.0, card.y0 + pad + 4.0)),
        None,
    );
    text_renderer.draw_text(
        scene,
        &format!("Signed in as {display_name}"),
        14.0,
        theme.tokens.colors.text_dim,
        transform * Affine::translate((card.x0 + pad + avatar_d + 16.0, card.y0 + pad + 32.0)),
        None,
    );
    text_renderer.draw_text(
        scene,
        "Your identity is saved. You can close this window.",
        13.0,
        theme.tokens.colors.text_dim,
        transform * Affine::translate((card.x0 + pad, card.y0 + pad + avatar_d + 32.0)),
        None,
    );
    text_renderer.draw_text(
        scene,
        "Start the desktop from a terminal to begin collaborating.",
        13.0,
        theme.tokens.colors.text_dim,
        transform * Affine::translate((card.x0 + pad, card.y0 + pad + avatar_d + 56.0)),
        None,
    );
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityFlowError {
    #[error("identity-flow encoding failed: {0}")]
    EncodeFailed(String),
    #[error("identity-flow decoding failed: {0}")]
    DecodeFailed(String),
    #[error("unknown disk schema version: {0}")]
    UnknownSchemaVersion(u32),
    #[error("identity record's secret does not derive its recorded public key")]
    PublicKeyMismatch,
    #[error("signing failed: {0}")]
    SigningFailed(String),
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),
    #[error("invite expired")]
    InviteExpired,
    #[error("pairing MAC mismatch")]
    PairingMacInvalid,
    #[error("threshold identity engine: {0}")]
    Threshold(#[from] ThresholdError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Identifier for the current screen in the ceremony.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityFlowScreen {
    /// Screen 1.
    CreateIdentity,
    /// Screen 2.
    RecoveryShare,
    /// Screen 2b.
    RecoveryVerify,
    /// Screen 3.
    AddDeviceHost,
    /// Screen 4.
    PairDevice,
    /// Screen 5.
    InviteMint,
    /// Screen 6.
    InviteAccept,
    /// Terminal — the host should transition to the desktop.
    Done,
}

/// Outcome the screen reports back to the container; drives the
/// state-machine transition.
#[derive(Clone, Debug)]
pub enum ScreenOutcome {
    Advance,
    /// Cancel — back to the previous screen, or close the flow at the
    /// start.
    Back,
    Stay,
    /// Branch to a specific screen (e.g. invite-accept jumping to
    /// Done on decline).
    JumpTo(IdentityFlowScreen),
}

/// The shell instantiates one of these on boot when no identity
/// exists; the example bin instantiates one programmatically with
/// `set_screen` to inspect any screen in isolation.
pub struct IdentityFlow {
    current: IdentityFlowScreen,
    /// All seven screens are pre-built and toggled via `current`, so
    /// per-screen state (text-input contents, etc.) survives navigation.
    pub create_identity: Box<create_identity::CreateIdentityScreen>,
    pub recovery_share: Box<recovery_share::RecoveryShareScreen>,
    pub recovery_verify: Box<recovery_verify::RecoveryVerifyScreen>,
    pub add_device_host: Box<add_device_host::AddDeviceHostScreen>,
    pub pair_device: Box<pair_device::PairDeviceScreen>,
    pub invite_mint: Box<invite_mint::InviteMintScreen>,
    pub invite_accept: Box<invite_accept::InviteAcceptScreen>,
    /// Persistence backend the create screen writes to on submit.
    store: Arc<dyn IdentityStore>,
    rect: Rect,
}

impl IdentityFlow {
    pub fn new(store: Arc<dyn IdentityStore>) -> Self {
        Self {
            current: IdentityFlowScreen::CreateIdentity,
            create_identity: Box::new(create_identity::CreateIdentityScreen::new()),
            recovery_share: Box::new(recovery_share::RecoveryShareScreen::new()),
            recovery_verify: Box::new(recovery_verify::RecoveryVerifyScreen::new()),
            add_device_host: Box::new(add_device_host::AddDeviceHostScreen::new()),
            pair_device: Box::new(pair_device::PairDeviceScreen::new()),
            invite_mint: Box::new(invite_mint::InviteMintScreen::new()),
            invite_accept: Box::new(invite_accept::InviteAcceptScreen::new()),
            store,
            rect: Rect::ZERO,
        }
    }

    pub fn current_screen(&self) -> IdentityFlowScreen {
        self.current
    }

    /// Used by the example bin and tests to jump straight to a screen.
    pub fn set_screen(&mut self, screen: IdentityFlowScreen) {
        self.current = screen;
    }

    pub fn advance(&mut self, outcome: ScreenOutcome) {
        let next = match (self.current, outcome) {
            (_, ScreenOutcome::Stay) => self.current,
            (_, ScreenOutcome::JumpTo(s)) => s,
            (IdentityFlowScreen::CreateIdentity, ScreenOutcome::Advance)
            | (IdentityFlowScreen::RecoveryVerify, ScreenOutcome::Back) => {
                IdentityFlowScreen::RecoveryShare
            }
            (IdentityFlowScreen::RecoveryShare, ScreenOutcome::Advance) => {
                IdentityFlowScreen::RecoveryVerify
            }
            (IdentityFlowScreen::RecoveryShare, ScreenOutcome::Back) => {
                IdentityFlowScreen::CreateIdentity
            }
            (IdentityFlowScreen::RecoveryVerify, ScreenOutcome::Advance)
            | (
                IdentityFlowScreen::AddDeviceHost
                | IdentityFlowScreen::PairDevice
                | IdentityFlowScreen::InviteMint
                | IdentityFlowScreen::InviteAccept,
                ScreenOutcome::Advance | ScreenOutcome::Back,
            )
            | (IdentityFlowScreen::Done, _) => IdentityFlowScreen::Done,
            (other, _) => other,
        };
        self.current = next;
    }

    /// True once the host should swap to the desktop.
    pub fn is_done(&self) -> bool {
        matches!(self.current, IdentityFlowScreen::Done)
    }

    pub fn store(&self) -> &Arc<dyn IdentityStore> {
        &self.store
    }
}

impl Widget for IdentityFlow {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        match self.current {
            IdentityFlowScreen::CreateIdentity => self.create_identity.sync(state, path),
            IdentityFlowScreen::RecoveryShare => self.recovery_share.sync(state, path),
            IdentityFlowScreen::RecoveryVerify => self.recovery_verify.sync(state, path),
            IdentityFlowScreen::AddDeviceHost => self.add_device_host.sync(state, path),
            IdentityFlowScreen::PairDevice => self.pair_device.sync(state, path),
            IdentityFlowScreen::InviteMint => self.invite_mint.sync(state, path),
            IdentityFlowScreen::InviteAccept => self.invite_accept.sync(state, path),
            IdentityFlowScreen::Done => {}
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        match self.current {
            IdentityFlowScreen::CreateIdentity => {
                self.create_identity.build_layout(taffy, theme, node_map)
            }
            IdentityFlowScreen::RecoveryShare => {
                self.recovery_share.build_layout(taffy, theme, node_map)
            }
            IdentityFlowScreen::RecoveryVerify => {
                self.recovery_verify.build_layout(taffy, theme, node_map)
            }
            IdentityFlowScreen::AddDeviceHost => {
                self.add_device_host.build_layout(taffy, theme, node_map)
            }
            IdentityFlowScreen::PairDevice => self.pair_device.build_layout(taffy, theme, node_map),
            IdentityFlowScreen::InviteMint => self.invite_mint.build_layout(taffy, theme, node_map),
            IdentityFlowScreen::InviteAccept => {
                self.invite_accept.build_layout(taffy, theme, node_map)
            }
            IdentityFlowScreen::Done => taffy.leaf_node(Style::default()),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        match self.current {
            IdentityFlowScreen::CreateIdentity => {
                self.create_identity.apply_layout(taffy, node, origin);
            }
            IdentityFlowScreen::RecoveryShare => {
                self.recovery_share.apply_layout(taffy, node, origin);
            }
            IdentityFlowScreen::RecoveryVerify => {
                self.recovery_verify.apply_layout(taffy, node, origin);
            }
            IdentityFlowScreen::AddDeviceHost => {
                self.add_device_host.apply_layout(taffy, node, origin);
            }
            IdentityFlowScreen::PairDevice => self.pair_device.apply_layout(taffy, node, origin),
            IdentityFlowScreen::InviteMint => self.invite_mint.apply_layout(taffy, node, origin),
            IdentityFlowScreen::InviteAccept => {
                self.invite_accept.apply_layout(taffy, node, origin);
            }
            IdentityFlowScreen::Done => {
                // No inner screen; self.rect already set above.
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        match self.current {
            IdentityFlowScreen::CreateIdentity => {
                self.create_identity
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::RecoveryShare => {
                self.recovery_share
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::RecoveryVerify => {
                self.recovery_verify
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::AddDeviceHost => {
                self.add_device_host
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::PairDevice => {
                self.pair_device
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::InviteMint => {
                self.invite_mint
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::InviteAccept => {
                self.invite_accept
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::Done => {
                draw_done_card(
                    self.store.as_ref(),
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    self.rect,
                );
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let outcome = match self.current {
            IdentityFlowScreen::CreateIdentity => self.create_identity.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
                &*self.store,
            ),
            IdentityFlowScreen::RecoveryShare => self.recovery_share.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::RecoveryVerify => self.recovery_verify.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::AddDeviceHost => self.add_device_host.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::PairDevice => self.pair_device.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::InviteMint => self.invite_mint.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::InviteAccept => self.invite_accept.handle_event_screen(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            ),
            IdentityFlowScreen::Done => ScreenOutcome::Stay,
        };
        // Seed the recovery-share screen's mnemonic display from the new record.
        if matches!(self.current, IdentityFlowScreen::CreateIdentity)
            && matches!(outcome, ScreenOutcome::Advance)
            && let Some(record) = self.create_identity.last_record()
        {
            self.recovery_share.seed_from_record(record);
            self.recovery_verify
                .seed_mnemonic(self.recovery_share.mnemonic().to_string());
        }
        self.advance(outcome);
    }

    fn name(&self) -> &'static str {
        "IdentityFlow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_mnemonic() -> String {
        // 24 distinct BIP39 words for deterministic verification.
        "ability abandon able about above absent absorb abstract absurd \
         abuse access accident account accuse achieve acid acoustic acquire \
         across act action actor actress actual"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn pairing_code_derivation_is_deterministic() {
        let nonce = [0x42u8; 16];
        let a = derive_pairing_code(&nonce);
        let b = derive_pairing_code(&nonce);
        assert_eq!(a, b);
        assert!(a < 1_000_000);
    }

    #[test]
    fn pairing_code_differs_per_nonce() {
        let a = derive_pairing_code(&[1u8; 16]);
        let b = derive_pairing_code(&[2u8; 16]);
        assert_ne!(a, b, "different nonce should produce different code");
    }

    #[test]
    fn pairing_code_format_pads_to_6_digits() {
        assert_eq!(format_pairing_code(0), "000000");
        assert_eq!(format_pairing_code(42), "000042");
        assert_eq!(format_pairing_code(999_999), "999999");
    }

    #[test]
    fn pairing_mac_round_trip() {
        let key = derive_pairing_mac_key(&[7u8; 16]);
        let payload = b"some round-2 reply bytes";
        let tag = compute_pairing_mac(&key, payload);
        assert!(verify_pairing_mac(&key, payload, &tag));
    }

    #[test]
    fn pairing_mac_rejects_tamper() {
        let key = derive_pairing_mac_key(&[7u8; 16]);
        let payload = b"some round-2 reply bytes";
        let mut tag = compute_pairing_mac(&key, payload);
        tag[0] ^= 1;
        assert!(!verify_pairing_mac(&key, payload, &tag));
    }

    #[test]
    fn pairing_payload_round_trip() {
        let nonce = [9u8; 16];
        let mut rng = OsRng;
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (_session, round1) = id.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let payload = PairingPayload::new(nonce, id.group_public_key(), &round1).unwrap();
        let bytes = payload.to_json_bytes().unwrap();
        let back = PairingPayload::from_json_bytes(&bytes).unwrap();
        assert_eq!(payload, back);
        assert_eq!(back.nonce().unwrap(), nonce);
        assert_eq!(
            back.initiator_public_key().unwrap().as_bytes(),
            id.group_public_key().as_bytes()
        );
        // Round1 packets aren't `PartialEq` — round-trip via bytes.
        let r1 = back.initiator_round1().unwrap();
        assert_eq!(r1.from_share_index, 1);
        assert_eq!(r1.package_bytes, round1.package_bytes);
    }

    #[test]
    fn invite_envelope_round_trip_via_url() {
        let (sk, pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        let url = env.to_url().unwrap();
        assert!(url.starts_with("qos://invite/"));
        let back = InviteEnvelope::from_url(&url).unwrap();
        assert_eq!(env, back);
        back.verify_signature()
            .expect("owner signature must verify");
    }

    #[test]
    fn invite_signature_detects_tamper() {
        let (sk, pk) = generate_keypair();
        let mut env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        env.channel_name = "treasury".into();
        assert!(
            env.verify_signature().is_err(),
            "renamed envelope must fail"
        );

        let mut endpoint_tampered = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        endpoint_tampered.redemption_endpoint = "127.0.0.1:9".into();
        assert!(endpoint_tampered.verify_signature().is_err());
        endpoint_tampered.redemption_endpoint.clear();
        endpoint_tampered.redemption_certificate.push(1);
        assert!(endpoint_tampered.verify_signature().is_err());
    }

    #[test]
    fn invite_expiry_rejected_by_attestation_builder() {
        let (sk, pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap();
        let (_inv_sk, inv_pk) = generate_keypair();
        let err = build_attestation_from_invite(
            &env,
            inv_pk,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
        )
        .unwrap_err();
        assert!(matches!(err, IdentityFlowError::InviteExpired));
    }

    #[test]
    #[allow(
        clippy::similar_names,
        reason = "owner_sk/owner_pk and invitee_sk/invitee_pk are the clearest names for a test exercising both sides of an invite; the sk/pk suffixes are a standard convention, not a typo risk"
    )]
    fn invite_accept_produces_valid_attestation() {
        let (owner_sk, owner_pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            owner_pk,
            &owner_sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        let (_invitee_sk, invitee_pk) = generate_keypair();
        let event = build_attestation_from_invite(&env, invitee_pk, SystemTime::now()).unwrap();
        if let ChannelEvent::AttestationIssued(ra) = &event {
            assert_eq!(ra.attestation.subject.as_bytes(), invitee_pk.as_bytes());
            assert_eq!(ra.role, Role::Editor);
            // Inner attestation signature must verify against the
            // delegation pubkey (which the owner signed).
            let bytes = ra.attestation.signing_bytes().unwrap();
            assert!(
                verify_event(&bytes, &ra.attestation.signature, &ra.attestation.issued_by).is_ok()
            );
        } else {
            panic!("expected AttestationIssued, got {event:?}");
        }
    }

    #[test]
    fn invite_capability_is_one_use() {
        let (owner_signing_key, owner_verifying_key) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            owner_verifying_key,
            &owner_signing_key,
            "Alice",
            Role::Editor,
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        let (_, first) = generate_keypair();
        let (_, second) = generate_keypair();
        build_attestation_from_invite(&env, first, SystemTime::now()).unwrap();
        let error = build_attestation_from_invite(&env, second, SystemTime::now()).unwrap_err();
        assert!(error.to_string().contains("already redeemed"));
    }

    #[test]
    fn invite_authority_survives_restart_and_atomically_consumes() {
        let dir = tempfile::tempdir().unwrap();
        let capability = [9; 32];
        let secret = [7; 32];
        InviteRedemptionAuthority::at(dir.path())
            .unwrap()
            .issue(capability, secret, u64::MAX)
            .unwrap();
        let restarted = InviteRedemptionAuthority::at(dir.path()).unwrap();
        assert_eq!(restarted.redeem(&capability, 1).unwrap().as_slice(), secret);
        assert!(restarted.redeem(&capability, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn invite_authority_secrets_are_private_authenticated_and_race_safe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("authority");
        let capability = [11; 32];
        let authority = InviteRedemptionAuthority::at(&root).unwrap();
        authority.issue(capability, [12; 32], u64::MAX).unwrap();
        assert_eq!(
            std::fs::metadata(root.join("authority-key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(authority.path(&capability))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let authority = InviteRedemptionAuthority::at(root).unwrap();
                barrier.wait();
                authority.redeem(&capability, 1).is_ok()
            }));
        }
        barrier.wait();
        let successes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|succeeded| *succeeded)
            .count();
        assert_eq!(successes, 1, "only one concurrent redeemer may win");

        let tampered_capability = [21; 32];
        authority
            .issue(tampered_capability, [22; 32], u64::MAX)
            .unwrap();
        let record = authority.path(&tampered_capability);
        let mut bytes = std::fs::read(&record).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&record, bytes).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(authority.redeem(&tampered_capability, 1).is_err());
        assert!(
            authority.redeem(&tampered_capability, 1).is_err(),
            "a failed authentication must still consume the bearer capability"
        );
    }

    #[cfg(unix)]
    #[test]
    fn invite_authority_refuses_a_symlink_root_or_key() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let linked = dir.path().join("linked");
        symlink(&target, &linked).unwrap();
        assert!(InviteRedemptionAuthority::at(&linked).is_err());

        let authority = InviteRedemptionAuthority::at(dir.path().join("authority")).unwrap();
        let external = dir.path().join("external-key");
        std::fs::write(&external, [9; 32]).unwrap();
        symlink(&external, authority.root.join("authority-key")).unwrap();
        assert!(authority.issue([8; 32], [7; 32], u64::MAX).is_err());
    }

    #[test]
    fn invite_authority_consumes_expired_secret_without_releasing_it() {
        let dir = tempfile::tempdir().unwrap();
        let authority = InviteRedemptionAuthority::at(dir.path()).unwrap();
        authority.issue([3; 32], [4; 32], 10).unwrap();
        assert!(matches!(
            authority.redeem(&[3; 32], 10),
            Err(IdentityFlowError::InviteExpired)
        ));
        assert!(authority.redeem(&[3; 32], 9).is_err());
    }

    #[test]
    fn oversized_invite_is_rejected_before_decode() {
        let url = format!("qos://invite/{}", "A".repeat(16 * 1024 + 1));
        assert!(InviteEnvelope::from_url(&url).is_err());
    }

    #[test]
    fn invite_expiry_resolve_progression() {
        let now = SystemTime::UNIX_EPOCH;
        let hour = InviteExpiry::OneHour.resolve(now);
        let day = InviteExpiry::OneDay.resolve(now);
        let week = InviteExpiry::OneWeek.resolve(now);
        assert!(hour < day);
        assert!(day < week);
    }

    #[test]
    fn recovery_challenge_distinct_positions() {
        let m = fixture_mnemonic();
        for _ in 0..16 {
            let c = RecoveryChallenge::from_mnemonic(&m).unwrap();
            assert_ne!(c.positions[0], c.positions[1]);
            assert_ne!(c.positions[1], c.positions[2]);
            assert_ne!(c.positions[0], c.positions[2]);
            for p in &c.positions {
                assert!((1..=24).contains(p));
            }
        }
    }

    #[test]
    fn recovery_challenge_accepts_correct_words() {
        let m = fixture_mnemonic();
        let c = RecoveryChallenge::at_positions(&m, [1, 12, 24]).unwrap();
        let words: Vec<&str> = m.split_whitespace().collect();
        assert!(c.check(&[words[0], words[11], words[23]]));
    }

    #[test]
    fn recovery_challenge_rejects_wrong_word() {
        let m = fixture_mnemonic();
        let c = RecoveryChallenge::at_positions(&m, [1, 12, 24]).unwrap();
        let words: Vec<&str> = m.split_whitespace().collect();
        assert!(!c.check(&[words[0], "wrong", words[23]]));
    }

    #[test]
    fn recovery_challenge_case_insensitive() {
        let m = fixture_mnemonic();
        let c = RecoveryChallenge::at_positions(&m, [1, 12, 24]).unwrap();
        let words: Vec<&str> = m.split_whitespace().collect();
        let upper0 = words[0].to_ascii_uppercase();
        let upper11 = words[11].to_ascii_uppercase();
        let upper23 = words[23].to_ascii_uppercase();
        assert!(c.check(&[upper0.as_str(), upper11.as_str(), upper23.as_str()]));
    }

    #[test]
    fn identity_record_json_round_trip() {
        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let rec = IdentityRecord::from_threshold_identity(
            &id,
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        )
        .unwrap();
        let bytes = rec.to_json_bytes().unwrap();
        let back = IdentityRecord::from_json_bytes(&bytes).unwrap();
        assert_eq!(rec, back);
        assert_eq!(back.display_name, "alice");
        assert_eq!(back.avatar_color, [248, 113, 113]);
        assert_eq!(
            back.public_key().as_bytes(),
            id.group_public_key().as_bytes()
        );
    }

    /// The property that makes a minted identity an identity: the
    /// secret the ceremony wrote to disk must sign for the public key
    /// written next to it. Loads the way every consumer does — expand
    /// the stored bytes with `SigningKey::from_bytes` — asserting the
    /// real on-disk contract, not an in-process handle.
    #[test]
    fn ceremony_minted_record_signs_for_its_own_recorded_public_key() {
        use ed25519_dalek::{Signer, Verifier, VerifyingKey};

        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let rec = IdentityRecord::from_threshold_identity(
            &id,
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        )
        .unwrap();
        let on_disk = IdentityRecord::from_json_bytes(&rec.to_json_bytes().unwrap()).unwrap();

        let seed: [u8; 32] = on_disk
            .threshold_identity
            .secret_bytes
            .as_slice()
            .try_into()
            .expect("secret_bytes must be exactly 32 bytes");
        let sk = DalekSigningKey::from_bytes(&seed);

        assert_eq!(
            sk.verifying_key().to_bytes(),
            on_disk.threshold_identity.group_public_key,
            "the recorded secret must derive the recorded public key"
        );

        let msg = b"an op minted by a ceremony identity";
        let sig = sk.sign(msg);
        let vk = VerifyingKey::from_bytes(&on_disk.threshold_identity.group_public_key).unwrap();
        vk.verify(msg, &sig)
            .expect("a ceremony identity must be able to sign for itself");
    }

    /// A record read back from disk rebuilds an identity that signs
    /// under the same public key.
    #[test]
    fn identity_record_round_trips_back_into_a_signing_identity() {
        use collab_types::verify_event;

        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let rec = IdentityRecord::from_threshold_identity(&id, "bob", [1, 2, 3], SystemTime::now())
            .unwrap();
        let back = IdentityRecord::from_json_bytes(&rec.to_json_bytes().unwrap()).unwrap();

        let revived = back.into_threshold_identity().unwrap();
        assert_eq!(
            revived.group_public_key().as_bytes(),
            id.group_public_key().as_bytes()
        );

        let payload: Vec<u8> = b"signed after a disk round-trip".to_vec();
        let sig = revived.sign(&payload, &mut rng).unwrap();
        verify_event(&payload, &sig, id.group_public_key()).expect("revived identity must sign");
    }

    #[test]
    fn identity_record_rejects_unknown_schema() {
        let body = r#"{
            "display_name": "x",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": []
            },
            "created_at": "now",
            "schema_version": 99
        }"#;
        let err = IdentityRecord::from_json_bytes(body.as_bytes()).unwrap_err();
        assert!(matches!(err, IdentityFlowError::UnknownSchemaVersion(99)));
    }

    #[test]
    fn memory_store_round_trip() {
        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let rec =
            IdentityRecord::from_threshold_identity(&id, "bob", [180, 200, 220], SystemTime::now())
                .unwrap();
        let store = MemoryIdentityStore::new();
        assert!(store.load().unwrap().is_none());
        store.save(&rec).unwrap();
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.display_name, "bob");
    }

    #[test]
    fn pairing_handshake_round_trip() {
        let hs = PairingHandshake::new();
        let nonce = [0xAAu8; 16];
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1, 2, 3, 4, 5],
        };
        let mac = [0xCDu8; 64];
        assert!(hs.take_pairer_reply(&nonce).is_none());
        hs.publish_pairer_reply(&nonce, reply.clone(), mac);
        let (back, tag) = hs.take_pairer_reply(&nonce).unwrap();
        assert_eq!(back.from_share_index, reply.from_share_index);
        assert_eq!(tag, mac);
        assert!(hs.take_pairer_reply(&nonce).is_none());
    }

    #[test]
    fn durable_pairing_expiry_survives_restart_and_cannot_be_extended() {
        let dir = tempfile::tempdir().unwrap();
        let original_expiry = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let first = PairingHandshake::durable(dir.path(), original_expiry).unwrap();
        let nonce = [31; 16];
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1],
        };
        first.publish_pairer_reply(&nonce, reply.clone(), [2; 64]);
        assert!(first.take_pairer_reply(&nonce).is_none());

        let restarted =
            PairingHandshake::durable(dir.path(), SystemTime::now() + Duration::from_secs(3600))
                .unwrap();
        restarted.publish_pairer_reply(&nonce, reply, [2; 64]);
        assert!(
            restarted.take_pairer_reply(&nonce).is_none(),
            "reopening a ceremony must preserve, not extend, its original expiry"
        );
    }

    /// Headline integration test: after the ceremony completes, the
    /// two halves of the threshold identity can co-sign an op that
    /// verifies via `collab_types::verify_event`. Drives the protocol
    /// directly (no winit, no taffy).
    #[test]
    fn end_to_end_ceremony_lets_both_devices_co_sign() {
        use collab_types::{canonical_bytes, verify_event};
        use threshold_identity::SigningRound1Commitment;

        // Host and pairer each bootstrap, then begin enrolment toward a 2-of-2 group.
        let mut rng = OsRng;
        let mut host = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (mut host_session, host_round1) = host.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let mut pairer = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (mut pairer_session, pairer_round1) =
            pairer.begin_enrolment(2, 2, 2, &mut rng).unwrap();
        // Each processes the other's round-1, producing round-2 replies
        // addressed to the peer.
        let host_outgoing = host_session
            .receive_round1(std::slice::from_ref(&pairer_round1))
            .unwrap();
        let pairer_outgoing = pairer_session
            .receive_round1(std::slice::from_ref(&host_round1))
            .unwrap();
        let host_to_pairer = host_outgoing.into_iter().find(|(i, _)| *i == 2).unwrap().1;
        let pairer_to_host = pairer_outgoing
            .into_iter()
            .find(|(i, _)| *i == 1)
            .unwrap()
            .1;
        host_session.receive_round2(&[pairer_to_host]).unwrap();
        pairer_session.receive_round2(&[host_to_pairer]).unwrap();
        host.adopt_dkg_outcome(host_session).unwrap();
        pairer.adopt_dkg_outcome(pairer_session).unwrap();
        assert_eq!(
            host.group_public_key().as_bytes(),
            pairer.group_public_key().as_bytes(),
            "DKG must produce a shared group public key"
        );
        assert!(!host.is_single_device(), "host must be in threshold mode");
        assert!(
            !pairer.is_single_device(),
            "pairer must be in threshold mode"
        );

        let message = b"hello from the ceremony".to_vec();
        let host_round1 = host.sign_round1(&mut rng).unwrap();
        let pairer_round1 = pairer.sign_round1(&mut rng).unwrap();
        let commitments: Vec<SigningRound1Commitment> =
            vec![host_round1.commitment, pairer_round1.commitment];
        let canon = canonical_bytes(&message).unwrap();
        let host_partial = host
            .sign_round2_raw(host_round1.nonces, &canon, &commitments)
            .unwrap();
        let pairer_partial = pairer
            .sign_round2_raw(pairer_round1.nonces, &canon, &commitments)
            .unwrap();
        let sig = host
            .aggregate_signature(&[host_partial, pairer_partial])
            .unwrap();
        // verify_event canonical_bytes()-es its argument; we signed
        // `canon` directly, so feeding `message` back through the same
        // encoder gives matching byte streams.
        verify_event(&message, &sig, host.group_public_key())
            .expect("threshold-produced signature must verify as plain Ed25519");
    }
}
