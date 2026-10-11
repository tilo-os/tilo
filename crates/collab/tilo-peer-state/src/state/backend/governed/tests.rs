use super::*;
use crate::StatePeer;
use crate::resources::native::NativeBatch;
use crate::room;
use crate::state::err;
use automerge::{ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;

fn limits() -> ImportLimits {
    ImportLimits {
        collection_bytes: 1024 * 1024,
        certificate_bytes: 1024 * 1024,
        json: JsonLimits {
            allocation_bytes: 128 * 1024,
            collection_entries: 4096,
        },
    }
}

#[test]
fn bounded_governed_import_matches_real_legacy_import_and_retains_fresh_certificates() {
    let (mut peer, _, _, member, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let (_, identities) = room
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap();
    let mut added = member.clone();
    added.device_public_key = SigningKey::from_bytes(&[69; 32]).verifying_key().to_bytes();
    added.device_label = "governed long label 🦀".repeat(512);
    tilo_user_identity::sign_device_cert(&mut added, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let row = GovernedIdentity {
        device_cert: added.clone(),
        revocations: vec![],
        admission: None,
        admissions: vec![],
        workspace_delegations: vec![],
        agent: None,
    };
    room.put(
        &identities,
        hex::encode(added.device_public_key),
        serde_json::to_vec(&row).unwrap(),
    )
    .unwrap();
    room.commit();
    let certificates =
        roster::BoundedRoster::restore(&peer.certificates(), &[], peer.authority(), now_ns(now))
            .unwrap();
    let revocations = revocations::BoundedRevocations::restore(
        &[],
        &RevocationList::new(),
        &certificates,
        peer.authority().device_public_key,
    )
    .unwrap();
    let native = NativeBatch::new(8 * 1024 * 1024)
        .load(&room.save())
        .unwrap();
    let read = native.read();
    let imported = bounded(
        &read,
        &certificates,
        &revocations,
        peer.authority(),
        now,
        limits(),
    )
    .unwrap();
    let changed = original_import(&mut peer, &room, now).unwrap();
    assert_eq!(imported.changed, changed);
    for cert in peer.certificates() {
        assert_eq!(
            imported.certificates.certificate(&cert.device_public_key),
            Some(&cert)
        );
    }
    assert_ne!(
        imported
            .certificates
            .certificate(&member.device_public_key)
            .unwrap()
            .device_label
            .as_ptr(),
        certificates
            .certificate(&member.device_public_key)
            .unwrap()
            .device_label
            .as_ptr()
    );
    drop(native);
    drop(certificates);
    drop(revocations);
    drop(peer);
    drop(room);
    assert_eq!(
        imported.certificates.certificate(&added.device_public_key),
        Some(&added)
    );
}

#[test]
fn bounded_governed_import_ignores_legacy_malformed_identity_container() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    room.put(automerge::ROOT, "peer_identities_v1", "not a map")
        .unwrap();
    room.commit();
    let certificates =
        roster::BoundedRoster::restore(&peer.certificates(), &[], peer.authority(), now_ns(now))
            .unwrap();
    let revocations = revocations::BoundedRevocations::restore(
        &[],
        &RevocationList::new(),
        &certificates,
        peer.authority().device_public_key,
    )
    .unwrap();
    let native = NativeBatch::new(8 * 1024 * 1024)
        .load(&room.save())
        .unwrap();
    let read = native.read();
    let imported = bounded(
        &read,
        &certificates,
        &revocations,
        peer.authority(),
        now,
        limits(),
    )
    .unwrap();
    assert!(!imported.changed);
}

// Literal pre-extraction production algorithm; intentionally unqualified test oracle.
fn original_import(
    peer: &mut StatePeer,
    doc: &AutoCommit,
    now: SystemTime,
) -> Result<bool, String> {
    let mut changed = false;
    let mut certificates = peer.core.certificates.clone();
    let mut revocations = peer.core.revocations.clone();
    let identities = room::governance_of(Some(doc))
        .identities
        .into_iter()
        .map(|(id, bytes)| {
            serde_json::from_slice::<GovernedIdentity>(&bytes)
                .map(|identity| (id, identity))
                .map_err(err)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut redemptions = BTreeMap::new();
    for (key, values) in room::invitation_receipts(doc)? {
        for bytes in values {
            let receipt: RedemptionReceipt = serde_json::from_slice(&bytes).map_err(err)?;
            if key != hex::encode(receipt.invite_id) {
                return Err("governed invitation receipt key mismatch".into());
            }
            if let Some(existing) = redemptions.insert(receipt.invite_id, receipt.clone())
                && existing != receipt
            {
                return Err("governed invitation has conflicting redemption receipts".into());
            }
        }
    }
    for (_, identity) in &identities {
        for receipt in identity.admission.iter().chain(&identity.admissions) {
            if let Some(existing) = redemptions.insert(receipt.invite_id, receipt.clone())
                && existing != *receipt
            {
                return Err("governed invitation has conflicting redemption receipts".into());
            }
        }
    }
    for (id, identity) in identities {
        let has_admission = identity.has_admission();
        let cert = identity.device_cert;
        if id != hex::encode(cert.device_public_key) {
            return Err("governed identity key mismatch".into());
        }
        let verified = if let Some(known) = certificates.get(&cert.device_public_key) {
            if known.as_ref() != &cert {
                return Err("certificate conflict".into());
            }
            known.clone()
        } else {
            VerifiedCertificate::new(cert.clone(), now_ns(now))?
        };
        // Same-root device enrollment remains supported. Independent roots
        // must carry the durable receipt written by invitation redemption.
        if !peer.core.certificates.contains_key(&cert.device_public_key)
            && cert.user_public_key != peer.core.authority.user_public_key
            && !has_admission
        {
            return Err("independent identity is missing governed admission receipt".into());
        }
        for entry in identity.revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err("revocation root mismatch".into());
            }
            if certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|target| target.user_public_key != entry.user_public_key)
            {
                return Err("revocation cannot cross identity roots".into());
            }
            changed |= revocations.insert(entry).map_err(err)?;
        }
        changed |= certificates.get(&cert.device_public_key).map(AsRef::as_ref) != Some(&cert);
        certificates.insert(cert.device_public_key, verified);
    }
    peer.core.certificates = certificates;
    peer.core.revocations = revocations;
    Ok(changed)
}

#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts parity with the original before returning the value"
)]
fn compare_case(
    peer: &StatePeer,
    room: &AutoCommit,
    now: SystemTime,
) -> Result<Imported, ImportError> {
    let mut oracle = peer.clone();
    let mut actual = peer.clone();
    let expected = original_import(&mut oracle, room, now);
    let got = actual.import_governed_certificates(room, now);
    assert_eq!(got, expected);
    assert_eq!(actual.certificates(), oracle.certificates());
    assert_eq!(actual.revocations(), oracle.revocations());
    let certificates =
        roster::BoundedRoster::restore(&peer.certificates(), &[], peer.authority(), now_ns(now))
            .unwrap();
    let revocations = revocations::BoundedRevocations::restore(
        &peer.revocations(),
        &RevocationList::new(),
        &certificates,
        peer.authority().device_public_key,
    )
    .unwrap();
    let mut saved = room.clone();
    let native = NativeBatch::new(8 * 1024 * 1024)
        .load(&saved.save())
        .unwrap();
    let read = native.read();
    let result = bounded(
        &read,
        &certificates,
        &revocations,
        peer.authority(),
        now,
        limits(),
    );
    match (&result, &expected) {
        (Ok(imported), Ok(changed)) => {
            assert_eq!(imported.changed, *changed);
            for cert in oracle.certificates() {
                assert_eq!(
                    imported.certificates.certificate(&cert.device_public_key),
                    Some(&cert)
                );
            }
            let mut revocations = imported.revocations.iter().cloned().collect::<Vec<_>>();
            revocations.sort_by_key(|entry| entry.revoked_device_public_key);
            assert_eq!(revocations, oracle.revocations());
        }
        (Err(ImportError::Decode(PeerResourceError::Malformed)), Err(_)) => {}
        (Err(error), Err(expected)) => assert_eq!(&error.to_string(), expected),
        _ => panic!("native/legacy import outcome mismatch"),
    }
    result
}
fn set_identity(room: &mut AutoCommit, id: &str, row: &GovernedIdentity) {
    let (_, object) = room
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap();
    room.put(object, id, serde_json::to_vec(row).unwrap())
        .unwrap();
    room.commit();
}
fn row(cert: DeviceCert) -> GovernedIdentity {
    GovernedIdentity {
        device_cert: cert,
        revocations: vec![],
        admission: None,
        admissions: vec![],
        workspace_delegations: vec![],
        agent: None,
    }
}

