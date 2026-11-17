//! A holder of a workspace that is no member: what it may write there.
use super::*;

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

/// A holder of the workspace that is no member, another person's server
/// keeping it, writes the redemption of an invitation it answers, and every
/// receiver admits whom it redeemed: only while the workspace names it among
/// its holders, and nothing but redemptions.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn a_holder_that_is_no_member_writes_redemptions_and_nothing_else() {
    use crate::join::{
        InviteGrant, JoinChallenge, Role, decode_and_verify_invitation, issue_invitation,
        sign_join_proof, verify_join_proof,
    };

    let (mut peer, authority_key, _, _, now) = seeded();
    let now_ns = ns_since_epoch(now);
    let workspace = peer.workspace_id().to_owned();
    let (holder_key, holder) = device_of(91, 92, "Holder");
    let (guest_key, guest) = device_of(93, 94, "Guest");
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(&workspace).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce: [95; 16],
            recipient_user_public_key: Some(guest.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
            transport: vec![],
        },
        &authority_key,
    )
    .unwrap();
    let join = decode_and_verify_invitation(
        &signed,
        Some(guest.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest,
        [96; 32],
        [97; 32],
        now_ns.saturating_add(1_000_000_000),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_key).unwrap();
    let redemption =
        verify_join_proof(&proof, &challenge, &guest, &RevocationList::new(), now_ns).unwrap();
    // What the server writes on its own copy, as changes on `held`'s.
    let written = |held: &StatePeer, write: &dyn Fn(&mut StatePeer)| -> Vec<Vec<u8>> {
        let mut copy = held.clone();
        let heads = copy.admitted_workspace().unwrap().clone().get_heads();
        write(&mut copy);
        copy.admitted_workspace()
            .unwrap()
            .clone()
            .get_changes(&heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    };
    let from_host = |receiver: &mut StatePeer, changes: Vec<Vec<u8>>| {
        receiver.receive_from(
            TransportOrigin::ServerDelta(holder.device_public_key),
            NeighbourMsg::SyncResp {
                changes,
                workspace: workspace.clone(),
            },
            now,
        )
    };

    // Not named among the holders, the server writes nothing, not even locally.
    let mut unnamed = peer.clone();
    assert!(
        unnamed
            .governance_mut()
            .admit_invited_member(&redemption, &holder_key, now)
            .is_err()
    );

    let hint = tilo_workspace_join::sign_transport_hint(
        "iroh",
        "an address",
        holder.clone(),
        [3; 32],
        &holder_key,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .author_workspace_host(&hint, &authority_key, now)
            .unwrap()
    );
    let redeemed = written(&peer, &|copy| {
        assert!(
            copy.governance_mut()
                .admit_invited_member(&redemption, &holder_key, now)
                .unwrap()
        );
    });
    // A receiver that does not hold the server named refuses it.
    let mut before_named = unnamed.clone();
    assert!(from_host(&mut before_named, redeemed.clone()).is_err());

    // Anything else the server writes is refused, and lands nowhere.
    let renamed = written(&peer, &|copy| {
        let mut doc = copy.admitted_workspace().unwrap().clone();
        let heads = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
        doc.put(ROOT, "name", "Taken").unwrap();
        doc.commit_edit("rename", "name");
        let changes = doc
            .get_changes(&heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        copy.receive_from(
            TransportOrigin::ServerDelta(authority_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                changes,
                workspace: workspace.clone(),
            },
            now,
        )
        .unwrap();
    });
    let mut receiver = peer.clone();
    assert!(from_host(&mut receiver, renamed).is_err());
    assert_ne!(
        receiver.governance().workspace_name().as_deref(),
        Some("Taken")
    );

    // The redemption is admitted by every receiver alike.
    from_host(&mut receiver, redeemed).unwrap();
    assert_eq!(
        receiver.role(&guest.device_public_key, now).unwrap(),
        collab_types::Role::Editor
    );
    assert!(
        receiver.role(&holder.device_public_key, now).is_err(),
        "the server is no member for having admitted someone"
    );
}
