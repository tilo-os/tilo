//! HTTP client for a qOS registry.
//!
//! Two endpoints, both blocking — every caller runs these on a worker
//! thread, never on a frame thread:
//!
//!   * `GET {base_url}/registry` — the handshake, used to lazily pin
//!     the owner pubkey on first contact.
//!   * `GET {base_url}/entries`  — the full catalogue.
//!
//! Every fetched entry is signature-checked against its own declared
//! publisher pubkey via [`registry_types::verify_entry_signature`]. A
//! separate allowlist check compares the entry's publisher pubkey
//! against the registry *owner's* allowlisted publishers.
//!
//! Nothing here is a trust decision about *installing*. A registry
//! describes what exists; `qos-install` re-verifies the actual bytes
//! fail-closed before it extracts one of them. An entry that fails its
//! signature check is [`TrustLevel::Untrusted`], and callers that put
//! software in front of a user are expected to drop those rather than
//! rank them — see [`crate::available`].

use std::time::Duration;

use registry_types::{RegistryEntry, RegistryHandshake};
use serde::Deserialize;
use thiserror::Error;

use crate::config::{RegistryConfig, placeholder_owner_pubkey};

/// Reasonable default request timeout — a fresh registry contacted
/// over WAN should still fit. Overridable via `QOS_STORE_HTTP_TIMEOUT`.
const DEFAULT_TIMEOUT_SECS: u64 = 10;

fn http_timeout() -> Duration {
    std::env::var("QOS_STORE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or_else(
            || Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("http: {0}")]
    Http(String),
    #[error("json decode: {0}")]
    Decode(String),
    #[error("empty response")]
    Empty,
}

/// Trust classification for a fetched entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustLevel {
    /// Publisher signature verified AND the registry declares a
    /// bootstrapped (non-placeholder) owner key.
    Verified,
    /// Publisher signature verified, but no owner key was available to
    /// compare against — trust-on-first-use.
    UnverifiedOwner,
    /// The publisher signature failed to verify.
    Untrusted,
}

impl TrustLevel {
    /// Whether an entry at this level may be offered to a user as
    /// something to open.
    ///
    /// `Untrusted` means the metadata itself — the name, the
    /// description, the download URL — was not signed by the key it
    /// claims. Offering that as a row would let a registry put any
    /// name it liked on any download, so it is dropped before it
    /// reaches a list.
    pub fn is_offerable(self) -> bool {
        matches!(self, TrustLevel::Verified | TrustLevel::UnverifiedOwner)
    }
}

/// One entry plus its trust state.
#[derive(Clone, Debug)]
pub struct VerifiedEntry {
    pub entry: RegistryEntry,
    pub trust: TrustLevel,
}

/// Response of `GET /entries` — a server can return either a bare
/// array of entries or a `{ "entries": [...] }` wrapper.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EntriesResponse {
    Bare(Vec<RegistryEntry>),
    Wrapped { entries: Vec<RegistryEntry> },
}

impl EntriesResponse {
    fn into_vec(self) -> Vec<RegistryEntry> {
        match self {
            EntriesResponse::Bare(v) => v,
            EntriesResponse::Wrapped { entries } => entries,
        }
    }
}

fn client() -> Result<reqwest::blocking::Client, RegistryError> {
    reqwest::blocking::Client::builder()
        .user_agent("qos-catalogue/0.1")
        .timeout(http_timeout())
        .build()
        .map_err(|e| RegistryError::Http(e.to_string()))
}

/// Fetch and parse the handshake — used to discover the owner pubkey
/// on first contact (or to detect an owner-key rotation).
pub fn fetch_handshake(reg: &RegistryConfig) -> Result<RegistryHandshake, RegistryError> {
    let url = format!("{}/registry", reg.base_url.trim_end_matches('/'));
    let c = client()?;
    let text = c
        .get(&url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        return Err(RegistryError::Empty);
    }
    serde_json::from_str(&text).map_err(|e| RegistryError::Decode(e.to_string()))
}

/// Fetch `/entries` and classify each result.
pub fn fetch_entries(reg: &RegistryConfig) -> Result<Vec<VerifiedEntry>, RegistryError> {
    let url = format!("{}/entries", reg.base_url.trim_end_matches('/'));
    let c = client()?;
    let text = c
        .get(&url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let raw: EntriesResponse =
        serde_json::from_str(&text).map_err(|e| RegistryError::Decode(e.to_string()))?;
    Ok(classify_entries(raw.into_vec(), &reg.owner_pubkey_hex))
}

/// The pure part of [`fetch_entries`]: given a decoded list and the
/// owner pubkey the registry advertises, produce a [`VerifiedEntry`]
/// per input. No I/O — so tests can feed fixtures directly.
pub fn classify_entries(entries: Vec<RegistryEntry>, owner_pubkey_hex: &str) -> Vec<VerifiedEntry> {
    // At this level "owner-allowlisted" collapses to "the registry's
    // declared owner pubkey is non-placeholder and the entry was
    // signed by an ed25519 key we can verify". The full
    // publisher-cert chain is a follow-up.
    let owner_bootstrapped =
        !owner_pubkey_hex.is_empty() && owner_pubkey_hex != placeholder_owner_pubkey();
    entries
        .into_iter()
        .map(|e| {
            let sig_ok = registry_types::verify_entry_signature(&e);
            let trust = match (sig_ok, owner_bootstrapped) {
                (false, _) => TrustLevel::Untrusted,
                (true, false) => TrustLevel::UnverifiedOwner,
                (true, true) => TrustLevel::Verified,
            };
            VerifiedEntry { entry: e, trust }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use registry_types::{RegistryVersion, sign_entry};

    pub(crate) fn signed_entry() -> RegistryEntry {
        // Deterministic key so signing is reproducible under `cargo
        // test`; cryptographic hygiene is not what this asserts.
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk_hex = hex::encode(key.verifying_key().to_bytes());
        let mut entry = RegistryEntry {
            package_id: "dev.qos.editor".into(),
            name: "Editor".into(),
            description: "CRDT-native editor".into(),
            categories: vec!["Productivity".into()],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: pk_hex,
            versions: vec![RegistryVersion {
                version: "0.1.0".into(),
                qpkg_url: "https://apps.qos.dev/editor/0.1.0.qpkg".into(),
                qpkg_root_hash_hex: "cd".repeat(32),
                qpkg_size_bytes: 1024,
                released_at_unix: 100,
            }],
            updated_at_unix: 200,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &key);
        entry
    }

    #[test]
    fn signed_entry_with_bootstrapped_owner_reads_verified() {
        let out = classify_entries(vec![signed_entry()], &"01".repeat(32));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].trust, TrustLevel::Verified);
        assert!(out[0].trust.is_offerable());
    }

    #[test]
    fn signed_entry_with_placeholder_owner_reads_tofu() {
        let out = classify_entries(vec![signed_entry()], &placeholder_owner_pubkey());
        assert_eq!(out[0].trust, TrustLevel::UnverifiedOwner);
        assert!(
            out[0].trust.is_offerable(),
            "an unpinned registry still describes real, signed software"
        );
    }

    #[test]
    fn tampered_entry_is_untrusted_and_not_offerable() {
        let mut entry = signed_entry();
        entry.name = "hijacked".into();
        let out = classify_entries(vec![entry], &"01".repeat(32));
        assert_eq!(out[0].trust, TrustLevel::Untrusted);
        assert!(
            !out[0].trust.is_offerable(),
            "unsigned metadata must never become a row a user can activate"
        );
    }
}
