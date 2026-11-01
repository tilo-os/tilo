//! A room has members, not one governing device: a participant that does not
//! host the room answers a join and removes a member, every device admits
//! what it wrote, and the removed device is refused everywhere.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_desktop_model::RoomId;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
    MemberRole, TransportHint,
};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::testing::{self, ROOM, governor::TestGovernor};
use tilo_peer_state::owner::{
    NativeWorkspaceOwner, OwnerJoinRooms, VerifiedNativeEnrollmentIntent, open_file,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_user_identity::{DeviceCert, RevocationList};

tilo_automerge_isolate::test_worker!();

/// A device at `key` under `root`.
fn device(root: u8, key: u8, label: &str) -> (SigningKey, SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[key; 32]);
    let cert = testing::certificate(&root, &key, label);
    (root, key, cert)
}

/// The host's signed hint that it answers the room's joins at its own endpoint.
fn hint(cert: &DeviceCert, key: &SigningKey) -> TransportHint {
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id: join::room_workspace_id(ROOM).unwrap(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
        ),
        host_user_public_key: cert.user_public_key,
        host_device_certificate: cert.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .unwrap()
}

/// `governor`'s answer to `request` from `remote` at `now`. Every step of an
/// engine's answer is decided before its future is first polled.
fn answer(
    governor: &JoinAnswer<OwnerJoinRooms>,
    remote: [u8; 32],
    request: JoinTransportRequest,
    now: SystemTime,
) -> JoinTransportResponse {
    let answer = std::pin::pin!(governor.answer(remote, request, testing::ns(now).unwrap()));
    match answer.poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(answer) => answer,
        std::task::Poll::Pending => panic!("an engine answers at once"),
    }
}

/// Enroll `owner` as `certificate`'s device into the room `governor` holds.
fn enroll(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
    governor: &mut TestGovernor,
    role: MemberRole,
    request_id: [u8; 32],
    now: SystemTime,
) {
    let now_ns = testing::ns(now).unwrap();
    let invitation = governor.issue(
        vec![governor.hint(iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&certificate.device_public_key).unwrap(),
        ))],
        role,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let hint = verified
        .grant()
        .transport
        .first()
        .cloned()
        .expect("the invitation appoints the host's hint");
    let JoinTransportResponse::Challenge(challenge) = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id,
        }),
    ) else {
        panic!("the room's host challenges the device");
    };
    let complete = JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, key).unwrap(),
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Complete(complete),
    ) else {
        panic!("the room's host admits the device");
    };
    owner
        .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
}

/// What `owner` publishes for its room now.
fn outgoing(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> Vec<NeighbourMsg> {
    owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).unwrap())
                .collect())
        })
        .unwrap()
}

/// Whether `owner`'s room still holds `certificate` as a member device.
fn is_member(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    certificate: &DeviceCert,
    now: SystemTime,
) -> bool {
    owner
        .with_current(room, |view| {
            Ok(view
                .peer()
                .permissions(certificate.device_public_key, now)
                .is_ok())
        })
        .unwrap()
}

