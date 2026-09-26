//! `EntryChangeRequest` — the wire shape a publisher POSTs to
//! `/entries` to add, update, or delete their entry.
//!
//! The publisher signs
//! `blake3(ENTRY_DOMAIN || postcard(change_signing_payload))`
//! where `change_signing_payload = (action, entry_signing_payload)`.
//! The registry server then verifies against the pubkey the
//! publisher-allowlist assigns to the entry (see
//! [`crate::verify::verify_change_request`]).
//!
//! Delete requests still carry the full entry — the server treats
//! that as the "last known state" the publisher intended to
//! remove, which the moderator can review. The server only acts
//! on `package_id + action`; the rest of the entry is there for
//! auditing.

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::entry::{ENTRY_DOMAIN, RegistryEntry};

/// What a change request asks the registry to do with an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryAction {
    /// Insert a new entry. Fails if `package_id` already exists.
    Add,
    /// Replace an existing entry. The `updated_at_unix` timestamp
    /// on the new entry MUST be > the stored one for the request
    /// to be accepted.
    Update,
    /// Remove an entry from the registry. The full entry is
    /// carried for the audit log.
    Delete,
    // Adding another variant is a wire break; bump the protocol
    // version.
}

/// A signed change request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryChangeRequest {
    /// What the publisher wants the registry to do.
    pub action: EntryAction,
    /// The entry payload. Its `signature_hex` is filled with the
    /// publisher's entry-level sig for downstream federation; the
    /// change-level sig sits on this struct instead.
    pub entry: RegistryEntry,
    /// Publisher's Ed25519 signature over
    /// `blake3(ENTRY_DOMAIN || postcard((action, entry_signing_payload)))`.
    /// Hex-encoded 64 bytes.
    pub publisher_signature_hex: String,
}

/// The canonical signing payload — action tag + every field of
/// the entry except its own signature.
#[derive(Serialize)]
struct ChangeSigningPayload<'a> {
    action: EntryAction,
    package_id: &'a str,
    name: &'a crate::entry::LocalizedText,
    description: &'a crate::entry::LocalizedText,
    tagline: &'a crate::entry::LocalizedText,
    images: &'a crate::entry::LocalizedImages,
    supported_languages: &'a [String],
    categories: &'a [String],
    source_url: &'a Option<String>,
    homepage: &'a Option<String>,
    publisher_pubkey_hex: &'a str,
    versions: &'a [crate::entry::RegistryVersion],
    updated_at_unix: u64,
}

impl EntryChangeRequest {
    /// The 32-byte BLAKE3 digest the publisher signs on the
    /// change (as distinct from the entry's own signature).
    pub fn signing_message(&self) -> [u8; 32] {
        change_signing_message(self.action, &self.entry)
    }
}

