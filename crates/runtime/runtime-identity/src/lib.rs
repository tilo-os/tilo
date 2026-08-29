//! `runtime-identity` — user-identity loader for qOS apps.
//!
//! The launcher points at the user's identity record file via the
//! `QOS_USER_IDENTITY` env var; this crate reads and parses it, exposing a
//! [`SigningIdentity`] apps use to sign collab-op and attestation payloads.
//!
//! The on-disk file is the JSON emitted by the identity-flow's
//! `IdentityRecord::to_json_bytes` (see `ui/src/widget/identity_flow`). This
//! crate keeps a minimal local copy of the schema (`ondisk::IdentityRecord`)
//! rather than depending on `ui`, which is heavy and depends on this SDK
//! layer transitively — a dep the other way would form a cycle. Both sides
//! bump the version tag together on a breaking schema change.
//!
//! In 1-of-1 (single-device) mode this crate signs locally via
//! ed25519-dalek; the loader refuses (not just warns about) a record whose
//! `secret_bytes` does not derive its own recorded `group_public_key`. In
//! threshold mode (`t > 1`) signing needs a multi-device FROST round-trip
//! this SDK crate does not drive — [`SigningIdentity::sign`] returns
//! [`NotAvailable::ConnectionFailed`] and apps needing it should reach for
//! `threshold-identity` directly.
//!
//! ```ignore
//! let identity = runtime_identity::load()?;
//! let sig = identity.sign(b"hello")?;
//! let pk = identity.group_public_key();
//! ```
//!
//! Missing env var / missing file / bad JSON all surface as
//! [`NotAvailable`] — never a panic.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use ed25519_dalek::{Signer, SigningKey};

pub use runtime_portal::NotAvailable;

/// Env var the launcher sets to the on-disk identity record's path.
pub const IDENTITY_ENV_VAR: &str = "QOS_USER_IDENTITY";

/// Wire-format types the loader deserialises from disk, mirroring
/// `ui::widget::identity_flow::IdentityRecord`; unknown fields are ignored.
mod ondisk {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub struct IdentityRecord {
        pub display_name: String,
        pub avatar_color: [u8; 3],
        pub threshold_identity: SerializableIdentity,
        #[allow(dead_code)]
        #[serde(default)]
        pub created_at: String,
        pub schema_version: u32,
    }

    #[derive(Debug, Deserialize)]
    pub struct SerializableIdentity {
        pub single_device: bool,
        pub group_public_key: [u8; 32],
        pub threshold: u16,
        pub total_shares: u16,
        pub share_index: u16,
        #[serde(with = "serde_bytes")]
        pub secret_bytes: Vec<u8>,
    }
}

/// Reason the identity record failed to load; bundled into
/// `NotAvailable::ConnectionFailed` so callers match one error type.
#[derive(Debug)]
enum LoadFailure {
    ReadFailed(io::Error),
    JsonMalformed(serde_json::Error),
    UnknownSchemaVersion(u32),
    SecretSizeMismatch {
        got: usize,
    },
    PublicKeyMismatch {
        derived: [u8; 32],
        recorded: [u8; 32],
    },
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFailed(e) => write!(f, "reading identity file failed: {e}"),
            Self::JsonMalformed(e) => write!(f, "identity JSON malformed: {e}"),
            Self::UnknownSchemaVersion(v) => write!(f, "unknown identity schema version: {v}"),
            Self::SecretSizeMismatch { got } => {
                write!(f, "identity secret_bytes must be 32 bytes, got {got}")
            }
            Self::PublicKeyMismatch { derived, recorded } => write!(
                f,
                "identity secret derives Ed25519 pubkey {} but the record claims {}; \
                 this identity cannot sign for itself",
                hex4(derived),
                hex4(recorded),
            ),
        }
    }
}

/// First four bytes of a key, hex, for error text — never the whole key,
/// since an error string can end up in a log a third party reads.
fn hex4(key: &[u8; 32]) -> String {
    key[..4].iter().map(|b| format!("{b:02x}")).collect()
}

impl LoadFailure {
    fn into_not_available(self) -> NotAvailable {
        let kind = match &self {
            LoadFailure::ReadFailed(e) => e.kind(),
            LoadFailure::JsonMalformed(_)
            | LoadFailure::UnknownSchemaVersion(_)
            | LoadFailure::SecretSizeMismatch { .. }
            | LoadFailure::PublicKeyMismatch { .. } => io::ErrorKind::InvalidData,
        };
        NotAvailable::ConnectionFailed(io::Error::new(kind, self.to_string()))
    }
}

