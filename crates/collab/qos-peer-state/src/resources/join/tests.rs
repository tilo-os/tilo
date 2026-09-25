use super::*;
use crate::join::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use tilo_user_identity::RevocationList;

fn responses() -> Vec<JoinTransportResponse> {
    let (mut peer, governor, member_key, member, now) = crate::admission_tests::seeded();
    let now_ns = now
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
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
            cert: Box::new(peer.authority().clone()),
            revocations: vec![revocation],
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
                Some(now_ns + 10_000),
                &governor,
            )
            .unwrap(),
        ),
        role: MemberRole::Editor,
        nonce: [17; 16],
        recipient_user_public_key: Some(member.user_public_key),
        invitee_name: Some("A real guest 🦀".into()),
        expires_at_ns: now_ns + 10_000,
        transport: vec![
            sign_transport_hint(
                TRANSPORT_QOS_SESSION_JOIN_HTTP_V1,
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
        now_ns + 5_000,
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
        now_ns + 5_000,
        now_ns,
    )
    .unwrap();
    let bootstrap = JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: member.clone(),
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
        let allowance = NativeAllowance::new(128 * 1024).unwrap();
        let original = allowance.available_bytes();
        let held = assert_precharged(&allowance, || {
            BoundedJoinResponse::decode(&bytes, limits(), &allowance)
        })
        .unwrap();
        assert_eq!(held.response(), &response);
        assert_eq!(postcard::to_allocvec(held.response()).unwrap(), bytes);
        assert!(allowance.available_bytes() < original);
        drop(held);
        assert_eq!(allowance.available_bytes(), original);
    }
}

#[test]
fn bounded_join_response_nested_buffers_refuse_and_release_shared_credit() {
    let mut response = responses().remove(2);
    let JoinTransportResponse::Complete(bootstrap) = &mut response else {
        panic!()
    };
    bootstrap.governance_messages = vec![vec![42; 4096]; 8];
    let bytes = postcard::to_allocvec(&response).unwrap();
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
    let original = allowance.available_bytes();
    let small = OuterLimits {
        allocation_bytes: 16 * 1024,
        collection_entries: 16384,
    };
    assert!(matches!(
        assert_precharged(&allowance, || BoundedJoinResponse::decode(
            &bytes, small, &allowance
        )),
        Err(PeerResourceError::Capacity)
    ));
    assert_eq!(allowance.available_bytes(), original);
    let bytes = postcard::to_allocvec(&responses()[0]).unwrap();
    let first = BoundedJoinResponse::decode(&bytes, limits(), &allowance).unwrap();
    assert!(matches!(
        BoundedJoinResponse::decode(&bytes, limits(), &allowance),
        Err(PeerResourceError::Capacity)
    ));
    drop(first);
    assert!(BoundedJoinResponse::decode(&bytes, limits(), &allowance).is_ok());
}

#[test]
fn bounded_join_response_malformed_nested_signature_has_precharged_error_and_no_leak() {
    let response = responses().remove(0);
    let JoinTransportResponse::Inspection(inspection) = &response else {
        panic!()
    };
    let signatures = [
        inspection.grant.authority.signature,
        inspection.grant.transport[0].host_signature,
        inspection.grant.delegation.as_ref().unwrap().signature,
    ];
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
    let original = allowance.available_bytes();
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
            assert_precharged(&allowance, || BoundedJoinResponse::decode(
                &bytes,
                limits(),
                &allowance
            )),
            Err(PeerResourceError::Malformed)
        ));
        assert_eq!(allowance.available_bytes(), original);
    }
}

#[test]
fn bounded_join_response_announced_nested_length_and_trailing_bytes_fail_before_escape() {
    let response = responses().remove(2);
    let JoinTransportResponse::Complete(bootstrap) = &response else {
        panic!()
    };
    let original = postcard::to_allocvec(&response).unwrap();
    let nested = postcard::to_allocvec(&bootstrap.governance_messages).unwrap();
    let generation = postcard::to_allocvec(&bootstrap.generation).unwrap();
    let position = original.len() - generation.len() - nested.len();
    assert_eq!(&original[position..position + nested.len()], nested);
    let mut huge = original[..position].to_vec();
    huge.extend(postcard::to_allocvec(&u64::MAX).unwrap());
    huge.extend(generation);
    let mut trailing = original.clone();
    trailing.push(0);
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
    let baseline = allowance.available_bytes();
    for bytes in [huge, trailing] {
        assert!(
            assert_precharged(&allowance, || BoundedJoinResponse::decode(
                &bytes,
                limits(),
                &allowance
            ))
            .is_err()
        );
        assert_eq!(allowance.available_bytes(), baseline);
    }
    assert!(matches!(
        assert_precharged(&allowance, || BoundedJoinResponse::decode(
            &original,
            OuterLimits {
                allocation_bytes: 1,
                collection_entries: 16384
            },
            &allowance
        )),
        Err(PeerResourceError::Capacity)
    ));
}

#[test]
fn bounded_join_response_retains_decode_custody_after_allowance_seals() {
    let bytes = postcard::to_allocvec(&responses()[0]).unwrap();
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
    let ledger_metadata = allowance.retained_bytes();
    let held = BoundedJoinResponse::decode(&bytes, limits(), &allowance).unwrap();
    allowance.finish_turn();
    assert!(allowance.retained_bytes() > ledger_metadata);
    assert_eq!(postcard::to_allocvec(held.response()).unwrap(), bytes);
    assert!(BoundedJoinResponse::decode(&bytes, limits(), &allowance).is_err());
    drop(held);
    assert_eq!(allowance.retained_bytes(), ledger_metadata);
}

#[test]
fn owner_bootstrap_wire_preserves_legacy_response_tags_and_role_bytes() {
    for (tag, response) in responses().iter().enumerate() {
        assert_eq!(postcard::to_allocvec(response).unwrap()[0], tag as u8);
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
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
    let baseline = allowance.retained_bytes();
    let held = assert_precharged(&allowance, || {
        BoundedJoinResponse::decode(&bytes, limits(), &allowance)
    })
    .unwrap();
    allowance.finish_turn();
    assert_eq!(held.response(), &response);
    assert!(allowance.retained_bytes() > baseline);
    drop(held);
    assert_eq!(allowance.retained_bytes(), baseline);
    let allowance = NativeAllowance::new(128 * 1024).unwrap();
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
        assert!(BoundedJoinResponse::decode(&bytes, limited, &allowance).is_err());
    }
}
