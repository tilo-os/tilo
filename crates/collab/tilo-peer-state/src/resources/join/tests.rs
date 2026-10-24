use super::*;
use crate::join::*;
use tilo_user_identity::RevocationList;

#[expect(
    clippy::too_many_lines,
    reason = "one fixture, kept whole so every entry stays in wire order"
)]
fn responses() -> Vec<JoinTransportResponse> {
    let (mut peer, governor, member_key, member, now) = crate::admission_tests::seeded();
    let now_ns: u64 = now
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: member.user_public_key,
        revoked_device_public_key: [99; 32],
        revoked_at_ns: now_ns,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(
        &mut revocation,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    peer.receive_from(
        crate::TransportOrigin::RelayDelta(peer.authority().device_public_key),
        crate::NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(peer.authority().clone()),
            revocations: vec![revocation],
            successions: Vec::new(),
        },
        now,
    )
    .unwrap();
    let grant = InviteGrant {
        workspace_id: 42,
        room: peer.room().into(),
        authority: peer.authority().clone(),
        issuer: member.clone(),
        delegation: Some(
            sign_workspace_delegation(
                peer.room(),
                42,
                peer.authority(),
                member.clone(),
                WorkspaceDelegationPermissions {
                    issue_invitations: true,
                    admit_invited_members: true,
                },
                [29; 16],
                Some(now_ns.checked_add(10_000).unwrap()),
                &governor,
            )
            .unwrap(),
        ),
        role: MemberRole::Editor,
        nonce: [17; 16],
        recipient_user_public_key: Some(member.user_public_key),
        invitee_name: Some("A real guest 🦀".into()),
        expires_at_ns: now_ns.checked_add(10_000).unwrap(),
        transport: vec![
            sign_transport_hint(
                TRANSPORT_TILO_SESSION_JOIN_HTTP_V1,
                r#"{"endpoint":"https://localhost/join","label":"a\\b"}"#,
                peer.authority().clone(),
                [44; 32],
                &governor,
            )
            .unwrap(),
        ],
    };
    let ticket = issue_invitation(grant.clone(), &member_key).unwrap();
    let verified = decode_and_verify_invitation(
        &ticket,
        Some(member.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &verified,
        &member,
        [18; 32],
        [19; 32],
        now_ns.checked_add(5_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let membership = MembershipChallenge::new(
        peer.room(),
        42,
        peer.authority(),
        &member,
        [20; 32],
        [21; 32],
        now_ns.checked_add(5_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let bootstrap = JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: member,
        role: MemberRole::Editor,
        room: peer.room().into(),
        workspace_id: 42,
        authority: peer.authority().clone(),
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: peer.rows(),
        governance_messages: peer
            .bootstrap_messages(&governor, now)
            .unwrap()
            .iter()
            .map(|message| postcard::to_allocvec(message).unwrap())
            .collect(),
        generation: 7,
    };
    let owner = OwnerBootstrapV1 {
        version: 1,
        device_cert: bootstrap.device_cert.clone(),
        room: bootstrap.room.clone(),
        workspace_id: bootstrap.workspace_id,
        authority: bootstrap.authority.clone(),
        certified_roster: bootstrap.certified_roster.clone(),
        revocations: bootstrap.revocations.clone(),
        records: bootstrap.records.clone(),
        governance_messages: bootstrap.governance_messages.clone(),
        generation: bootstrap.generation,
    };
    vec![
        JoinTransportResponse::Inspection(JoinInspection {
            invite_id: verified.invite_id(),
            grant,
        }),
        JoinTransportResponse::Challenge(JoinChallengeResponse { challenge }),
        JoinTransportResponse::Complete(bootstrap.clone()),
        JoinTransportResponse::MembershipChallenge(MembershipChallengeResponse {
            challenge: membership,
        }),
        JoinTransportResponse::MembershipComplete(bootstrap),
        JoinTransportResponse::Error {
            code: 409,
            message: "request already completed — retry status".into(),
        },
        JoinTransportResponse::OwnerMembershipComplete(owner),
    ]
}
fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 96 * 1024,
        collection_entries: 16384,
    }
}

#[test]
fn bounded_join_response_all_seven_real_variants_preserve_exact_original_bytes() {
    for response in responses() {
        let bytes = postcard::to_allocvec(&response).unwrap();
        let held = BoundedJoinResponse::decode(&bytes, limits()).unwrap();
        assert_eq!(held.response(), &response);
        assert_eq!(postcard::to_allocvec(held.response()).unwrap(), bytes);
    }
}

#[test]
fn bounded_join_response_nested_buffers_past_the_limit_are_refused() {
    let mut response = responses().remove(2);
    let JoinTransportResponse::Complete(bootstrap) = &mut response else {
        panic!()
    };
    bootstrap.governance_messages = vec![vec![42; 4096]; 8];
    let bytes = postcard::to_allocvec(&response).unwrap();
    let small = OuterLimits {
        allocation_bytes: 16 * 1024,
        collection_entries: 16384,
    };
    assert!(matches!(
        BoundedJoinResponse::decode(&bytes, small),
        Err(PeerResourceError::Capacity)
    ));
    // Each message is a byte sequence, so every byte is a collection entry.
    let roomy = OuterLimits {
        allocation_bytes: 256 * 1024,
        collection_entries: 65536,
    };
    assert!(BoundedJoinResponse::decode(&bytes, roomy).is_ok());
}

#[test]
fn bounded_join_response_malformed_nested_signature_is_refused() {
    let response = responses().remove(0);
    let JoinTransportResponse::Inspection(inspection) = &response else {
        panic!()
    };
    let signatures = [
        inspection.grant.authority.signature,
        inspection.grant.transport[0].host_signature,
        inspection.grant.delegation.as_ref().unwrap().signature,
    ];
    for signature in signatures {
        let mut bytes = postcard::to_allocvec(&response).unwrap();
        let mut needle = vec![64];
        needle.extend_from_slice(&signature);
        let position = bytes
            .windows(needle.len())
            .position(|value| value == needle)
            .unwrap();
        bytes.splice(position..position + needle.len(), [0]);
        assert!(matches!(
            BoundedJoinResponse::decode(&bytes, limits()),
            Err(PeerResourceError::Malformed)
        ));
    }
}

#[test]
fn bounded_join_response_announced_nested_length_and_trailing_bytes_are_refused() {
    let response = responses().remove(2);
    let JoinTransportResponse::Complete(bootstrap) = &response else {
        panic!()
    };
    let original = postcard::to_allocvec(&response).unwrap();
    let nested = postcard::to_allocvec(&bootstrap.governance_messages).unwrap();
    let position = original
        .windows(nested.len())
        .position(|window| window == nested)
        .unwrap();
    let mut huge = original[..position].to_vec();
    huge.extend(postcard::to_allocvec(&u64::MAX).unwrap());
    huge.extend(&original[position + nested.len()..]);
    let mut trailing = original.clone();
    trailing.push(0);
    for bytes in [huge, trailing] {
        assert!(BoundedJoinResponse::decode(&bytes, limits()).is_err());
    }
    assert!(matches!(
        BoundedJoinResponse::decode(
            &original,
            OuterLimits {
                allocation_bytes: 1,
                collection_entries: 16384
            },
        ),
        Err(PeerResourceError::Capacity)
    ));
}

#[test]
fn owner_bootstrap_wire_keeps_response_tags_and_role_bytes() {
    for (tag, response) in responses().iter().enumerate() {
        assert_eq!(
            postcard::to_allocvec(response).unwrap()[0],
            u8::try_from(tag).unwrap()
        );
    }
    for (role, wire) in [
        (MemberRole::Viewer, vec![0]),
        (MemberRole::Commenter, vec![1]),
        (MemberRole::Editor, vec![2]),
    ] {
        assert_eq!(postcard::to_allocvec(&role).unwrap(), wire);
    }
    let response = responses().pop().unwrap();
    let bytes = postcard::to_allocvec(&response).unwrap();
    let held = BoundedJoinResponse::decode(&bytes, limits()).unwrap();
    assert_eq!(held.response(), &response);
    for limited in [
        OuterLimits {
            allocation_bytes: 1024,
            collection_entries: 16384,
        },
        OuterLimits {
            allocation_bytes: 96 * 1024,
            collection_entries: 1,
        },
    ] {
        assert!(BoundedJoinResponse::decode(&bytes, limited).is_err());
    }
}
