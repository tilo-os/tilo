//! A room held on its host's word: installed from the membership answer,
//! admitting the replay and members' certificates, and kept across a reopen.
use super::*;
use crate::join::JoinTransportResponse;

struct Vouched {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
}

/// Hold the fixture's room on its membership answer and admit its replay.
fn vouched() -> Vouched {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-rooms.json");
    let owner = open_file(&path).unwrap();
    let (_, bootstrap, messages, governor, now) = raw_fixture();
    let device = bootstrap.device_cert.clone();
    let workspace = bootstrap.workspace_id;
    let answer =
        postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(bootstrap)).unwrap();
    assert!(
        owner
            .install_vouched(workspace, &answer, &device, now)
            .unwrap()
    );
    assert!(
        !owner
            .install_vouched(workspace, &answer, &device, now)
            .unwrap(),
        "a room held under the same authority stays as it is"
    );
    let room = RoomId::parse(ROOM).unwrap();
    for message in messages {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    Vouched {
        _directory: directory,
        path,
        owner,
        room,
        now,
    }
}

#[test]
fn a_room_its_host_vouches_for_admits_its_replay_and_reopens() {
    let held = vouched();
    let calls = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(held.room, |view| {
                Ok(view
                    .peer()
                    .validated_media_policy()?
                    .ok_or("missing admitted policy")?
                    .calls
                    .len())
            })
            .unwrap()
    };
    assert_eq!(calls(&held.owner), 1);
    assert!(
        held.owner
            .with_current(held.room, |view| view.enrollment_invitation().map(drop))
            .is_err(),
        "a vouched room has no enrollment of its own"
    );
    drop(held.owner);
    let reopened = open_file(&held.path).unwrap();
    assert_eq!(reopened.rooms(), vec![held.room]);
    assert_eq!(calls(&reopened), 1);
}

#[test]
fn a_membership_answer_for_another_device_or_room_is_refused() {
    let (_, bootstrap, _, _, now) = raw_fixture();
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = open_file(&directory.path().join("joined-rooms.json")).unwrap();
    let workspace = bootstrap.workspace_id;
    let stranger = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[71; 32]),
        &SigningKey::from_bytes(&[72; 32]),
        "Stranger",
    );
    let answer = postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(
        bootstrap.clone(),
    ))
    .unwrap();
    assert!(
        owner
            .install_vouched(workspace, &answer, &stranger, now)
            .is_err()
    );
    assert!(
        owner
            .install_vouched(workspace ^ 1, &answer, &bootstrap.device_cert, now)
            .is_err()
    );
    let error = JoinTransportResponse::Error {
        code: 403,
        message: "no".into(),
    };
    assert!(
        owner
            .install_vouched(
                workspace,
                &postcard::to_stdvec(&error).unwrap(),
                &bootstrap.device_cert,
                now
            )
            .is_err()
    );
    assert!(owner.rooms().is_empty());
}

#[test]
fn a_member_introduces_itself_and_an_unlisted_device_does_not() {
    let held = vouched();
    let hello = |cert: &tilo_user_identity::DeviceCert| NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
    };
    let receive = |cert: &tilo_user_identity::DeviceCert| {
        held.owner.receive(
            held.room,
            TransportOrigin::RelayDelta(cert.device_public_key),
            hello(cert),
            held.now,
        )
    };
    let member =
        crate::owner::testing::certificate(&SigningKey::from_bytes(&[64; 32]), &member(), "Member");
    receive(&member).unwrap();
    let unlisted = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[64; 32]),
        &SigningKey::from_bytes(&[70; 32]),
        "Second",
    );
    assert!(
        receive(&unlisted).is_err(),
        "the room's roster lists who it admits"
    );
}
