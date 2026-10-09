use super::*;
use crate::room::media;
use automerge::{ObjType, transaction::Transactable};
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

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
    let channel = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
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
#[test]
fn room_media_native_rejects_malformed_policy_before_any_mutation() {
    let (mut peer, key, now) = fixture();
    let before = peer.durable().unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    room.put(ROOT, media::FIELD, "not policy bytes").unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            message(&mut room),
            now
        )
        .is_err()
    );
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_restore_rejects_malformed_present_policy() {
    let (mut peer, _, now) = fixture();
    peer.core
        .room_doc
        .as_mut()
        .unwrap()
        .put(ROOT, media::FIELD, "not policy bytes")
        .unwrap();
    let bytes = peer.durable().unwrap();
    assert!(
        StatePeer::restore(
            &bytes,
            ROOM,
            peer.core.authority.clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
}
#[test]
fn room_media_retained_rejects_malformed_present_policy() {
    let (peer, key, _) = fixture();
    let mut room = peer.admitted_room().unwrap().clone();
    room.put(ROOT, media::FIELD, "not policy bytes").unwrap();
    let body = postcard::to_allocvec(&message(&mut room)).unwrap();
    let authority =
        crate::retained_proof::RetentionAuthority::new(ROOM, peer.core.authority).unwrap();
    assert!(
        crate::retained_proof::verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            *blake3::hash(b"shell-tree").as_bytes(),
            &body,
            crate::retained_proof::RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
}
fn policy(peer: &StatePeer) -> media::RoomMediaV1 {
    let chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = chat.get(&channels, "voice").unwrap().unwrap().1;
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
fn install_policy(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    value: &media::RoomMediaV1,
) -> Result<ReceiveOutcome, String> {
    let mut doc = peer.admitted_room().unwrap().clone();
    doc.put(
        ROOT,
        media::FIELD,
        automerge::ScalarValue::Bytes(media::encode(value).unwrap()),
    )
    .unwrap();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        message(&mut doc),
        now,
    )
}
fn edited_chat(peer: &StatePeer, operation: u8) -> (Vec<u8>, Vec<u8>) {
    let mut chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let heads = chat.get_heads();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let object = chat.get(&channels, "voice").unwrap().unwrap().1;
    match operation {
        0 => chat.delete(&channels, "voice").unwrap(),
        1 => {
            chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        }
        2 => chat.put(&object, "kind", "text").unwrap(),
        3 => chat.put(&object, "workspace", "elsewhere").unwrap(),
        4 => chat.put(&object, "archived", true).unwrap(),
        _ => chat.put(&object, "name", "new title").unwrap(),
    }
    chat.commit();
    let changes = chat
        .get_changes(&heads)
        .iter()
        .flat_map(|c| c.raw_bytes().iter().copied())
        .collect();
    (chat.save(), changes)
}
#[test]
fn room_media_active_chat_publication_rejects_destructive_edits_atomically() {
    for operation in 0..5 {
        for snapshot in [false, true] {
            let (mut peer, key, now) = fixture();
            let value = policy(&peer);
            install_policy(&mut peer, &key, now, &value).unwrap();
            let before = peer.durable().unwrap();
            let (snapshot_bytes, incremental) = edited_chat(&peer, operation);
            let result = if snapshot {
                peer.publish_document(APP, "", snapshot_bytes, &key, now)
            } else {
                peer.publish_incremental(APP, "", incremental, &key, now)
            };
            assert!(
                result.is_err(),
                "operation {operation}, snapshot {snapshot}"
            );
            assert_eq!(
                before,
                peer.durable().unwrap(),
                "coverage, floors and index must remain atomic"
            );
        }
    }
}
#[test]
fn room_media_room_policy_cannot_activate_a_present_mismatched_binding() {
    let (mut peer, key, now) = fixture();
    let mut value = policy(&peer);
    value
        .calls
        .get_mut(&[7; 32])
        .unwrap()
        .binding
        .chat_workspace_id = "wrong".into();
    let before = peer.durable().unwrap();
    assert!(install_policy(&mut peer, &key, now, &value).is_err());
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_policy_epoch_and_basis_cannot_change_at_equal_revision() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let before = peer.durable().unwrap();
    let mut changed = value.clone();
    changed.calls.get_mut(&[7; 32]).unwrap().allowed.publish = false;
    assert!(install_policy(&mut peer, &key, now, &changed).is_err());
    assert_eq!(before, peer.durable().unwrap());
    changed = value;
    changed.authorization_basis[0] ^= 1;
    assert!(install_policy(&mut peer, &key, now, &changed).is_err());
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_restore_rejects_present_mismatched_bound_chat() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let bytes = edited_chat(&peer, 0).0;
    peer.core
        .documents
        .get_mut(&Route {
            app: APP.into(),
            record: String::new(),
        })
        .unwrap()
        .doc = AutoCommit::load(&bytes).unwrap().into();
    assert!(
        StatePeer::restore(
            &peer.durable().unwrap(),
            ROOM,
            peer.core.authority.clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
}

fn member(root_seed: u8, device_seed: u8) -> (SigningKey, DeviceCert) {
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
    (key, cert)
}
fn members() -> (
    StatePeer,
    SigningKey,
    [(SigningKey, DeviceCert); 3],
    SystemTime,
) {
    let (original, key, now) = fixture();
    let members = [member(110, 111), member(120, 121), member(101, 103)];
    let roster = std::iter::once(original.core.authority.clone())
        .chain(members.iter().map(|(_, cert)| cert.clone()))
        .collect::<Vec<_>>();
    let mut room = room::candidate(
        None,
        &seed_room(ROOM, &original.core.authority, &roster).unwrap(),
    )
    .unwrap();
    let caps = room.get(ROOT, "members").unwrap().unwrap().1;
    put_member(&mut room, &caps, members[0].1.user_public_key, Role::Editor).unwrap();
    put_member(&mut room, &caps, members[1].1.user_public_key, Role::Viewer).unwrap();
    let mut peer = StatePeer::new(
        ROOM,
        original.core.authority.clone(),
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
    (peer, key, members, now)
}
#[test]
fn room_media_only_exact_live_governor_can_change_policy() {
    let (mut peer, authority, members, now) = members();
    let value = policy(&peer);
    for (key, _) in &members {
        let before = peer.durable().unwrap();
        assert!(install_policy(&mut peer, key, now, &value).is_err());
        assert_eq!(before, peer.durable().unwrap());
    }
    install_policy(&mut peer, &authority, now, &value).unwrap();
    for (key, _) in members {
        for delete in [false, true] {
            let before = peer.durable().unwrap();
            let mut doc = peer.admitted_room().unwrap().clone();
            if delete {
                doc.delete(ROOT, media::FIELD).unwrap();
            } else {
                let mut changed = value.clone();
                changed.calls.get_mut(&[7; 32]).unwrap().epoch += 1;
                changed.calls.get_mut(&[7; 32]).unwrap().phase = media::CallPhaseV1::Ended;
                doc.put(
                    ROOT,
                    media::FIELD,
                    automerge::ScalarValue::Bytes(media::encode(&changed).unwrap()),
                )
                .unwrap();
            }
            assert!(
                peer.receive_from(
                    TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                    message(&mut doc),
                    now
                )
                .is_err()
            );
            assert_eq!(before, peer.durable().unwrap());
        }
    }
}
#[test]
fn room_media_signed_editor_edits_preserve_call_binding_and_reader_cannot_write() {
    let (mut peer, authority, members, now) = members();
    let value = policy(&peer);
    install_policy(&mut peer, &authority, now, &value).unwrap();
    for operation in 0..5 {
        let (_, delta) = edited_chat(&peer, operation);
        let (message, _) = peer
            .prepare_signed_mutation(
                APP,
                "",
                DocumentMutation::Incremental(delta),
                &members[0].0,
                now,
            )
            .unwrap();
        let before = peer.durable().unwrap();
        assert!(
            peer.receive_from_report(
                TransportOrigin::RelayDelta(members[0].1.device_public_key),
                message,
                now
            )
            .is_err()
        );
        assert_eq!(before, peer.durable().unwrap());
    }
    let (_, delta) = edited_chat(&peer, 5);
    assert!(
        peer.publish_incremental(APP, "", delta.clone(), &members[1].0, now)
            .is_err()
    );
    peer.publish_incremental(APP, "", delta, &members[0].0, now)
        .unwrap();
    assert_eq!(peer.validated_media_policy().unwrap(), Some(value));
}
fn room_first_receiver(author: &StatePeer, key: &SigningKey, now: SystemTime) -> StatePeer {
    let mut receiver = StatePeer::new(
        ROOM,
        author.core.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver.select_admission_v5().unwrap();
    receiver
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            author.room_message(),
            now,
        )
        .unwrap();
    assert!(receiver.validated_media_policy().is_err());
    receiver
}

fn assert_invalid_then_valid_chat(
    mut receiver: StatePeer,
    author: &StatePeer,
    key: &SigningKey,
    now: SystemTime,
    expected: media::RoomMediaV1,
) {
    assert!(receiver.admission_v5_selected());
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let bad_bytes = edited_chat(author, 3).0;
    let (bad, coverage) = author
        .prepare_signed_mutation(APP, "", DocumentMutation::Snapshot(bad_bytes), key, now)
        .unwrap();
    let mut room = author.admitted_room().unwrap().clone();
    crate::coverage::put(&mut room, coverage.unwrap()).unwrap();
    receiver
        .receive_from_report(origin, message(&mut room), now)
        .unwrap();
    let bytes = receiver.durable().unwrap();
    let floors = receiver.replay_floors();
    let index = receiver.core.exact_admissions.encode().unwrap();
    let error = receiver.receive_from_report(origin, bad, now).unwrap_err();
    assert_eq!(
        error,
        AdmissionError::Rejected(media::MediaPolicyError::Binding.to_string()),
        "the signed snapshot must reach the exact Chat binding guard"
    );
    assert_eq!(bytes, receiver.durable().unwrap());
    assert_eq!(floors, receiver.replay_floors());
    assert_eq!(index, receiver.core.exact_admissions.encode().unwrap());
    assert!(receiver.validated_media_policy().is_err());

    // A different snapshot needs its own authentic coverage and a higher
    // sequence. Continue the same room history so the malicious candidate and
    // recovery checkpoint cannot collide at an Automerge actor sequence.
    let good_bytes = author.document(APP, "").unwrap();
    let (mut valid, coverage) = author
        .prepare_signed_mutation(APP, "", DocumentMutation::Snapshot(good_bytes), key, now)
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut valid else {
        panic!("unexpected fixture shape")
    };
    envelope.seq = envelope.seq.checked_add(1).unwrap();
    collab_types::sign_op(envelope, key).unwrap();
    let mut coverage = coverage.unwrap();
    coverage.sequence = envelope.seq;
    coverage
        .watermarks
        .insert(hex::encode(key.verifying_key().to_bytes()), envelope.seq);
    crate::coverage::put(&mut room, coverage).unwrap();
    receiver
        .receive_from_report(origin, message(&mut room), now)
        .unwrap();
    let admitted = receiver.receive_from_report(origin, valid, now).unwrap();
    assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
    assert_eq!(receiver.validated_media_policy().unwrap(), Some(expected));
}

#[test]
fn room_media_room_first_restore_preserves_pending_exact_chat() {
    let (mut author, key, now) = fixture();
    let value = policy(&author);
    install_policy(&mut author, &key, now, &value).unwrap();
    author.bootstrap_messages(&key, now).unwrap();
    let receiver = room_first_receiver(&author, &key, now);
    let bytes = receiver.durable().unwrap();
    let restored = StatePeer::restore(
        &bytes,
        ROOM,
        author.core.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .expect("accepted room-first state must remain recoverable before Chat arrives");
    assert_eq!(bytes, restored.durable().unwrap());
    assert!(restored.admission_v5_selected());
    assert!(restored.validated_media_policy().is_err());
    assert_invalid_then_valid_chat(restored, &author, &key, now, value);
}

#[test]
fn room_media_strict_missing_chat_snapshot_reaches_binding_guard() {
    let (mut author, key, now) = fixture();
    let value = policy(&author);
    install_policy(&mut author, &key, now, &value).unwrap();
    author.bootstrap_messages(&key, now).unwrap();
    let receiver = room_first_receiver(&author, &key, now);
    assert_invalid_then_valid_chat(receiver, &author, &key, now, value);
}
#[test]
fn room_media_exact_record_foreign_room_and_alias_are_not_substitutes() {
    let (mut peer, key, now) = fixture();
    let other = edited_chat(&peer, 5).0;
    peer.publish_document(APP, "second", other, &key, now)
        .unwrap();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let bad = edited_chat(&peer, 0).0;
    peer.publish_document(APP, "second", bad.clone(), &key, now)
        .unwrap();
    peer.publish_document("dev.tilo.chat", "", bad, &key, now)
        .unwrap();
    assert_eq!(peer.validated_media_policy().unwrap(), Some(value.clone()));
    let mut wrong = value;
    wrong.room = *uuid::Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        .unwrap()
        .as_bytes();
    let before = peer.durable().unwrap();
    assert!(install_policy(&mut peer, &key, now, &wrong).is_err());
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_ended_call_allows_delete_but_cannot_restart_missing_object() {
    let (mut peer, key, now) = fixture();
    let mut value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    value.calls.get_mut(&[7; 32]).unwrap().phase = media::CallPhaseV1::Ended;
    value.calls.get_mut(&[7; 32]).unwrap().epoch = 2;
    install_policy(&mut peer, &key, now, &value).unwrap();
    let deleted = edited_chat(&peer, 0).0;
    peer.publish_document(APP, "", deleted, &key, now).unwrap();
    assert_eq!(peer.validated_media_policy().unwrap(), Some(value.clone()));
    value.calls.get_mut(&[7; 32]).unwrap().phase = media::CallPhaseV1::Active;
    value.calls.get_mut(&[7; 32]).unwrap().epoch = 3;
    assert!(install_policy(&mut peer, &key, now, &value).is_err());
}
#[test]
fn room_media_security_basis_excludes_labels_and_normalizes_identity_json() {
    let (peer, _, _) = fixture();
    let mut room = peer.admitted_room().unwrap().clone();
    let initial = media::security_basis(&room, &[]).unwrap();
    room.put(ROOT, "name", "renamed").unwrap();
    let peers = room.get(ROOT, "peers").unwrap().unwrap().1;
    room.put(
        &peers,
        hex::encode(peer.core.authority.device_public_key),
        "label",
    )
    .unwrap();
    let ids = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(peer.core.authority.device_public_key);
    let row: GovernedIdentity =
        serde_json::from_slice(&room::governance_of(Some(&room)).identities[&id]).unwrap();
    room.put(
        &ids,
        id,
        automerge::ScalarValue::Bytes(serde_json::to_vec_pretty(&row).unwrap()),
    )
    .unwrap();
    assert_eq!(initial, media::security_basis(&room, &[]).unwrap());
    let caps = room.get(ROOT, "members").unwrap().unwrap().1;
    put_member(&mut room, &caps, [44; 32], Role::Viewer).unwrap();
    assert_ne!(initial, media::security_basis(&room, &[]).unwrap());
}
#[test]
fn room_media_governance_role_removal_and_enrollment_refresh_once() {
    let (mut peer, key, members, now) = members();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    // Public removal already authenticates the exact governor. It must update
    // the enabled media policy in the same candidate before native receive.
    assert!(
        peer.remove_member(members[0].1.user_public_key, &key, now)
            .unwrap()
    );
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
    assert!(
        !peer
            .remove_member(members[0].1.user_public_key, &key, now)
            .unwrap()
    );
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
    let (_, cert) = member(101, 104);
    assert!(peer.admit_certificate(cert.clone(), &key, now).unwrap());
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        3
    );
    assert!(!peer.admit_certificate(cert, &key, now).unwrap());
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        3
    );
    peer.bootstrap_messages(&key, now).unwrap();
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        3
    );
}
#[test]
fn room_media_verified_independent_revocation_denies_before_promotion() {
    let (mut peer, key, members, now) = members();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: members[0].1.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &SigningKey::from_bytes(&[110; 32]))
        .unwrap();
    peer.receive_from(
        TransportOrigin::UntrustedPresence(members[0].1.device_public_key),
        NeighbourMsg::IdentityHello {
            cert: Box::new(members[0].1.clone()),
            revocations: vec![revoked.clone()],
        },
        now,
    )
    .unwrap();
    assert!(peer.validated_media_policy().is_err());
    assert!(
        peer.publish_governed_revocations(&[revoked.clone()], &key, now)
            .is_err(),
        "same-root public restriction remains"
    );
    let old = peer.admitted_room().unwrap().clone();
    let mut room = old.clone();
    let ids = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(members[0].1.device_public_key);
    let mut row: GovernedIdentity =
        serde_json::from_slice(&room::governance_of(Some(&room)).identities[&id]).unwrap();
    row.revocations.push(revoked);
    room.put(
        &ids,
        id,
        automerge::ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
    )
    .unwrap();
    media::refresh_authorization(&old, &mut room).unwrap();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        message(&mut room),
        now,
    )
    .unwrap();
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
}
#[test]
fn room_media_current_revoked_or_expired_governor_cannot_publish_policy() {
    for expired in [false, true] {
        let (mut peer, key, now) = fixture();
        let value = policy(&peer);
        let validation_time = if expired {
            // Construct a genuinely root-signed finite certificate and rebuild
            // its governed seed, rather than mutating trusted certificate data.
            let mut cert = peer.core.authority.clone();
            cert.expires_at_ns = Some(now_ns(now) + 1);
            sign_device_cert(&mut cert, &SigningKey::from_bytes(&[101; 32])).unwrap();
            let doc = peer.document(APP, "").unwrap();
            peer = StatePeer::new(ROOM, cert.clone(), vec![], RevocationList::new(), now).unwrap();
            peer.install_seed(
                SeedState {
                    room_changes: seed_room(ROOM, &cert, &[cert.clone()]).unwrap(),
                    documents: vec![SeedDocument {
                        app_id: APP.into(),
                        record_id: String::new(),
                        kind: "chat".into(),
                        bytes: doc,
                    }],
                },
                &key,
                now,
            )
            .unwrap();
            now + Duration::from_nanos(1)
        } else {
            let mut entry = tilo_user_identity::RevocationEntry {
                user_public_key: peer.core.authority.user_public_key,
                revoked_device_public_key: peer.core.authority.device_public_key,
                revoked_at_ns: 2,
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(
                &mut entry,
                &SigningKey::from_bytes(&[101; 32]),
            )
            .unwrap();
            peer.core.revocations.insert(entry).unwrap();
            now
        };
        let before = peer.durable().unwrap();
        assert!(install_policy(&mut peer, &key, validation_time, &value).is_err());
        assert_eq!(before, peer.durable().unwrap());
    }
}

#[test]
fn room_media_restore_rejects_governed_certificate_replacement() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let ids = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(peer.core.authority.device_public_key);
    let mut row: GovernedIdentity =
        serde_json::from_slice(&room::governance_of(Some(&room)).identities[&id]).unwrap();
    row.device_cert.device_label = "replacement certificate".into();
    sign_device_cert(&mut row.device_cert, &SigningKey::from_bytes(&[101; 32])).unwrap();
    room.put(
        &ids,
        id,
        automerge::ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
    )
    .unwrap();
    media::refresh_authorization(peer.admitted_room().unwrap(), &mut room).unwrap();
    peer.core.room_doc = Some(room.into());
    assert!(
        StatePeer::restore(
            &peer.durable().unwrap(),
            ROOM,
            peer.core.authority.clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .is_err()
    );
}
#[test]
fn room_media_retained_snapshot_rejects_mismatched_exact_channel() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let authority =
        crate::retained_proof::RetentionAuthority::new(ROOM, peer.core.authority.clone()).unwrap();
    let verify_room = |message: &NeighbourMsg| {
        crate::retained_proof::verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            *blake3::hash(b"shell-tree").as_bytes(),
            &postcard::to_allocvec(message).unwrap(),
            crate::retained_proof::RoomInput::Checkpoint,
            None,
        )
        .unwrap()
    };
    let room = verify_room(&batch[0]);
    let channel = *blake3::hash(format!("app\0{APP}\0").as_bytes()).as_bytes();
    assert!(
        crate::retained_proof::verify_snapshot(
            &room,
            key.verifying_key().to_bytes(),
            channel,
            &postcard::to_allocvec(&batch[1]).unwrap()
        )
        .is_ok()
    );
    let bad_bytes = edited_chat(&peer, 2).0;
    let (bad, coverage) = peer
        .prepare_signed_mutation(APP, "", DocumentMutation::Snapshot(bad_bytes), &key, now)
        .unwrap();
    let coverage = coverage.unwrap();
    let mut document = peer.admitted_room().unwrap().clone();
    crate::coverage::put(&mut document, coverage.clone()).unwrap();
    crate::durability::put(
        &mut document,
        BTreeMap::from([(
            crate::coverage::route_key(APP, ""),
            crate::durability::DurableReceipt::new(coverage, 2).unwrap(),
        )]),
    )
    .unwrap();
    let room = verify_room(&message(&mut document));
    assert!(
        crate::retained_proof::verify_snapshot(
            &room,
            key.verifying_key().to_bytes(),
            channel,
            &postcard::to_allocvec(&bad).unwrap()
        )
        .is_err()
    );
}
#[test]
fn room_media_sdk_snapshot_uses_same_binding_guard() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let bad = edited_chat(&peer, 4).0;
    let (NeighbourMsg::AppDocument { envelope, .. }, _) = peer
        .prepare_signed_mutation(APP, "", DocumentMutation::Snapshot(bad), &key, now)
        .unwrap()
    else {
        panic!("unexpected fixture shape")
    };
    let before = peer.durable().unwrap();
    assert!(
        peer.admit_local_app_document(APP, "", *envelope, now)
            .is_err()
    );
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_governed_security_change_without_revision_rejected_and_exhaustion_atomic() {
    let (mut peer, key, members, now) = members();
    let mut value = policy(&peer);
    value.authorization_revision = u64::MAX;
    install_policy(&mut peer, &key, now, &value).unwrap();
    let before = peer.durable().unwrap();
    assert!(
        peer.remove_member(members[0].1.user_public_key, &key, now)
            .is_err()
    );
    assert_eq!(before, peer.durable().unwrap());
    let mut document = peer.admitted_room().unwrap().clone();
    let caps = document.get(ROOT, "members").unwrap().unwrap().1;
    put_member(
        &mut document,
        &caps,
        members[0].1.user_public_key,
        Role::Viewer,
    )
    .unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            message(&mut document),
            now
        )
        .is_err()
    );
    assert_eq!(before, peer.durable().unwrap());
}
#[test]
fn room_media_governance_helpers_refresh_policy_and_leave_legacy_disabled() {
    let (peer, key, members, now) = members();
    let mut doc = peer.admitted_room().unwrap().clone();
    crate::governance::remove_member(
        &mut doc,
        &peer.core.authority,
        members[0].1.user_public_key,
        &key,
    )
    .unwrap();
    assert!(media::parse(&doc).unwrap().is_none());
    let mut peer = peer;
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let mut doc = peer.admitted_room().unwrap().clone();
    crate::governance::remove_member(
        &mut doc,
        &peer.core.authority,
        members[0].1.user_public_key,
        &key,
    )
    .unwrap();
    assert_eq!(
        media::validate_transition(
            Some(peer.admitted_room().unwrap()),
            &doc,
            media::TransitionMode::Authoring
        )
        .unwrap()
        .unwrap()
        .authorization_revision,
        2
    );
}

