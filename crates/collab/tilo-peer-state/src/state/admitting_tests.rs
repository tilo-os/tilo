//! Any participant admits: an invitation's redemption is written by any
//! member who may invite and checked by every receiver from the invitation
//! itself, and a device certified by a member's root is written in by any
//! participant.
use super::*;
use crate::join::{
    InviteGrant, JoinChallenge, MemberRole, VerifiedRedemption, issue_invitation, sign_join_proof,
    verify_join_proof,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};

/// A device of the root seeded `root`, its key seeded `device`.
fn device(root: u8, device: u8) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[device; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{device}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// `joiner` redeeming an invitation `issuer` signed with `issuer_key`.
fn redemption(
    peer: &StatePeer,
    issuer: &DeviceCert,
    issuer_key: &SigningKey,
    (key, joiner): &(SigningKey, DeviceCert),
    role: MemberRole,
    nonce: u8,
    now: SystemTime,
) -> VerifiedRedemption {
    let now_ns = now_ns(now);
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.workspace_id().into(),
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

#[test]
fn an_administrator_admits_an_owners_invitation_and_an_editor_does_not() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let owner = peer.owner_certificates()[0].clone();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let guest = device(70, 71);
    let invited = redemption(
        &peer,
        &owner,
        &owner_key,
        &guest,
        MemberRole::Viewer,
        72,
        now,
    );

    let refused = peer
        .clone()
        .governance_mut()
        .admit_invited_member(&invited, &editor_key, now)
        .unwrap_err();
    assert!(refused.contains("may not invite"), "{refused}");

    peer.governance_mut()
        .set_member_administrator(user, &owner_key, now)
        .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&invited, &editor_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(guest.1.device_public_key, now)
            .unwrap()
            .role,
        Role::Viewer
    );
    let governance = received(&peer);
    let ledgered = crate::state::LedgerReceipt::decode(
        &governance.invitation_receipts[&hex::encode(invited.join().invite_id())][0],
    )
    .unwrap();
    assert_eq!(
        ledgered
            .invitation(|digest| invitation_ledger::part_in(&governance.invitation_parts, digest))
            .as_deref(),
        Some(invited.join().signed_invitation()),
        "the ledger keeps the invitation, its issuer's certificate named once"
    );
    assert_eq!(editor.user_public_key, user);

    // Spent: no participant admits another device with it.
    let other = device(73, 74);
    let replay = redemption(
        &peer,
        &owner,
        &owner_key,
        &other,
        MemberRole::Viewer,
        72,
        now,
    );
    assert_eq!(replay.join().invite_id(), invited.join().invite_id());
    assert!(
        peer.governance_mut()
            .admit_invited_member(&replay, &owner_key, now)
            .unwrap_err()
            .contains("already redeemed")
    );
}

#[test]
fn an_invitation_admits_only_what_its_issuer_could_invite() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, _) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let guest = device(75, 76);
    // An editor holds no invite: what they sign admits no one, whoever
    // writes it.
    let invited = redemption(
        &peer,
        &editor,
        &editor_key,
        &guest,
        MemberRole::Viewer,
        77,
        now,
    );
    let refused = peer
        .governance_mut()
        .admit_invited_member(&invited, &owner_key, now)
        .unwrap_err();
    assert!(refused.contains("issuer may not invite"), "{refused}");
    assert!(peer.permissions(guest.1.device_public_key, now).is_err());
}

