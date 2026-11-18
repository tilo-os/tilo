//! A holder of a workspace that is no member: what it may write there.
use super::*;
use tilo_user_identity::prepare_minted_grant;

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
            .author_keeping_device(&hint, &authority_key, now)
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
    assert!(
        from_holder(
            &mut before_named,
            &workspace,
            holder.device_public_key,
            redeemed.clone(),
            now,
        )
        .is_err()
    );

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
    assert!(
        from_holder(
            &mut receiver,
            &workspace,
            holder.device_public_key,
            renamed,
            now,
        )
        .is_err()
    );
    assert_ne!(
        receiver.governance().workspace_name().as_deref(),
        Some("Taken")
    );

    // The redemption is admitted by every receiver alike.
    from_holder(
        &mut receiver,
        &workspace,
        holder.device_public_key,
        redeemed,
        now,
    )
    .unwrap();
    assert_eq!(
        receiver.role(&guest.device_public_key, now).unwrap(),
        collab_types::Role::Editor
    );
    assert!(
        receiver.role(&holder.device_public_key, now).is_err(),
        "the server is no member for having admitted someone"
    );
}

/// What the server writes on its own copy, as changes on `held`'s.
fn written(held: &StatePeer, write: &dyn Fn(&mut StatePeer)) -> Vec<Vec<u8>> {
    written_onto(&mut held.clone(), write)
}

/// [`written`] on `copy` itself: the next write on it builds on this one.
fn written_onto(copy: &mut StatePeer, write: &dyn Fn(&mut StatePeer)) -> Vec<Vec<u8>> {
    let heads = copy.admitted_workspace().unwrap().clone().get_heads();
    write(copy);
    copy.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// `changes` reaching `receiver` from the holder `holder` of `workspace`.
fn from_holder(
    receiver: &mut StatePeer,
    workspace: &str,
    holder: [u8; 32],
    changes: Vec<Vec<u8>>,
    now: SystemTime,
) -> Result<(), String> {
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(holder),
            NeighbourMsg::SyncResp {
                changes,
                workspace: workspace.to_owned(),
            },
            now,
        )
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A holder that is no member admits a member's further device and a key
/// minted for the workspace, on the device's own certificate, and no device
/// of a person the workspace has no member of: every receiver admits them
/// alike, as it admits the holder's redemptions.
#[test]
fn a_holder_that_is_no_member_admits_a_members_devices() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let now_ns = ns_since_epoch(now);
    let workspace = peer.workspace_id().to_owned();
    let (holder_key, holder) = device_of(91, 92, "Holder");
    let (_, member, _) = add_member_with(&mut peer, &authority_key, now, 5, 6);
    let member_root = SigningKey::from_bytes(&[5; 32]);
    let (_, further) = device_of(5, 7, "Second desktop");
    // The member's key for this workspace: their person's all-workspaces key
    // minted it, as a backend key does, and the workspace's governance takes
    // it as the member's device ([`tilo_user_identity::prepare_minted_grant`]).
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
    let mut api_key = prepare_minted_grant(
        &backend,
        api_key_key.verifying_key().to_bytes(),
        *uuid::Uuid::parse_str(&workspace).unwrap().as_bytes(),
        "API key",
        now_ns,
        now_ns.saturating_add(3_600_000_000_000),
    );
    sign_device_cert(&mut api_key, &backend_key).unwrap();

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
            .author_keeping_device(&hint, &authority_key, now)
            .unwrap()
    );
    assert_ne!(member.device_public_key, further.device_public_key);

    // The holder writes both in on its own copy, as the device that keeps the
    // workspace's keys.
    let mut holder_copy = peer.clone();
    let device_admitted = written_onto(&mut holder_copy, &|copy| {
        assert!(
            copy.governance_mut()
                .admit_member_device(&further, &holder_key, now)
                .unwrap()
        );
    });
    let key_admitted = written_onto(&mut holder_copy, &|copy| {
        assert!(
            copy.governance_mut()
                .admit_member_device(&api_key, &holder_key, now)
                .unwrap()
        );
    });
    let mut receiver = peer.clone();
    from_holder(
        &mut receiver,
        &workspace,
        holder.device_public_key,
        device_admitted,
        now,
    )
    .unwrap();
    from_holder(
        &mut receiver,
        &workspace,
        holder.device_public_key,
        key_admitted,
        now,
    )
    .unwrap();
    assert!(
        receiver.permissions(further.device_public_key, now).is_ok(),
        "every receiver admits the member's further device"
    );
    assert!(
        receiver.permissions(api_key.device_public_key, now).is_ok(),
        "and the member's key for the workspace"
    );
    assert!(
        receiver.role(&holder.device_public_key, now).is_err(),
        "the holder is no member for having admitted them"
    );

    // A device of a person the workspace has no member of is nobody's.
    let (_, stranger) = device_of(93, 94, "Stranger");
    let refused = peer
        .governance_mut()
        .admit_member_device(&stranger, &holder_key, now)
        .unwrap_err();
    assert!(refused.contains("not a member"), "{refused}");
}
