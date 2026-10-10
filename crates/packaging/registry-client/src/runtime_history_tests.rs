use ed25519_dalek::SigningKey;
use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion};

use crate::runtime_history::reconcile;

fn version(value: &str, architecture: &str, build: u8) -> RegistryVersion {
    RegistryVersion {
        version: value.into(),
        tapp_url: format!("https://apps.tilo.so/artifacts/editor/{value}-{architecture}.tapp"),
        tapp_root_hash_hex: format!("{build:064x}"),
        tapp_size_bytes: 12_288 + u64::from(build),
        released_at_unix: 1_790_000_000 + u64::from(build),
        tapp_blake3_hex: format!("{:064x}", u16::from(build) + 256),
        target: RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: architecture.into(),
            isa: architecture.into(),
            runtime: Some(format!("tilo-runtime/2026.07+native.{build:032x}")),
            interfaces: Vec::new(),
        },
    }
}

fn signed_entry(id: &str, versions: Vec<RegistryVersion>, key: &SigningKey) -> RegistryEntry {
    let mut entry = RegistryEntry {
        package_id: id.into(),
        name: "Code".into(),
        description: "A native code editor".into(),
        tagline: "Edit together".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Development".into()],
        source_url: Some("https://code.tilo.so/tilo/tilo".into()),
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions,
        updated_at_unix: 1_790_000_100,
        signature_hex: String::new(),
    };
    assert!(entry.validate().is_ok(), "fixture must be a valid entry");
    registry_types::sign_entry(&mut entry, key);
    assert!(registry_types::verify_entry_signature(&entry));
    entry
}

fn offered(key: &SigningKey, versions: &[RegistryVersion]) -> RegistryEntry {
    signed_entry("so.tilo.runtime", versions.to_vec(), key)
}

fn six_releases() -> Vec<RegistryVersion> {
    (2..=7)
        .map(|n| version(&format!("1.0.{n}"), "aarch64", n))
        .collect()
}

#[test]
fn delayed_code_drops_only_unpublished_orphan_and_preserves_signed_artifacts() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let installable = six_releases();
    let orphan = version("1.0.0", "aarch64", 10);
    let published_orphan = version("0.9.0", "aarch64", 11);
    let mut versions = installable.clone();
    versions.extend([orphan.clone(), published_orphan.clone()]);
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    let before = entry.clone();
    let catalogue = vec![
        offered(&key, &installable),
        signed_entry("so.tilo.code-editor", vec![published_orphan.clone()], &key),
    ];
    assert_eq!(
        reconcile(
            &mut entry,
            &catalogue,
            "https://apps.tilo.so/current.tapp",
            &key
        )
        .ok(),
        Some(vec![orphan])
    );
    let mut expected = before.clone();
    expected.versions = installable;
    expected.versions.push(published_orphan);
    registry_types::sign_entry(&mut expected, &key);
    // Whole-entry equality covers metadata, target, URL, hashes and size.
    assert_eq!(entry, expected);
    assert_ne!(entry.signature_hex, before.signature_hex);
    assert!(registry_types::verify_entry_signature(&entry));
    let unchanged = entry.clone();
    assert_eq!(
        reconcile(&mut entry, &catalogue, "", &key).ok(),
        Some(Vec::new())
    );
    assert_eq!(entry, unchanged);
}

#[test]
fn insufficient_offered_rollback_history_fails_atomically_after_an_eligible_drop() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let installable: Vec<_> = (2..=5)
        .map(|n| version(&format!("1.0.{n}"), "aarch64", n))
        .collect();
    let mut versions = installable.clone();
    // The first orphan has four newer offered versions, the second only three.
    versions.extend([
        version("1.0.0", "aarch64", 10),
        version("1.0.2+unoffered", "aarch64", 11),
    ]);
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    let before = entry.clone();
    assert!(reconcile(&mut entry, &[offered(&key, &installable)], "", &key).is_err());
    assert_eq!(entry, before);
    assert!(registry_types::verify_entry_signature(&entry));
}

