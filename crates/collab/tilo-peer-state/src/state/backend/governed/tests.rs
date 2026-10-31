use super::*;
use crate::StatePeer;
use crate::room;
use crate::state::err;
use automerge::{ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;

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
    for (key, values) in room::invitation_receipts(doc)? {
        for bytes in values {
            let receipt: RedemptionReceipt = serde_json::from_slice(&bytes).map_err(err)?;
            if key != hex::encode(receipt.invite_id) {
                return Err("governed invitation receipt key mismatch".into());
            }
        }
    }
    let members = room::governance_of(Some(doc)).members;
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
            && cert.user_public_key != peer.owner()
            && !has_admission
            && !members.contains_key(&hex::encode(cert.user_public_key))
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
) -> Result<LegacyImport, ImportError> {
    let mut oracle = peer.clone();
    let mut actual = peer.clone();
    let expected = original_import(&mut oracle, room, now);
    let got = actual.import_governed_certificates(room, now);
    assert_eq!(got, expected);
    assert_eq!(actual.certificates(), oracle.certificates());
    assert_eq!(actual.revocations(), oracle.revocations());
    legacy(
        room,
        &peer.core.certificates,
        &peer.core.revocations,
        &peer.core.owner,
        now,
    )
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
        delegate: None,
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
        invitation: None,
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
        Err(ImportError::LegacyJson(_))
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
            .1
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
