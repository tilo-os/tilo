//! A device admits itself: an invitation's redemption is written by the
//! device redeeming it and checked by every receiver from the invitation
//! itself, and a device certified by a member's root writes its own row. No
//! other device writes either in its place.
use super::*;
use crate::join::WorkspaceId;
use crate::join::{
    InviteGrant, JoinChallenge, Role, VerifiedRedemption, issue_invitation, sign_join_proof,
    verify_join_proof,
};
use tilo_user_identity::{RevocationList, sign_device_cert};

/// A device of the root seeded `root`, its key seeded `device`.
fn device(root: u8, device: u8) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[device; 32]);
    let cert = crate::fixture::cert(
        &root,
        &key,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    (key, cert)
}

/// `joiner` redeeming an invitation `issuer` signed with `issuer_key`.
fn redemption(
    peer: &StatePeer,
    issuer: &DeviceCert,
    issuer_key: &SigningKey,
    (key, joiner): &(SigningKey, DeviceCert),
    role: Role,
    nonce: u8,
    now: SystemTime,
) -> VerifiedRedemption {
    let now_ns = now_ns(now);
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: issuer.clone(),
            role,
            nonce: [nonce; 16],
            recipient_user_public_key: None,
            invitee_name: None,
            expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
            transport: vec![],
        },
        issuer_key,
    )
    .unwrap();
    let join =
        crate::join::decode_and_verify_invitation(&signed, None, &RevocationList::new(), now_ns)
            .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        joiner,
        [nonce; 32],
        [nonce ^ 1; 32],
        now_ns.checked_add(1_000_000_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), key).unwrap();
    verify_join_proof(&proof, &challenge, joiner, &RevocationList::new(), now_ns).unwrap()
}

/// The workspace's governance, as a fresh replica of the owner's receives it
/// from `peer`'s document.
fn received(peer: &StatePeer) -> crate::workspace::WorkspaceGovernance {
    let NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message() else {
        panic!("workspace message is a SyncResp")
    };
    crate::workspace::governance_of(Some(&crate::workspace::candidate(None, &changes).unwrap()))
}

