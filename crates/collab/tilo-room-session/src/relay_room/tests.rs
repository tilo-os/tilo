#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions: a failed step is the test failing"
)]
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

fn cert(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn now() -> SystemTime {
    SystemTime::now()
}

const COMMUNITY: &str = "7a1a9e00-0000-4c0d-9a11-0000000000c1";

/// A standing invitation to `COMMUNITY`, minted by `owner`'s device.
fn standing_invite(
    owner: &SigningKey,
    owner_cert: &DeviceCert,
    host: iroh::EndpointAddr,
) -> String {
    let key = iroh::SecretKey::from_bytes(&owner.to_bytes());
    let mut draft = neighbour_wire::invite::InviteDraft::new(
        tilo_room::room_workspace_id(COMMUNITY).unwrap(),
        COMMUNITY,
        iroh::EndpointAddr::new(key.public()),
        neighbour_wire::invite::InviteRole::Commenter,
        u64::MAX,
    );
    draft.owner_device_cert = Some(owner_cert.clone());
    draft.host_addrs = vec![host];
    draft.standing = true;
    neighbour_wire::invite::mint_invite(&key, draft).unwrap()
}

#[test]
fn a_public_room_is_pinned_to_the_device_that_minted_its_invitation() {
    let team_root = SigningKey::from_bytes(&[1; 32]);
    let team = SigningKey::from_bytes(&[2; 32]);
    let team_cert = cert(&team_root, &team, "team laptop");
    let host = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[3; 32]).public());
    let invite = standing_invite(&team, &team_cert, host.clone());
    let own = cert(
        &SigningKey::from_bytes(&[4; 32]),
        &SigningKey::from_bytes(&[5; 32]),
        "Cloud host",
    );

    let room = HeldRoom::enter_public(&invite, &own, now()).unwrap();
    assert_eq!(room.room().to_string(), COMMUNITY);
    assert_eq!(room.peer().authority(), &team_cert);
    assert_eq!(room.hosts(), &[host]);
    assert_eq!(
        room.redeems(),
        Some(neighbour_wire::invite::invite_id(&invite)),
        "a new session redeems the invitation it was entered by"
    );
    assert!(
        !room.admits(own.device_public_key, now()),
        "nobody is admitted before the room's host says so"
    );
    let announced = room.announcements(&own, &RevocationList::new()).unwrap();
    assert_eq!(announced.len(), 1, "a member announces only its hello");

    let restored = HeldRoom::restore(room.record().unwrap(), &own, now()).unwrap();
    assert_eq!(restored.room(), room.room());
    assert_eq!(restored.entry(), room.entry());
    assert_eq!(restored.peer().authority(), &team_cert);
}

#[test]
fn a_single_use_invitation_enters_no_public_room() {
    let team_root = SigningKey::from_bytes(&[1; 32]);
    let team = SigningKey::from_bytes(&[2; 32]);
    let team_cert = cert(&team_root, &team, "team laptop");
    let key = iroh::SecretKey::from_bytes(&team.to_bytes());
    let mut draft = neighbour_wire::invite::InviteDraft::new(
        tilo_room::room_workspace_id(COMMUNITY).unwrap(),
        COMMUNITY,
        iroh::EndpointAddr::new(key.public()),
        neighbour_wire::invite::InviteRole::Commenter,
        u64::MAX,
    );
    draft.owner_device_cert = Some(team_cert.clone());
    let invite = neighbour_wire::invite::mint_invite(&key, draft).unwrap();
    assert!(HeldRoom::enter_public(&invite, &team_cert, now()).is_err());
}

#[test]
fn a_direct_room_is_governed_here_and_admits_the_other_person_on_any_device() {
    let me_root = SigningKey::from_bytes(&[11; 32]);
    let me = SigningKey::from_bytes(&[12; 32]);
    let me_cert = cert(&me_root, &me, "Cloud host");
    let team_root = SigningKey::from_bytes(&[21; 32]);
    let team = SigningKey::from_bytes(&[22; 32]);
    let team_cert = cert(&team_root, &team, "team laptop");

    let mut room = HeldRoom::open_direct(
        team_root.verifying_key().to_bytes(),
        Vec::new(),
        &me_cert,
        &me,
        now(),
    )
    .unwrap();
    let expected = tilo_reference::DirectRoom::between(
        &hex::encode(me_root.verifying_key().to_bytes()),
        &hex::encode(team_root.verifying_key().to_bytes()),
    )
    .unwrap()
    .room_id();
    assert_eq!(
        room.room().to_string(),
        expected,
        "both ends derive the same room"
    );
    assert_eq!(
        room.redeems(),
        None,
        "a direct room is joined without an invitation"
    );
    assert_eq!(room.peer().authority(), &me_cert);
    assert_eq!(
        room.announcements(&me_cert, &RevocationList::new())
            .unwrap()
            .len(),
        2,
        "the governing end announces its hello and the room's governance"
    );

    let hello = postcard::to_stdvec(&NeighbourMsg::IdentityHello {
        cert: Box::new(team_cert.clone()),
        revocations: Vec::new(),
    })
    .unwrap();
    assert!(
        room.receive(team_cert.device_public_key, &hello, false, now())
            .unwrap()
    );
    assert_eq!(
        room.peer()
            .permissions(team_cert.device_public_key, now())
            .unwrap()
            .0,
        collab_types::Role::Editor
    );

    let stranger = cert(
        &SigningKey::from_bytes(&[31; 32]),
        &SigningKey::from_bytes(&[32; 32]),
        "stranger",
    );
    let hello = postcard::to_stdvec(&NeighbourMsg::IdentityHello {
        cert: Box::new(stranger.clone()),
        revocations: Vec::new(),
    })
    .unwrap();
    assert!(
        room.receive(stranger.device_public_key, &hello, false, now())
            .is_err(),
        "nobody else joins a direct room"
    );
}

#[test]
fn nobody_has_a_direct_room_with_themselves() {
    let root = SigningKey::from_bytes(&[41; 32]);
    let device = SigningKey::from_bytes(&[42; 32]);
    let own = cert(&root, &device, "laptop");
    assert!(
        HeldRoom::open_direct(
            root.verifying_key().to_bytes(),
            Vec::new(),
            &own,
            &device,
            now()
        )
        .is_err()
    );
}
