use crate::testing::{self, ROOM};
use crate::{Nonces, RoomId, VerifiedNativeEnrollmentIntent, open_file};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::join::invitations::{CancelInvitationRequest, IssueInvitationRequest};
use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse, MemberRole};
use tilo_peer_state::{SeedState, seed_room};
use tilo_user_identity::RevocationList;

fn nonces(seed: u8) -> Nonces {
    Nonces {
        invitation: [seed; 16],
        challenge: [seed; 32],
    }
}

fn request(id: u8, recipient: [u8; 32], now_ns: u64) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [id; 32],
        role: MemberRole::Editor,
        recipient_user_public_key: Some(recipient),
        invitee_name: None,
        expires_at_ns: now_ns.saturating_add(60_000_000_000),
        inviter_name: Some("Ada".into()),
    }
}

/// A room this device governs is joined the way a relay's room is: its
/// invitation names this device as the host, the joining device redeems it
/// through the workspace-join exchange this device answers, installs the
/// bootstrap, and the governor keeps the receipt across a restart, so the
/// spent invitation admits nobody again.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one join read top to bottom: governor, invitation, exchange, install, restart"
)]
fn a_device_governed_room_is_joined_through_the_same_exchange_a_relay_answers() {
    let directory = tempfile::tempdir().unwrap();
    let governor_path = directory.path().join("governor.json");
    let governor = open_file(&governor_path).unwrap();
    let person = SigningKey::from_bytes(&[91; 32]);
    let key = SigningKey::from_bytes(&[92; 32]);
    let device = testing::certificate(&person, &key, "Desktop");
    let room = RoomId::parse(ROOM).unwrap();
    let now = SystemTime::now();
    let now_ns = testing::ns(now).unwrap();
    governor
        .create_governed(
            room,
            7,
            &device,
            &key,
            SeedState {
                room_changes: seed_room(ROOM, &device, std::slice::from_ref(&device)).unwrap(),
                documents: Vec::new(),
            },
            now,
        )
        .unwrap();
    let workspace_id = join::room_workspace_id(ROOM).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id,
        governing_authority: device.clone(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&device.device_public_key).unwrap(),
        ),
        host_user_public_key: device.user_public_key,
        host_device_certificate: device.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(&key)
    .unwrap();

    let guest_root = SigningKey::from_bytes(&[81; 32]);
    let guest_key = SigningKey::from_bytes(&[82; 32]);
    let guest = testing::certificate(&guest_root, &guest_key, "Laptop");
    let stranger = SigningKey::from_bytes(&[93; 32]);
    assert!(
        governor
            .issue_invitation(
                room,
                request(1, guest.user_public_key, now_ns),
                hint.clone(),
                &stranger,
                nonces(1),
                now,
            )
            .is_err(),
        "only the governing device issues"
    );
    let issued = governor
        .issue_invitation(
            room,
            request(1, guest.user_public_key, now_ns),
            hint.clone(),
            &key,
            nonces(1),
            now,
        )
        .unwrap();
    assert_eq!(
        governor.pending_invitations(room, &key, now).unwrap().len(),
        1
    );
    let verified = join::decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(guest.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    assert_eq!(verified.grant().workspace_id, workspace_id);

    let JoinTransportResponse::Challenge(challenge) = governor.serve_join(
        guest.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: issued.signed_invitation.clone(),
            device_cert: guest.clone(),
            request_id: [95; 32],
        }),
        &key,
        nonces(2),
        now,
    ) else {
        panic!("the governor challenges the invited device");
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &guest_key).unwrap(),
    };
    let member = open_file(&directory.path().join("member.json")).unwrap();
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        guest.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = member.prepare_enrollment_into(&intent, None, now).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = governor.serve_join(
        guest.device_public_key,
        JoinTransportRequest::Complete(complete.clone()),
        &key,
        nonces(3),
        now,
    ) else {
        panic!("the governor admits the invited device");
    };
    assert_eq!(
        governor.serve_join(
            guest.device_public_key,
            JoinTransportRequest::Complete(complete),
            &key,
            nonces(4),
            now,
        ),
        JoinTransportResponse::Complete(bootstrap.clone()),
        "a retried Complete hears the same bootstrap"
    );
    member
        .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    member
        .with_current(room, |view| {
            assert!(!view.governed_here(), "the guest holds a replica");
            assert_eq!(view.governor(), &device);
            Ok(())
        })
        .unwrap();
    governor
        .with_current(room, |view| {
            view.peer()
                .permissions(guest.device_public_key, now)
                .map(|_| ())
        })
        .unwrap();
    assert!(
        governor
            .pending_invitations(room, &key, now)
            .unwrap()
            .is_empty(),
        "the invitation is spent"
    );

    // What the governor recorded between the steps is in its record: a
    // restart still refuses the spent invitation.
    drop(governor);
    let governor = open_file(&governor_path).unwrap();
    let JoinTransportResponse::Error { message, .. } = governor.serve_join(
        guest.device_public_key,
        JoinTransportRequest::Inspect {
            signed_invitation: issued.signed_invitation,
        },
        &key,
        nonces(5),
        now,
    ) else {
        panic!("a spent invitation is refused after a restart");
    };
    assert!(message.contains("redeemed"), "{message}");

    // An invitation cancelled before anyone redeems it admits nobody.
    let second = governor
        .issue_invitation(
            room,
            request(2, guest.user_public_key, now_ns),
            governor
                .with_current(room, |view| {
                    Ok(IrohWorkspaceHostHintV1 {
                        version: 1,
                        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
                        room: ROOM.into(),
                        workspace_id,
                        governing_authority: view.governor().clone(),
                        endpoint_addr: iroh_base::EndpointAddr::new(
                            iroh_base::EndpointId::from_bytes(&device.device_public_key).unwrap(),
                        ),
                        host_user_public_key: device.user_public_key,
                        host_device_certificate: device.clone(),
                        packet_relay_url: "iroh:direct".into(),
                    }
                    .transport_hint(&key)
                    .unwrap())
                })
                .unwrap(),
            &key,
            nonces(6),
            now,
        )
        .unwrap();
    governor
        .cancel_invitation(
            room,
            &CancelInvitationRequest {
                request_id: [3; 32],
                invite_id: second.invite_id,
            },
            &key,
        )
        .unwrap();
    assert!(matches!(
        governor.serve_join(
            guest.device_public_key,
            JoinTransportRequest::Inspect {
                signed_invitation: second.signed_invitation,
            },
            &key,
            nonces(7),
            now,
        ),
        JoinTransportResponse::Error { .. }
    ));
}