tilo_test_env::scenario! {
    fn a_device_is_admitted_only_by_itself;

    phase a_device_redeems_its_own_invitation_and_no_other_device_writes_it;
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let owner = peer.owner_certificates()[0].clone();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        let guest = device(70, 71);
        let invited = redemption(&peer, &owner, &owner_key, &guest, Role::Viewer, 72, now);

        peer.governance_mut()
            .set_member_administrator(user, None, &owner_key, now)
            .unwrap();
        for (who, key) in [("an administrator", &editor_key), ("the owner", &owner_key)] {
            let refused = peer
                .clone()
                .governance_mut()
                .admit_invited_member(&invited, key, now)
                .unwrap_err();
            assert!(refused.contains("written by the device it admits"), "{who}: {refused}");
        }
        assert!(
            peer.governance_mut()
                .admit_invited_member(&invited, &guest.0, now)
                .unwrap()
        );
        assert_eq!(
            peer.role(&guest.1.device_public_key, now).unwrap(),
            Role::Viewer
        );
        let governance = received(&peer);
        let logged = crate::state::StoredReceipt::decode(
            &governance.invitation_receipts[&hex::encode(invited.join().invite_id())][0],
        )
        .unwrap();
        assert_eq!(
            logged
                .invitation(|digest| invitation_receipts::part_in(&governance.invitation_parts, digest))
                .as_deref(),
            Some(invited.join().signed_invitation()),
            "the log keeps the invitation, its issuer's certificate named once"
        );
        assert_eq!(editor.user_public_key, user);

        // Spent: no other device comes in with it.
        let other = device(73, 74);
        let replay = redemption(&peer, &owner, &owner_key, &other, Role::Viewer, 72, now);
        assert_eq!(replay.join().invite_id(), invited.join().invite_id());
        assert!(
            peer.governance_mut()
                .admit_invited_member(&replay, &other.0, now)
                .unwrap_err()
                .contains("already redeemed")
        );
    }

    phase an_invitation_admits_only_what_its_issuer_could_invite;
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, _) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        let guest = device(75, 76);
        // An editor holds no invite: what they sign admits no one.
        let invited = redemption(&peer, &editor, &editor_key, &guest, Role::Viewer, 77, now);
        let refused = peer
            .governance_mut()
            .admit_invited_member(&invited, &guest.0, now)
            .unwrap_err();
        assert!(refused.contains("issuer may not invite"), "{refused}");
        assert!(peer.permissions(guest.1.device_public_key, now).is_err());
    }

    phase a_members_further_device_lists_itself_and_no_other_device_writes_it;
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (_, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        // A member's second device writes its own row, for its root certifies
        // it as a member's; another member's device does not write it.
        let owner = peer.owner_certificates()[0].clone();
        let guest = device(82, 83);
        let invited = redemption(&peer, &owner, &owner_key, &guest, Role::Viewer, 84, now);
        peer.governance_mut()
            .admit_invited_member(&invited, &guest.0, now)
            .unwrap();
        let (further_key, further) = device(82, 85);
        let refused = peer
            .clone()
            .governance_mut()
            .admit_member_device(&further, &editor_key, now)
            .unwrap_err();
        assert!(refused.contains("no other device writes it in"), "{refused}");
        assert!(
            peer.governance_mut()
                .admit_member_device(&further, &further_key, now)
                .unwrap()
        );
        assert_eq!(
            peer.role(&further.device_public_key, now).unwrap(),
            Role::Viewer
        );
        assert!(
            !peer
                .governance_mut()
                .admit_member_device(&further, &further_key, now)
                .unwrap(),
            "admitted once"
        );
        // A stranger's device is no member's.
        let (stranger_key, stranger) = device(86, 87);
        assert!(
            peer.governance_mut()
                .admit_member_device(&stranger, &stranger_key, now)
                .is_err()
        );
        assert_ne!(user, stranger.user_public_key);
    }

    phase a_workspace_forgets_an_expired_page_devices_row_and_keeps_admitted_ones;
    // A page's device certificate lasts an hour and a page certifies a fresh one
    // on every load: the workspace forgets an expired row nothing keeps, and keeps
    // every row that records an admission.
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let owner = peer.owner_certificates()[0].clone();
        crate::admission_tests::add_editor(&mut peer, &owner_key, now);
        let hour = 3_600_000_000_000;
        let expires = |cert: &mut DeviceCert, root: u8| {
            cert.expires_at_ns = Some(now_ns(now).checked_add(hour).unwrap());
            sign_device_cert(cert, &SigningKey::from_bytes(&[root; 32])).unwrap();
        };

        // A visitor's person joins as a member with the Viewer role the host gives
        // visitors; its member device and the page device it certifies for a visit.
        let member = device(80, 81);
        let invited = redemption(&peer, &owner, &owner_key, &member, Role::Viewer, 82, now);
        peer.governance_mut()
            .admit_invited_member(&invited, &member.0, now)
            .unwrap();
        let (page_key, mut page) = device(80, 83);
        expires(&mut page, 80);
        peer.governance_mut()
            .admit_member_device(&page, &page_key, now)
            .unwrap();
        // A person the invitation admitted with a certificate that also expires:
        // their row records the admission, so it stays.
        let (invited_key, mut expiring_invitee) = device(84, 85);
        expires(&mut expiring_invitee, 84);
        let invitee = (invited_key, expiring_invitee.clone());
        let admitted = redemption(&peer, &owner, &owner_key, &invitee, Role::Viewer, 86, now);
        peer.governance_mut()
            .admit_invited_member(&admitted, &invitee.0, now)
            .unwrap();

        let rows = |peer: &StatePeer| received(peer).identities;
        let before = rows(&peer);
        assert!(
            !peer
                .governance_mut()
                .forget_expired_devices(&owner_key, now)
                .unwrap()
        );
        assert_eq!(rows(&peer).len(), before.len(), "live rows stay");

        let later = now + std::time::Duration::from_secs(2 * 60 * 60);
        assert!(
            peer.governance_mut()
                .forget_expired_devices(&owner_key, later)
                .unwrap()
        );
        let after = rows(&peer);
        assert!(
            !after.contains_key(&hex::encode(page.device_public_key)),
            "an expired page device's row is gone"
        );
        for kept in [
            owner.device_public_key,
            member.1.device_public_key,
            expiring_invitee.device_public_key,
        ] {
            assert!(after.contains_key(&hex::encode(kept)));
        }
        assert_eq!(
            peer.role(&member.1.device_public_key, later).unwrap(),
            Role::Viewer
        );
    }
}

