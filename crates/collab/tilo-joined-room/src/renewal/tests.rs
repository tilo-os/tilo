//! A device whose certificate expires while it runs keeps its membership:
//! its person's root renews the certificate, the device proves its
//! membership with the renewal, and the governor and the device's own
//! record take it in place of the expired one.
use crate::testing::{self, ROOM};
use crate::{Nonces, RoomId, VerifiedNativeEnrollmentIntent, open_file};
use ed25519_dalek::SigningKey;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse, MemberRole};
use tilo_peer_state::{SeedState, seed_room};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const HOUR: Duration = Duration::from_secs(60 * 60);

fn nonces(seed: u8) -> Nonces {
    Nonces {
        invitation: [seed; 16],
        challenge: [seed; 32],
    }
}

/// A certificate of `key` from `root`, valid from `issued` for `lifetime`.
fn expiring(
    root: &SigningKey,
    key: &SigningKey,
    issued: SystemTime,
    lifetime: Duration,
) -> DeviceCert {
    let issued_at_ns = testing::ns(issued).unwrap();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Test slot".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns,
        expires_at_ns: Some(
            issued_at_ns.saturating_add(u64::try_from(lifetime.as_nanos()).unwrap()),
        ),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one device's life read top to bottom: join, expiry, renewal, restart"
)]
fn a_device_whose_certificate_expires_while_it_runs_keeps_its_membership() {
    let directory = tempfile::tempdir().unwrap();
    let governor = open_file(&directory.path().join("governor.json")).unwrap();
    let person = SigningKey::from_bytes(&[91; 32]);
    let key = SigningKey::from_bytes(&[92; 32]);
    let device = testing::certificate(&person, &key, "Desktop");
    let room = RoomId::parse(ROOM).unwrap();
    let joined_at = SystemTime::now();
    let joined_ns = testing::ns(joined_at).unwrap();
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
            joined_at,
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

    // A test slot: its throwaway root certifies its device for a day.
    let slot_root = SigningKey::from_bytes(&[81; 32]);
    let slot_key = SigningKey::from_bytes(&[82; 32]);
    let first = expiring(
        &slot_root,
        &slot_key,
        joined_at.checked_sub(HOUR).unwrap(),
        24 * HOUR,
    );
    let issued = governor
        .issue_invitation(
            room,
            IssueInvitationRequest {
                request_id: [1; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(first.user_public_key),
                invitee_name: None,
                expires_at_ns: joined_ns.saturating_add(60_000_000_000),
                inviter_name: None,
            },
            hint.clone(),
            &key,
            nonces(1),
            joined_at,
        )
        .unwrap();
    let verified = join::decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(first.user_public_key),
        &RevocationList::new(),
        joined_ns,
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = governor.serve_join(
        first.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: issued.signed_invitation,
            device_cert: first.clone(),
            request_id: [95; 32],
        }),
        &key,
        nonces(2),
        joined_at,
    ) else {
        panic!("the governor challenges the slot");
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &slot_key).unwrap(),
    };
    let member_path = directory.path().join("member.json");
    let member = open_file(&member_path).unwrap();
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        first.clone(),
        hint,
        joined_at,
    )
    .unwrap();
    let pending = member
        .prepare_enrollment_into(&intent, None, joined_at)
        .unwrap();
    let JoinTransportResponse::Complete(bootstrap) = governor.serve_join(
        first.device_public_key,
        JoinTransportRequest::Complete(complete),
        &key,
        nonces(3),
        joined_at,
    ) else {
        panic!("the governor admits the slot");
    };
    member
        .install(
            &testing::finalize(intent, pending, bootstrap).unwrap(),
            joined_at,
        )
        .unwrap();

    // Two days on, the first certificate has expired; the slot's root
    // renewed it when it was due.
    let later = joined_at.checked_add(48 * HOUR).unwrap();
    let later_ns = testing::ns(later).unwrap();
    let renewed = expiring(
        &slot_root,
        &slot_key,
        later.checked_sub(HOUR).unwrap(),
        24 * HOUR,
    );
    assert!(first.is_expired_at(later_ns));
    let membership = |device_cert: &DeviceCert, seed: u8| {
        governor.serve_join(
            slot_key.verifying_key().to_bytes(),
            JoinTransportRequest::MembershipChallenge(join::MembershipChallengeRequest {
                room: ROOM.into(),
                workspace_id,
                device_cert: device_cert.clone(),
                request_id: [seed; 32],
            }),
            &key,
            nonces(seed),
            later,
        )
    };
    assert!(
        matches!(membership(&first, 10), JoinTransportResponse::Error { .. }),
        "the expired certificate proves nothing"
    );
    let JoinTransportResponse::MembershipChallenge(challenge) = membership(&renewed, 11) else {
        panic!("the governor challenges the renewed certificate as the device it renews");
    };
    let JoinTransportResponse::MembershipComplete(answer) = governor.serve_join(
        slot_key.verifying_key().to_bytes(),
        JoinTransportRequest::MembershipComplete(join::MembershipCompleteRequest {
            proof: join::sign_membership_proof(challenge.challenge, &slot_key).unwrap(),
        }),
        &key,
        nonces(12),
        later,
    ) else {
        panic!("the governor answers the slot's renewed membership");
    };
    assert_eq!(answer.device_cert, renewed);
    assert_eq!(
        answer.role,
        MemberRole::Editor,
        "the same member, with its role"
    );
    member
        .renew_membership(room, &renewed, &answer, later)
        .unwrap();

    governor
        .with_current(room, |view| {
            assert!(view.peer().certificates().contains(&renewed));
            assert!(!view.peer().certificates().contains(&first));
            view.peer()
                .permissions(renewed.device_public_key, later)
                .map(|_| ())
        })
        .unwrap();
    member
        .with_current(room, |view| {
            assert_eq!(view.device(), &renewed);
            assert!(view.peer().certificates().contains(&renewed));
            Ok(())
        })
        .unwrap();
    // A stale certificate is refused after the renewal.
    assert!(matches!(
        membership(&first, 13),
        JoinTransportResponse::Error { .. }
    ));

    // The renewed membership is what the device starts with next time.
    drop(member);
    let member = open_file(&member_path).unwrap();
    member
        .with_current(room, |view| {
            assert_eq!(view.device(), &renewed);
            Ok(())
        })
        .unwrap();
}
