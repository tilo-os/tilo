//! A room this relay governs decides its own administration: what another
//! person's device may do is what the room's governance lets that person,
//! and what no room decides stays the relay's person's.

use ed25519_dalek::Signer as _;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::rooms::{
    JoinReply, JoinStep, RoomsReply, RoomsRequest, join_proof_digest,
};
use tilo_relay_protocol::voice::{CallOperation, CallState};
use tilo_relay_protocol::{AuthenticatedPeer, PeerRole};
use tilo_user_identity::{DeviceCert, sign_device_cert};

use crate::rooms::tests::{governor, guest_device, signed_pre_admission, view_read_relay};
use crate::rooms::{
    CancelInvitationRequest, IssueInvitationRequest, PreAdmitRequest, RemoveMemberRequest, Rooms,
    SetMemberRoleRequest, WaitListed,
};

/// A new person admitted to `room` as an Editor, through the owner's
/// pre-admission: their device's certificate.
async fn admit(rooms: &Rooms, room: &str, owner: &DeviceCert, seed: u8) -> DeviceCert {
    let token = [seed; 8];
    let issued = rooms
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(room, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (key, cert) = guest_device();
    let JoinReply::Challenge { challenge } = rooms
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: cert.clone(),
            request_id: rand::random(),
        })
        .await
    else {
        panic!("no challenge");
    };
    let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
    let reply = rooms
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: format!("Person {seed}"),
            token: token.to_vec(),
        })
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    cert
}

