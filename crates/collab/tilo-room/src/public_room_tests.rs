//! A public room admits strangers through its delegated host: a device that
//! redeemed the standing invitation is a member by the host's signed
//! admission, verified against the owner's delegation, never by governance.

use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::{Ed25519PublicKey, HostAdmissionSpec, HostAdmissionV1, Role};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const ROOM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const APP: &str = "so.tilo.chat";

struct World {
    peer: StatePeer,
    owner_root: SigningKey,
    host: SigningKey,
    now: SystemTime,
}

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

/// A room of `class` the owner's device governs.
fn room(class: collab_types::ChannelClass) -> World {
    let owner_root = SigningKey::from_bytes(&[1; 32]);
    let owner_device = SigningKey::from_bytes(&[2; 32]);
    let owner = cert(&owner_root, &owner_device, "team laptop");
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(1_000_000))
        .unwrap();
    let mut changes = seed_room(ROOM, &owner, std::slice::from_ref(&owner)).unwrap();
    let mut doc =
        AutoCommit::new().with_actor(automerge::ActorId::from(owner.device_public_key.to_vec()));
    for change in &changes {
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, change).unwrap();
    }
    let before = doc.get_heads();
    tilo_desktop_model::projection::project_channel_class(&mut doc, class);
    doc.commit();
    changes.push(doc.save_after(&before));
    let mut peer =
        StatePeer::new(ROOM, owner.clone(), vec![owner], RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: changes,
            documents: Vec::new(),
        },
        &owner_device,
        now,
    )
    .unwrap();
    World {
        peer,
        owner_root,
        host: SigningKey::from_bytes(&[3; 32]),
        now,
    }
}

/// The host's signed admission of the person `root`, under the owner's
/// delegation.
fn admission(world: &World, root: &SigningKey) -> HostAdmissionV1 {
    let workspace = crate::room_workspace_id(ROOM).unwrap();
    let delegation = collab_types::mint_root(
        collab_types::InviteSpec {
            subject: Ed25519PublicKey::from_bytes(world.host.verifying_key().to_bytes()),
            workspace,
            role: Role::Commenter,
            caps: [
                collab_types::caps::Verb::Read,
                collab_types::caps::Verb::Comment,
            ]
            .into_iter()
            .collect(),
            sub_resource: None,
            not_after: None,
        },
        &world.owner_root,
    )
    .unwrap();
    HostAdmissionV1::sign(
        HostAdmissionSpec {
            room: ROOM.into(),
            workspace,
            member_user: Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
            role: Role::Commenter,
            invite: [7; 32],
            admitted_at_unix_seconds: 999_000,
            delegation: vec![delegation],
        },
        &world.host,
    )
    .unwrap()
}

fn receive(world: &mut World, source: [u8; 32], message: NeighbourMsg) -> Result<(), String> {
    world
        .peer
        .receive_from(TransportOrigin::RelayDelta(source), message, world.now)
        .map(|_| ())
        .map_err(|error| String::clone(&error))
}

fn hello(cert: &DeviceCert) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
    }
}

fn comment(world: &mut World, device: &SigningKey) -> Result<NeighbourMsg, String> {
    let author = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let record =
        collab_types::comment_record_id(&author, "report", "chat.channel:c_feedback").unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "Send fails").unwrap();
    doc.commit();
    let now = world.now;
    world
        .peer
        .publish_incremental(APP, &record, doc.save(), device, now)
}

#[test]
fn a_host_admitted_stranger_comments_in_a_public_room() {
    let mut world = room(collab_types::ChannelClass::Public);
    let stranger_root = SigningKey::from_bytes(&[41; 32]);
    let stranger = SigningKey::from_bytes(&[42; 32]);
    let stranger_cert = cert(&stranger_root, &stranger, "reporter");
    let host = world.host.verifying_key().to_bytes();

    assert!(
        receive(
            &mut world,
            stranger_cert.device_public_key,
            hello(&stranger_cert)
        )
        .is_err(),
        "a stranger's hello is refused before any admission"
    );
    let admitted = admission(&world, &stranger_root);
    receive(
        &mut world,
        host,
        NeighbourMsg::RoomAdmission {
            room: ROOM.into(),
            admission: Box::new(admitted),
        },
    )
    .unwrap();
    receive(
        &mut world,
        stranger_cert.device_public_key,
        hello(&stranger_cert),
    )
    .unwrap();

    let (role, _, _) = world
        .peer
        .permissions(stranger_cert.device_public_key, world.now)
        .unwrap();
    assert_eq!(role, Role::Commenter, "admitted at the host's role");
    comment(&mut world, &stranger).expect("an admitted Commenter comments");
    let access = world.peer.governed_access(world.now).unwrap();
    assert!(
        access
            .member_devices
            .contains(&stranger_cert.device_public_key),
        "the admitted device is served the room"
    );

    // The admission survives a restart, as governance does.
    let bytes = world.peer.durable().unwrap();
    let owner = world.peer.authority().clone();
    let restored = StatePeer::restore(
        &bytes,
        ROOM,
        owner.clone(),
        vec![owner],
        RevocationList::new(),
        world.now,
    )
    .unwrap();
    assert_eq!(
        restored
            .permissions(stranger_cert.device_public_key, world.now)
            .unwrap()
            .0,
        Role::Commenter
    );
}

