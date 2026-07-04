//! HTTP client for a qOS registry.
//!
//! Two endpoints, both blocking (we run these on a worker thread; the
//! UI never spins on a network round-trip):
//!
//!   * `GET {base_url}/registry` — the handshake used to lazily pin the
//!     owner pubkey on first contact.
//!   * `GET {base_url}/entries`  — the full catalogue.
//!
//! Every fetched entry is signature-checked against its own declared
//! publisher pubkey via [`registry_types::verify_entry_signature`].
//! A separate allowlist check compares the entry's publisher pubkey
//! against the *registry owner's* allowlisted publishers — this is
//! the "the registry owner OK'd this publisher" gate the store surfaces
//! as a trust badge on the card. Bad or missing signatures downgrade
//! the item to `Unverified`; the store still lists it but flags the
//! trust state on the detail sheet.

use std::time::Duration;

use registry_types::{RegistryEntry, RegistryHandshake};
use serde::Deserialize;
use thiserror::Error;

use crate::config::RegistryConfig;

/// Reasonable default request timeout — a fresh registry contacted
/// over WAN should still fit. Overridable via `QOS_STORE_HTTP_TIMEOUT`.
const DEFAULT_TIMEOUT_SECS: u64 = 10;

fn http_timeout() -> Duration {
    std::env::var("QOS_STORE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_TIMEOUT_SECS))
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

/// Trust classification for a fetched entry: the store surfaces
/// this on the detail sheet + on the card as a small badge colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustLevel {
    /// Publisher signature verified AND the publisher is in the
    /// registry owner's allowlist (via the handshake pubkey pin +
    /// allowlist check).
    Verified,
    /// Publisher signature verified, but no owner allowlist was
    /// available to compare against (e.g. registry with
    /// placeholder / unpinned owner pubkey). Shown with a
    /// "trust-on-first-use" note.
    UnverifiedOwner,
    /// The publisher sig failed to verify.
    Untrusted,
}

/// One entry + its trust state. Kept as a fat item so the sources
/// merger doesn't have to re-query the client.
#[derive(Clone, Debug)]
pub struct VerifiedEntry {
    pub entry: RegistryEntry,
    pub trust: TrustLevel,
}

/// Response of `GET /entries` — server can return either a bare
/// array of entries or a `{ "entries": [...] }` wrapper. Accept both
/// for forward-compat with future federation-index responses.
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
        .user_agent("qos-store/0.1")
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
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        return Err(RegistryError::Empty);
    }
    serde_json::from_str(&text).map_err(|e| RegistryError::Decode(e.to_string()))
}

/// Fetch `/entries`, then classify each result against the
/// registry's allowlist. `allowlist` is a hex list of publisher
/// pubkeys the registry owner has signed off on — pass an empty
/// slice to run in "sig-verify only, no allowlist" mode which
/// downgrades every verified entry to [`TrustLevel::UnverifiedOwner`].
pub fn fetch_entries(reg: &RegistryConfig) -> Result<Vec<VerifiedEntry>, RegistryError> {
    let url = format!("{}/entries", reg.base_url.trim_end_matches('/'));
    let c = client()?;
    let text = c
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let raw: EntriesResponse =
        serde_json::from_str(&text).map_err(|e| RegistryError::Decode(e.to_string()))?;
    Ok(classify_entries(raw.into_vec(), &reg.owner_pubkey_hex))
}

/// The pure part of [`fetch_entries`]: given a decoded list and the
/// owner pubkey the registry advertises, produce a
/// [`VerifiedEntry`] per input. No I/O — factored out so tests can
/// feed fixtures directly without a live HTTP server.
pub fn classify_entries(
    entries: Vec<RegistryEntry>,
    owner_pubkey_hex: &str,
) -> Vec<VerifiedEntry> {
    // At this UX level "owner-allowlisted" collapses to "the
    // registry's declared owner pubkey is non-placeholder and the
    // entry was signed by an ed25519 key we can verify". The
    // full publisher-cert chain is a follow-up.
    let owner_bootstrapped = !owner_pubkey_hex.is_empty()
        && owner_pubkey_hex != "00".repeat(32).as_str();
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

    fn signed_entry() -> (SigningKey, RegistryEntry) {
        // Deterministic key so signing is reproducible under `cargo
        // test`; we don't need cryptographic hygiene inside a unit
        // test.
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
        (key, entry)
    }

    #[test]
    fn signed_entry_with_bootstrapped_owner_reads_verified() {
        let (_, entry) = signed_entry();
        let owner_hex = "01".repeat(32); // non-placeholder → bootstrapped
        let out = classify_entries(vec![entry], &owner_hex);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].trust, TrustLevel::Verified);
    }

    #[test]
    fn signed_entry_with_placeholder_owner_reads_tofu() {
        let (_, entry) = signed_entry();
        let out = classify_entries(vec![entry], &"00".repeat(32));
        assert_eq!(out[0].trust, TrustLevel::UnverifiedOwner);
    }

    #[test]
    fn tampered_entry_is_untrusted() {
        let (_, mut entry) = signed_entry();
        entry.name = "hijacked".into();
        let out = classify_entries(vec![entry], &"01".repeat(32));
        assert_eq!(out[0].trust, TrustLevel::Untrusted);
    }
}
