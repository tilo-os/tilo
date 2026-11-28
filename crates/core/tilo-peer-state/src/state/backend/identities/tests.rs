use super::*;
use crate::StatePeer;
use automerge::{AutoCommit, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::fixture::labelled_with;
/// The import of `workspace` over `peer`'s certificates, read fresh and read
/// after the state the peer holds, whose views carry over: both alike.
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper: asserts both readings agree before returning the value"
)]
fn compare_case(
    peer: &StatePeer,
    workspace: &AutoCommit,
    now: SystemTime,
) -> Result<Import, ImportError> {
    let fresh = crate::cached_document::CachedDocument::from(workspace.clone());
    let expected = import(
        &fresh,
        &peer.core.certificates,
        &peer.core.revocations,
        &peer.core.owner,
        now,
    );
    let mut oracle = peer.clone();
    let imported = oracle.import_device_rows(&fresh, now);
    if let Some(held) = peer.core.workspace_doc.as_ref() {
        let _ = (held.identities(), held.identity_certificates());
        let mut following = peer.clone();
        let got = following.import_device_rows(&held.followed_by(workspace.clone()), now);
        assert_eq!(got, imported);
        assert_eq!(following.certificates(), oracle.certificates());
        assert_eq!(following.revocations(), oracle.revocations());
    }
    expected
}

fn set_identity(workspace: &mut AutoCommit, id: &str, row: &DeviceRow) {
    let (_, object) = workspace
        .get(automerge::ROOT, "peer_identities_v1")
        .unwrap()
        .unwrap();
    workspace.put(object, id, row.encode().unwrap()).unwrap();
    workspace.commit();
}
fn row(cert: DeviceCert) -> DeviceRow {
    DeviceRow {
        device_cert: cert,
        revocations: vec![],
        admission: None,
        admissions: vec![],
        delegate: None,
        workspace_delegations: vec![],
        removed: false,
        entry: None,
    }
}

tilo_test_env::scenario! {
    fn identity_import_keeps_what_the_signed_workspace_proves;

    phase identity_import_preserves_real_signed_identity_admission_and_certificate_failures;
    {
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
            role: crate::join::Role::Viewer,
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

    phase identity_import_keeps_a_minted_grants_issuer;
    {
        let (peer, _, _, _, now) = crate::admission_tests::seeded();
        let now_ns = now_ns(now);
        let root = SigningKey::from_bytes(&[91; 32]);
        let grant_key = SigningKey::from_bytes(&[92; 32]);
        let grant = labelled_with(
            &root,
            &grant_key,
            "agent",
            DeviceCapabilities::agent_grant(tilo_user_identity::AgentScope::AllWorkspaces),
            now_ns - 1,
            Some(now_ns + 3_600_000_000_000),
        );
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
            role: crate::join::Role::Editor,
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
            .import_device_rows(&workspace.into(), now)
            .unwrap();
        assert!(
            imported.certificates().contains(&minted),
            "the minted grant is kept with its issuer"
        );
    }

    phase identity_import_preserves_all_receipt_shapes_and_error_order;
    {
        let (peer, _, _, member, now) = crate::admission_tests::seeded();
        let mut workspace = peer.admitted_workspace().unwrap().clone();
        let log = workspace
            .get(automerge::ROOT, "invitation_receipts_v1")
            .unwrap()
            .unwrap()
            .1;
        workspace.put(&log, "a", vec![b'{']).unwrap();
        workspace.put(&log, "z", "not bytes").unwrap();
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
                Err(ImportError::LogShape)
            ));
            let mut workspace = peer.admitted_workspace().unwrap().clone();
            workspace
                .put_object(automerge::ROOT, "peer_identities_v1", kind)
                .unwrap();
            workspace.commit();
            compare_case(&peer, &workspace, now).unwrap();
        }
    }

    phase identity_import_keeps_verified_earliest_revocations_and_refuses_bad_root_or_signature;
    {
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
}
