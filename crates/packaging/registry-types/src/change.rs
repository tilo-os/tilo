//! The wire shape a publisher POSTs to `/entries` to add, update or delete its entry, signed as
//! `blake3(domain || postcard((action, entry_signing_payload)))` with the entry kind's domain.
//!
//! A delete carries the full entry as the last known state a moderator can review; the server acts on
//! `package_id` and `action` alone.

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::entry::RegistryEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryAction {
    /// Insert a new entry; fails if it exists.
    Add,
    /// Replace an entry; its `updated_at_unix` must be newer than the stored one.
    Update,
    /// Remove an entry; the full entry is carried for the audit log.
    Delete,
    // Adding another variant is a wire break; bump the protocol
    // version.
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryChangeRequest {
    pub action: EntryAction,
    /// The entry; its own signature is kept for federation, and is not what this request signs.
    pub entry: RegistryEntry,
    /// Ed25519 over `blake3(domain || postcard((action, entry_signing_payload)))`, hex.
    pub publisher_signature_hex: String,
}

/// The signing payload: the action tag and every entry field but the entry's own signature.
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
    /// The digest the publisher signs on the change, not the entry's own signature.
    pub fn signing_message(&self) -> [u8; 32] {
        change_signing_message(self.action, &self.entry)
    }
}

/// The signing digest for an `(action, entry)` pair; the entry's `signature_hex` is ignored.
///
/// # Panics
///
/// When postcard cannot encode the payload, which no field here can cause (see
/// [`crate::entry::entry_signing_message`]).
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
    hasher.update(entry.kind.signing_domain());
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Build a signed change request. The entry's own signature is filled in too, so a consumer can
/// still verify the entry alone.
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
    use crate::entry::{EntryKind, RegistryVersion, TappArtifact, VersionArtifact, sign_entry};
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            kind: EntryKind::App,
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
                released_at_unix: 100,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                    tapp_root_hash_hex: "cd".repeat(32),
                    tapp_size_bytes: 1024,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: crate::entry::RegistryTarget {
                        platform: crate::entry::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 200,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn change_requests_sign_and_serialise_canonically() {
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
        {
            let entry = sample_entry();
            let a = change_signing_message(EntryAction::Add, &entry);
            let u = change_signing_message(EntryAction::Update, &entry);
            let d = change_signing_message(EntryAction::Delete, &entry);
            assert_ne!(a, u);
            assert_ne!(u, d);
            assert_ne!(a, d);
        }
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let req = sign_change_request(EntryAction::Add, sample_entry(), &key);
            let s = serde_json::to_string(&req).unwrap();
            let back: EntryChangeRequest = serde_json::from_str(&s).unwrap();
            assert_eq!(back, req);
        }
        {
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            let mut entry = sample_entry();
            // Real workflow: publisher pubkey on the entry matches
            // the signing key; otherwise verify_entry_signature
            // (correctly) rejects.
            entry.publisher_pubkey_hex = hex::encode(key.verifying_key().to_bytes());
            // The entry's pubkey must match the signing key, or `verify_entry_signature` rejects it.
            let req = sign_change_request(EntryAction::Add, entry, &key);
            // Entry sig must be a real signature — 128 hex chars.
            assert_eq!(req.entry.signature_hex.len(), 128);
            // And it must verify against the same key it was signed with.
            assert!(crate::verify::verify_entry_signature(&req.entry));
        }
        {
            let mut e = sample_entry();
            let m1 = change_signing_message(EntryAction::Update, &e);
            // Even a nonsense value in signature_hex must NOT change the
            // change-level signing digest.
            e.signature_hex = "junk".into();
            let m2 = change_signing_message(EntryAction::Update, &e);
            assert_eq!(m1, m2);
            // A nonsense `signature_hex` must not change the change-level digest.
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            sign_entry(&mut e, &key);
            let m3 = change_signing_message(EntryAction::Update, &e);
            // Resigning the entry with another key leaves that digest alone too.
            assert_eq!(m1, m3);
        }
    }
}