/// What a receiver says to an administrator's admission of a guest, its
/// log changed by `tamper` before it is sent: the log's map, the
/// receipt's key, the receipt as written, and the document. The guest is
/// admitted only by the admission as written.
fn refusal_of_tampered_admission(
    tamper: impl FnOnce(&mut AutoCommit, &str, crate::state::StoredReceipt, &VerifiedRedemption),
) -> String {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (_, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    peer.governance_mut()
        .set_member_administrator(user, None, &owner_key, now)
        .unwrap();
    // The guest's device admits itself as every device does — the rows its
    // operation names beside the receipt — and the administrator's device
    // changes what the log holds in the batch it carries.
    let guest = device(78, 79);
    let owner = peer.owner_certificates()[0].clone();
    let invited = redemption(&peer, &owner, &owner_key, &guest, Role::Editor, 80, now);
    let other = redemption(&peer, &owner, &owner_key, &guest, Role::Editor, 81, now);
    let before = (**peer.core.workspace_doc.as_ref().unwrap())
        .clone()
        .get_heads();
    let mut writer = peer.clone();
    writer
        .governance_mut()
        .admit_invited_member(&invited, &guest.0, now)
        .unwrap();
    let mut doc = (**writer.core.workspace_doc.as_ref().unwrap()).clone();
    let key = hex::encode(invited.join().invite_id());
    let receipt = crate::state::StoredReceipt::decode(
        &crate::workspace::invitation_receipts(&doc).unwrap()[&key][0],
    )
    .unwrap();
    doc.set_actor(automerge::ActorId::from(
        editor_key.verifying_key().to_bytes().to_vec(),
    ));
    tamper(&mut doc, &key, receipt, &other);
    doc.commit();
    let changes = doc
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let workspace = peer.workspace_id().to_owned();
    let refused = peer
        .receive_from(
            TransportOrigin::ServerDelta(editor_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, workspace },
            now,
        )
        .unwrap_err()
        .to_string();
    assert!(peer.permissions(guest.1.device_public_key, now).is_err());
    refused
}

fn put_receipt(doc: &mut AutoCommit, key: &str, receipt: &crate::state::StoredReceipt) {
    let log = doc.get(ROOT, "invitation_receipts_v1").unwrap().unwrap().1;
    doc.put(
        &log,
        key,
        automerge::ScalarValue::Bytes(receipt.encode().unwrap()),
    )
    .unwrap();
}

fn put_part(doc: &mut AutoCommit, key: &str, bytes: Vec<u8>) {
    let parts = doc.get(ROOT, "invitation_parts_v1").unwrap().unwrap().1;
    doc.put(&parts, key, automerge::ScalarValue::Bytes(bytes))
        .unwrap();
}

/// A receiver refuses a member's admission whose log was changed in the
/// batch that carries it: one case per change, each with the reason it is
/// refused for.
#[test]
fn a_receiver_refuses_a_members_admission_whose_log_was_changed() {
    let refuses = |case: &str, reason: &str, refused: String| {
        assert!(refused.contains(reason), "{case}: {refused}");
    };
    refuses(
        "a receipt filed under one invitation that spells another",
        "names no invitation",
        refusal_of_tampered_admission(|doc, key, mut receipt, other| {
            receipt.invitation = vec![invitation_receipts::Piece::Own(
                other.join().signed_invitation().to_vec(),
            )];
            put_receipt(doc, key, &receipt);
        }),
    );
    refuses(
        "a receipt whose own bytes were changed",
        "names no invitation",
        refusal_of_tampered_admission(|doc, key, mut receipt, _| {
            let own = receipt
                .invitation
                .iter_mut()
                .rev()
                .find_map(|piece| match piece {
                    invitation_receipts::Piece::Own(bytes) => Some(bytes),
                    invitation_receipts::Piece::Shared(_) => None,
                })
                .unwrap();
            // The invitation's signature, at its end.
            let last = own.len() - 1;
            own[last] ^= 1;
            put_receipt(doc, key, &receipt);
        }),
    );
    refuses(
        "a part changed under its digest",
        "part under another key",
        refusal_of_tampered_admission(|doc, _, receipt, _| {
            let digest = *receipt.shared().next().expect("the issuer is named once");
            let parts = invitation_receipts::parts(doc).unwrap();
            let mut forged = invitation_receipts::part_in(&parts, &digest)
                .unwrap()
                .to_vec();
            forged[0] ^= 1;
            put_part(doc, &invitation_receipts::part_key(&digest), forged);
        }),
    );
    refuses(
        "a part no receipt names",
        "no receipt of the batch names",
        refusal_of_tampered_admission(|doc, _, _, other| {
            let stray = other.join().signed_invitation().to_vec();
            put_part(
                doc,
                &invitation_receipts::part_key(blake3::hash(&stray).as_bytes()),
                stray,
            );
        }),
    );
}

/// A device of the owner's that redeems an invitation comes in on its certificate: every copy
/// admits the change that records it even when the invitation had expired by the time the
/// change was written. Anyone else's expired invitation still admits nobody.
#[test]
fn an_owners_device_comes_in_on_an_expired_invitation_and_nobody_else_does() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let owner = peer.owner_certificates()[0].clone();
    let later = now + std::time::Duration::from_secs(60);
    let own = device(1, 90);
    let invited = redemption(&peer, &owner, &owner_key, &own, Role::Viewer, 91, now);
    let mut writer = peer.clone();
    assert!(
        writer
            .governance_mut()
            .admit_invited_member(&invited, &own.0, later)
            .unwrap()
    );
    let NeighbourMsg::SyncResp { changes, workspace } = writer.workspace_message() else {
        panic!("workspace message is a SyncResp")
    };
    peer.receive_from(
        TransportOrigin::ServerDelta(owner.device_public_key),
        NeighbourMsg::SyncResp { changes, workspace },
        later,
    )
    .unwrap();
    assert!(peer.permissions(own.1.device_public_key, later).is_ok());

    let guest = device(92, 93);
    let invited = redemption(&peer, &owner, &owner_key, &guest, Role::Viewer, 94, now);
    let refused = peer
        .clone()
        .governance_mut()
        .admit_invited_member(&invited, &guest.0, later)
        .unwrap_err();
    assert!(refused.contains("expired"), "{refused}");
}