/// Compute the signing digest for a `(action, entry)` pair.
/// The entry's `signature_hex` field is deliberately ignored.
///
/// # Panics
///
/// Panics if `postcard::to_stdvec` fails to encode the payload. In
/// practice unreachable — every field is owned data with the derived
/// `Serialize` impl, and postcard only errors on a failing custom
/// `Serialize` impl or a sequence length overflowing its varint
/// encoding, neither of which applies here (see
/// [`crate::entry::entry_signing_message`]'s `# Panics` note).
#[allow(
    clippy::expect_used,
    reason = "postcard::to_stdvec cannot fail for this owned, derive(Serialize) payload — see \
              the # Panics note above; propagating a phantom Result buys no real safety here"
)]
pub fn change_signing_message(action: EntryAction, entry: &RegistryEntry) -> [u8; 32] {
    let payload = ChangeSigningPayload {
        action,
        package_id: &entry.package_id,
        name: &entry.name,
        description: &entry.description,
        tagline: &entry.tagline,
        images: &entry.images,
        supported_languages: &entry.supported_languages,
        categories: &entry.categories,
        source_url: &entry.source_url,
        homepage: &entry.homepage,
        publisher_pubkey_hex: &entry.publisher_pubkey_hex,
        versions: &entry.versions,
        updated_at_unix: entry.updated_at_unix,
    };
    let body = postcard::to_stdvec(&payload).expect("postcard encode change payload");
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTRY_DOMAIN);
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Build a fully-signed change request. `signing_key` is the
/// publisher's Ed25519 private key. Entry-level signature is
/// filled in-place by [`crate::entry::sign_entry`] so
/// downstream consumers can still verify the entry alone.
pub fn sign_change_request(
    action: EntryAction,
    mut entry: RegistryEntry,
    signing_key: &SigningKey,
) -> EntryChangeRequest {
    crate::entry::sign_entry(&mut entry, signing_key);
    let msg = change_signing_message(action, &entry);
    let sig: Signature = signing_key.sign(&msg);
    EntryChangeRequest {
        action,
        entry,
        publisher_signature_hex: hex::encode(sig.to_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{RegistryVersion, sign_entry};
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            name: "Editor".into(),
            description: "CRDT-native text editor".into(),
            tagline: "Edit together".into(),
            images: crate::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![RegistryVersion {
                version: "2026.06".into(),
                tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                tapp_root_hash_hex: "cd".repeat(32),
                tapp_size_bytes: 1024,
                released_at_unix: 100,
                tapp_blake3_hex: "ef".repeat(32),
                target: crate::entry::RegistryTarget {
                    platform: crate::entry::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: 200,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn change_requests_sign_and_serialise_canonically() {
        // phase: action_serde_lowercase_wire
        {
            for (a, expected) in [
                (EntryAction::Add, "\"add\""),
                (EntryAction::Update, "\"update\""),
                (EntryAction::Delete, "\"delete\""),
            ] {
                let s = serde_json::to_string(&a).unwrap();
                assert_eq!(s, expected);
                let back: EntryAction = serde_json::from_str(&s).unwrap();
                assert_eq!(back, a);
            }
        }
        // phase: signing_message_distinguishes_actions
        {
            let entry = sample_entry();
            let a = change_signing_message(EntryAction::Add, &entry);
            let u = change_signing_message(EntryAction::Update, &entry);
            let d = change_signing_message(EntryAction::Delete, &entry);
            assert_ne!(a, u);
            assert_ne!(u, d);
            assert_ne!(a, d);
        }
        // phase: change_request_json_roundtrip
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let req = sign_change_request(EntryAction::Add, sample_entry(), &key);
            let s = serde_json::to_string(&req).unwrap();
            let back: EntryChangeRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(back, req);
        }
        // phase: sign_change_request_also_signs_entry
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut entry = sample_entry();
            // Real workflow: publisher pubkey on the entry matches
            // the signing key; otherwise verify_entry_signature
            // (correctly) rejects.
            entry.publisher_pubkey_hex = hex::encode(key.verifying_key().to_bytes());
            let req = sign_change_request(EntryAction::Add, entry, &key);
            // Entry sig must be a real signature — 128 hex chars.
            assert_eq!(req.entry.signature_hex.len(), 128);
            // And it must verify against the same key it was signed with.
            assert!(crate::verify::verify_entry_signature(&req.entry));
        }
        // phase: change_signing_bytes_ignore_entry_signature_field
        {
            let mut e = sample_entry();
            let m1 = change_signing_message(EntryAction::Update, &e);
            // Even a nonsense value in signature_hex must NOT change the
            // change-level signing digest.
            e.signature_hex = "junk".into();
            let m2 = change_signing_message(EntryAction::Update, &e);
            assert_eq!(m1, m2);
            // But the entry sig hex used to be an assertion tool — resign
            // it with a fresh key and prove that too.
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            sign_entry(&mut e, &key);
            let m3 = change_signing_message(EntryAction::Update, &e);
            assert_eq!(m1, m3);
        }
    }
}
