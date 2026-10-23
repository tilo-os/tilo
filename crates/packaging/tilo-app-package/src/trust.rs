//! Consumer-side trust policy — trusted attestors + revoked publisher
//! keys, loaded from user config by the caller (usually `tilo-install`)
//! and threaded into [`crate::verify::verify`]. The foundation crate
//! itself never touches `$HOME`.
//!
//! ```text
//! ~/.config/tilo/trusted-attestors.toml   # [[attestor]] id/pubkey_hex/scope
//! ~/.config/tilo/revoked-keys.toml        # [[revoked]] pubkey_hex/reason
//! ```
//!
//! An attestor's signature covers a domain-separated message:
//! `b"tilo-app-package-attest-v1\0" || payload_root_hash[32] || publisher_pubkey[32]`
//! (83 bytes, Ed25519 → 64-byte signature).
//!
//! **Why `payload_root_hash` and not `package_id`?** `package_id`
//! transitively includes the manifest bytes, which include the
//! attestation signatures themselves — signing over `package_id`
//! creates a fixed-point loop. `payload_root_hash` (BLAKE3 of the
//! payload collection) is stable across attestation updates and binds
//! the attestation to "this payload, signed by this publisher".
//!
//! `signature_hex` on `[[trust.attested_by]]` holds this signature's
//! hex; threshold-Ed25519 (FROST) signatures verify identically at the
//! wire. For k-of-n attestation, add multiple `[[trust.attested_by]]`
//! rows — `verify_with_trust` returns after the first trusted-attestor
//! match, so a stricter policy can walk and check each row itself.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::blobs::BlobHash;

/// Domain-separation prefix for attestation signatures.
pub const ATTESTATION_DOMAIN: &[u8] = b"tilo-app-package-attest-v1\0";
const ATTESTATION_PREIMAGE_LEN: usize = ATTESTATION_DOMAIN.len() + 32 + 32;

/// Compute the preimage the attestor signs: `DOMAIN ||
/// payload_root_hash || publisher_pubkey` (see module docs).
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "every index and length here is a compile-time constant \
              (ATTESTATION_DOMAIN.len() plus two fixed 32-byte fields \
              summing to the fixed-size output), not data-dependent"
)]
pub fn attestation_preimage(
    payload_root_hash: &BlobHash,
    publisher_pubkey: &[u8; 32],
) -> [u8; ATTESTATION_PREIMAGE_LEN] {
    let mut out = [0u8; ATTESTATION_PREIMAGE_LEN];
    out[..ATTESTATION_DOMAIN.len()].copy_from_slice(ATTESTATION_DOMAIN);
    let mut off = ATTESTATION_DOMAIN.len();
    out[off..off + 32].copy_from_slice(&payload_root_hash.0);
    off += 32;
    out[off..off + 32].copy_from_slice(publisher_pubkey);
    out
}

/// The scope an attestor is trusted for, parsed from the `scope`
/// string: `"all"`, `"mime:<mime>[,...]"`, or `"prefix:<pref>[,...]"`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum AttestorScope {
    /// Trusts every package the attestor signs.
    #[default]
    All,
    /// Only trusts packages whose manifest declares one of these MIME types in `providers`.mime.
    Mime(Vec<String>),
    /// Only trusts packages whose [package](crate::package).id starts with one of these prefixes.
    Prefix(Vec<String>),
}

impl AttestorScope {
    /// Parse the scope string in the TOML config.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::BadScope`] if `raw` is not `"all"`, a
    /// non-empty `"mime:<list>"`, or a non-empty `"prefix:<list>"`.
    pub fn parse(raw: &str) -> Result<Self, TrustLoadError> {
        let raw = raw.trim();
        if raw == "all" {
            return Ok(AttestorScope::All);
        }
        if let Some(rest) = raw.strip_prefix("mime:") {
            let items: Vec<String> = rest.split(',').map(|s| s.trim().to_string()).collect();
            if items.is_empty() || items.iter().any(std::string::String::is_empty) {
                return Err(TrustLoadError::BadScope(raw.to_string()));
            }
            return Ok(AttestorScope::Mime(items));
        }
        if let Some(rest) = raw.strip_prefix("prefix:") {
            let items: Vec<String> = rest.split(',').map(|s| s.trim().to_string()).collect();
            if items.is_empty() || items.iter().any(std::string::String::is_empty) {
                return Err(TrustLoadError::BadScope(raw.to_string()));
            }
            return Ok(AttestorScope::Prefix(items));
        }
        Err(TrustLoadError::BadScope(raw.to_string()))
    }

