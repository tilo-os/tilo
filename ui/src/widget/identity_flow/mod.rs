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
/// equivalent identity. `secret_bytes` is the raw 32-byte Ed25519
/// seed — treat this struct as secret material.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SerializableIdentity {
    /// Always `true` here: only a single-device identity has a whole
    /// secret to persist.
    pub single_device: bool,
    pub group_public_key: [u8; 32],
    /// `t` in t-of-n (`1` in single-device mode).
    pub threshold: u16,
    /// `n` in t-of-n (`1` in single-device mode).
    pub total_shares: u16,
    pub share_index: u16,
    /// The 32-byte Ed25519 seed `SigningKey::from_bytes` expands from;
    /// anything else here signs for the wrong key.
    #[serde(default, with = "serde_bytes_codec")]
    pub secret_bytes: Vec<u8>,
}

impl SerializableIdentity {
    /// Only valid in single-device mode: a threshold identity's secret
    /// share is not a signing key. Threshold identities return
    /// [`IdentityFlowError::ThresholdSerialiseUnimplemented`] and stay
    /// live in-process instead.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::ThresholdSerialiseUnimplemented`] if
    /// `id` has no single-device secret seed to extract.
    pub fn from_identity(id: &ThresholdIdentity) -> Result<Self, IdentityFlowError> {
        let seed = id
            .single_device_seed()
            .ok_or(IdentityFlowError::ThresholdSerialiseUnimplemented)?;
        Ok(Self {
            single_device: true,
            group_public_key: *id.group_public_key().as_bytes(),
            threshold: id.threshold,
            total_shares: id.total_shares,
            share_index: id.this_share_index,
            secret_bytes: seed.to_vec(),
        })
    }
}

impl IdentityRecord {
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::ThresholdSerialiseUnimplemented`] if
    /// `identity` is not single-device.
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

    /// Checks the derived public key against the record's stored one —
    /// a mismatch is refused rather than silently loaded. Threshold-mode
    /// records can't be revived (a single share isn't a signing key).
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::ThresholdSerialiseUnimplemented`] for
    /// a threshold-mode record, [`IdentityFlowError::DecodeFailed`] if
    /// the stored secret isn't 32 bytes, or
    /// [`IdentityFlowError::PublicKeyMismatch`] if the derived public key
    /// doesn't match the recorded one.
    pub fn into_threshold_identity(self) -> Result<ThresholdIdentity, IdentityFlowError> {
        let ti = self.threshold_identity;
        if !ti.single_device {
            return Err(IdentityFlowError::ThresholdSerialiseUnimplemented);
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
        })
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
        let p: Self = serde_json::from_slice(bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(e.to_string()))?;
        if p.ty != "qos-pairing" {
            return Err(IdentityFlowError::DecodeFailed(format!(
                "unexpected type tag: {}",
                p.ty
            )));
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
    let mut mac = HmacSha512::new_from_slice(key).expect("HMAC accepts any key length");
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
    let mut mac = HmacSha512::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload);
    mac.verify_slice(tag).is_ok()
}

/// In-memory stand-in for the broker-mediated pairing channel, so
/// tests and the example bin exercise the full ceremony without
/// standing up Iroh. Production replaces this with the real broker
/// shim in `shell/src/shell/identity_provisioning.rs`.
#[derive(Default, Clone)]
pub struct PairingHandshake {
    inner: Arc<Mutex<HashMap<String, PairingChannelState>>>,
}

#[derive(Default, Debug)]
struct PairingChannelState {
    /// Host polls this.
    reply: Option<(EnrolmentRound2, [u8; 64])>,
    /// Pairer polls this for the host's round-2 contribution.
    host_round2: Option<(EnrolmentRound2, [u8; 64])>,
}

impl PairingHandshake {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish_pairer_reply(&self, nonce: &[u8; 16], reply: EnrolmentRound2, mac: [u8; 64]) {
        let key = hex::encode(nonce);
        // Recover rather than panic on a poisoned lock: a panic here
        // would abort the whole process in a release build.
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.entry(key).or_default().reply = Some((reply, mac));
    }

