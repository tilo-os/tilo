use super::*;
use crate::StatePeer;
use crate::state::err;
use crate::workspace;
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
    let identities = workspace::governance_of(Some(doc))
        .identities
        .into_iter()
        .map(|(id, bytes)| {
            GovernedIdentity::decode(&bytes)
                .map(|identity| (id, identity))
                .map_err(err)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (key, values) in workspace::invitation_receipts(doc)? {
        for bytes in values {
            let receipt: RedemptionReceipt = crate::state::LedgerReceipt::decode(&bytes)
                .map_err(err)?
                .receipt;
            if key != hex::encode(receipt.invite_id) {
                return Err("governed invitation receipt key mismatch".into());
            }
        }
    }
    let members = crate::members::replay_declared(doc).members;
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
    workspace: &AutoCommit,
    now: SystemTime,
) -> Result<LegacyImport, ImportError> {
    let mut oracle = peer.clone();
    let mut actual = peer.clone();
    let expected = original_import(&mut oracle, workspace, now);
    let got = actual.import_governed_certificates(workspace, now);
    assert_eq!(got, expected);
    assert_eq!(actual.certificates(), oracle.certificates());
    assert_eq!(actual.revocations(), oracle.revocations());
    // The rows read once per document state import alike, read fresh and
    // read after the state the peer holds, whose views carry over.
    let fresh = crate::cached_document::CachedDocument::from(workspace.clone());
    let following = peer.core.workspace_doc.as_ref().map(|held| {
        let _ = (held.identities(), held.identity_certificates());
        held.followed_by(workspace.clone())
    });
    for rows in std::iter::once(fresh).chain(following) {
        let mut cached = peer.clone();
        assert_eq!(cached.import_governed_rows(&rows, now), expected);
        assert_eq!(cached.certificates(), oracle.certificates());
        assert_eq!(cached.revocations(), oracle.revocations());
    }
    legacy(
        workspace,
        &peer.core.certificates,
        &peer.core.revocations,
        &peer.core.owner,
        now,
    )
}

fn set_identity(workspace: &mut AutoCommit, id: &str, row: &GovernedIdentity) {
    let (_, object) = workspace
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap();
    workspace.put(object, id, row.encode().unwrap()).unwrap();
    workspace.commit();
}
fn row(cert: DeviceCert) -> GovernedIdentity {
    GovernedIdentity {
        device_cert: cert,
        revocations: vec![],
        admission: None,
        admissions: vec![],
        delegate: None,
        workspace_delegations: vec![],
    }
}

#[test]
fn governed_import_preserves_real_signed_identity_admission_and_certificate_failures() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let original = peer.admitted_workspace().unwrap();
    let foreign_root = SigningKey::from_bytes(&[83; 32]);
    let mut foreign = member.clone();
    foreign.user_public_key = foreign_root.verifying_key().to_bytes();
    foreign.device_public_key = SigningKey::from_bytes(&[84; 32]).verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut foreign, &foreign_root).unwrap();
    let mut workspace = original.clone();
    let mut identity = row(foreign.clone());
    set_identity(
        &mut workspace,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &workspace, now),
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
        &mut workspace,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    compare_case(&peer, &workspace, now).unwrap();
    identity.device_cert.signature[0] ^= 1;
    set_identity(
        &mut workspace,
        &hex::encode(foreign.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::Certificate(DeviceCertError::BadSignature))
    ));
    let mut workspace = original.clone();
    let mut changed = member.clone();
    changed.device_label.push_str(" changed");
    tilo_user_identity::sign_device_cert(&mut changed, &SigningKey::from_bytes(&[1; 32])).unwrap();
    set_identity(
        &mut workspace,
        &hex::encode(member.device_public_key),
        &row(changed),
    );
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::CertificateConflict)
    ));
    let mut workspace = original.clone();
    set_identity(&mut workspace, "WRONG KEY", &row(member));
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::IdentityKey)
    ));
}

#[test]
fn governed_import_keeps_a_minted_grants_issuer() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let now_ns = now_ns(now);
    let root = SigningKey::from_bytes(&[91; 32]);
    let grant_key = SigningKey::from_bytes(&[92; 32]);
    let mut grant = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: grant_key.verifying_key().to_bytes(),
        device_label: "agent".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::AllWorkspaces,
        ),
        issued_at_ns: now_ns - 1,
        expires_at_ns: Some(now_ns + 3_600_000_000_000),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, &root).unwrap();
    let mut minted = tilo_user_identity::prepare_minted_grant(
        &grant,
        SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        [7; 16],
        "minted",
        now_ns - 1,
        now_ns + 60_000_000_000,
    );
    tilo_user_identity::sign_device_cert(&mut minted, &grant_key).unwrap();
    let mut identity = row(minted.clone());
    identity.admission = Some(RedemptionReceipt {
        invite_id: [7; 32],
        user_public_key: minted.user_public_key,
        device_public_key: minted.device_public_key,
        request_id: [8; 32],
        role: crate::join::MemberRole::Editor,
    });
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    set_identity(
        &mut workspace,
        &hex::encode(minted.device_public_key),
        &identity,
    );
    compare_case(&peer, &workspace, now).unwrap();
    let mut imported = peer;
    imported
        .import_governed_certificates(&workspace, now)
        .unwrap();
    assert!(
        imported.certificates().contains(&minted),
        "the minted grant is kept with its issuer"
    );
}

#[test]
fn governed_import_preserves_all_receipt_shapes_and_error_order() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    let ledger = workspace
        .get(automerge::ROOT, "invitation_receipts_v1")
        .unwrap()
        .unwrap()
        .1;
    workspace.put(&ledger, "a", vec![b'{']).unwrap();
    workspace.put(&ledger, "z", "not bytes").unwrap();
    workspace.commit();
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::ReceiptShape)
    ));
    // All raw shapes precede receipt JSON decoding, while identity JSON
    // decoding precedes every receipt shape check.
    let identities = workspace
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap()
        .1;
    workspace
        .put(
            &identities,
            hex::encode(member.device_public_key),
            vec![b'{'],
        )
        .unwrap();
    workspace.commit();
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::Row(_))
    ));
    for kind in [automerge::ObjType::List, automerge::ObjType::Text] {
        let mut workspace = peer.admitted_workspace().unwrap().clone();
        workspace
            .put_object(automerge::ROOT, "invitation_receipts_v1", kind)
            .unwrap();
        workspace.commit();
        assert!(matches!(
            compare_case(&peer, &workspace, now),
            Err(ImportError::LedgerShape)
        ));
        let mut workspace = peer.admitted_workspace().unwrap().clone();
        workspace
            .put_object(automerge::ROOT, "peer_identities_v1", kind)
            .unwrap();
        workspace.commit();
        compare_case(&peer, &workspace, now).unwrap();
    }
}

#[test]
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
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    set_identity(
        &mut workspace,
        &hex::encode(member.device_public_key),
        &identity,
    );
    let imported = compare_case(&peer, &workspace, now).unwrap();
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
    set_identity(
        &mut workspace,
        &hex::encode(member.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::Revocation(_))
    ));
    identity.revocations[0].user_public_key[0] ^= 1;
    set_identity(
        &mut workspace,
        &hex::encode(member.device_public_key),
        &identity,
    );
    assert!(matches!(
        compare_case(&peer, &workspace, now),
        Err(ImportError::RevocationRoot)
    ));
}
