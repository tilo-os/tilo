//! A direct room between two people: the end that creates it governs it and
//! admits the other person by who they are; their devices join by their
//! certificates, with no invitation.

use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::Role;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

fn cert(root: &SigningKey, device: &SigningKey) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

struct Pair {
    peer: StatePeer,
    reporter: SigningKey,
    team_root: SigningKey,
    now: SystemTime,
}

/// The reporter's governing replica of their direct room with the team.
fn direct_room(team_root: SigningKey) -> Pair {
    let reporter_root = SigningKey::from_bytes(&[11; 32]);
    let reporter = SigningKey::from_bytes(&[12; 32]);
    let reporter_cert = cert(&reporter_root, &reporter);
    let room = tilo_reference::DirectRoom::between(
        &hex::encode(reporter_root.verifying_key().to_bytes()),
        &hex::encode(team_root.verifying_key().to_bytes()),
    )
    .unwrap()
    .room_id();
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(1_000_000))
        .unwrap();
    let mut peer = StatePeer::new(
        &room,
        reporter_cert.clone(),
        vec![reporter_cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(&room, &reporter_cert, std::slice::from_ref(&reporter_cert))
                .unwrap(),
            documents: Vec::new(),
        },
        &reporter,
        now,
    )
    .unwrap();
    Pair {
        peer,
        reporter,
        team_root,
        now,
    }
}

fn hello(cert: &DeviceCert) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
    }
}

#[test]
fn the_other_person_of_a_direct_room_joins_on_any_device() {
    let mut pair = direct_room(SigningKey::from_bytes(&[21; 32]));
    let team_laptop = SigningKey::from_bytes(&[22; 32]);
    let team_laptop_cert = cert(&pair.team_root, &team_laptop);
    let now = pair.now;
    assert!(
        pair.peer
            .receive_from(
                TransportOrigin::RelayDelta(team_laptop_cert.device_public_key),
                hello(&team_laptop_cert),
                now,
            )
            .is_err(),
        "nobody is admitted before the room names them"
    );
    let reporter = pair.reporter.clone();
    assert!(
        pair.peer
            .admit_direct_member(pair.team_root.verifying_key().to_bytes(), &reporter, now)
            .unwrap()
    );
    pair.peer
        .receive_from(
            TransportOrigin::RelayDelta(team_laptop_cert.device_public_key),
            hello(&team_laptop_cert),
            now,
        )
        .unwrap();
    assert_eq!(
        pair.peer
            .permissions(team_laptop_cert.device_public_key, now)
            .unwrap()
            .0,
        Role::Editor
    );
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "Thanks, we are on it").unwrap();
    doc.commit();
    pair.peer
        .publish_incremental("so.tilo.chat", "", doc.save(), &team_laptop, now)
        .expect("the team writes the conversation");
}

#[test]
fn only_the_pairs_other_person_is_admitted() {
    let mut pair = direct_room(SigningKey::from_bytes(&[31; 32]));
    let stranger_root = SigningKey::from_bytes(&[32; 32]);
    let reporter = pair.reporter.clone();
    let now = pair.now;
    assert!(
        pair.peer
            .admit_direct_member(stranger_root.verifying_key().to_bytes(), &reporter, now)
            .is_err(),
        "a room between the reporter and the team admits nobody else this way"
    );
    let stranger = SigningKey::from_bytes(&[33; 32]);
    let stranger_cert = cert(&stranger_root, &stranger);
    assert!(
        pair.peer
            .receive_from(
                TransportOrigin::RelayDelta(stranger_cert.device_public_key),
                hello(&stranger_cert),
                now,
            )
            .is_err()
    );
}
