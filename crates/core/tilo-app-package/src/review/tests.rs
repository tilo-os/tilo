use ed25519_dalek::{Signer, SigningKey};

use super::*;
use crate::trust::{AttestorScope, RevokedKey, TrustedAttestor, attestation_preimage};
use crate::{SigningIdentity, TrustAttestation, pack};

/// A local app signed by its publisher and attested by `existing-local-fixture`, as full and
/// thin archives, with the trust that accepts it and the publisher's key.
fn fixture() -> (Vec<u8>, Vec<u8>, TrustedAttestors, String) {
    let publisher = SigningKey::from_bytes(&[41; 32]);
    let attestor = SigningKey::from_bytes(&[42; 32]);
    let payload = tempfile::tempdir().unwrap();
    std::fs::create_dir(payload.path().join("bin")).unwrap();
    std::fs::write(payload.path().join("bin/local"), b"local signed payload").unwrap();
    let key = hex::encode(publisher.verifying_key().to_bytes());
    let mut manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.unlisted-local"
name = "Trusted local fixture"
version = "1.0.0"
kind = "app"
[publisher]
identity = "ed25519:{key}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/local"
splash-segments = []
splash-budget = 0
"#
    ))
    .unwrap();
    let identity = SigningIdentity::from_signing_key(publisher.clone());
    let initial = pack(payload.path(), &manifest, &identity).unwrap();
    let preimage = attestation_preimage(
        &initial.meta.root.payload_root_hash,
        &publisher.verifying_key().to_bytes(),
    );
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: "existing-local-fixture".into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(attestor.sign(&preimage).to_bytes()),
    });
    let packed = pack(payload.path(), &manifest, &identity).unwrap();
    let mut full = Vec::new();
    let mut thin = Vec::new();
    crate::tapp::write(&mut full, &packed.meta, Some(&packed)).unwrap();
    crate::tapp::write(&mut thin, &packed.meta, None).unwrap();
    let trust = TrustedAttestors {
        attestor: vec![TrustedAttestor {
            id: "existing-local-fixture".into(),
            pubkey_hex: hex::encode(attestor.verifying_key().to_bytes()),
            scope: AttestorScope::Prefix(vec!["so.tilo.unlisted-".into()]),
        }],
    };
    (full, thin, trust, key)
}

/// Full and thin forms of one attested package review as the same signed identity.
#[test]
fn an_attested_package_reviews_the_same_full_or_thin() {
    let (full, thin, trust, _) = fixture();
    let full = review_attested(&full, &trust, &RevocationList::default()).unwrap();
    let thin = review_attested(&thin, &trust, &RevocationList::default()).unwrap();
    assert_eq!(full.root, thin.root);
    assert_eq!(full.manifest.package.id, "so.tilo.unlisted-local");
    assert_eq!(full.attestor, "existing-local-fixture");
    assert_ne!(full.archive_hash, thin.archive_hash);
    let expected = full.expectation(42).unwrap();
    assert_eq!(expected.package_root.to_hex(), full.root);
    assert_eq!(hex::encode(expected.publisher), full.publisher);
    assert_eq!(expected.size_bytes, 42);
}

/// No trusted attestor, a revoked publisher, an attestor outside its scope and a payload that
/// does not match its signed name are each refused.
#[test]
fn an_unattested_revoked_out_of_scope_or_tampered_package_is_refused() {
    let (full, _, mut trust, publisher) = fixture();
    let original_trust = trust.clone();
    assert!(
        review_attested(
            &full,
            &TrustedAttestors::default(),
            &RevocationList::default()
        )
        .is_err()
    );
    let revoked = RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: publisher,
            reason: "fixture revoked".into(),
        }],
    };
    assert!(review_attested(&full, &trust, &revoked).is_err());
    trust.attestor.first_mut().unwrap().scope = AttestorScope::Prefix(vec!["org.other.".into()]);
    assert!(review_attested(&full, &trust, &RevocationList::default()).is_err());

    // The carried bytes are inside tar framing: corrupt the real payload, keeping the metadata
    // and its signature intact.
    let mut damaged = full;
    let marker = b"local signed payload";
    let index = damaged
        .windows(marker.len())
        .position(|bytes| bytes == marker)
        .unwrap();
    damaged[index] ^= 1;
    assert!(
        review_attested(&damaged, &original_trust, &RevocationList::default())
            .unwrap_err()
            .contains("does not match its name")
    );
}

/// An archive is read as an application only, and accepts only an offer that names every part
/// of its signed identity.
#[test]
fn an_archive_accepts_only_the_offer_that_names_it() {
    let (full, _, _, publisher) = fixture();
    assert_eq!(
        review_archive(&[]).unwrap_err(),
        ArchiveReviewError::Invalid
    );
    assert_eq!(
        review_archive(b"not a package").unwrap_err(),
        ArchiveReviewError::Invalid
    );
    let archive = review_archive(&full).unwrap();
    assert_eq!(archive.app_id(), "so.tilo.unlisted-local");
    let decoded = crate::tapp::read(full.as_slice(), &mut DiscardBlobs).unwrap();
    let key: [u8; 32] = hex::decode(publisher).unwrap().try_into().unwrap();
    let offer = InstallExpectation {
        package_root: decoded.meta.id,
        publisher: key,
        app_id: "so.tilo.unlisted-local".into(),
        version: "1.0.0".into(),
        size_bytes: full.len() as u64,
    };
    assert!(archive.accepts_offer(&offer));
    let mut others = Vec::new();
    let mut other = offer.clone();
    other.publisher = [9; 32];
    others.push(other);
    let mut other = offer.clone();
    other.app_id = "so.tilo.other".into();
    others.push(other);
    let mut other = offer.clone();
    other.version = "2.0.0".into();
    others.push(other);
    let mut other = offer.clone();
    other.size_bytes = 0;
    others.push(other);
    let mut other = offer;
    other.package_root = PackageId::from_bytes([7; 32]);
    others.push(other);
    for other in others {
        assert!(!archive.accepts_offer(&other), "{other:?}");
    }
}
