//! `RegistryEntry` and the canonical signing form.
//!
//! The publisher signs `blake3(ENTRY_DOMAIN ||
//! postcard(entry_signing_payload))`, where `entry_signing_payload`
//! mirrors [`RegistryEntry`] *without* `signature_hex` — excluding it
//! avoids a fixed-point loop (the signature must not cover itself).
//!
//! Verifiers reconstruct the same bytes by peeling `signature_hex` off
//! the received entry, re-encoding, and re-hashing; since every other
//! field is included, any tamper (a swapped `qpkg_url`, a changed root
//! hash, an added category) flips the digest and fails verification.

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::Deserialize;
use serde::Serialize;

/// Domain-separation prefix for a registry-entry publisher signature.
pub const ENTRY_DOMAIN: &[u8] = b"qos-registry-entry-v1\0";

/// Domain-separation prefix for a registry-owner index signature; not
/// used in this crate but exposed for federation clients snapshotting registry state.
pub const INDEX_DOMAIN: &[u8] = b"qos-registry-index-v1\0";

/// One version of a .qpkg, with the download URL and the BLAKE3 root the client verifies after fetching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryVersion {
    /// Semver string, or `CalVer` YYYY.MM as some publishers use; treated
    /// as opaque text — client picks a "latest".
    pub version: String,
    /// HTTPS URL of the `.qpkg` file.
    pub qpkg_url: String,
    /// BLAKE3 hash of the .qpkg's signed root (same hash
    /// `pkg::PackageId` derives); clients recompute on download — a
    /// mismatch means bytes were tampered in transit.
    pub qpkg_root_hash_hex: String,
    /// Byte size of the .qpkg, for progress bars and integrity pre-flight.
    pub qpkg_size_bytes: u64,
    /// Unix seconds when the publisher released this version.
    pub released_at_unix: u64,
}

/// One package entry inside a registry — everything the client needs to render a store card and start a download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Reverse-DNS id from `pkg::Manifest.package.id`.
    pub package_id: String,
    /// Display name.
    pub name: String,
    /// Short description shown on the card.
    pub description: String,
    /// Free-form category tags — the UI groups on these.
    pub categories: Vec<String>,
    /// Optional icon URL (PNG or SVG).
    pub icon_url: Option<String>,
    /// Optional source-code URL.
    pub source_url: Option<String>,
    /// Optional project homepage.
    pub homepage: Option<String>,
    /// Ed25519 identity of the publisher (32 bytes, hex); matches
    /// `pkg::Manifest.publisher.identity` on every `qpkg_url` this entry advertises.
    pub publisher_pubkey_hex: String,
    /// All versions the publisher has released. Order-preserving.
    pub versions: Vec<RegistryVersion>,
    /// Unix seconds of the last publisher change — signed alongside the
    /// rest of the entry; used to order concurrent updates (POST takes the newer value).
    pub updated_at_unix: u64,
    /// Publisher's Ed25519 signature over `blake3(ENTRY_DOMAIN ||
    /// postcard(signing_payload))`, hex-encoded 64 bytes.
    pub signature_hex: String,
}

/// The canonical signing payload for a [`RegistryEntry`] — every
/// field *except* the signature itself.
///
/// A borrowed view (avoids cloning the entry). Postcard walks fields
/// in declaration order, so keep this shape aligned with
/// `RegistryEntry`'s fields (minus `signature_hex`).
#[derive(Serialize)]
struct EntrySigningPayload<'a> {
    package_id: &'a str,
    name: &'a str,
    description: &'a str,
    categories: &'a [String],
    icon_url: &'a Option<String>,
    source_url: &'a Option<String>,
    homepage: &'a Option<String>,
    publisher_pubkey_hex: &'a str,
    versions: &'a [RegistryVersion],
    updated_at_unix: u64,
}

impl RegistryEntry {
    /// The 32-byte BLAKE3 digest the publisher signs.
    pub fn signing_message(&self) -> [u8; 32] {
        entry_signing_message(self)
    }

    /// Detach the publisher signature, returning an entry with an empty
    /// `signature_hex` — for preparing an entry to sign or re-signing
    /// after a moderator edit. Verification uses [`signing_message`]
    /// directly and doesn't need this.
    pub fn without_signature(mut self) -> Self {
        self.signature_hex.clear();
        self
    }
}

