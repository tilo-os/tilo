//! Media-call governance as a historical retention proof replays it: the same
//! room policy and bound Chat guards `StatePeer` applies live must refuse a
//! retained room or snapshot that violates them.
use super::*;
use automerge::{ObjType, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::{Role, SignedOp};
use ed25519_dalek::SigningKey;
use tilo_peer_state::room::{self, media};
use tilo_peer_state::{
    SeedDocument, SeedState, StatePeer, TransportOrigin, peer_handle, seed_room,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "so.tilo.chat";
fn fixture() -> (StatePeer, SigningKey, SystemTime) {
    let root = SigningKey::from_bytes(&[101; 32]);
    let key = SigningKey::from_bytes(&[102; 32]);
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "media governor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, &root).unwrap();
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let mut peer = StatePeer::new(
        ROOM,
        certificate.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let channel = {
        let entry = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        chat.put_object(&entry, "v", ObjType::Map).unwrap()
    };
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "home").unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &certificate, &[certificate.clone()]).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    (peer, key, now)
}
fn message(doc: &mut AutoCommit) -> NeighbourMsg {
    NeighbourMsg::SyncResp {
        room: ROOM.into(),
        changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    }
}
fn policy(peer: &StatePeer) -> media::RoomMediaV1 {
    let chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = {
        let entry = chat.get(&channels, "voice").unwrap().unwrap().1;
        chat.get(&entry, "v").unwrap().unwrap().1
    };
    media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(ROOM).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(peer.admitted_room().unwrap(), &[]).unwrap(),
        calls: BTreeMap::from([(
            [7; 32],
            media::CallV1 {
                binding: media::ChatCallBindingV1 {
                    app_id: APP.into(),
                    record_id: String::new(),
                    channel_id: "voice".into(),
                    channel_object: object.to_string(),
                    chat_workspace_id: "home".into(),
                },
                epoch: 1,
                phase: media::CallPhaseV1::Active,
                allowed: media::DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    }
}
fn install_policy(peer: &mut StatePeer, key: &SigningKey, now: SystemTime) {
    let value = policy(peer);
    let mut doc = peer.admitted_room().unwrap().clone();
    doc.put(
        ROOT,
        media::FIELD,
        ScalarValue::Bytes(media::encode(&value).unwrap()),
    )
    .unwrap();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        message(&mut doc),
        now,
    )
    .unwrap();
}
/// The Chat snapshot after one edit: 2 re-kinds the bound channel, which the
/// call binding forbids; 5 renames it, which the binding allows.
fn edited_chat(peer: &StatePeer, operation: u8) -> Vec<u8> {
    let mut chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = {
        let entry = chat.get(&channels, "voice").unwrap().unwrap().1;
        chat.get(&entry, "v").unwrap().unwrap().1
    };
    match operation {
        2 => chat.put(&object, "kind", "text").unwrap(),
        _ => chat.put(&object, "name", "new title").unwrap(),
    }
    chat.commit_edit("fixture", "name");
    chat.save()
}
fn shell_channel() -> [u8; 32] {
    *blake3::hash(b"shell-tree").as_bytes()
}

#[test]
fn retained_room_rejects_malformed_present_media_policy() {
    let (peer, key, _) = fixture();
    let mut room = peer.admitted_room().unwrap().clone();
    room.put(ROOT, media::FIELD, "not policy bytes").unwrap();
    let body = postcard::to_allocvec(&message(&mut room)).unwrap();
    let authority = RetentionAuthority::new(ROOM, peer.authority().clone()).unwrap();
    assert!(
        verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            shell_channel(),
            &body,
            RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
}

/// The governor's signed Chat snapshot of `bytes` at the next sequence after
/// `prior`, and the coverage pairing it, as the live publication path signs a
/// retained checkpoint.
fn signed_snapshot(
    key: &SigningKey,
    now: SystemTime,
    prior: &Coverage,
    bytes: Vec<u8>,
) -> (NeighbourMsg, Coverage) {
    let author = key.verifying_key().to_bytes();
    let seq = prior.sequence.checked_add(1).unwrap();
    let heads = AutoCommit::load(&bytes)
        .unwrap()
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut envelope = SignedOp::new_unsigned(
        DocumentMutation::Snapshot(bytes).encode().unwrap(),
        collab_types::document_artifact_id(APP, ROOM, ""),
        collab_types::Ed25519PublicKey::from_bytes(author),
        collab_types::document_attestation_id(APP, ROOM, ""),
        collab_types::OpKind::Change,
        now,
        seq,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    let mut watermarks = prior.watermarks.clone();
    watermarks.insert(hex::encode(author), seq);
    let coverage = Coverage {
        room: ROOM.into(),
        app: APP.into(),
        record: String::new(),
        payload_hash: *blake3::hash(&envelope.payload).as_bytes(),
        heads,
        author,
        sequence: seq,
        watermarks,
    };
    let message = NeighbourMsg::AppDocument {
        peer: peer_handle(&author),
        app_id: APP.into(),
        cluster_id: ROOM.into(),
        record_id: String::new(),
        envelope: Box::new(envelope),
    };
    (message, coverage)
}

#[test]
fn retained_snapshot_rejects_mismatched_exact_channel() {
    let (mut peer, key, now) = fixture();
    install_policy(&mut peer, &key, now);
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let authority = RetentionAuthority::new(ROOM, peer.authority().clone()).unwrap();
    let verify_room = |message: &NeighbourMsg| {
        verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            shell_channel(),
            &postcard::to_allocvec(message).unwrap(),
            RoomInput::Checkpoint,
            None,
        )
        .unwrap()
    };
    let room = verify_room(&batch[0]);
    let channel = *blake3::hash(format!("app\0{APP}\0").as_bytes()).as_bytes();
    assert!(
        verify_snapshot(
            &room,
            key.verifying_key().to_bytes(),
            channel,
            &postcard::to_allocvec(&batch[1]).unwrap()
        )
        .is_ok()
    );
    let prior = room
        .coverages()
        .unwrap()
        .remove(&tilo_peer_state::coverage::route_key(APP, ""))
        .unwrap();
    // A room that pairs `snapshot` exactly, so only the binding can refuse it.
    let paired = |snapshot: Vec<u8>| {
        let (signed, coverage) = signed_snapshot(&key, now, &prior, snapshot);
        let mut document = peer.admitted_room().unwrap().clone();
        tilo_peer_state::coverage::put(&mut document, coverage.clone()).unwrap();
        tilo_peer_state::durability::put(
            &mut document,
            BTreeMap::from([(
                tilo_peer_state::coverage::route_key(APP, ""),
                DurableReceipt::new(coverage, 2).unwrap(),
            )]),
        )
        .unwrap();
        (verify_room(&message(&mut document)), signed)
    };
    let (room, renamed) = paired(edited_chat(&peer, 5));
    assert!(
        verify_snapshot(
            &room,
            key.verifying_key().to_bytes(),
            channel,
            &postcard::to_allocvec(&renamed).unwrap()
        )
        .is_ok(),
        "an edit the binding allows pairs and verifies"
    );
    let (room, bad) = paired(edited_chat(&peer, 2));
    assert!(
        verify_snapshot(
            &room,
            key.verifying_key().to_bytes(),
            channel,
            &postcard::to_allocvec(&bad).unwrap()
        )
        .is_err()
    );
}

fn member(root_seed: u8, device_seed: u8) -> DeviceCert {
    let root = SigningKey::from_bytes(&[root_seed; 32]);
    let key = SigningKey::from_bytes(&[device_seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "member".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}
fn members() -> (StatePeer, SigningKey, SystemTime) {
    let (original, key, now) = fixture();
    let members = [member(110, 111), member(120, 121), member(101, 103)];
    let roster = std::iter::once(original.authority().clone())
        .chain(members.iter().cloned())
        .collect::<Vec<_>>();
    let mut room = room::candidate(
        None,
        &seed_room(ROOM, original.authority(), &roster).unwrap(),
    )
    .unwrap();
    let caps = room.get(ROOT, "members").unwrap().unwrap().1;
    for (cert, role) in [(&members[0], Role::Editor), (&members[1], Role::Viewer)] {
        room.put(
            &caps,
            hex::encode(cert.user_public_key),
            ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
        )
        .unwrap();
    }
    let mut peer = StatePeer::new(
        ROOM,
        original.authority().clone(),
        roster,
        RevocationList::new(),
        now,
    )
    .unwrap();
    let NeighbourMsg::SyncResp { changes, .. } = message(&mut room) else {
        panic!("unexpected fixture shape")
    };
    peer.install_seed(
        SeedState {
            room_changes: changes,
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: original.document(APP, "").unwrap(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    (peer, key, now)
}

#[test]
fn retained_room_governed_revocation_fence_cannot_be_removed_by_higher_revision() {
    let (mut peer, key, now) = members();
    install_policy(&mut peer, &key, now);
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: peer.authority().user_public_key,
        revoked_device_public_key: [99; 32],
        revoked_at_ns: 10,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[101; 32]))
        .unwrap();
    peer.publish_governed_revocations(&[revocation], &key, now)
        .unwrap();
    let before = peer.admitted_room().unwrap().clone();
    let authority = RetentionAuthority::new(ROOM, peer.authority().clone()).unwrap();
    let mut current = before;
    let proof = verify_room(
        &authority,
        key.verifying_key().to_bytes(),
        shell_channel(),
        &postcard::to_allocvec(&message(&mut current)).unwrap(),
        RoomInput::Checkpoint,
        None,
    )
    .unwrap();
    let ids = current.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(peer.authority().device_public_key);
    let mut row: serde_json::Value =
        serde_json::from_slice(&room::governance_of(Some(&current)).identities[&id]).unwrap();
    assert!(
        !row["revocations"].as_array().unwrap().is_empty(),
        "the fence is published before it is stripped"
    );
    row["revocations"] = serde_json::Value::Array(Vec::new());
    current
        .put(
            &ids,
            id,
            ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
        )
        .unwrap();
    // A malicious pinned writer supplies a self-consistent higher counter.
    let mut changed = media::parse(&current).unwrap().unwrap();
    changed.authorization_revision += 1;
    changed.authorization_basis = media::security_basis(&current, &[]).unwrap();
    current
        .put(
            ROOT,
            media::FIELD,
            ScalarValue::Bytes(media::encode(&changed).unwrap()),
        )
        .unwrap();
    assert!(
        verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            shell_channel(),
            &postcard::to_allocvec(&message(&mut current)).unwrap(),
            RoomInput::Checkpoint,
            Some(&proof)
        )
        .is_err()
    );
    // Live admission refusing the same candidate is `tilo-peer-state`'s own
    // media governance test.
}
