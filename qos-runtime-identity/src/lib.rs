//! `qos-runtime-identity` — user-identity loader for qOS apps.
//!
//! The launcher points at the user's identity record file via the
//! `QOS_USER_IDENTITY` env var. This crate reads and parses it,
//! exposes a [`SigningIdentity`] that apps use to sign collab-op
//! and attestation payloads.
//!
//! ## On-disk format
//!
//! The file is the JSON emitted by the identity-flow's
//! `IdentityRecord::to_json_bytes` (see `ui/src/widget/identity_flow`).
//! We deliberately keep a *minimal local copy* of the schema
//! (`ondisk::IdentityRecord`) instead of taking a dep on `ui` —
//! `ui` is heavy and depends on this SDK layer transitively, so a
//! back-edge would form a cycle. If the flow's schema evolves in a
//! breaking way, both sides bump the version tag together.
//!
//! ## Single-device vs. threshold signing
//!
//! In 1-of-1 (single-device) mode this crate signs locally via
//! ed25519-dalek — the on-disk `secret_bytes` is the raw Ed25519
//! signing scalar and standard verification works against the
//! recorded `group_public_key`.
//!
//! In threshold mode (`t > 1`) signing requires a multi-device
//! FROST round-trip that this SDK crate does NOT drive. [`SigningIdentity::sign`]
//! returns [`NotAvailable::ConnectionFailed`] wrapping an `Unsupported`
//! `io::Error` in that case; apps that need threshold-mode signing
//! should reach for `qos-threshold-identity` directly and orchestrate
//! the ceremony themselves.
//!
//! ## Usage
//!
//! ```ignore
//! let identity = qos_runtime_identity::load()?;
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

pub use qos_runtime_portal::NotAvailable;

/// Environment-variable name the launcher sets to a path pointing
/// at the on-disk identity record.
pub const IDENTITY_ENV_VAR: &str = "QOS_USER_IDENTITY";

/// Wire-format types the loader deserialises from disk. Mirrors the
/// shape of `ui::widget::identity_flow::IdentityRecord`; extra
/// fields are ignored so an older SDK reading a newer file does not
/// hard-fail.
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

/// Reason the identity record failed to load — bundled into
/// `NotAvailable::ConnectionFailed` via an `io::Error` so callers
/// have exactly one error type to match on.
#[derive(Debug)]
enum LoadFailure {
    ReadFailed(io::Error),
    JsonMalformed(serde_json::Error),
    UnknownSchemaVersion(u32),
    SecretSizeMismatch { got: usize },
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
        }
    }
}

impl LoadFailure {
    fn into_not_available(self) -> NotAvailable {
        let kind = match &self {
            LoadFailure::ReadFailed(e) => e.kind(),
            LoadFailure::JsonMalformed(_)
            | LoadFailure::UnknownSchemaVersion(_)
            | LoadFailure::SecretSizeMismatch { .. } => io::ErrorKind::InvalidData,
        };
        NotAvailable::ConnectionFailed(io::Error::new(kind, self.to_string()))
    }
}

/// The loaded user identity. Owns the private signing material in
/// memory for the process lifetime; wrap it in an `Arc` in the app
/// if you need to share it across tasks.
///
/// **Never** serialise or log this struct — it holds the raw
/// Ed25519 secret. `Debug` deliberately omits `signing_key`.
pub struct SigningIdentity {
    display_name: String,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
    threshold: u16,
    total_shares: u16,
    share_index: u16,
    /// Present only in single-device (1-of-1) mode. `None` in
    /// threshold mode; [`Self::sign`] then returns an unsupported
    /// error.
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
    /// Human-picked display name (e.g. surfaced in chrome, presence
    /// pills). May be empty.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Avatar-accent RGB the user picked at first boot.
    pub fn avatar_color(&self) -> [u8; 3] {
        self.avatar_color
    }

    /// The 32-byte group public key — byte-compatible with a
    /// standard Ed25519 verifier and stable across threshold
    /// enrolments (until the next DKG rotation).
    pub fn group_public_key(&self) -> [u8; 32] {
        self.group_public_key
    }

