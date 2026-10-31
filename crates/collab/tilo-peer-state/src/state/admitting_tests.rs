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
            room: peer.room().into(),
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

/// The room's governance, as a fresh replica of the owner's receives it
/// from `peer`'s document.
fn received(peer: &StatePeer) -> crate::room::RoomGovernance {
    let NeighbourMsg::SyncResp { changes, .. } = peer.room_message() else {
        panic!("room message is a SyncResp")
    };
    crate::room::governance_of(Some(&crate::room::candidate(None, &changes).unwrap()))
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
    let receipt: RedemptionReceipt = serde_json::from_slice(
        &governance.invitation_receipts[&hex::encode(invited.join().invite_id())][0],
    )
    .unwrap();
    assert!(
        receipt.invitation.is_some(),
        "the ledger keeps the invitation"
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

#[test]
fn a_receiver_refuses_a_members_admission_without_a_valid_invitation() {
    let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
    let (_, user) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    peer.governance_mut()
        .set_member_administrator(user, &owner_key, now)
        .unwrap();
    // The administrator's device writes a receipt whose invitation is not
    // the one it names.
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
    let mut doc = (**peer.core.room_doc.as_ref().unwrap()).clone();
    doc.set_actor(automerge::ActorId::from(
        editor_key.verifying_key().to_bytes().to_vec(),
    ));
    crate::governance::admit_invited_member(&mut doc, &invited, now).unwrap();
    let ledger = doc.get(ROOT, "invitation_receipts_v1").unwrap().unwrap().1;
    let key = hex::encode(invited.join().invite_id());
    let mut receipt: RedemptionReceipt =
        serde_json::from_slice(&crate::room::invitation_receipts(&doc).unwrap()[&key][0]).unwrap();
    let other = redemption(
        &peer,
        &owner,
        &owner_key,
        &guest,
        MemberRole::Editor,
        81,
        now,
    );
    receipt.invitation = Some(crate::join::encode_invitation_text(
        other.join().signed_invitation(),
    ));
    let before = (**peer.core.room_doc.as_ref().unwrap()).clone().get_heads();
    doc.set_actor(automerge::ActorId::from(
        editor_key.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(
        &ledger,
        key.as_str(),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&receipt).unwrap()),
    )
    .unwrap();
    doc.commit();
    let changes = doc
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let room = peer.room().to_owned();
    let refused = peer
        .receive_from(
            TransportOrigin::RelayDelta(editor_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )
        .unwrap_err();
    assert!(refused.contains("names another invitation"), "{refused}");
    assert!(peer.permissions(guest.1.device_public_key, now).is_err());
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
