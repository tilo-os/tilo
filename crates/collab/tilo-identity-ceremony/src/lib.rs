//! Persisted identity records: the disk format an identity ceremony writes
//! and every other consumer reads, and the store the ceremony keeps it in.
//! The toolkit's identity-flow screens drive it; hosts and tests use it
//! directly.

use std::sync::{Arc, Mutex};
use tilo_clock::time::SystemTime;

use collab_types::Ed25519PublicKey;
use serde::{Deserialize, Serialize};
use serde_bytes as serde_bytes_codec;
use threshold_identity::{ThresholdError, ThresholdIdentity};
use tilo_lock::MutexRecover as _;

/// Disk-format record for a persisted identity.
///
/// Schema 1 contains a legacy plaintext share. Schema 2 contains public
/// metadata plus an opaque secret-store reference; its `secret_bytes` is
/// empty, so ceremonies that need signing must use the runtime broker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IdentityRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_photo: Option<runtime_identity::ProfilePhoto>,
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
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(
            i64::try_from(
                created_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
            )
            .unwrap_or(i64::MAX),
            0,
        )
        .unwrap_or_default()
        .to_rfc3339();
        Ok(Self {
            profile_photo: None,
            display_name: display_name.into(),
            avatar_color,
            threshold_identity: serialisable,
            created_at: ts,
            schema_version: 1,
            secret_ref: None,
        })
    }

    /// Persisted under `~/.config/tilo/identity/primary.json`.
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
}

/// The shell wires the disk-backed implementation; tests use
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
        Ok(self.inner.lock_recover().clone())
    }
    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        *self.inner.lock_recover() = Some(record.clone());
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityFlowError {
    #[error("identity-flow encoding failed: {0}")]
    EncodeFailed(String),
    #[error("identity-flow decoding failed: {0}")]
    DecodeFailed(String),
    #[error("unknown disk schema version: {0}")]
    UnknownSchemaVersion(u32),
    #[error("threshold identity engine: {0}")]
    Threshold(#[from] ThresholdError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey as DalekSigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use tilo_clock::time::Duration;

    #[test]
    fn identity_record_serialization_and_signing_round_trips() {
        fn identity_record_json_round_trip() {
            let mut rng = UnwrapErr(SysRng);
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
        /// on-disk contract, rather than an in-process handle.
        fn ceremony_minted_record_signs_for_its_own_recorded_public_key() {
            use ed25519_dalek::{Signer, Verifier, VerifyingKey};

            let mut rng = UnwrapErr(SysRng);
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
            let vk =
                VerifyingKey::from_bytes(&on_disk.threshold_identity.group_public_key).unwrap();
            vk.verify(msg, &sig)
                .expect("a ceremony identity must be able to sign for itself");
        }

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

        fn memory_store_round_trip() {
            let mut rng = UnwrapErr(SysRng);
            let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            let rec = IdentityRecord::from_threshold_identity(
                &id,
                "bob",
                [180, 200, 220],
                tilo_clock::wall(),
            )
            .unwrap();
            let store = MemoryIdentityStore::new();
            assert!(store.load().unwrap().is_none());
            store.save(&rec).unwrap();
            let back = store.load().unwrap().unwrap();
            assert_eq!(back.display_name, "bob");
        }

        identity_record_json_round_trip();
        ceremony_minted_record_signs_for_its_own_recorded_public_key();
        identity_record_rejects_unknown_schema();
        memory_store_round_trip();
    }
}