#[test]
fn governed_import_preserves_real_signed_identity_admission_and_certificate_failures() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let original = peer.admitted_room().unwrap();
    let foreign_root = SigningKey::from_bytes(&[83; 32]);
    let mut foreign = member.clone();
    foreign.user_public_key = foreign_root.verifying_key().to_bytes();
    foreign.device_public_key = SigningKey::from_bytes(&[84; 32]).verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut foreign, &foreign_root).unwrap();
    let mut room = original.clone();
    let mut identity = row(foreign.clone());
    set_identity(
        &mut room,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::MissingAdmission)
    ));
    identity.admission = Some(RedemptionReceipt {
        invite_id: [7; 32],
        user_public_key: foreign.user_public_key,
        device_public_key: foreign.device_public_key,
        request_id: [8; 32],
        role: crate::join::MemberRole::Viewer,
    });
    set_identity(
        &mut room,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    compare_case(&peer, &room, now).unwrap();
    identity.device_cert.signature[0] ^= 1;
    set_identity(
        &mut room,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::Certificate(DeviceCertError::BadSignature))
    ));
    let mut room = original.clone();
    let mut changed = member.clone();
    changed.device_label.push_str(" changed");
    tilo_user_identity::sign_device_cert(&mut changed, &SigningKey::from_bytes(&[1; 32])).unwrap();
    set_identity(
        &mut room,
        &hex::encode(member.device_public_key),
        &row(changed),
    );
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::CertificateConflict)
    ));
    let mut room = original.clone();
    set_identity(&mut room, "WRONG KEY", &row(member));
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::IdentityKey)
    ));
}