#[test]
fn other_architectures_channels_and_platforms_cannot_supply_rollback_slots() {
    let key = SigningKey::from_bytes(&[7; 32]);
    for extra in [
        (2..=5)
            .map(|n| version(&format!("1.0.{n}"), "x86_64", n))
            .collect::<Vec<_>>(),
        (2..=5)
            .map(|n| version(&format!("2.0.0-dev.{n}"), "aarch64", n))
            .collect(),
    ] {
        let mut versions = extra.clone();
        versions.push(version("1.0.0", "aarch64", 10));
        let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
        let before = entry.clone();
        assert!(reconcile(&mut entry, &[offered(&key, &extra)], "", &key).is_err());
        assert_eq!(entry, before);
    }
    let installable = six_releases();
    let mut foreign = installable.clone();
    for v in &mut foreign {
        v.target.platform = "linux".into();
    }
    let mut versions = installable;
    versions.push(version("1.0.0", "aarch64", 10));
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    let before = entry.clone();
    assert!(reconcile(&mut entry, &[offered(&key, &foreign)], "", &key).is_err());
    assert_eq!(entry, before);
}

#[test]
fn current_artifact_is_protected_even_without_an_offered_runtime() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let installable = six_releases();
    let current = version("1.0.1", "aarch64", 10);
    let orphan = version("1.0.0", "aarch64", 11);
    let mut versions = installable.clone();
    versions.extend([current.clone(), orphan.clone()]);
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    assert_eq!(
        reconcile(
            &mut entry,
            &[offered(&key, &installable)],
            &current.tapp_url,
            &key
        )
        .ok(),
        Some(vec![orphan])
    );
    assert!(entry.versions.contains(&current));
    assert!(registry_types::verify_entry_signature(&entry));
}

#[test]
fn published_version_protection_requires_the_exact_signed_artifact_identity() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let installable = six_releases();
    let published = version("1.0.0", "aarch64", 10);
    let mut replacement = published.clone();
    replacement.tapp_blake3_hex = "ab".repeat(32);
    let mut versions = installable.clone();
    versions.push(replacement.clone());
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    let catalogue = vec![
        offered(&key, &installable),
        signed_entry("so.tilo.code-editor", vec![published], &key),
    ];
    assert_eq!(
        reconcile(&mut entry, &catalogue, "", &key).ok(),
        Some(vec![replacement])
    );
    assert_eq!(entry.versions, installable);
    assert!(registry_types::verify_entry_signature(&entry));
}

#[test]
fn wrong_signer_and_tampered_signature_fail_without_changing_entry() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let mut entry = signed_entry("so.tilo.code-editor", six_releases(), &key);
    let before = entry.clone();
    assert!(reconcile(&mut entry, &[], "", &SigningKey::from_bytes(&[8; 32])).is_err());
    assert_eq!(entry, before);
    entry.description = "Tampered after signing".into();
    let tampered = entry.clone();
    assert!(!registry_types::verify_entry_signature(&entry));
    assert!(reconcile(&mut entry, &[], "", &key).is_err());
    assert_eq!(entry, tampered);
}

#[test]
fn runtime_provider_history_is_not_reconciled_or_resigned() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let mut entry = signed_entry("so.tilo.runtime", six_releases(), &key);
    let before = entry.clone();
    assert_eq!(reconcile(&mut entry, &[], "", &key).ok(), Some(Vec::new()));
    assert_eq!(entry, before);
}

#[test]
fn architecture_fold_preserves_the_whole_current_release_before_sdk_submission() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let mut installable = six_releases();
    installable.extend((2..=7).map(|n| version(&format!("1.0.{n}"), "x86_64", n)));
    let arm = version("1.1.0", "aarch64", 20);
    let x86 = version("1.1.0", "x86_64", 21);
    let orphan = version("0.9.0", "aarch64", 22);
    let mut versions = installable.clone();
    versions.extend([arm.clone(), x86.clone(), orphan.clone()]);
    let mut entry = signed_entry("so.tilo.code-editor", versions, &key);
    let catalogue = vec![offered(&key, &installable)];
    assert_eq!(
        reconcile(&mut entry, &catalogue, &x86.tapp_url, &key).ok(),
        Some(vec![orphan])
    );
    assert!(entry.versions.contains(&arm));
    assert!(entry.versions.contains(&x86));
    assert!(registry_types::verify_entry_signature(&entry));
}