/// A room with three people: its owner's device answers the room's host, a
/// second person's device holds it as a participant, and a third joins
/// through that participant, which then removes them.
#[test]
fn a_participant_that_does_not_host_the_room_admits_and_removes_members() {
    let directory = tempfile::tempdir().unwrap();
    let (_root_a, key_a, cert_a) = device(91, 92, "A's desktop");
    let (_root_b, key_b, cert_b) = device(73, 74, "B's desktop");
    let (_root_c, key_c, cert_c) = device(75, 76, "C's desktop");
    let room = RoomId::parse(ROOM).unwrap();

    // A's device is the room's host: it made the room.
    let mut governor =
        TestGovernor::empty_interactive(cert_a.clone(), key_a.clone(), ROOM).unwrap();
    let now = SystemTime::now();
    governor.pin_clock(now);
    let a = open_file(&directory.path().join("a.json")).unwrap();
    enroll(
        &a,
        &key_a,
        &cert_a,
        &mut governor,
        MemberRole::Moderator,
        [1; 32],
        now,
    );
    assert!(
        a.with_current(room, |view| Ok(view.hosted_here())).unwrap(),
        "A's device holds the room it made"
    );

    // The room's owner lets B invite: what a participant admits with.
    let invite = tilo_action_policy::Grant::new(
        "invite:b",
        tilo_action_policy::Subject::person(hex::encode(cert_b.user_public_key)),
        tilo_action_policy::roles::governance_action(
            ROOM,
            tilo_action_policy::roles::governance::INVITE,
        ),
        tilo_action_policy::KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(hex::encode(cert_a.user_public_key)),
    );
    a.commit_workspace_membership_command(
        OwnerCommitRequest {
            room: ROOM.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: cert_a.clone(),
                command: WorkspaceMembershipCommand::Grant { grant: invite },
            },
        },
        &key_a,
        now,
    )
    .unwrap();
    for message in outgoing(&a, room, now) {
        governor
            .receive(
                TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
                message,
            )
            .unwrap();
    }

    // B is a member, a participant of the room on their own device.
    let b = open_file(&directory.path().join("b.json")).unwrap();
    enroll(
        &b,
        &key_b,
        &cert_b,
        &mut governor,
        MemberRole::Moderator,
        [2; 32],
        now,
    );
    assert!(
        !b.with_current(room, |view| Ok(view.hosted_here())).unwrap(),
        "B's device holds the room without hosting it"
    );
    // A's device takes the room as its host has it now, B's membership
    // included.
    for (bytes, _) in governor.outgoing() {
        a.receive(
            room,
            TransportOrigin::RetainedRelayDelta(cert_a.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            now,
        )
        .unwrap();
    }
    assert!(is_member(&a, room, &cert_b, now), "A admits B's membership");

    // C joins through B's own engine: the invitation B issues and the
    // admission B commits are B's, published like any change B makes.
    let c = open_file(&directory.path().join("c.json")).unwrap();
    let issued = b
        .issue_invitation(
            room,
            &cert_b,
            IssueInvitationRequest {
                request_id: [3; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(cert_c.user_public_key),
                invitee_name: None,
                expires_at_ns: testing::ns(now)
                    .unwrap()
                    .checked_add(60_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
            vec![hint(&cert_b, &key_b)],
            &key_b,
            now,
        )
        .unwrap();
    let joined = JoinAnswer(OwnerJoinRooms::new(b.clone(), key_b.clone()));
    let verified = join::decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(cert_c.user_public_key),
        &RevocationList::new(),
        testing::ns(now).unwrap(),
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = answer(
        &joined,
        cert_c.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: issued.signed_invitation,
            device_cert: cert_c.clone(),
            request_id: [4; 32],
        }),
        now,
    ) else {
        panic!("B's engine challenges C");
    };
    let complete = JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &key_c).unwrap(),
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        cert_c.clone(),
        hint(&cert_b, &key_b),
        now,
    )
    .unwrap();
    let pending = c.prepare_enrollment_into(&intent, None, now).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = answer(
        &joined,
        cert_c.device_public_key,
        JoinTransportRequest::Complete(complete),
        now,
    ) else {
        panic!("B's engine admits C");
    };
    c.install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    assert!(is_member(&b, room, &cert_c, now), "B's engine admitted C");
    for message in outgoing(&b, room, now) {
        a.receive(
            room,
            TransportOrigin::RelayDelta(cert_b.device_public_key),
            message,
            now,
        )
        .unwrap();
    }
    assert!(
        is_member(&a, room, &cert_c, now),
        "A admits what B admitted"
    );
    // B removes C: no governing device signs it, the room's policy does.
    b.commit_workspace_membership_command(
        OwnerCommitRequest {
            room: ROOM.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: cert_b.clone(),
                command: WorkspaceMembershipCommand::Member {
                    change: tilo_action_policy::Change::RemoveMember {
                        person: hex::encode(cert_c.user_public_key),
                    },
                },
            },
        },
        &key_b,
        now,
    )
    .unwrap();
    assert!(
        !is_member(&b, room, &cert_c, now),
        "B's own engine dropped C"
    );
    for (label, owner) in [("A", &a), ("C", &c)] {
        for message in outgoing(&b, room, now) {
            owner
                .receive(
                    room,
                    TransportOrigin::RelayDelta(cert_b.device_public_key),
                    message.clone(),
                    now,
                )
                .unwrap_or_else(|error| panic!("{label} refuses B's removal: {error}"));
        }
    }
    assert!(
        !is_member(&a, room, &cert_c, now),
        "A converged on the removal B made"
    );
    // The removed device holds the same room and may write nothing into it.
    assert!(
        !is_member(&c, room, &cert_c, now),
        "C holds its own removal"
    );
    assert!(
        c.commit_workspace_membership_command(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: cert_c,
                    command: WorkspaceMembershipCommand::Settings(
                        tilo_peer_state::workspace_membership::SettingsChange::Rename {
                            name: "C's room".into(),
                        },
                    ),
                },
            },
            &key_c,
            now,
        )
        .is_err(),
        "the removed device's writes are refused"
    );
}