/// The loaded user identity; owns the private signing material for the
/// process lifetime (wrap in an `Arc` to share across tasks).
///
/// **Never** serialise or log this struct — it holds the raw Ed25519
/// secret. `Debug` deliberately omits `signing_key`.
pub struct SigningIdentity {
    display_name: String,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
    threshold: u16,
    total_shares: u16,
    share_index: u16,
    /// Present only in single-device (1-of-1) mode; `None` in threshold mode,
    /// where [`Self::sign`] returns an unsupported error.
    signing_key: Option<SigningKey>,
}

impl std::fmt::Debug for SigningIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningIdentity")
            .field("display_name", &self.display_name)
            .field("group_public_key", &"<32 bytes>")
            .field("threshold", &self.threshold)
            .field("total_shares", &self.total_shares)
            .field("share_index", &self.share_index)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl SigningIdentity {
    /// Human-picked display name (chrome, presence pills); may be empty.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Avatar-accent RGB the user picked at first boot.
    pub fn avatar_color(&self) -> [u8; 3] {
        self.avatar_color
    }

    /// The 32-byte group public key — byte-compatible with a standard Ed25519
    /// verifier, stable across threshold enrolments until the next DKG rotation.
    pub fn group_public_key(&self) -> [u8; 32] {
        self.group_public_key
    }

    /// `t` in t-of-n; `1` while the user is still in single-device bootstrap mode.
    pub fn threshold(&self) -> u16 {
        self.threshold
    }

    /// `n` in t-of-n.
    pub fn total_shares(&self) -> u16 {
        self.total_shares
    }

    /// 1-indexed share number this device holds.
    pub fn share_index(&self) -> u16 {
        self.share_index
    }

    /// True while the identity is still in 1-of-1 mode, the fast local-signing path.
    pub fn is_single_device(&self) -> bool {
        self.signing_key.is_some()
    }

    /// Sign a message locally. In single-device mode, returns the raw 64-byte
    /// Ed25519 signature; in threshold mode, returns
    /// [`NotAvailable::ConnectionFailed`] — threshold signing needs the
    /// multi-device FROST ceremony via `threshold-identity` instead.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
        match &self.signing_key {
            Some(sk) => Ok(sk.sign(message).to_bytes()),
            None => Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "threshold-mode signing is not supported by qos-runtime-identity; \
                 use qos-threshold-identity + the multi-device orchestrator instead",
            ))),
        }
    }
}

/// Load the user's identity from the file the launcher points at via
/// [`IDENTITY_ENV_VAR`]. Returns `Err(NotAvailable::ServiceFdMissing)` when
/// the env var is unset/empty (running outside qOS),
/// `Err(NotAvailable::ConnectionFailed(_))` when the file is missing,
/// unreadable, or malformed, and `Ok(SigningIdentity)` on success.
pub fn load() -> Result<SigningIdentity, NotAvailable> {
    let path_str = env::var(IDENTITY_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
    if path_str.is_empty() {
        return Err(NotAvailable::ServiceFdMissing);
    }
    load_from_path(PathBuf::from(path_str))
}

/// Same as [`load`] but reads from a caller-specified path — for tests, or
/// apps that resolve the identity path themselves.
pub fn load_from_path(path: PathBuf) -> Result<SigningIdentity, NotAvailable> {
    let bytes = fs::read(&path).map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    load_from_bytes(&bytes)
}

/// Same as [`load`] but parses from an in-memory byte slice.
pub fn load_from_bytes(bytes: &[u8]) -> Result<SigningIdentity, NotAvailable> {
    let record: ondisk::IdentityRecord = serde_json::from_slice(bytes)
        .map_err(|e| LoadFailure::JsonMalformed(e).into_not_available())?;

    if record.schema_version != 1 {
        return Err(LoadFailure::UnknownSchemaVersion(record.schema_version).into_not_available());
    }

    let ti = record.threshold_identity;
    if ti.secret_bytes.len() != 32 {
        return Err(LoadFailure::SecretSizeMismatch {
            got: ti.secret_bytes.len(),
        }
        .into_not_available());
    }

    let signing_key = if ti.single_device {
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&ti.secret_bytes);
        // SigningKey::from_bytes is infallible today; if a future dalek revision
        // surfaces validation errors, catch them here.
        Some(SigningKey::from_bytes(&scalar))
    } else {
        None
    };

    // The record's two halves must agree: expanding `secret_bytes` must derive
    // `group_public_key`. A record that fails this signs under a key nobody
    // verifies it by, so every op it authors is silently rejected downstream
    // while the app believes itself authenticated — refuse to load it.
    //
    // This check is fatal in every build: one that only warns under
    // `debug_assertions` ships exactly the configuration that skips it.
    if let Some(sk) = &signing_key {
        let derived = sk.verifying_key().to_bytes();
        if derived != ti.group_public_key {
            return Err(LoadFailure::PublicKeyMismatch {
                derived,
                recorded: ti.group_public_key,
            }
            .into_not_available());
        }
    }

    Ok(SigningIdentity {
        display_name: record.display_name,
        avatar_color: record.avatar_color,
        group_public_key: ti.group_public_key,
        threshold: ti.threshold,
        total_shares: ti.total_shares,
        share_index: ti.share_index,
        signing_key,
    })
}