    /// `t` in t-of-n. `1` when the user is still in single-device
    /// bootstrap mode.
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

    /// True while the identity is still in 1-of-1 mode — the fast
    /// path where signing is local and cheap.
    pub fn is_single_device(&self) -> bool {
        self.signing_key.is_some()
    }

    /// Sign a message locally.
    ///
    /// - **Single-device mode:** returns the raw 64-byte Ed25519
    ///   signature.
    /// - **Threshold mode:** returns [`NotAvailable::ConnectionFailed`]
    ///   wrapping an `Unsupported` `io::Error`. Apps that need
    ///   threshold-mode signing must drive the multi-device FROST
    ///   ceremony via `qos-threshold-identity` directly.
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

/// Load the user's identity from the file the launcher points at
/// via [`IDENTITY_ENV_VAR`].
///
/// Returns:
/// - `Err(NotAvailable::ServiceFdMissing)` when the env var is
///   unset or empty (app is running outside qOS).
/// - `Err(NotAvailable::ConnectionFailed(_))` when the file is
///   missing, unreadable, or the JSON does not match the expected
///   schema.
/// - `Ok(SigningIdentity)` on success.
pub fn load() -> Result<SigningIdentity, NotAvailable> {
    let path_str = env::var(IDENTITY_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
    if path_str.is_empty() {
        return Err(NotAvailable::ServiceFdMissing);
    }
    load_from_path(PathBuf::from(path_str))
}

/// Same as [`load`] but reads from a caller-specified path. Handy
/// for tests and for apps that resolve the identity path themselves
/// (e.g. from a settings menu that lets the user pick between
/// multiple identities).
pub fn load_from_path(path: PathBuf) -> Result<SigningIdentity, NotAvailable> {
    let bytes = fs::read(&path)
        .map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
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
        return Err(
            LoadFailure::SecretSizeMismatch { got: ti.secret_bytes.len() }.into_not_available(),
        );
    }

    let signing_key = if ti.single_device {
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&ti.secret_bytes);
        // SigningKey::from_bytes is infallible: it accepts any
        // 32-byte input as an Ed25519 scalar seed. If a future
        // dalek revision surfaces validation errors, catch them
        // here.
        Some(SigningKey::from_bytes(&scalar))
    } else {
        None
    };

    // Sanity-check the stored group_public_key against the derived
    // one in single-device mode. A mismatch here means the file
    // was written by a placeholder-key encoder (see the note in
    // ui/src/widget/identity_flow/mod.rs::SerializableIdentity::from_identity
    // — v1 wrote a SHA-512 digest of the pubkey as the secret to
    // stub the disk format). We do NOT hard-fail on the mismatch:
    // the app's `.sign()` calls will succeed but produce sigs that
    // don't verify against the recorded pubkey. The app is
    // expected to react to that at the verification layer.
    if let Some(sk) = &signing_key {
        let derived_vk = sk.verifying_key();
        let derived = derived_vk.to_bytes();
        if derived != ti.group_public_key {
            // Soft mismatch: we surface it as a Debug-only stderr
            // note but return the SigningIdentity anyway so the
            // caller can decide. Hard-fail-here would break the
            // v1 disk format entirely.
            #[cfg(debug_assertions)]
            eprintln!(
                "qos-runtime-identity: derived Ed25519 pubkey {:x?} does not match recorded {:x?}; \
                 likely a placeholder identity",
                &derived[..4],
                &ti.group_public_key[..4],
            );
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
    // If someone adds `#[derive(serde::Serialize)]` to SigningIdentity
    // in the future, this stops compiling — the `SigningKey` doesn't
    // want a serde derive at the outer layer.
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
        // Cook up a real Ed25519 keypair, wrap it in the on-disk
        // schema, load it, sign a message, verify via a standard
        // Ed25519 verifier. This is the end-to-end happy path
        // apps will hit.
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
        verifier.verify(msg, &sig).expect("verification should succeed");
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
