use super::*;
use ed25519_dalek::SigningKey;

#[test]
fn sealed_metadata_storage_lends_actual_verified_roster_and_revocation_union() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let roster =
        roster::BoundedRoster::restore(&peer.certificates(), &[], peer.authority(), now_ns(now))
            .unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: member.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let revoked = revocations::BoundedRevocations::restore(
        &[entry],
        &RevocationList::new(),
        &roster,
        peer.authority().device_public_key,
    )
    .unwrap();
    let verified = BoundedNative::certificate(&roster, &member.device_public_key).unwrap();
    assert_eq!(verified.current_typed(now_ns(now)).unwrap(), &member);
    assert!(std::ptr::eq(
        verified.as_ref(),
        roster.certificate(&member.device_public_key).unwrap()
    ));
    assert!(BoundedNative::is_revoked(
        &revoked,
        &member.device_public_key
    ));
    assert!(!BoundedNative::is_revoked(
        &revoked,
        &peer.authority().device_public_key
    ));
    assert!(BoundedNative::certificate(&roster, &[99; 32]).is_none());
}

#[test]
fn sealed_metadata_storage_lends_real_v5_exact_facts_without_decoded_map_escape() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    peer.select_admission_v5().unwrap();
    let current_document = peer.document("test.app", "tab:1").unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = peer
        .publication_mut()
        .publish_document("test.app", "tab:1", current_document, &key, now)
        .unwrap()
    else {
        panic!("real signed publication");
    };
    let admission = peer
        .lookup_exact_local_document("test.app", "tab:1", &envelope, now)
        .unwrap()
        .unwrap();
    let limits = crate::resources::OuterLimits {
        allocation_bytes: 1024 * 1024,
        collection_entries: 65536,
    };
    let held = durable::DecodedDurable::decode(&peer.durable().unwrap(), limits, limits).unwrap();
    let exact = held.exact.as_ref().unwrap();
    assert!(
        exact
            .iter()
            .any(|(key, id)| key == &admission.key && id == &admission.operation_id)
    );
    for (key, id) in exact.iter() {
        assert_eq!(exact.get(key), Some(id));
    }
}