#[test]
fn only_a_public_room_admits_by_host() {
    let mut world = room(collab_types::ChannelClass::Encrypted);
    let stranger_root = SigningKey::from_bytes(&[51; 32]);
    let stranger = SigningKey::from_bytes(&[52; 32]);
    let stranger_cert = cert(&stranger_root, &stranger, "reporter");
    let admitted = admission(&world, &stranger_root);
    let host = world.host.verifying_key().to_bytes();
    receive(
        &mut world,
        host,
        NeighbourMsg::RoomAdmission {
            room: ROOM.into(),
            admission: Box::new(admitted),
        },
    )
    .unwrap();
    receive(
        &mut world,
        stranger_cert.device_public_key,
        hello(&stranger_cert),
    )
    .unwrap();
    assert!(
        world
            .peer
            .permissions(stranger_cert.device_public_key, world.now)
            .is_err(),
        "an encrypted room grants nothing by host admission"
    );
    assert!(comment(&mut world, &stranger).is_err());
}

#[test]
fn an_admission_the_owner_did_not_delegate_grants_nothing() {
    let mut world = room(collab_types::ChannelClass::Public);
    let stranger_root = SigningKey::from_bytes(&[61; 32]);
    let stranger = SigningKey::from_bytes(&[62; 32]);
    let stranger_cert = cert(&stranger_root, &stranger, "reporter");
    // Delegated by someone who does not own the room.
    world.owner_root = SigningKey::from_bytes(&[99; 32]);
    let forged = admission(&world, &stranger_root);
    let host = world.host.verifying_key().to_bytes();
    receive(
        &mut world,
        host,
        NeighbourMsg::RoomAdmission {
            room: ROOM.into(),
            admission: Box::new(forged),
        },
    )
    .unwrap();
    receive(
        &mut world,
        stranger_cert.device_public_key,
        hello(&stranger_cert),
    )
    .unwrap();
    assert!(
        world
            .peer
            .permissions(stranger_cert.device_public_key, world.now)
            .is_err()
    );
}

#[test]
fn a_public_room_refuses_sealed_content() {
    let mut world = room(collab_types::ChannelClass::Public);
    let stranger_root = SigningKey::from_bytes(&[71; 32]);
    let stranger = SigningKey::from_bytes(&[72; 32]);
    let stranger_cert = cert(&stranger_root, &stranger, "reporter");
    let admitted = admission(&world, &stranger_root);
    let host = world.host.verifying_key().to_bytes();
    receive(
        &mut world,
        host,
        NeighbourMsg::RoomAdmission {
            room: ROOM.into(),
            admission: Box::new(admitted),
        },
    )
    .unwrap();
    receive(
        &mut world,
        stranger_cert.device_public_key,
        hello(&stranger_cert),
    )
    .unwrap();
    let author = Ed25519PublicKey::from_bytes(stranger_cert.device_public_key);
    let record =
        collab_types::comment_record_id(&author, "sealed", "chat.channel:c_feedback").unwrap();
    let mut op = collab_types::SignedOp::new_unsigned(
        vec![1, 2, 3],
        collab_types::document_artifact_id(APP, ROOM, &record),
        author,
        collab_types::document_attestation_id(APP, ROOM, &record),
        collab_types::OpClass::Comment,
        world.now,
        1,
    );
    op.encryption = Some(collab_types::EncryptionMetadata {
        epoch: 1,
        channel: collab_types::ChannelId(uuid::Uuid::nil()),
        scheme: collab_types::EncryptionScheme::MlsApplication,
    });
    collab_types::sign_op(&mut op, &stranger).unwrap();
    let sealed = NeighbourMsg::AppDocument {
        peer: peer_handle(&stranger_cert.device_public_key),
        app_id: APP.into(),
        cluster_id: ROOM.into(),
        record_id: record,
        envelope: Box::new(op),
    };
    let refused = receive(&mut world, stranger_cert.device_public_key, sealed).unwrap_err();
    assert!(refused.contains("public"), "{refused}");
}