async fn grant(rooms: &Rooms, room: &str, owner: &DeviceCert, person: &DeviceCert, action: &str) {
    let who = hex::encode(person.user_public_key);
    let grant = Grant::new(
        format!("test:{action}:{who}"),
        Subject::Person(who),
        roles::governance_action(room, action),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    rooms
        .put_room_grants(room, owner, request_id, vec![grant])
        .await
        .unwrap();
}

async fn set_role(
    rooms: &Rooms,
    room: &str,
    by: &DeviceCert,
    person: &DeviceCert,
    role: MemberRole,
) {
    rooms
        .set_member_role(room, by, role_request(person, role, rand::random()))
        .await
        .unwrap();
}

fn role_request(
    person: &DeviceCert,
    role: MemberRole,
    request_id: [u8; 32],
) -> SetMemberRoleRequest {
    SetMemberRoleRequest {
        request_id,
        user_public_key: person.user_public_key,
        role,
    }
}

fn invitation(request_id: u8, role: MemberRole) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [request_id; 32],
        role,
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        inviter_name: None,
    }
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn an_address_left_at_the_door_is_on_the_wait_list_and_never_a_person_to_admit() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    assert_eq!(
        rooms.wait(&room, " Bo@Example.org ", 10).await.unwrap(),
        WaitListed::Added
    );
    let ask = |request| rooms.page_rooms(&ada, request);
    let list = || RoomsRequest::WaitList { room: room.clone() };

    // Someone the room does not let invite reads no wait list.
    assert!(matches!(ask(list()).await, RoomsReply::Refused(_)));
    grant(rooms, &room, &owner, &ada, governance::INVITE).await;
    let RoomsReply::WaitList(listed) = ask(list()).await else {
        panic!("someone who may invite reads the wait list");
    };
    let [entry] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    assert_eq!(entry.contact, "bo@example.org");
    let request_id = entry.request_id;

    // The address is nobody to let in: no join request, nothing to admit
    // or decline under its id.
    assert!(matches!(
        ask(RoomsRequest::Requests { room: room.clone() }).await,
        RoomsReply::Requests(waiting) if waiting.is_empty()
    ));
    for request in [
        RoomsRequest::Admit {
            room: room.clone(),
            request_id,
        },
        RoomsRequest::Decline {
            room: room.clone(),
            request_id,
        },
    ] {
        assert!(matches!(ask(request).await, RoomsReply::Refused(_)));
    }

    // Taken off the list once, then there is nothing to take off.
    let unlist = || RoomsRequest::Unlist {
        room: room.clone(),
        request_id,
    };
    assert!(matches!(
        ask(unlist()).await,
        RoomsReply::WaitList(left) if left.is_empty()
    ));
    assert!(matches!(ask(unlist()).await, RoomsReply::Refused(_)));
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn a_member_granted_invite_runs_invitations_and_an_editor_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;

    // An Editor by role alone governs nothing.
    assert!(
        rooms
            .issue(&room, &ada, invitation(10, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(rooms.pending(&room, &ada).await.is_err());
    assert!(matches!(
        rooms
            .page_rooms(&ada, RoomsRequest::Requests { room: room.clone() })
            .await,
        RoomsReply::Refused(_)
    ));

    // The room grants Ada its invite action: she invites, up to her role.
    grant(rooms, &room, &owner, &ada, governance::INVITE).await;
    let issued = rooms
        .issue(&room, &ada, invitation(11, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        rooms
            .pending(&room, &ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = rooms
        .issue(&room, &ada, invitation(12, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        rooms
            .page_rooms(&ada, RoomsRequest::Requests { room: room.clone() })
            .await,
        RoomsReply::Requests(_)
    ));
    assert!(matches!(
        rooms
            .page_rooms(&ada, RoomsRequest::Notices { room: room.clone() })
            .await,
        RoomsReply::Notices(_)
    ));
    rooms
        .cancel(
            &room,
            &ada,
            CancelInvitationRequest {
                request_id: [13; 32],
                invite_id: issued.invite_id,
            },
        )
        .await
        .unwrap();
    assert!(
        !rooms
            .pending(&room, &owner)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn roles_and_removals_follow_the_rooms_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    let cy = admit(rooms, &room, &owner, 3).await;
    let dee = admit(rooms, &room, &owner, 4).await;

    // A Moderator removes an Editor, but sets no roles.
    set_role(rooms, &room, &owner, &bo, MemberRole::Moderator).await;
    assert!(
        rooms
            .set_member_role(&room, &bo, role_request(&cy, MemberRole::Viewer, [20; 32]))
            .await
            .is_err()
    );
    rooms
        .remove_member(
            &room,
            &bo,
            RemoveMemberRequest {
                request_id: [21; 32],
                user_public_key: dee.user_public_key,
            },
        )
        .await
        .unwrap();

    // Granted set-role, Ada hands out roles up to her own, and never
    // changes an administrator's: only the owner makes or unmakes those.
    assert!(
        rooms
            .set_member_role(&room, &ada, role_request(&cy, MemberRole::Viewer, [22; 32]))
            .await
            .is_err()
    );
    grant(rooms, &room, &owner, &ada, governance::SET_ROLE).await;
    set_role(rooms, &room, &ada, &cy, MemberRole::Viewer).await;
    assert!(
        rooms
            .set_member_role(
                &room,
                &ada,
                role_request(&cy, MemberRole::Moderator, [23; 32])
            )
            .await
            .is_err()
    );
    rooms
        .set_member_administrator(&room, &owner, [24; 32], cy.user_public_key)
        .await
        .unwrap();
    let administrator = rooms
        .set_member_role(&room, &ada, role_request(&cy, MemberRole::Viewer, [25; 32]))
        .await
        .unwrap_err();
    assert!(
        administrator.to_string().contains("administrators"),
        "{administrator}"
    );
    // The owner's devices still change anyone's role.
    set_role(rooms, &room, &owner, &cy, MemberRole::Editor).await;

    // Nobody removes the owner, the owner's devices included.
    for by in [&owner, &bo] {
        assert!(
            rooms
                .remove_member(
                    &room,
                    by,
                    RemoveMemberRequest {
                        request_id: rand::random(),
                        user_public_key: owner.user_public_key,
                    },
                )
                .await
                .is_err()
        );
    }
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn an_administrator_governs_within_their_role_without_further_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    let cy = admit(rooms, &room, &owner, 3).await;
    set_role(rooms, &room, &owner, &cy, MemberRole::Viewer).await;

    rooms
        .set_member_administrator(&room, &owner, [50; 32], ada.user_public_key)
        .await
        .unwrap();
    // The co-owner invites, up to their own role, with no grant beyond the
    // owner making them an administrator.
    let issued = rooms
        .issue(&room, &ada, invitation(51, MemberRole::Editor))
        .await
        .unwrap();
    assert!(
        rooms
            .pending(&room, &ada)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.invite_id == issued.invite_id)
    );
    let above = rooms
        .issue(&room, &ada, invitation(52, MemberRole::Moderator))
        .await
        .unwrap_err();
    assert!(above.to_string().contains("Moderator"), "{above}");
    assert!(matches!(
        rooms
            .page_rooms(&ada, RoomsRequest::Requests { room: room.clone() })
            .await,
        RoomsReply::Requests(_)
    ));

    // They set roles up to their own and remove those ranked below them,
    // never a fellow Editor.
    set_role(rooms, &room, &ada, &cy, MemberRole::Commenter).await;
    assert!(
        rooms
            .set_member_role(
                &room,
                &ada,
                role_request(&bo, MemberRole::Moderator, [53; 32])
            )
            .await
            .is_err()
    );
    assert!(
        rooms
            .remove_member(
                &room,
                &ada,
                RemoveMemberRequest {
                    request_id: [54; 32],
                    user_public_key: bo.user_public_key,
                },
            )
            .await
            .is_err()
    );
    rooms
        .remove_member(
            &room,
            &ada,
            RemoveMemberRequest {
                request_id: [55; 32],
                user_public_key: cy.user_public_key,
            },
        )
        .await
        .unwrap();

    // They run the room's calls.
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let answer = rooms.voice_call(&room, &peer(&ada), &end).await;
    assert!(
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call")),
        "{answer:?}"
    );

    // An Editor by role alone still governs nothing, and the owner unmaking
    // the administrator takes it all back.
    assert!(
        rooms
            .issue(&room, &bo, invitation(56, MemberRole::Viewer))
            .await
            .is_err()
    );
    set_role(rooms, &room, &owner, &ada, MemberRole::Editor).await;
    assert!(
        rooms
            .issue(&room, &ada, invitation(57, MemberRole::Viewer))
            .await
            .is_err()
    );
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn the_next_governance_change_rewrites_an_administrators_old_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    let who = hex::encode(ada.user_public_key);
    // An administrator's grant as rooms held it before: on the room's
    // resource, where the Editor's own Deny outranked it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&who).as_str(),
        Subject::Person(who),
        roles::room_resource(&room),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    rooms
        .put_room_grants(&room, &owner, [60; 32], vec![legacy])
        .await
        .unwrap();
    rooms
        .issue(&room, &ada, invitation(61, MemberRole::Editor))
        .await
        .unwrap();
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn what_no_room_decides_stays_the_relay_persons() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    // Ada holds every governance action the room has.
    for action in governance::ALL {
        grant(rooms, &room, &owner, &ada, action).await;
    }
    rooms
        .issue(&room, &ada, invitation(30, MemberRole::Viewer))
        .await
        .unwrap();

    assert!(
        rooms.enter(&room, &ada).await.is_err(),
        "entering makes an owner"
    );
    for request in [
        RoomsRequest::List,
        RoomsRequest::Create {
            name: "Another".into(),
        },
        RoomsRequest::Enter { room: room.clone() },
    ] {
        let reply = rooms.page_rooms(&ada, request.clone()).await;
        assert!(
            matches!(reply, RoomsReply::Refused(_)),
            "{request:?}: {reply:?}"
        );
    }
    assert!(
        rooms
            .set_member_administrator(&room, &ada, [31; 32], bo.user_public_key)
            .await
            .is_err()
    );
    assert!(
        rooms
            .put_room_grants(&room, &ada, [32; 32], Vec::new())
            .await
            .is_err()
    );
    assert!(
        rooms
            .pre_admit(
                &ada,
                PreAdmitRequest {
                    request_id: [33; 32],
                    grant: signed_pre_admission(&room, b"token"),
                    recipient_user_public_key: None,
                    inviter_name: None,
                },
            )
            .await
            .is_err()
    );
    assert_eq!(rooms.list().await.len(), 1);
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn calls_are_run_by_the_owner_and_by_members_granted_call() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    set_role(rooms, &room, &owner, &bo, MemberRole::Moderator).await;
    // Ending a call the room never had: past the gate, the call itself
    // answers that it is unknown.
    let end = CallOperation::End {
        call: [9; 32],
        epoch: 1,
    };
    let past_the_gate = |answer: &Result<CallState, String>| {
        answer
            .as_ref()
            .err()
            .is_some_and(|reason| reason.contains("unknown call"))
    };
    let owners = rooms.voice_call(&room, &peer(&owner), &end).await;
    assert!(past_the_gate(&owners), "{owners:?}");
    for member in [&ada, &bo] {
        let answer = rooms.voice_call(&room, &peer(member), &end).await;
        assert!(answer.is_err() && !past_the_gate(&answer), "{answer:?}");
    }
    grant(rooms, &room, &owner, &ada, governance::CALL).await;
    let granted = rooms.voice_call(&room, &peer(&ada), &end).await;
    assert!(past_the_gate(&granted), "{granted:?}");
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn an_expired_certificate_administers_nothing_even_the_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Team").await.unwrap().room;
    let mut expired = owner.clone();
    expired.expires_at_ns = Some(2);
    sign_device_cert(
        &mut expired,
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    assert!(
        rooms
            .issue(&room, &expired, invitation(40, MemberRole::Viewer))
            .await
            .is_err()
    );
    assert!(rooms.pending(&room, &expired).await.is_err());
    assert!(rooms.enter(&room, &expired).await.is_err());
    rooms.pending(&room, &owner).await.unwrap();
    relay.shutdown().await;
}

fn peer(cert: &DeviceCert) -> AuthenticatedPeer {
    AuthenticatedPeer {
        cert: cert.clone(),
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        role: PeerRole::Device,
    }
}