    /// Render back to the string form, for CLI printing + on-disk round-trip.
    pub fn render(&self) -> String {
        match self {
            AttestorScope::All => "all".into(),
            AttestorScope::Mime(items) => format!("mime:{}", items.join(",")),
            AttestorScope::Prefix(items) => format!("prefix:{}", items.join(",")),
        }
    }

    /// Does this scope match a package with the given id + mime list from `providers`.mime?
    pub fn covers(&self, package_id: &str, mime: &[String]) -> bool {
        match self {
            AttestorScope::All => true,
            AttestorScope::Mime(allowed) => mime.iter().any(|m| allowed.iter().any(|a| a == m)),
            AttestorScope::Prefix(prefixes) => prefixes.iter().any(|p| package_id.starts_with(p)),
        }
    }
}

/// One row of trusted-attestors.toml — an attestor we accept third-party attestations from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedAttestor {
    /// Human-readable id, e.g. `tilo-curated/2026.07`; must match the
    /// `attestor` field in `[[trust.attested_by]]` for a signature to count.
    pub id: String,
    /// The attestor's Ed25519 public key, hex encoded (64 chars).
    pub pubkey_hex: String,
    pub scope: AttestorScope,
}

impl TrustedAttestor {
    /// Parse the hex pubkey into raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::BadHex`] if `pubkey_hex` is not valid hex,
    /// or [`TrustLoadError::BadPubkeyLength`] if it doesn't decode to
    /// exactly 32 bytes.
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], TrustLoadError> {
        parse_pubkey_hex(&self.pubkey_hex)
    }
}

// Custom TOML shape — `scope` is a bare string on disk but a typed enum in memory.
#[derive(Serialize, Deserialize)]
struct TrustedAttestorToml {
    id: String,
    pubkey_hex: String,
    #[serde(default = "default_scope_str")]
    scope: String,
}

fn default_scope_str() -> String {
    "all".into()
}

impl<'de> Deserialize<'de> for TrustedAttestor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = TrustedAttestorToml::deserialize(d)?;
        let scope = AttestorScope::parse(&raw.scope).map_err(serde::de::Error::custom)?;
        Ok(TrustedAttestor {
            id: raw.id,
            pubkey_hex: raw.pubkey_hex,
            scope,
        })
    }
}

impl Serialize for TrustedAttestor {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let raw = TrustedAttestorToml {
            id: self.id.clone(),
            pubkey_hex: self.pubkey_hex.clone(),
            scope: self.scope.render(),
        };
        raw.serialize(s)
    }
}

/// The full contents of `trusted-attestors.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedAttestors {
    #[serde(default)]
    pub attestor: Vec<TrustedAttestor>,
}

impl TrustedAttestors {
    /// Look up an attestor by id.
    pub fn get(&self, id: &str) -> Option<&TrustedAttestor> {
        self.attestor.iter().find(|a| a.id == id)
    }

    /// Return the slice of attestors as a plain slice — convenience for `verify()`.
    pub fn as_slice(&self) -> &[TrustedAttestor] {
        &self.attestor
    }
}

/// A trust list kept as a TOML file in user config. A missing file is an
/// empty list: nothing trusted, nothing revoked.
pub trait TrustFile: Default + Serialize + DeserializeOwned {
    /// Decode from TOML source.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::Parse`] if `s` is not valid TOML for this
    /// shape.
    fn from_toml_str(s: &str) -> Result<Self, TrustLoadError> {
        toml::from_str(s).map_err(|e| TrustLoadError::Parse(e.to_string()))
    }

    /// Encode to TOML source.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::Encode`] if serialisation fails.
    fn to_toml_string(&self) -> Result<String, TrustLoadError> {
        toml::to_string(self).map_err(|e| TrustLoadError::Encode(e.to_string()))
    }

    /// Load from disk; the empty list if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::Io`] if the file exists but can't be read
    /// or isn't valid UTF-8, or [`TrustLoadError::Parse`] if it isn't valid
    /// TOML for this shape.
    fn load(path: &Path) -> Result<Self, TrustLoadError> {
        let Some(text) = read_optional_policy(path)? else {
            return Ok(Self::default());
        };
        Self::from_toml_str(&text)
    }