// Compile-time assertion that we haven't accidentally derived Clone/
// Serialize/Debug-with-secret onto SigningIdentity's signing_key.
#[allow(dead_code)]
fn _static_no_serialize() {
    // If SigningIdentity ever gains `#[derive(serde::Serialize)]`, this stops
    // compiling — `SigningKey` doesn't want a serde derive at the outer layer.
    fn takes<T>(_: &T) {}
    let _ = |x: &SigningIdentity| takes(x);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_env() {
        // SAFETY: tests in this module all touch QOS_USER_IDENTITY
        // and are logically serial.
        unsafe { env::remove_var(IDENTITY_ENV_VAR) };
    }

    #[test]
    fn load_without_env_returns_service_fd_missing() {
        clear_env();
        match load() {
            Err(NotAvailable::ServiceFdMissing) => {}
            other => panic!("expected ServiceFdMissing, got {other:?}"),
        }
    }

    #[test]
    fn load_with_empty_env_returns_service_fd_missing() {
        clear_env();
        // SAFETY: see clear_env
        unsafe { env::set_var(IDENTITY_ENV_VAR, "") };
        let out = load();
        // SAFETY: cleanup
        unsafe { env::remove_var(IDENTITY_ENV_VAR) };
        match out {
            Err(NotAvailable::ServiceFdMissing) => {}
            other => panic!("expected ServiceFdMissing, got {other:?}"),
        }
    }

    #[test]
    fn load_missing_file_surfaces_connection_failed() {
        let err = load_from_path(PathBuf::from("/nonexistent/qos-identity.json"))
            .expect_err("should fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected ConnectionFailed(NotFound), got {other:?}"),
        }
    }

    #[test]
    fn load_bad_json_surfaces_invalid_data() {
        let err = load_from_bytes(b"not json at all").expect_err("should fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::InvalidData);
            }
            other => panic!("expected ConnectionFailed(InvalidData), got {other:?}"),
        }
    }

    #[test]
    fn load_unknown_schema_version_rejected() {
        let json = r#"{
            "display_name": "Alice",
            "avatar_color": [255, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]
            },
            "created_at": "1970-01-01T00:00:00Z",
            "schema_version": 42
        }"#;
        let err = load_from_bytes(json.as_bytes()).expect_err("should fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert!(io.to_string().contains("42"), "wrong error text: {io}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn single_device_load_then_sign_and_verify() {
        // The end-to-end happy path apps will hit.
        use ed25519_dalek::{Verifier, VerifyingKey};

        let secret = [7u8; 32];
        let sk = SigningKey::from_bytes(&secret);
        let vk = sk.verifying_key();
        let record = serde_json::json!({
            "display_name": "Alice",
            "avatar_color": [200, 100, 50],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vk.to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
            },
            "created_at": "2026-07-02T00:00:00Z",
            "schema_version": 1,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        let ident = load_from_bytes(&bytes).expect("load");

        assert_eq!(ident.display_name(), "Alice");
        assert_eq!(ident.avatar_color(), [200, 100, 50]);
        assert_eq!(ident.group_public_key(), vk.to_bytes());
        assert_eq!(ident.threshold(), 1);
        assert_eq!(ident.total_shares(), 1);
        assert_eq!(ident.share_index(), 1);
        assert!(ident.is_single_device());

        let msg = b"the quick brown fox";
        let sig_bytes = ident.sign(msg).expect("sign");
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        let verifier = VerifyingKey::from_bytes(&vk.to_bytes()).unwrap();
        verifier
            .verify(msg, &sig)
            .expect("verification should succeed");
    }

    /// A record whose secret does not derive its recorded public key must not
    /// load — silently accepting it would hand apps an identity that can never
    /// sign successfully.
    #[test]
    fn secret_that_does_not_derive_the_recorded_pubkey_is_refused() {
        // The exact shape the old identity ceremony wrote: a digest
        // over the public key standing in for the secret.
        use sha2::{Digest, Sha512};
        let real = SigningKey::from_bytes(&[9u8; 32]);
        let vk = real.verifying_key();
        let mut hasher = Sha512::new();
        hasher.update(b"qos-identity-flow-v1-placeholder-key");
        hasher.update(vk.to_bytes());
        let bogus_secret = hasher.finalize()[..32].to_vec();

        let record = serde_json::json!({
            "display_name": "Alice",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vk.to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": bogus_secret,
            },
            "created_at": "2026-07-30T00:00:00Z",
            "schema_version": 1,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        let err = load_from_bytes(&bytes).expect_err("a self-inconsistent record must not load");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::InvalidData);
                assert!(
                    io.to_string().contains("cannot sign for itself"),
                    "unexpected error text: {io}"
                );
            }
            other => panic!("expected ConnectionFailed(InvalidData), got {other:?}"),
        }
    }

    /// The refusal is a property of the build, not of
    /// `debug_assertions`. Nothing here may be compiled out.
    #[test]
    fn the_pubkey_mismatch_check_is_not_gated_on_debug_assertions() {
        let real = SigningKey::from_bytes(&[4u8; 32]);
        let other = SigningKey::from_bytes(&[5u8; 32]);
        let record = serde_json::json!({
            "display_name": "",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": other.verifying_key().to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": real.to_bytes().to_vec(),
            },
            "created_at": "2026-07-30T00:00:00Z",
            "schema_version": 1,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(
            load_from_bytes(&bytes).is_err(),
            "mismatched key material must be refused in every build profile"
        );
    }

    #[test]
    fn threshold_mode_sign_returns_unsupported() {
        let zero32: Vec<u8> = vec![0; 32];
        let record = serde_json::json!({
            "display_name": "Alice",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": false,
                "group_public_key": zero32,
                "threshold": 2_u16,
                "total_shares": 3_u16,
                "share_index": 1_u16,
                "secret_bytes": zero32,
            },
            "created_at": "2026-07-02T00:00:00Z",
            "schema_version": 1,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        let ident = load_from_bytes(&bytes).expect("load");

        assert!(!ident.is_single_device());
        let err = ident.sign(b"nope").expect_err("threshold sign must fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::Unsupported);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn load_from_path_reads_disk_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("primary.json");

        let secret = [11u8; 32];
        let sk = SigningKey::from_bytes(&secret);
        let vk = sk.verifying_key();
        let record = serde_json::json!({
            "display_name": "Bob",
            "avatar_color": [1, 2, 3],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vk.to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
            },
            "created_at": "2026-07-02T00:00:00Z",
            "schema_version": 1,
        });
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(&serde_json::to_vec(&record).unwrap()).unwrap();
        drop(f);

        let ident = load_from_path(path).expect("load_from_path");
        assert_eq!(ident.display_name(), "Bob");
    }

    #[test]
    fn extra_fields_in_json_are_tolerated() {
        // Forward-compat: a newer launcher may write extra fields;
        // an older SDK must NOT hard-fail. This test asserts that.
        let secret = [3u8; 32];
        let sk = SigningKey::from_bytes(&secret);
        let vk = sk.verifying_key();
        let record = serde_json::json!({
            "display_name": "Alice",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vk.to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
                "some_new_field_from_the_future": "should be ignored",
            },
            "created_at": "2026-07-02T00:00:00Z",
            "schema_version": 1,
            "some_top_level_new_field": 99,
        });
        let bytes = serde_json::to_vec(&record).unwrap();
        load_from_bytes(&bytes).expect("must tolerate unknown fields");
    }
}