#[test]
fn room_media_reader_uses_current_room_read_without_room_write() {
    let (mut peer, key, members, now) = members();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let member = peer
        .permissions(members[1].1.device_public_key, now)
        .unwrap();
    let room_node = TreePath::room(ROOM);
    assert!(member.allows(&room_node, ActionKind::Read, now));
    assert!(!member.allows(&room_node, ActionKind::Undoable, now));
    peer.remove_member(members[1].1.user_public_key, &key, now)
        .unwrap();
    assert!(
        peer.permissions(members[1].1.device_public_key, now)
            .is_err()
    );
}
#[test]
fn room_media_signed_present_policy_schema_matrix_is_rejected_atomically() {
    let (mut peer, key, now) = fixture();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let valid = media::encode(&value).unwrap();
    let mut unknown = valid.clone();
    unknown[0] = 2;
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut zero = value.clone();
    zero.authorization_revision = 0;
    let mut duplicate = value.clone();
    duplicate
        .calls
        .insert([8; 32], duplicate.calls[&[7; 32]].clone());
    let mut wrong_basis = value.clone();
    wrong_basis.authorization_basis[0] ^= 1;
    wrong_basis.authorization_revision += 1;
    for bytes in [
        unknown,
        trailing,
        postcard::to_allocvec(&zero).unwrap(),
        postcard::to_allocvec(&duplicate).unwrap(),
        media::encode(&wrong_basis).unwrap(),
        vec![0; media::MAX_POLICY_BYTES + 1],
    ] {
        let mut doc = peer.admitted_room().unwrap().clone();
        doc.put(ROOT, media::FIELD, automerge::ScalarValue::Bytes(bytes))
            .unwrap();
        let before = peer.durable().unwrap();
        assert!(
            peer.receive_from(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message(&mut doc),
                now
            )
            .is_err()
        );
        assert_eq!(before, peer.durable().unwrap());
    }
    let mut doc = AutoCommit::load(&peer.core.room_doc.as_ref().unwrap().saved()).unwrap();
    let mut branch = doc.fork();
    doc.set_actor(automerge::ActorId::from(vec![31]));
    branch.set_actor(automerge::ActorId::from(vec![32]));
    doc.put(ROOT, media::FIELD, automerge::ScalarValue::Bytes(valid))
        .unwrap();
    branch
        .put(ROOT, media::FIELD, "hidden malformed conflict")
        .unwrap();
    doc.commit();
    branch.commit();
    doc.reset_diff_cursor();
    branch.reset_diff_cursor();
    doc.merge_incoming(&mut branch).unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            message(&mut doc),
            now
        )
        .is_err()
    );
}
#[test]
fn room_media_governed_revocation_fence_cannot_be_removed_by_higher_revision() {
    let (mut peer, key, _, now) = members();
    let value = policy(&peer);
    install_policy(&mut peer, &key, now, &value).unwrap();
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: peer.core.authority.user_public_key,
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
    let authority =
        crate::retained_proof::RetentionAuthority::new(ROOM, peer.core.authority.clone()).unwrap();
    let mut current = before;
    let proof = crate::retained_proof::verify_room(
        &authority,
        key.verifying_key().to_bytes(),
        *blake3::hash(b"shell-tree").as_bytes(),
        &postcard::to_allocvec(&message(&mut current)).unwrap(),
        crate::retained_proof::RoomInput::Checkpoint,
        None,
    )
    .unwrap();
    let ids = current.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let id = hex::encode(peer.core.authority.device_public_key);
    let mut row: GovernedIdentity =
        serde_json::from_slice(&room::governance_of(Some(&current)).identities[&id]).unwrap();
    row.revocations.clear();
    current
        .put(
            &ids,
            id,
            automerge::ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
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
            automerge::ScalarValue::Bytes(media::encode(&changed).unwrap()),
        )
        .unwrap();
    assert!(
        crate::retained_proof::verify_room(
            &authority,
            key.verifying_key().to_bytes(),
            *blake3::hash(b"shell-tree").as_bytes(),
            &postcard::to_allocvec(&message(&mut current)).unwrap(),
            crate::retained_proof::RoomInput::Checkpoint,
            Some(&proof)
        )
        .is_err()
    );
    let durable = peer.durable().unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            message(&mut current),
            now
        )
        .is_err()
    );
    assert_eq!(durable, peer.durable().unwrap());
}
