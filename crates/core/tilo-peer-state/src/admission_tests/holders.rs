//! A holder of a workspace that is no member: it carries what members and
//! joining devices signed, byte for byte, and writes nothing of its own.
use super::*;
use crate::join::{
    InviteGrant, Role, decode_and_verify_invitation, issue_invitation, verify_connection_redemption,
};
use std::collections::BTreeSet;

/// A device of `root_seed`'s person, `device_seed`'s key, certified by that
/// person's root.
fn device_of(root_seed: u8, device_seed: u8, label: &str) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root_seed; 32]);
    let key = SigningKey::from_bytes(&[device_seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// `peer`, the owner's copy, with `holder` named among the workspace's
/// holders by its own signed address: another person's server keeping it.
fn holding(peer: &mut StatePeer, authority: &SigningKey, holder: (&SigningKey, &DeviceCert)) {
    let hint = tilo_workspace_join::sign_transport_hint(
        "iroh",
        "an address",
        holder.1.clone(),
        [3; 32],
        holder.0,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .author_keeping_device(&hint, authority, tilo_clock::wall())
            .unwrap()
    );
}

/// The workspace's governance as a join answer carries it.
fn governance_of(peer: &StatePeer) -> Vec<Vec<u8>> {
    vec![postcard::to_stdvec(&peer.workspace_message()).unwrap()]
}

/// An invitation the owner (`authority`) issued for `guest`, redeemed by its
/// device under request `[96; 32]`.
fn invited(
    peer: &StatePeer,
    authority: &SigningKey,
    guest: &DeviceCert,
    now: SystemTime,
) -> crate::join::VerifiedRedemption {
    let now_ns = ns_since_epoch(now);
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce: [95; 16],
            recipient_user_public_key: Some(guest.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
            transport: vec![],
        },
        authority,
    )
    .unwrap();
    decode_and_verify_invitation(
        &signed,
        Some(guest.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    verify_connection_redemption(&signed, guest, [96; 32], &RevocationList::new(), now_ns).unwrap()
}

/// What `held`'s copy took in since `heads`, as the changes it would carry on.
fn carried_since(held: &StatePeer, heads: &[automerge::ChangeHash]) -> BTreeSet<Vec<u8>> {
    held.workspace_changes_since(heads).into_iter().collect()
}

/// `changes` reaching `receiver` from `author`, as a holder relays a frame:
/// under the device that wrote it.
fn from_author(
    receiver: &mut StatePeer,
    author: [u8; 32],
    changes: Vec<Vec<u8>>,
    now: SystemTime,
) -> Result<(), String> {
    let workspace = receiver.workspace_id().to_owned();
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(author),
            NeighbourMsg::SyncResp { changes, workspace },
            now,
        )
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The authors of the membership operations `peer` holds that `before` did not.
fn new_authors(before: &StatePeer, peer: &StatePeer) -> Vec<[u8; 32]> {
    let held = crate::members::ops(before.admitted_workspace().unwrap());
    crate::members::ops(peer.admitted_workspace().unwrap())
        .into_iter()
        .filter(|(id, _)| !held.contains_key(id))
        .map(|(_, op)| op.author.device_public_key)
        .collect()
}

/// A joining device signs its own admission, the invitation its authority. A
/// server of another person keeping the workspace takes it in as that device's
/// and carries it on byte for byte, and every member's device admits whom it
/// redeemed under the device that signed it: the server wrote no operation and
/// no row, and is no member for having carried it.
#[test]
fn a_joining_device_signs_its_own_admission_and_a_holder_carries_it() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let (holder_key, holder) = device_of(91, 92, "Holder");
    let (guest_key, guest) = device_of(93, 94, "Guest");
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let redemption = invited(&peer, &authority_key, &guest, now);
    let admission = crate::own_admission::own_admission(
        peer.workspace_id(),
        &governance_of(&peer),
        &redemption,
        Some("Guest"),
        &guest_key,
        now,
    )
    .unwrap();
    // A device signs only its own admission.
    assert!(
        crate::own_admission::own_admission(
            peer.workspace_id(),
            &governance_of(&peer),
            &redemption,
            None,
            &holder_key,
            now,
        )
        .is_err()
    );

    let heads = peer.workspace_heads().unwrap();
    let mut held = peer.clone();
    held.admit_own_redemption(&redemption, admission.clone(), now)
        .unwrap();
    assert_eq!(
        carried_since(&held, &heads),
        admission.iter().cloned().collect(),
        "the holder carries the device's admission byte for byte"
    );
    assert_eq!(new_authors(&peer, &held), vec![guest.device_public_key]);
    assert!(held.role(&holder.device_public_key, now).is_err());

    let mut member = peer.clone();
    from_author(
        &mut member,
        guest.device_public_key,
        carried_since(&held, &heads).into_iter().collect(),
        now,
    )
    .unwrap();
    assert_eq!(
        member.role(&guest.device_public_key, now).unwrap(),
        collab_types::Role::Editor
    );
    assert_eq!(
        new_authors(&peer, &member),
        vec![guest.device_public_key],
        "the one operation admitting the guest is the guest's own"
    );
    assert!(
        member.role(&holder.device_public_key, now).is_err(),
        "the holder is no member for having carried an admission"
    );
}

/// A member's further device and a key minted for the workspace each sign
/// their own row, admissible because their certificates chain to a root the
/// list names; a server of another person carries the row on, and every
/// receiver admits the device under its own operation. A device of a person
/// the workspace has no member of signs nothing in.
#[test]
fn a_further_device_signs_its_own_row_through_a_holder() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let now_ns = ns_since_epoch(now);
    let (holder_key, holder) = device_of(91, 92, "Holder");
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let (_, member, _) = add_member_with(&mut peer, &authority_key, now, 5, 6);
    let (further_key, further) = device_of(5, 7, "Second desktop");
    assert_ne!(member.device_public_key, further.device_public_key);
    let member_root = SigningKey::from_bytes(&[5; 32]);
    let backend_key = SigningKey::from_bytes(&[8; 32]);
    let mut backend = DeviceCert {
        user_public_key: member_root.verifying_key().to_bytes(),
        device_public_key: backend_key.verifying_key().to_bytes(),
        device_label: "backend".into(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now_ns.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut backend, &member_root).unwrap();
    let api_key_key = SigningKey::from_bytes(&[9; 32]);
    let mut api_key = tilo_user_identity::prepare_minted_grant(
        &backend,
        api_key_key.verifying_key().to_bytes(),
        *uuid::Uuid::parse_str(peer.workspace_id())
            .unwrap()
            .as_bytes(),
        "API key",
        now_ns,
        now_ns.saturating_add(3_600_000_000_000),
    );
    sign_device_cert(&mut api_key, &backend_key).unwrap();

    let mut receiver = peer.clone();
    for (key, cert) in [(&further_key, &further), (&api_key_key, &api_key)] {
        let row = crate::own_admission::further_device_row(
            peer.workspace_id(),
            &governance_of(&receiver),
            cert,
            key,
            now,
        )
        .unwrap();
        assert!(!row.is_empty());
        let mut held = receiver.clone();
        assert!(held.admit_own(cert, row.clone(), now).unwrap());
        assert!(held.role(&holder.device_public_key, now).is_err());
        let before = receiver.clone();
        from_author(&mut receiver, cert.device_public_key, row, now).unwrap();
        assert!(receiver.permissions(cert.device_public_key, now).is_ok());
        assert_eq!(
            new_authors(&before, &receiver),
            vec![cert.device_public_key],
            "the device's row is its own operation"
        );
    }
    // A device the workspace lists already writes nothing again.
    assert!(
        crate::own_admission::further_device_row(
            peer.workspace_id(),
            &governance_of(&receiver),
            &further,
            &further_key,
            now,
        )
        .unwrap()
        .is_empty()
    );

    let (stranger_key, stranger) = device_of(93, 94, "Stranger");
    let refused = crate::own_admission::further_device_row(
        peer.workspace_id(),
        &governance_of(&peer),
        &stranger,
        &stranger_key,
        now,
    )
    .unwrap_err();
    assert!(refused.contains("not a member"), "{refused}");
}

/// A holder that is no member writes nothing of its own: not the row saying
/// where its copy stands, not a redemption it signed, not a renamed
/// workspace, not someone else's admission passed off as its own batch, not
/// an operation it backdated, and none of its record writes lands.
#[test]
fn a_holder_that_is_no_member_writes_nothing_of_its_own() {
    let (mut peer, authority_key, _, bc, now) = seeded();
    let workspace = peer.workspace_id().to_owned();
    let (holder_key, holder) = device_of(91, 92, "Holder");
    let (guest_key, guest) = device_of(93, 94, "Guest");
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let heads = peer.workspace_heads().unwrap();

    // Its own copy's row.
    let held = crate::HeldCopy {
        catching_up_from: [[7; 32]].into_iter().collect(),
        ..Default::default()
    };
    assert!(
        peer.clone()
            .workspace_mut()
            .report_copy(&holder, &held, now)
            .is_err(),
        "a holder that is no member says nothing of its copy in the workspace"
    );
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    let rows = match doc.get(ROOT, crate::HELD_COPIES_KEY).unwrap() {
        Some((_, rows)) => rows,
        None => doc
            .put_object(ROOT, crate::HELD_COPIES_KEY, automerge::ObjType::Map)
            .unwrap(),
    };
    doc.put(&rows, hex::encode(holder.device_public_key).as_str(), "{}")
        .unwrap();
    doc.commit();
    let copy_row = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut peer.clone(), holder.device_public_key, copy_row, now).is_err());

    // A redemption it signs.
    let redemption = invited(&peer, &authority_key, &guest, now);
    assert!(
        peer.clone()
            .governance_mut()
            .admit_invited_member(&redemption, &holder_key, now)
            .is_err(),
        "a holder that is no member signs nobody in"
    );

    // The guest's own admission, sent as the holder's own batch.
    let admission = crate::own_admission::own_admission(
        &workspace,
        &governance_of(&peer),
        &redemption,
        None,
        &guest_key,
        now,
    )
    .unwrap();
    let mut receiver = peer.clone();
    assert!(
        from_author(&mut receiver, holder.device_public_key, admission, now).is_err(),
        "operations another device signed are no holder's own batch"
    );
    assert!(receiver.role(&guest.device_public_key, now).is_err());

    // An operation it signs itself, backdated to before anything it could
    // claim: its person held no role at any point.
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    let op = crate::members::SignedMembershipOp::signed(
        &holder,
        &holder_key,
        crate::members::heads_of(&doc, &workspace),
        1,
        crate::members::MembershipOpCommand::Admit {
            invite: redemption.join().invite_id(),
            person: guest.user_public_key,
        },
    )
    .unwrap();
    crate::members::put_op(&mut doc, &op).unwrap();
    doc.commit();
    let backdated = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut receiver, holder.device_public_key, backdated, now).is_err());

    // A rename.
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    doc.put(ROOT, "name", "Taken").unwrap();
    doc.commit_edit("rename", "name", &bc.user_public_key);
    let renamed = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut receiver, holder.device_public_key, renamed, now).is_err());
    assert_ne!(
        receiver.governance().workspace_name().as_deref(),
        Some("Taken")
    );
    assert_eq!(receiver.workspace_heads().unwrap(), heads);
}