fn recording(world: &World, device: &SigningKey, active: bool, at: u64) -> NeighbourMsg {
    let owner = world.peer.authority().clone();
    let delegation = collab_types::RecordingDelegationV1::sign(
        collab_types::RecordingDelegationSpec {
            room: ROOM.into(),
            workspace: crate::room_workspace_id(ROOM).unwrap(),
            person: Ed25519PublicKey::from_bytes(owner.user_public_key),
            agent: "recorder".into(),
            active,
            issued_at_unix_seconds: at,
        },
        device,
    )
    .unwrap();
    NeighbourMsg::RecordingDelegation {
        room: ROOM.into(),
        delegation: Box::new(delegation),
        cert: Box::new(owner),
    }
}

fn lists_recorder(world: &World) -> bool {
    let owner = world.peer.authority().device_public_key;
    let (_, caps, _) = world.peer.permissions(owner, world.now).unwrap();
    caps.iter().any(|cap| {
        cap.subject == collab_types::caps::Subject::agent("recorder")
            && cap.verb == collab_types::caps::Verb::Read
    })
}

#[test]
fn a_members_recorder_is_listed_while_it_records() {
    let mut world = room(collab_types::ChannelClass::Encrypted);
    let owner_device = SigningKey::from_bytes(&[2; 32]);
    let source = owner_device.verifying_key().to_bytes();
    assert!(!lists_recorder(&world));
    let start = recording(&world, &owner_device, true, 10);
    receive(&mut world, source, start).unwrap();
    assert!(lists_recorder(&world), "the recorder is an agent member");
    let bytes = world.peer.durable().unwrap();
    let owner = world.peer.authority().clone();
    let restored = World {
        peer: StatePeer::restore(
            &bytes,
            ROOM,
            owner.clone(),
            vec![owner],
            RevocationList::new(),
            world.now,
        )
        .unwrap(),
        owner_root: SigningKey::from_bytes(&[1; 32]),
        host: SigningKey::from_bytes(&[3; 32]),
        now: world.now,
    };
    assert!(lists_recorder(&restored), "and after a restart");
    let replayed_start = recording(&world, &owner_device, true, 9);
    let stop = recording(&world, &owner_device, false, 11);
    receive(&mut world, source, stop).unwrap();
    receive(&mut world, source, replayed_start).unwrap();
    assert!(
        !lists_recorder(&world),
        "a stop holds against an older start replayed later"
    );
}

#[test]
fn every_device_of_an_admitted_person_joins() {
    let mut world = room(collab_types::ChannelClass::Public);
    let person = SigningKey::from_bytes(&[81; 32]);
    // The device that redeemed, and one the person set up later (a browser).
    let laptop = SigningKey::from_bytes(&[82; 32]);
    let browser = SigningKey::from_bytes(&[83; 32]);
    let laptop_cert = cert(&person, &laptop, "laptop");
    let browser_cert = cert(&person, &browser, "browser");
    let admitted = admission(&world, &person);
    let host = world.host.verifying_key().to_bytes();
    receive(
        &mut world,
        host,
        NeighbourMsg::RoomAdmission {
            room: ROOM.into(),
            admission: Box::new(admitted),
        },
    )
    .unwrap();
    for cert in [&laptop_cert, &browser_cert] {
        receive(&mut world, cert.device_public_key, hello(cert)).unwrap();
        assert_eq!(
            world
                .peer
                .permissions(cert.device_public_key, world.now)
                .unwrap()
                .0,
            Role::Commenter
        );
    }
    comment(&mut world, &browser).expect("the browser comments as the admitted person");
    let stranger = SigningKey::from_bytes(&[84; 32]);
    let stranger_cert = cert(&stranger, &SigningKey::from_bytes(&[85; 32]), "other");
    assert!(
        receive(
            &mut world,
            stranger_cert.device_public_key,
            hello(&stranger_cert)
        )
        .is_err(),
        "another person's device is not admitted by this admission"
    );
}
