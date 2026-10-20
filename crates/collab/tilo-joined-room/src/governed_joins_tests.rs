use super::{GovernedInvitation, GovernedJoins};
use crate::testing::{self, ROOM};
use crate::{NativeWorkspaceOwner, RoomId, VerifiedNativeEnrollmentIntent, open_file};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::join::{
    self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
    MemberRole,
};
use tilo_user_identity::{DeviceCert, RevocationList};

struct Device {
    _directory: tempfile::TempDir,
    owner: NativeWorkspaceOwner,
    key: SigningKey,
    cert: DeviceCert,
}

fn device(seed: u8, root: u8, label: &str) -> Device {
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("owner.json")).unwrap();
    let key = SigningKey::from_bytes(&[seed; 32]);
    let cert = testing::certificate(&SigningKey::from_bytes(&[root; 32]), &key, label);
    Device {
        _directory: directory,
        owner,
        key,
        cert,
    }
}

fn challenge(
    joins: &GovernedJoins,
    joining: &Device,
    invitation: &[u8],
    request_id: [u8; 32],
) -> Result<JoinCompleteRequest, String> {
    let response = joins.answer(
        joining.cert.device_public_key,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: invitation.to_vec(),
            device_cert: joining.cert.clone(),
            request_id,
        }),
    );
    let JoinTransportResponse::Challenge(response) = response else {
        return Err(format!("{response:?}"));
    };
    Ok(JoinCompleteRequest {
        proof: join::sign_join_proof(response.challenge, &joining.key)?,
    })
}

/// `device` governs `room` and serves it at its own endpoint: the hint it
/// signs for that.
fn govern(governor: &Device, room: RoomId, now: SystemTime) -> join::TransportHint {
    governor
        .owner
        .create_governed(
            room,
            1,
            &governor.cert,
            &governor.key,
            tilo_peer_state::workspace_seed(
                ROOM,
                &governor.cert,
                std::slice::from_ref(&governor.cert),
                "Garden",
                0x0042,
            )
            .unwrap(),
            now,
        )
        .unwrap();
    let endpoint = iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.cert.device_public_key).unwrap(),
    );
    governor
        .owner
        .governed_hint(
            room,
            &governor.key,
            endpoint,
            "https://relay.invalid".into(),
        )
        .unwrap()
}

/// An invitation into the room `governor` governs, signed with `key`.
fn invite(
    governor: &Device,
    key: &SigningKey,
    recipient: Option<&Device>,
    hint: &join::TransportHint,
    now_ns: u64,
) -> Result<Vec<u8>, String> {
    governor.owner.issue_invitation(
        RoomId::parse(ROOM).ok_or("room")?,
        key,
        GovernedInvitation {
            role: MemberRole::Editor,
            recipient_user_public_key: recipient.map(|device| device.cert.user_public_key),
            invitee_name: recipient.map(|_| "Guest".into()),
            expires_at_ns: now_ns.saturating_add(60_000_000_000),
            transport: vec![hint.clone()],
        },
    )
}

/// A person this desktop invites joins the room it governs straight from
/// it: the desktop admits them, and their device holds the room under this
/// desktop as its governor.
#[test]
fn a_room_this_device_governs_admits_the_person_it_invites() {
    let now = SystemTime::now();
    let governor = device(81, 82, "Desktop");
    let guest = device(83, 84, "Guest laptop");
    let stranger = device(85, 86, "Stranger");
    let room = RoomId::parse(ROOM).unwrap();
    let hint = govern(&governor, room, now);
    let ns = testing::ns(now).unwrap();
    let invitation = invite(&governor, &governor.key, Some(&guest), &hint, ns).unwrap();
    assert!(
        invite(&governor, &guest.key, None, &hint, ns).is_err(),
        "only the governor's key signs its invitations"
    );
    let joins = GovernedJoins::new(governor.owner.clone(), governor.key.clone());

    assert!(
        challenge(&joins, &stranger, &invitation, [1; 32]).is_err(),
        "an invitation naming one person is redeemed by no other"
    );
    let complete = challenge(&joins, &guest, &invitation, [2; 32]).unwrap();
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(guest.cert.user_public_key),
        &RevocationList::new(),
        ns,
    )
    .unwrap();
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        guest.cert.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = guest
        .owner
        .prepare_enrollment_into(&intent, None, now)
        .unwrap();
    let answer = |from: &Device| {
        joins.answer(
            from.cert.device_public_key,
            JoinTransportRequest::Complete(complete.clone()),
        )
    };
    assert!(
        matches!(answer(&stranger), JoinTransportResponse::Error { .. }),
        "a proof completes only from the device that made it"
    );
    let JoinTransportResponse::Complete(bootstrap) = answer(&guest) else {
        panic!("the governor admits its guest");
    };
    assert_eq!(
        answer(&guest),
        JoinTransportResponse::Complete(bootstrap.clone()),
        "a lost answer is given again"
    );
    let enrollment = testing::finalize(intent, pending, bootstrap).unwrap();
    guest.owner.install(&enrollment, now).unwrap();

    guest
        .owner
        .with_current(room, |view| {
            assert!(!view.governed_here());
            assert_eq!(view.governor(), &governor.cert);
            Ok(())
        })
        .unwrap();
    governor
        .owner
        .with_current(room, |view| {
            assert!(view.peer().certificates().contains(&guest.cert));
            Ok(())
        })
        .unwrap();
    assert!(
        challenge(&joins, &guest, &invitation, [3; 32])
            .and_then(|complete| {
                match joins.answer(
                    guest.cert.device_public_key,
                    JoinTransportRequest::Complete(complete),
                ) {
                    JoinTransportResponse::Complete(_) => Ok(()),
                    other => Err(format!("{other:?}")),
                }
            })
            .is_err(),
        "an invitation is redeemed once"
    );
}