/// The batch `key`'s device signs writing `listed`'s row onto `peer`'s member
/// list, put there by hand as a device that skips its own checks would: the
/// row, and the operation naming it under `key`'s device.
fn row_written_by(
    peer: &StatePeer,
    listed: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> Vec<Vec<u8>> {
    let mut doc = peer.core.workspace_doc.clone().unwrap();
    let before = doc.get_heads();
    doc.set_actor(peer.workspace_actor_of(key));
    let row = DeviceRow {
        device_cert: listed.clone(),
        revocations: Vec::new(),
        admission: None,
        admissions: Vec::new(),
        delegate: None,
        workspace_delegations: Vec::new(),
        removed: false,
        entry: None,
    };
    let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    doc.put(
        &identities,
        hex::encode(listed.device_public_key),
        automerge::ScalarValue::Bytes(row.encode().unwrap()),
    )
    .unwrap();
    super::governance_writes::commit_governance(&mut doc, super::now_secs(now), None);
    peer.record_governance(
        &mut doc,
        vec![crate::workspace::GovernanceRow::Identities],
        key,
        now,
    )
    .unwrap();
    super::governance_writes::commit_governance(&mut doc, super::now_secs(now), None);
    doc.get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A member's device forges the row of a further device of its own person
/// and sends it on through another person's server that holds the
/// workspace: the holder refuses it, and so does a member's copy the holder
/// hands it to, with the refusal of a change its writer may not make.
/// The forged device reaches nothing. The same row its own device signs is
/// taken in everywhere.
#[test]
fn every_receiver_refuses_a_row_one_device_writes_for_another() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    assert_eq!(
        editor.device_public_key,
        editor_key.verifying_key().to_bytes()
    );
    let (holder_key, holder) = device(90, 91);
    let hint = tilo_workspace_join::sign_transport_hint(
        "iroh",
        "an address",
        holder,
        [3; 32],
        &holder_key,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .author_keeping_device(&hint, &owner_key, now)
            .unwrap()
    );
    // A further device of the editor's own person, root 5.
    let further_key = SigningKey::from_bytes(&[77; 32]);
    let mut further = editor.clone();
    further.device_public_key = further_key.verifying_key().to_bytes();
    further.device_label = "further".into();
    sign_device_cert(&mut further, &SigningKey::from_bytes(&[5; 32])).unwrap();
    assert_eq!(further.user_public_key, user);

    let forged = row_written_by(&peer, &further, &editor_key, now);
    let refused = |receiver: &mut StatePeer, from: TransportOrigin| {
        let refusal = receiver
            .receive_from(
                from,
                NeighbourMsg::SyncResp {
                    changes: forged.clone(),
                    workspace: receiver.workspace_id().to_owned(),
                },
                now,
            )
            .unwrap_err();
        assert!(
            matches!(
                &refusal,
                crate::AdmissionError::Rejected {
                    reason: neighbour_wire::RefusalReason::Forbidden,
                    detail,
                } if detail.contains("writes its own row")
            ),
            "{refusal:?}"
        );
        assert!(
            receiver
                .permissions(further.device_public_key, now)
                .is_err(),
            "the forged device reaches nothing"
        );
        assert!(!receiver.certificates().contains(&further));
    };
    // The holder takes it live from the editor's device, and a member's copy
    // as the holder hands on what it kept, under the device that wrote it.
    let editor_device = editor.device_public_key;
    let mut on_holder = peer.clone();
    refused(&mut on_holder, TransportOrigin::ServerDelta(editor_device));
    let mut on_member = peer.clone();
    refused(
        &mut on_member,
        TransportOrigin::RetainedServerDelta(editor_device),
    );

    // The device signing its own row is taken in, handed on by the holder too.
    let own = row_written_by(&peer, &further, &further_key, now);
    let mut on_member = peer.clone();
    on_member
        .receive_from(
            TransportOrigin::RetainedServerDelta(further.device_public_key),
            NeighbourMsg::SyncResp {
                changes: own,
                workspace: peer.workspace_id().to_owned(),
            },
            now,
        )
        .unwrap();
    assert_eq!(
        on_member.role(&further.device_public_key, now).unwrap(),
        Role::Editor
    );
}

/// The batch that marks `device`'s row on `peer`'s member list removed, put there by hand as a
/// device that skips its own checks would: the mark written under `writer`'s actor, and an
/// operation naming the member list signed by `signer`'s device under its own.
fn removal_mark(
    peer: &StatePeer,
    device: [u8; 32],
    (writer, signer): (&SigningKey, &SigningKey),
    now: SystemTime,
) -> Vec<Vec<u8>> {
    let mut doc = peer.core.workspace_doc.clone().unwrap();
    let before = doc.get_heads();
    let key = hex::encode(device);
    let mut row =
        DeviceRow::decode(&crate::workspace::governance_of(Some(&doc)).identities[&key]).unwrap();
    row.removed = true;
    doc.set_actor(peer.workspace_actor_of(writer));
    let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    doc.put(
        &identities,
        key,
        automerge::ScalarValue::Bytes(row.encode().unwrap()),
    )
    .unwrap();
    super::governance_writes::commit_governance(&mut doc, super::now_secs(now), None);
    doc.set_actor(peer.workspace_actor_of(signer));
    peer.record_governance(
        &mut doc,
        vec![crate::workspace::GovernanceRow::Identities],
        signer,
        now,
    )
    .unwrap();
    super::governance_writes::commit_governance(&mut doc, super::now_secs(now), None);
    doc.get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A removal mark is a change only someone who may remove members makes, and only as the device
/// that carries it: every receiver refuses a mark an editor writes on another person's device, and
/// a mark on a device's own row that another device's actor wrote under that device's operation.
/// The marked device stays one of the workspace's either way.
#[test]
fn every_receiver_refuses_a_forged_removal_mark() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, _) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    assert_eq!(
        editor.device_public_key,
        editor_key.verifying_key().to_bytes()
    );
    let owner_device = owner_key.verifying_key().to_bytes();
    let refused = |changes: Vec<Vec<u8>>, from: [u8; 32], named: &str, marked: [u8; 32]| {
        let mut receiver = peer.clone();
        let refusal = receiver
            .receive_from(
                TransportOrigin::ServerDelta(from),
                NeighbourMsg::SyncResp {
                    changes,
                    workspace: receiver.workspace_id().to_owned(),
                },
                now,
            )
            .unwrap_err();
        assert!(
            matches!(
                &refusal,
                crate::AdmissionError::Rejected {
                    reason: neighbour_wire::RefusalReason::Forbidden,
                    detail,
                } if detail.contains(named)
            ),
            "{refusal:?}"
        );
        assert!(
            receiver.permissions(marked, now).is_ok(),
            "the device the mark names stays one of the workspace's"
        );
    };

    // An editor may not remove members: its mark on the owner's device is refused.
    refused(
        removal_mark(&peer, owner_device, (&editor_key, &editor_key), now),
        editor.device_public_key,
        "is not the writer's to remove",
        owner_device,
    );

    // A mark on the editor's own row, written by another device's actor beside an operation the
    // editor's device signed: the operation answers for no row it did not write, and the row is
    // refused as one no device writes in for another.
    let other_key = SigningKey::from_bytes(&[88; 32]);
    refused(
        removal_mark(
            &peer,
            editor.device_public_key,
            (&other_key, &editor_key),
            now,
        ),
        editor.device_public_key,
        "writes its own row",
        editor.device_public_key,
    );
}