#[test]
fn governed_import_preserves_all_receipt_shapes_and_error_order() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let ledger = room
        .get(automerge::ROOT, "invitation_receipts_v1")
        .unwrap()
        .unwrap()
        .1;
    room.put(&ledger, "a", vec![b'{']).unwrap();
    room.put(&ledger, "z", "not bytes").unwrap();
    room.commit();
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::ReceiptShape)
    ));
    // All raw shapes precede receipt JSON decoding, while identity JSON
    // decoding precedes every receipt shape check.
    let identities = room
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap()
        .1;
    room.put(
        &identities,
        hex::encode(member.device_public_key),
        vec![b'{'],
    )
    .unwrap();
    room.commit();
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::Decode(PeerResourceError::Malformed))
    ));
    let mut room = peer.admitted_room().unwrap().clone();
    let receipt = RedemptionReceipt {
        invite_id: [19; 32],
        user_public_key: member.user_public_key,
        device_public_key: member.device_public_key,
        request_id: [9; 32],
        role: crate::join::MemberRole::Editor,
    };
    let ledger = room
        .get(automerge::ROOT, "invitation_receipts_v1")
        .unwrap()
        .unwrap()
        .1;
    room.put(
        &ledger,
        hex::encode(receipt.invite_id),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let mut identity = row(member.clone());
    let mut conflict = receipt;
    conflict.request_id[0] ^= 1;
    identity.admissions.push(conflict);
    set_identity(&mut room, &hex::encode(member.device_public_key), &identity);
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::ReceiptConflict)
    ));
    for kind in [automerge::ObjType::List, automerge::ObjType::Text] {
        let mut room = peer.admitted_room().unwrap().clone();
        room.put_object(automerge::ROOT, "invitation_receipts_v1", kind)
            .unwrap();
        room.commit();
        assert!(matches!(
            compare_case(&peer, &room, now),
            Err(ImportError::LedgerShape)
        ));
        let mut room = peer.admitted_room().unwrap().clone();
        room.put_object(automerge::ROOT, "peer_identities_v1", kind)
            .unwrap();
        room.commit();
        compare_case(&peer, &room, now).unwrap();
    }
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn governed_import_keeps_verified_earliest_revocations_and_refuses_bad_root_or_signature() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let mut identity = row(member.clone());
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: member.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: 20,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    identity.revocations.push(entry.clone());
    entry.revoked_at_ns = 10;
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    identity.revocations.push(entry.clone());
    let mut room = peer.admitted_room().unwrap().clone();
    set_identity(&mut room, &hex::encode(member.device_public_key), &identity);
    let imported = compare_case(&peer, &room, now).unwrap();
    assert_eq!(
        imported
            .revocations
            .iter()
            .find(|entry| entry.revoked_device_public_key == member.device_public_key)
            .unwrap()
            .revoked_at_ns,
        10
    );
    identity.revocations[0].signature[0] ^= 1;
    set_identity(&mut room, &hex::encode(member.device_public_key), &identity);
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::Revocation(_))
    ));
    identity.revocations[0].user_public_key[0] ^= 1;
    set_identity(&mut room, &hex::encode(member.device_public_key), &identity);
    assert!(matches!(
        compare_case(&peer, &room, now),
        Err(ImportError::RevocationRoot)
    ));
}

#[test]
fn governed_import_refuses_collection_and_certificate_limits() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let certificates =
        roster::BoundedRoster::restore(&peer.certificates(), &[], peer.authority(), now_ns(now))
            .unwrap();
    let revocations = revocations::BoundedRevocations::restore(
        &[],
        &RevocationList::new(),
        &certificates,
        peer.authority().device_public_key,
    )
    .unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let native = NativeBatch::new(8 * 1024 * 1024)
        .load(&room.save())
        .unwrap();
    let read = native.read();
    for limits in [
        ImportLimits {
            collection_bytes: 64,
            ..limits()
        },
        ImportLimits {
            certificate_bytes: 64,
            ..limits()
        },
    ] {
        assert!(matches!(
            bounded(
                &read,
                &certificates,
                &revocations,
                peer.authority(),
                now,
                limits,
            ),
            Err(ImportError::Capacity)
        ));
    }
    assert!(
        bounded(
            &read,
            &certificates,
            &revocations,
            peer.authority(),
            now,
            limits(),
        )
        .is_ok()
    );
}
