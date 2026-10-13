//! Integration tests that pin down the canonical signing form —
//! the property publishers rely on for a signature they generated
//! yesterday to still verify after any client re-encodes the
//! entry.

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use registry_types::{
    EntryAction, EntryChangeRequest, EntryKind, RegistryEntry, RegistryVersion, TappArtifact,
    VersionArtifact, sign_change_request, sign_entry, verify_change_request,
    verify_entry_signature,
};

fn sample_entry(pubkey_hex: String) -> RegistryEntry {
    RegistryEntry {
        package_id: "so.tilo.editor".into(),
        kind: EntryKind::App,
        name: "Editor".into(),
        description: "CRDT-native text editor".into(),
        tagline: "Edit together".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Productivity".into(), "Text".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: pubkey_hex,
        versions: vec![RegistryVersion {
            version: "2026.06".into(),
            released_at_unix: 1_800_000_000,
            artifact: VersionArtifact::Tapp(TappArtifact {
                tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                tapp_root_hash_hex: "cd".repeat(32),
                tapp_size_bytes: 12_345,
                tapp_blake3_hex: "ef".repeat(32),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 1_800_000_100,
        signature_hex: String::new(),
    }
}

#[test]
fn signed_json_is_canonical_and_survives_reencoding() {
    // phase: entry_json_roundtrip_preserves_signature
    {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let mut entry = sample_entry(hex::encode(key.verifying_key().to_bytes()));
        sign_entry(&mut entry, &key);
        assert!(verify_entry_signature(&entry));

        let s = serde_json::to_string(&entry).unwrap();
        let back: RegistryEntry = serde_json::from_str(&s).unwrap();
        assert!(
            verify_entry_signature(&back),
            "signature must survive a JSON round-trip because postcard \
         is used for the signing form, not JSON"
        );
    }
    // phase: change_request_json_roundtrip_still_verifies
    {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let entry = sample_entry(hex::encode(key.verifying_key().to_bytes()));
        let req = sign_change_request(EntryAction::Add, entry, &key);
        let allow = [key.verifying_key().to_bytes()];

        let s = serde_json::to_string(&req).unwrap();
        let back: EntryChangeRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(verify_change_request(&back, &allow), Ok(()));
    }
    // phase: canonical_form_is_deterministic_across_two_processes_worth_of_encoders
    {
        // Same value → identical signing bytes. This is what makes
        // a detached signature portable across implementations.
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let entry = sample_entry(hex::encode(key.verifying_key().to_bytes()));
        let m1 = entry.signing_message();
        let m2 = entry.signing_message();
        assert_eq!(m1, m2);

        // Different entry → different signing bytes (sanity check).
        let mut other = entry;
        other.name = "Something Else".into();
        assert_ne!(other.signing_message(), m1);
    }
    // phase: signature_survives_field_reorder_in_json
    {
        // JSON key order is defined by the encoder; the signing form
        // is postcard so a client parsing JSON and re-serialising it
        // with a different key order must still verify.
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let mut entry = sample_entry(hex::encode(key.verifying_key().to_bytes()));
        sign_entry(&mut entry, &key);

        // Deserialise into a HashMap-backed serde_json::Value, then
        // re-serialise — that re-ordering happens naturally.
        let v: serde_json::Value = serde_json::to_value(&entry).unwrap();
        let s = serde_json::to_string(&v).unwrap();
        let reread: RegistryEntry = serde_json::from_str(&s).unwrap();
        assert!(verify_entry_signature(&reread));
    }
}