/// A copy that answers with its whole workspace hands on several devices'
/// own rows at once, each signed by the device it lists: every one is judged
/// under its own device's operation, and all are taken in. One operation in
/// such a batch writing another device's row is still refused, by name.
#[test]
fn a_batch_of_several_devices_own_rows_is_taken_in_and_a_forged_one_is_not() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let owner = peer.owner_certificates()[0].device_public_key;
    // Further devices of the editor's own person, root 5.
    let further = |seed: u8, label: &str| {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut cert = editor.clone();
        cert.device_public_key = key.verifying_key().to_bytes();
        cert.device_label = label.into();
        sign_device_cert(&mut cert, &SigningKey::from_bytes(&[5; 32])).unwrap();
        assert_eq!(cert.user_public_key, user);
        (key, cert)
    };
    let (first_key, first) = further(77, "first");
    let (second_key, second) = further(78, "second");
    // The batch of `second_writer`'s row for `second` written on top of
    // `first`'s own row, as one answer from the owner's whole copy.
    let batch = |second_writer: &SigningKey| {
        let mut changes = row_written_by(&peer, &first, &first_key, now);
        let mut writer = peer.clone();
        writer
            .receive_from(
                TransportOrigin::RetainedServerDelta(first.device_public_key),
                NeighbourMsg::SyncResp {
                    changes: changes.clone(),
                    workspace: peer.workspace_id().to_owned(),
                },
                now,
            )
            .unwrap();
        changes.extend(row_written_by(&writer, &second, second_writer, now));
        NeighbourMsg::SyncResp {
            changes,
            workspace: peer.workspace_id().to_owned(),
        }
    };

    let mut receiver = peer.clone();
    receiver
        .receive_from(
            TransportOrigin::RetainedServerDelta(owner),
            batch(&second_key),
            now,
        )
        .unwrap();
    for device in [&first, &second] {
        assert_eq!(
            receiver.role(&device.device_public_key, now).unwrap(),
            Role::Editor
        );
    }

    let mut receiver = peer.clone();
    let refusal = receiver
        .receive_from(
            TransportOrigin::RetainedServerDelta(owner),
            batch(&first_key),
            now,
        )
        .unwrap_err();
    let named = format!(
        "device {} writes its own row",
        hex::encode(second.device_public_key)
    );
    assert!(
        matches!(
            &refusal,
            crate::AdmissionError::Rejected {
                reason: neighbour_wire::RefusalReason::Forbidden,
                detail,
            } if detail.contains(&named)
        ),
        "{refusal:?}"
    );
    assert!(receiver.permissions(second.device_public_key, now).is_err());
    assert!(receiver.permissions(first.device_public_key, now).is_err());
}