    /// Atomic-rename-based save.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::Io`] if the parent directory can't be
    /// created or the write fails, or [`TrustLoadError::Encode`] if
    /// serialisation fails.
    fn save(&self, path: &Path) -> Result<(), TrustLoadError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TrustLoadError::Io(e.to_string()))?;
        }
        let text = self.to_toml_string()?;
        tilo_files::native::atomic::write(path, text.as_bytes())
            .map_err(|e| TrustLoadError::Io(e.to_string()))
    }
}

impl TrustFile for TrustedAttestors {}

/// One revoked publisher key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedKey {
    /// Ed25519 public key of the revoked publisher, hex encoded.
    pub pubkey_hex: String,
    /// Free-form human-readable reason for the record.
    #[serde(default)]
    pub reason: String,
}

impl RevokedKey {
    /// Parse the hex pubkey into raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TrustLoadError::BadHex`] if `pubkey_hex` is not valid hex,
    /// or [`TrustLoadError::BadPubkeyLength`] if it doesn't decode to
    /// exactly 32 bytes.
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], TrustLoadError> {
        parse_pubkey_hex(&self.pubkey_hex)
    }
}

/// The full contents of `revoked-keys.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationList {
    #[serde(default)]
    pub revoked: Vec<RevokedKey>,
}

impl TrustFile for RevocationList {
    /// Reject malformed keys rather than silently dropping a revocation.
    fn from_toml_str(s: &str) -> Result<Self, TrustLoadError> {
        let list: Self = toml::from_str(s).map_err(|e| TrustLoadError::Parse(e.to_string()))?;
        for entry in &list.revoked {
            entry.pubkey_bytes()?;
        }
        Ok(list)
    }
}

impl RevocationList {
    /// Test a raw pubkey against the revocation list; returns the matching `RevokedKey` if any.
    pub fn find(&self, pubkey: &[u8; 32]) -> Option<&RevokedKey> {
        for entry in &self.revoked {
            if let Ok(bytes) = entry.pubkey_bytes()
                && &bytes == pubkey
            {
                return Some(entry);
            }
        }
        None
    }
}

/// Only absence means an empty policy. Metadata lookup failures must not
/// disable revocations by pretending the file is absent.
fn read_optional_policy(path: &Path) -> Result<Option<String>, TrustLoadError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TrustLoadError::Io(format!("{}: {error}", path.display()))),
    }
}

/// Standard config-dir paths for the trust registry + revocation
/// list; follows XDG (`$XDG_CONFIG_HOME`, fallback `~/.config`) under a `tilo/` subdir.
pub fn default_trusted_attestors_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("trusted-attestors.toml"))
}

/// Sibling of [`default_trusted_attestors_path`].
pub fn default_revoked_keys_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("revoked-keys.toml"))
}

fn config_dir() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