/// Standalone form of [`RegistryEntry::signing_message`], as a free
/// function so verifier code can compute the digest without
/// constructing/mutating an entry.
pub fn entry_signing_message(entry: &RegistryEntry) -> [u8; 32] {
    let payload = EntrySigningPayload {
        package_id: &entry.package_id,
        name: &entry.name,
        description: &entry.description,
        categories: &entry.categories,
        icon_url: &entry.icon_url,
        source_url: &entry.source_url,
        homepage: &entry.homepage,
        publisher_pubkey_hex: &entry.publisher_pubkey_hex,
        versions: &entry.versions,
        updated_at_unix: entry.updated_at_unix,
    };
    // Postcard's to_stdvec never fails for owned inputs of this shape — unwrap is safe.
    let body = postcard::to_stdvec(&payload).expect("postcard encode entry payload");
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTRY_DOMAIN);
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Domain-separated index signing message, exposed for federation
/// clients snapshotting registry state for the owner to sign.
pub fn index_signing_message<T: Serialize>(index_manifest: &T) -> [u8; 32] {
    let body = postcard::to_stdvec(index_manifest).expect("postcard encode index manifest");
    let mut hasher = blake3::Hasher::new();
    hasher.update(INDEX_DOMAIN);
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Convenience: sign an entry in place, filling `signature_hex`; caller is responsible for the publisher's key.
pub fn sign_entry(entry: &mut RegistryEntry, signing_key: &SigningKey) {
    // Ensure the signing_message is computed over the payload
    // WITHOUT the signature field, so we clear it first.
    entry.signature_hex.clear();
    let msg = entry_signing_message(entry);
    let sig: Signature = signing_key.sign(&msg);
    entry.signature_hex = hex::encode(sig.to_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            package_id: "dev.qos.editor".into(),
            name: "Editor".into(),
            description: "CRDT-native text editor".into(),
            categories: vec!["Productivity".into(), "Text".into()],
            icon_url: Some("https://apps.qos.dev/editor/icon.png".into()),
            source_url: Some("https://code.podesta.ai/qos/editor".into()),
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![RegistryVersion {
                version: "2026.06".into(),
                qpkg_url: "https://apps.qos.dev/editor/2026.06.qpkg".into(),
                qpkg_root_hash_hex: "cd".repeat(32),
                qpkg_size_bytes: 12_345_678,
                released_at_unix: 1_800_000_000,
            }],
            updated_at_unix: 1_800_000_100,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn entry_json_roundtrip() {
        let e = sample_entry();
        let s = serde_json::to_string(&e).expect("encode");
        let back: RegistryEntry = serde_json::from_str(&s).expect("decode");
        assert_eq!(back, e);
    }

    #[test]
    fn signing_message_is_independent_of_signature_field() {
        let mut e = sample_entry();
        let m1 = e.signing_message();
        e.signature_hex = "deadbeef".into();
        let m2 = e.signing_message();
        assert_eq!(m1, m2, "signature_hex must not affect signing digest");
    }

    #[test]
    fn signing_message_changes_when_any_signed_field_changes() {
        let base = sample_entry();
        let base_msg = base.signing_message();

        {
            let mut alt = base.clone();
            alt.package_id = "dev.qos.other".into();
            assert_ne!(alt.signing_message(), base_msg);
        }
        {
            let mut alt = base.clone();
            alt.versions[0].qpkg_root_hash_hex = "ff".repeat(32);
            assert_ne!(alt.signing_message(), base_msg);
        }
        {
            let mut alt = base.clone();
            alt.updated_at_unix += 1;
            assert_ne!(alt.signing_message(), base_msg);
        }
        {
            let mut alt = base.clone();
            alt.categories.push("Extra".into());
            assert_ne!(alt.signing_message(), base_msg);
        }
    }

    #[test]
    fn sign_entry_fills_signature_field() {
        let mut e = sample_entry();
        let key = SigningKey::generate(&mut OsRng);
        sign_entry(&mut e, &key);
        assert_eq!(e.signature_hex.len(), 128, "64-byte hex-encoded sig");
        // Idempotent under re-sign with the same key + same payload.
        let sig1 = e.signature_hex.clone();
        sign_entry(&mut e, &key);
        assert_eq!(e.signature_hex, sig1);
    }

    #[test]
    fn index_signing_message_is_domain_separated() {
        let entry = sample_entry();
        let entry_msg = entry_signing_message(&entry);
        // Feeding the same postcard body through the *index*
        // domain must yield a different digest.
        let index_msg = index_signing_message(&entry);
        assert_ne!(entry_msg, index_msg);
    }
}