    pub fn take_pairer_reply(&self, nonce: &[u8; 16]) -> Option<(EnrolmentRound2, [u8; 64])> {
        let key = hex::encode(nonce);
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&key)?
            .reply
            .take()
    }

    pub fn publish_host_round2(&self, nonce: &[u8; 16], reply: EnrolmentRound2, mac: [u8; 64]) {
        let key = hex::encode(nonce);
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.entry(key).or_default().host_round2 = Some((reply, mac));
    }

    pub fn take_host_round2(&self, nonce: &[u8; 16]) -> Option<(EnrolmentRound2, [u8; 64])> {
        let key = hex::encode(nonce);
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&key)?
            .host_round2
            .take()
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
            InviteExpiry::OneHour => Duration::from_hours(1),
            InviteExpiry::OneDay => Duration::from_hours(24),
            InviteExpiry::OneWeek => Duration::from_hours(168),
            // `Never` is encoded as +100 years; downstream verifiers
            // treat that as "no practical expiry".
            InviteExpiry::Never => Duration::from_hours(876_000),
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
/// `delegation_secret` is a one-time Ed25519 secret shipped in
/// cleartext — safe only because invites travel over channels already
/// treated as confidential. A future version should encrypt it to the
/// recipient's pubkey for less-trusted transports.
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
    /// Secret — must not be displayed (see struct doc).
    #[serde(with = "serde_bytes_codec")]
    pub delegation_secret: Vec<u8>,
    /// Excludes `delegation_secret`; signing `delegation_pubkey` alone
    /// is enough to bind the delegation.
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
        }
        let view = View {
            channel: &self.channel,
            channel_name: &self.channel_name,
            owner_pubkey: &self.owner_pubkey,
            inviter_label: &self.inviter_label,
            role: &self.role,
            expires_unix: self.expires_unix,
            delegation_pubkey: &self.delegation_pubkey,
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
        let bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, suffix)
            .ok_or_else(|| IdentityFlowError::DecodeFailed("base32 decode failed".into()))?;
        postcard::from_bytes(&bytes)
            .map_err(|e| IdentityFlowError::DecodeFailed(format!("postcard: {e}")))
    }
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
    let (delegation_secret_full, delegation_pubkey) = generate_keypair();
    let mut env = InviteEnvelope {
        channel,
        channel_name: channel_name.into(),
        owner_pubkey,
        inviter_label: inviter_label.into(),
        role,
        expires_unix: expires_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(u64::MAX, |d| d.as_secs()),
        delegation_pubkey,
        // Store raw bytes so the recipient can rebuild a DalekSigningKey.
        delegation_secret: delegation_secret_full.to_bytes().to_vec(),
        owner_signature: Signature::from_bytes([0; 64]),
    };
    let bytes = env.signing_bytes()?;
    let sig = sign_event(&bytes, owner_signing_key)
        .map_err(|e| IdentityFlowError::SigningFailed(format!("{e:?}")))?;
    env.owner_signature = sig;
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
    let secret: [u8; 32] = envelope
        .delegation_secret
        .as_slice()
        .try_into()
        .map_err(|_| {
            IdentityFlowError::DecodeFailed("delegation secret must be 32 bytes".into())
        })?;
    let delegation_sk = DalekSigningKey::from_bytes(&secret);
    // `now` is a real wall-clock time, nowhere near the range limit
    // `SystemTime` can represent, so this can't realistically overflow;
    // `checked_add` with a same-value fallback keeps the function
    // panic-free without changing observable behaviour.
    let valid_until = now.checked_add(Duration::from_hours(8760)).unwrap_or(now);
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
    #[allow(
        clippy::similar_names,
        reason = "cx/cy is the standard center-x/center-y coordinate naming convention; distinguishing them further would reduce clarity"
    )]
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
    #[error("threshold-mode serialisation not implemented in v1 (see SerializableIdentity)")]
    ThresholdSerialiseUnimplemented,
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
            SystemTime::now() + Duration::from_hours(1),
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
            SystemTime::now() + Duration::from_hours(1),
        )
        .unwrap();
        env.channel_name = "treasury".into();
        assert!(
            env.verify_signature().is_err(),
            "renamed envelope must fail"
        );
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
            SystemTime::now() + Duration::from_hours(1),
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
