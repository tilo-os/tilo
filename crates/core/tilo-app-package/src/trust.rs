//! Consumer-side trust policy — trusted attestors + revoked publisher
//! keys, read from the workspace's trust list by the caller and threaded
//! into [`crate::verify::verify`]. The foundation crate itself never
//! touches a file or a workspace.

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

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum AttestorScope {
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

    pub fn render(&self) -> String {
        match self {
            AttestorScope::All => "all".into(),
            AttestorScope::Mime(items) => format!("mime:{}", items.join(",")),
            AttestorScope::Prefix(items) => format!("prefix:{}", items.join(",")),
        }
    }

    pub fn covers(&self, package_id: &str, mime: &[String]) -> bool {
        match self {
            AttestorScope::All => true,
            AttestorScope::Mime(allowed) => mime.iter().any(|m| allowed.iter().any(|a| a == m)),
            AttestorScope::Prefix(prefixes) => prefixes.iter().any(|p| package_id.starts_with(p)),
        }
    }
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedAttestors {
    #[serde(default)]
    pub attestor: Vec<TrustedAttestor>,
}

impl TrustedAttestors {
    pub fn get(&self, id: &str) -> Option<&TrustedAttestor> {
        self.attestor.iter().find(|a| a.id == id)
    }

    pub fn as_slice(&self) -> &[TrustedAttestor] {
        &self.attestor
    }
}

/// A trust list, as its TOML shape spells it.
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
}

impl TrustFile for TrustedAttestors {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedKey {
    /// Ed25519 public key of the revoked publisher, hex encoded.
    pub pubkey_hex: String,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationList {
    #[serde(default)]
    pub revoked: Vec<RevokedKey>,
}

impl TrustFile for RevocationList {
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

    tilo_test_env::scenario! {
        fn attestation_scopes_parse_round_trip_and_match;

        phase scopes_render_parse_and_round_trip;
        {
            let scopes = [
                (AttestorScope::All, "all"),
                (
                    AttestorScope::Mime(vec!["application/pdf".into()]),
                    "mime:application/pdf",
                ),
                (
                    AttestorScope::Prefix(vec!["so.tilo.".into(), "dev.example.".into()]),
                    "prefix:so.tilo.,dev.example.",
                ),
            ];
            for (scope, rendered) in scopes {
                assert_eq!(scope.render(), rendered);
                assert_eq!(AttestorScope::parse(rendered).unwrap(), scope);
            }
            assert!(matches!(
                AttestorScope::parse("hello"),
                Err(TrustLoadError::BadScope(_))
            ));
            assert!(matches!(
                AttestorScope::parse("mime:"),
                Err(TrustLoadError::BadScope(_))
            ));
        }

        phase a_scope_covers_the_package_ids_and_mimes_it_may_vouch_for;
        {
            let prefix = AttestorScope::Prefix(vec!["so.tilo.".into()]);
            assert!(prefix.covers("so.tilo.editor", &[]));
            assert!(!prefix.covers("com.example.foo", &[]));

            let mime = AttestorScope::Mime(vec!["application/pdf".into()]);
            assert!(mime.covers("dev.foo", &["application/pdf".into()]));
            assert!(!mime.covers("dev.foo", &["text/plain".into()]));
        }
    }

    tilo_test_env::scenario! {
        fn trust_lists_round_trip_and_attestations_are_domain_separated;

        phase malformed_revocation_keys_are_rejected_at_parse;
        {
            for key in ["not-hex".to_string(), "ab".repeat(31), "ab".repeat(33)] {
                let text = format!("[[revoked]]\npubkey_hex = {key:?}\n");
                assert!(matches!(
                    RevocationList::from_toml_str(&text),
                    Err(TrustLoadError::BadHex(_) | TrustLoadError::BadPubkeyLength { .. })
                ));
            }
            let text = format!("[[revoked]]\npubkey_hex = {:?}\n", "ab".repeat(32));
            assert!(
                RevocationList::from_toml_str(&text)
                    .unwrap()
                    .find(&[0xab; 32])
                    .is_some()
            );
        }

        phase the_policy_files_round_trip_and_a_revocation_finds_its_key;
        {
            let attestors = TrustedAttestors {
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
            let text = attestors.to_toml_string().unwrap();
            assert_eq!(TrustedAttestors::from_toml_str(&text).unwrap(), attestors);

            let revoked = RevocationList {
                revoked: vec![RevokedKey {
                    pubkey_hex: "de".repeat(32),
                    reason: "compromised 2026-07-05".into(),
                }],
            };
            let text = revoked.to_toml_string().unwrap();
            assert_eq!(RevocationList::from_toml_str(&text).unwrap(), revoked);
            assert!(revoked.find(&[0xde; 32]).is_some());
            assert!(revoked.find(&[0x00; 32]).is_none());
        }

        phase attestation_preimage_is_domain_separated;
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
