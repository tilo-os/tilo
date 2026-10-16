use super::*;
use tilo_app_package::trust::{AttestorScope, RevokedKey};

#[path = "fixture.rs"]
mod fixture;
use fixture::fixture;

#[test]
fn local_package_trusted_unlisted_full_and_thin_have_same_signed_identity() {
    let (full, thin, trust, _) = fixture();
    let full = review_with_trust(&full, &trust, &RevocationList::default()).unwrap();
    let thin = review_with_trust(&thin, &trust, &RevocationList::default()).unwrap();
    assert_eq!(full.root, thin.root);
    assert_eq!(full.manifest.package.id, "so.tilo.unlisted-local");
    assert_eq!(full.attestor, "existing-local-fixture");
    assert_ne!(full.archive_hash, thin.archive_hash);
}

#[test]
fn local_package_refuses_untrusted_revoked_wrong_scope_and_carried_tamper() {
    let (full, _, mut trust, publisher) = fixture();
    let original_trust = trust.clone();
    assert!(
        review_with_trust(
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
    assert!(review_with_trust(&full, &trust, &revoked).is_err());
    trust.attestor.first_mut().unwrap().scope = AttestorScope::Prefix(vec!["org.other.".into()]);
    assert!(review_with_trust(&full, &trust, &RevocationList::default()).is_err());

    // Exact carried bytes are inside tar framing; corrupt the real payload,
    // keeping the metadata and its signature intact.
    let mut damaged = full;
    let marker = b"local signed payload";
    let index = damaged
        .windows(marker.len())
        .position(|bytes| bytes == marker)
        .unwrap();
    damaged[index] ^= 1;
    assert!(
        review_with_trust(&damaged, &original_trust, &RevocationList::default())
            .unwrap_err()
            .contains("does not match its name")
    );
}