/// What a receiver says to an administrator's admission of a guest, its
/// ledger changed by `tamper` before it is sent: the ledger's map, the
/// receipt's key, the receipt as written, and the document. The guest is
/// admitted only by the admission as written.
fn refusal_of_tampered_admission(
    tamper: impl FnOnce(&mut AutoCommit, &str, crate::state::LedgerReceipt, &VerifiedRedemption),
) -> String {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (_, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    peer.governance_mut()
        .set_member_administrator(user, &owner_key, now)
        .unwrap();
    // The administrator's device admits the guest as every device does — the
    // rows its operation names beside the receipt — and then changes what
    // the ledger holds in the same batch.
    let guest = device(78, 79);
    let owner = peer.owner_certificates()[0].clone();
    let invited = redemption(
        &peer,
        &owner,
        &owner_key,
        &guest,
        MemberRole::Editor,
        80,
        now,
    );
    let other = redemption(
        &peer,
        &owner,
        &owner_key,
        &guest,
        MemberRole::Editor,
        81,
        now,
    );
    let before = (**peer.core.workspace_doc.as_ref().unwrap())
        .clone()
        .get_heads();
    let mut writer = peer.clone();
    writer
        .governance_mut()
        .admit_invited_member(&invited, &editor_key, now)
        .unwrap();
    let mut doc = (**writer.core.workspace_doc.as_ref().unwrap()).clone();
    let key = hex::encode(invited.join().invite_id());
    let receipt = crate::state::LedgerReceipt::decode(
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
            TransportOrigin::RelayDelta(editor_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )
        .unwrap_err();
    assert!(peer.permissions(guest.1.device_public_key, now).is_err());
    refused
}

fn put_receipt(doc: &mut AutoCommit, key: &str, receipt: &crate::state::LedgerReceipt) {
    let ledger = doc.get(ROOT, "invitation_receipts_v1").unwrap().unwrap().1;
    doc.put(
        &ledger,
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

#[test]
fn a_receiver_refuses_a_members_admission_without_a_valid_invitation() {
    // A receipt filed under one invitation that spells another.
    let refused = refusal_of_tampered_admission(|doc, key, mut receipt, other| {
        receipt.invitation = vec![invitation_ledger::Piece::Own(
            other.join().signed_invitation().to_vec(),
        )];
        put_receipt(doc, key, &receipt);
    });
    assert!(refused.contains("names no invitation"), "{refused}");
}

#[test]
fn a_receiver_refuses_a_receipt_whose_own_bytes_were_changed() {
    let refused = refusal_of_tampered_admission(|doc, key, mut receipt, _| {
        let own = receipt
            .invitation
            .iter_mut()
            .rev()
            .find_map(|piece| match piece {
                invitation_ledger::Piece::Own(bytes) => Some(bytes),
                invitation_ledger::Piece::Shared(_) => None,
            })
            .unwrap();
        // The invitation's signature, at its end.
        let last = own.len() - 1;
        own[last] ^= 1;
        put_receipt(doc, key, &receipt);
    });
    assert!(refused.contains("names no invitation"), "{refused}");
}

#[test]
fn a_receiver_refuses_a_part_changed_under_its_digest() {
    let refused = refusal_of_tampered_admission(|doc, _, receipt, _| {
        let digest = *receipt.shared().next().expect("the issuer is named once");
        let parts = invitation_ledger::parts(doc).unwrap();
        let mut forged = invitation_ledger::part_in(&parts, &digest)
            .unwrap()
            .to_vec();
        forged[0] ^= 1;
        put_part(doc, &invitation_ledger::part_key(&digest), forged);
    });
    assert!(refused.contains("part under another key"), "{refused}");
}

#[test]
fn a_receiver_refuses_a_part_no_receipt_names() {
    let refused = refusal_of_tampered_admission(|doc, _, _, other| {
        let stray = other.join().signed_invitation().to_vec();
        put_part(
            doc,
            &invitation_ledger::part_key(blake3::hash(&stray).as_bytes()),
            stray,
        );
    });
    assert!(
        refused.contains("no receipt of the batch names"),
        "{refused}"
    );
}

#[test]
fn any_participant_admits_a_members_further_device() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (_, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    // Someone else's second device: the editor holds no governance, yet
    // writes it in, for its root certifies it as a member's.
    let owner = peer.owner_certificates()[0].clone();
    let guest = device(82, 83);
    let invited = redemption(
        &peer,
        &owner,
        &owner_key,
        &guest,
        MemberRole::Viewer,
        84,
        now,
    );
    peer.governance_mut()
        .admit_invited_member(&invited, &owner_key, now)
        .unwrap();
    let (_, further) = device(82, 85);
    assert!(
        peer.governance_mut()
            .admit_member_device(&further, &editor_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(further.device_public_key, now)
            .unwrap()
            .role,
        Role::Viewer
    );
    assert!(
        !peer
            .governance_mut()
            .admit_member_device(&further, &editor_key, now)
            .unwrap(),
        "admitted once"
    );
    // A stranger's device is no member's.
    let (_, stranger) = device(86, 87);
    assert!(
        peer.governance_mut()
            .admit_member_device(&stranger, &editor_key, now)
            .is_err()
    );
    assert_ne!(user, stranger.user_public_key);
}

/// A page's device certificate lasts an hour and a page certifies a fresh one
/// on every load: the workspace forgets an expired row nothing keeps, and keeps
/// every row that records an admission.
#[test]
fn a_workspace_forgets_an_expired_page_devices_row_and_keeps_admitted_ones() {
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
    let invited = redemption(
        &peer,
        &owner,
        &owner_key,
        &member,
        MemberRole::Viewer,
        82,
        now,
    );
    peer.governance_mut()
        .admit_invited_member(&invited, &owner_key, now)
        .unwrap();
    let (_, mut page) = device(80, 83);
    expires(&mut page, 80);
    peer.governance_mut()
        .admit_member_device(&page, &owner_key, now)
        .unwrap();
    // A person the invitation admitted with a certificate that also expires:
    // their row records the admission, so it stays.
    let (invited_key, mut expiring_invitee) = device(84, 85);
    expires(&mut expiring_invitee, 84);
    let admitted = redemption(
        &peer,
        &owner,
        &owner_key,
        &(invited_key, expiring_invitee.clone()),
        MemberRole::Viewer,
        86,
        now,
    );
    peer.governance_mut()
        .admit_invited_member(&admitted, &owner_key, now)
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
        peer.permissions(member.1.device_public_key, later)
            .unwrap()
            .role,
        Role::Viewer
    );
}