fn parse_pubkey_hex(s: &str) -> Result<[u8; 32], TrustLoadError> {
    let bytes = hex::decode(s).map_err(|e| TrustLoadError::BadHex(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(TrustLoadError::BadPubkeyLength { got: bytes.len() });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Errors surfaced by config load / save + hex parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustLoadError {
    #[error("TOML parse: {0}")]
    Parse(String),
    #[error("TOML encode: {0}")]
    Encode(String),
    #[error("I/O: {0}")]
    Io(String),
    #[error("bad pubkey hex: {0}")]
    BadHex(String),
    #[error("pubkey length {got}, expected 32")]
    BadPubkeyLength { got: usize },
    #[error("bad scope {0:?} — expected 'all', 'mime:<list>', or 'prefix:<list>'")]
    BadScope(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_policy_files_are_optional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.toml");
        assert_eq!(
            TrustedAttestors::load(&path).unwrap(),
            TrustedAttestors::default()
        );
        assert_eq!(
            RevocationList::load(&path).unwrap(),
            RevocationList::default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn policy_lookup_errors_are_not_empty_policies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        // A symlink loop produces a metadata/read error even when run as root.
        std::os::unix::fs::symlink("policy.toml", &path).unwrap();
        for error in [
            TrustedAttestors::load(&path).unwrap_err(),
            RevocationList::load(&path).unwrap_err(),
        ] {
            assert!(matches!(error, TrustLoadError::Io(ref detail)
                if detail.contains(path.to_str().unwrap())));
        }
    }

    #[test]
    fn malformed_revocation_keys_are_rejected_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked-keys.toml");
        for key in ["not-hex".to_string(), "ab".repeat(31), "ab".repeat(33)] {
            let text = format!("[[revoked]]\npubkey_hex = {key:?}\n");
            std::fs::write(&path, &text).unwrap();
            assert!(matches!(
                RevocationList::load(&path),
                Err(TrustLoadError::BadHex(_) | TrustLoadError::BadPubkeyLength { .. })
            ));
        }
        let text = format!("[[revoked]]\npubkey_hex = {:?}\n", "ab".repeat(32));
        std::fs::write(&path, text).unwrap();
        assert!(
            RevocationList::load(&path)
                .unwrap()
                .find(&[0xab; 32])
                .is_some()
        );
    }

    #[test]
    fn attestation_scopes_parse_round_trip_and_match() {
        // phase: scope_roundtrip_all
        {
            let s = AttestorScope::All;
            assert_eq!(s.render(), "all");
            assert_eq!(AttestorScope::parse("all").unwrap(), s);
        }
        // phase: scope_roundtrip_mime
        {
            let s = AttestorScope::Mime(vec!["application/pdf".into()]);
            assert_eq!(s.render(), "mime:application/pdf");
            assert_eq!(AttestorScope::parse("mime:application/pdf").unwrap(), s);
        }
        // phase: scope_roundtrip_prefix
        {
            let s = AttestorScope::Prefix(vec!["so.tilo.".into(), "dev.example.".into()]);
            assert_eq!(s.render(), "prefix:so.tilo.,dev.example.");
            assert_eq!(
                AttestorScope::parse("prefix:so.tilo.,dev.example.").unwrap(),
                s
            );
        }
        // phase: scope_rejects_garbage
        {
            assert!(matches!(
                AttestorScope::parse("hello"),
                Err(TrustLoadError::BadScope(_))
            ));
            assert!(matches!(
                AttestorScope::parse("mime:"),
                Err(TrustLoadError::BadScope(_))
            ));
        }
        // phase: scope_covers_matches_prefix
        {
            let s = AttestorScope::Prefix(vec!["so.tilo.".into()]);
            assert!(s.covers("so.tilo.editor", &[]));
            assert!(!s.covers("com.example.foo", &[]));
        }
        // phase: scope_covers_matches_mime
        {
            let s = AttestorScope::Mime(vec!["application/pdf".into()]);
            assert!(s.covers("dev.foo", &["application/pdf".into()]));
            assert!(!s.covers("dev.foo", &["text/plain".into()]));
        }
    }

    #[test]
    fn trust_lists_round_trip_and_attestations_are_domain_separated() {
        // phase: trusted_attestors_toml_roundtrip
        {
            let orig = TrustedAttestors {
                attestor: vec![
                    TrustedAttestor {
                        id: "tilo-curated/2026.07".into(),
                        pubkey_hex: "aa".repeat(32),
                        scope: AttestorScope::All,
                    },
                    TrustedAttestor {
                        id: "acme/1".into(),
                        pubkey_hex: "bb".repeat(32),
                        scope: AttestorScope::Prefix(vec!["com.acme.".into()]),
                    },
                ],
            };
            let text = orig.to_toml_string().unwrap();
            let decoded = TrustedAttestors::from_toml_str(&text).unwrap();
            assert_eq!(decoded, orig);
        }
        // phase: revocation_toml_roundtrip
        {
            let orig = RevocationList {
                revoked: vec![RevokedKey {
                    pubkey_hex: "cc".repeat(32),
                    reason: "compromised 2026-07-05".into(),
                }],
            };
            let text = orig.to_toml_string().unwrap();
            let decoded = RevocationList::from_toml_str(&text).unwrap();
            assert_eq!(decoded, orig);
        }
        // phase: revocation_list_finds_by_pubkey
        {
            let list = RevocationList {
                revoked: vec![RevokedKey {
                    pubkey_hex: "de".repeat(32),
                    reason: "test".into(),
                }],
            };
            assert!(list.find(&[0xde; 32]).is_some());
            assert!(list.find(&[0x00; 32]).is_none());
        }
        // phase: attestation_preimage_is_domain_separated
        {
            let root = BlobHash([1u8; 32]);
            let pk = [2u8; 32];
            let msg = attestation_preimage(&root, &pk);
            assert!(msg.starts_with(ATTESTATION_DOMAIN));
            // Different payload root → different preimage.
            let root2 = BlobHash([9u8; 32]);
            let msg2 = attestation_preimage(&root2, &pk);
            assert_ne!(&msg[..], &msg2[..]);
            // Different publisher → different preimage.
            let msg3 = attestation_preimage(&root, &[7u8; 32]);
            assert_ne!(&msg[..], &msg3[..]);
        }
    }
}
