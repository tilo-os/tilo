use super::*;
use crate::room::media;
use crate::{NeighbourMsg, SeedDocument, SeedState, TransportOrigin, seed_room};
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use tilo_user_identity::RevocationList;
use web_time::SystemTime;
fn original(peer: &StatePeer, policy: &RoomMediaV1, complete: bool) -> Result<(), String> {
    for call in policy
        .calls
        .values()
        .filter(|call| call.phase == room::media::CallPhaseV1::Active)
    {
        let route = Route {
            app: call.binding.app_id.clone(),
            record: call.binding.record_id.clone(),
        };
        match peer.core.documents.get(&route) {
            Some(document) if !document.encryption_required => {
                room::media::validate_bound_chat_change(
                    policy,
                    &route.app,
                    &route.record,
                    &*document.doc,
                )
                .map_err(err)?;
            }
            _ if complete => {
                return Err("media binding requires exact admitted Chat checkpoint".into());
            }
            _ => {}
        }
    }
    Ok(())
}
fn original_ready(peer: &StatePeer, policy: &RoomMediaV1) -> Result<(), String> {
    original(peer, policy, true)?;
    if room::media::security_basis(
        peer.core.room_doc.as_deref().ok_or("missing room")?,
        &peer.revocations(),
    )
    .map_err(err)?
        != policy.authorization_basis
    {
        return Err("media security basis requires governed revocation reconciliation".into());
    }
    Ok(())
}
#[test]
fn actual_media_binding_loop_matches_literal_legacy_complete_and_partial_errors() {
    let (peer, _, _) = media_fixture();
    let policy = room::media::parse(peer.admitted_room().unwrap())
        .unwrap()
        .unwrap();
    for mode in 0..4 {
        let mut candidate = peer.clone();
        match mode {
            1 => candidate.core.documents.clear(),
            2 => {
                candidate
                    .core
                    .documents
                    .values_mut()
                    .next()
                    .unwrap()
                    .encryption_required = true;
            }
            3 => {
                let doc = &mut candidate.core.documents.values_mut().next().unwrap().doc;
                let channels = doc.get(ROOT, "channels").unwrap().unwrap().1;
                let voice = {
                    let entry = doc.get(&channels, "voice").unwrap().unwrap().1;
                    doc.get(&entry, "v").unwrap().unwrap().1
                };
                doc.put(&voice, "workspace", "wrong").unwrap();
                doc.commit();
            }
            _ => {}
        }
        for complete in [false, true] {
            assert_eq!(
                bindings(&policy, complete, |app, record| legacy_binding(
                    &candidate, &policy, app, record
                ))
                .map_err(ReadinessError::legacy),
                original(&candidate, &policy, complete)
            );
        }
    }
}
#[test]
fn actual_readiness_checks_complete_chat_before_independent_local_revocation_basis() {
    let (mut peer, _, _) = media_fixture();
    let policy = room::media::parse(peer.admitted_room().unwrap())
        .unwrap()
        .unwrap();
    let run = |peer: &StatePeer| {
        ready(
            &policy,
            |app, record| legacy_binding(peer, &policy, app, record),
            || {
                room::media::security_basis(peer.admitted_room().unwrap(), &peer.revocations())
                    .map_err(err)
            },
        )
        .map_err(ReadinessError::legacy)
    };
    assert_eq!(run(&peer), original_ready(&peer, &policy));
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: peer.owner(),
        revoked_device_public_key: [71; 32],
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    peer.core.revocations.insert(entry).unwrap();
    assert_eq!(run(&peer), original_ready(&peer, &policy));
    assert_eq!(
        run(&peer).unwrap_err(),
        "media security basis requires governed revocation reconciliation"
    );
    peer.core.documents.clear();
    assert_eq!(run(&peer), original_ready(&peer, &policy));
    assert_eq!(
        run(&peer).unwrap_err(),
        "media binding requires exact admitted Chat checkpoint"
    );
}

fn media_fixture() -> (StatePeer, SigningKey, SystemTime) {
    let (source, key, _, _, now) = crate::admission_tests::seeded();
    let cert = source.owner_certificates()[0].clone();
    let mut peer = StatePeer::new(
        source.room(),
        cert.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let voice = {
        let entry = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        chat.put_object(&entry, "v", ObjType::Map).unwrap()
    };
    chat.put(&voice, "kind", "voice").unwrap();
    chat.put(&voice, "workspace", "home").unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(source.room(), &cert, std::slice::from_ref(&cert)).unwrap(),
            documents: vec![SeedDocument {
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    let binding = media::resolve_chat_binding("so.tilo.chat", "", "voice", &chat).unwrap();
    let mut room = peer.admitted_room().unwrap().clone();
    let policy = media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(peer.room()).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(&room, &[]).unwrap(),
        calls: BTreeMap::from([(
            [7; 32],
            media::CallV1 {
                binding,
                epoch: 1,
                phase: media::CallPhaseV1::Active,
                allowed: media::DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    };
    room.put(ROOT, media::FIELD, media::encode(&policy).unwrap())
        .unwrap();
    room.commit();
    let changes = room
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now,
    )
    .unwrap();
    (peer, key, now)
}
