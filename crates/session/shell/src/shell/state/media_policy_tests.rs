//! Exercise shell admission/projection itself, including refused-batch side effects.
use super::*;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use std::collections::BTreeMap;
use tilo_peer_state::room::media;

fn fixture() -> (WorkspaceTree, WorkspaceTree, RoomId, PeerOrigin) {
    let mut host = WorkspaceTree::bootstrap_demo();
    let room = host.desktop.workspaces[0].room;
    let governor_root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let device = iroh::SecretKey::from_bytes(&[92; 32]);
    let endpoint = device.public();
    let owner = hex::encode(governor_root.verifying_key().as_bytes());
    host.desktop.workspaces[0].set_owner(owner.clone());
    assert!(host.name_peer(room, &owner, &endpoint.to_string(), "Governor"));
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: governor_root.verifying_key().to_bytes(),
        device_public_key: *endpoint.as_bytes(),
        device_label: "Governor device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &governor_root).unwrap();
    assert!(host.certify_peer_identity(room, &endpoint.to_string(), cert));
    host.sync_room_doc(room);
    let mut guest = WorkspaceTree::bootstrap_demo();
    let workspace = guest.desktop.workspaces[0].id;
    assert!(guest.set_room(workspace, room));
    (host, guest, room, PeerOrigin::peer(&endpoint))
}

fn changes(doc: &mut AutoCommit) -> Vec<Vec<u8>> {
    doc.get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect()
}

fn policy(doc: &AutoCommit, room: RoomId) -> media::RoomMediaV1 {
    media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(&room.to_string()).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(doc, &[]).unwrap(),
        calls: BTreeMap::from([(
            [7; 32],
            media::CallV1 {
                binding: media::ChatCallBindingV1 {
                    app_id: "so.tilo.chat".into(),
                    record_id: String::new(),
                    channel_id: "voice".into(),
                    channel_object: "1@01".into(),
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

fn put_policy(doc: &mut AutoCommit, policy: &media::RoomMediaV1) {
    doc.put(
        ROOT,
        media::FIELD,
        ScalarValue::Bytes(media::encode(policy).unwrap()),
    )
    .unwrap();
    doc.commit();
}

fn admit_legacy(
    host: &mut WorkspaceTree,
    guest: &mut WorkspaceTree,
    room: RoomId,
    origin: &PeerOrigin,
) {
    assert!(guest.pin_room_authority(room, origin.clone()));
    let bytes = host.export_room_changes_since(room, &[]).unwrap();
    guest.apply_room_changes(room, &bytes, origin).unwrap();
    assert!(!guest.room_heads(room).is_empty());
}

#[test]
fn malformed_first_snapshot_cannot_pin_or_install() {
    let (mut host, mut guest, room, origin) = fixture();
    let doc = host.room_docs.get_mut(&room).unwrap();
    doc.put(ROOT, media::FIELD, "not bytes").unwrap();
    let bytes = changes(doc);
    let identities = guest.room_peer_identities.clone();
    assert!(!guest.apply_room_changes(room, &bytes, &origin).unwrap());
    assert_eq!(guest.room_authority(room), RoomAuthority::Unpinned);
    assert!(!guest.room_docs.contains_key(&room));
    assert_eq!(guest.room_peer_identities, identities);
    assert!(guest.take_inbound_room_records().is_empty());
}

#[test]
fn media_bootstrap_refuses_unpinned_and_non_authority_origins() {
    for pinned in [
        None,
        Some(PeerOrigin::hub(1)),
        Some(PeerOrigin::local()),
        Some(PeerOrigin::peer(
            &iroh::SecretKey::from_bytes(&[93; 32]).public(),
        )),
    ] {
        let (mut host, mut guest, room, origin) = fixture();
        if let Some(pinned) = &pinned {
            assert!(guest.pin_room_authority(room, pinned.clone()));
        }
        let doc = host.room_docs.get_mut(&room).unwrap();
        let value = policy(doc, room);
        put_policy(doc, &value);
        assert!(
            !guest
                .apply_room_changes(room, &changes(doc), &origin)
                .unwrap()
        );
        assert!(!guest.room_docs.contains_key(&room));
        assert_eq!(
            guest.room_authority(room),
            pinned.map_or(RoomAuthority::Unpinned, RoomAuthority::Peer)
        );
    }
}

fn accept_media_invite(host: &WorkspaceTree, guest: &mut WorkspaceTree, room: RoomId) {
    let device = iroh::SecretKey::from_bytes(&[92; 32]);
    let endpoint = device.public();
    let certificate = host.room_peer_identities[&room][&endpoint.to_string()]
        .device_cert
        .clone();
    let token = crate::network::invite::make_invite_token_with_identity(
        &device,
        host.workspace_in_room(room).unwrap(),
        &iroh::EndpointAddr::new(endpoint),
        &[],
        Some(certificate),
        3600,
    )
    .unwrap();
    let local = guest.desktop.workspaces[0].id;
    assert_eq!(
        crate::network::invite::accept_invite_into(guest, local, &token).unwrap(),
        room
    );
}

#[test]
fn invite_pinned_guest_accepts_first_media_snapshot_after_empty_projection() {
    let (mut host, _unused_guest, room, origin) = fixture();
    let mut guest = WorkspaceTree::bootstrap_demo();
    accept_media_invite(&host, &mut guest, room);
    assert_eq!(
        guest.room_authority(room),
        RoomAuthority::Peer(origin.clone())
    );
    assert!(!guest.room_docs.contains_key(&room));
    // The replica may project an empty joined-room cache before its first SyncResp.
    guest.sync_room_doc(room);
    assert!(guest.room_peer_identities_in_doc(room).is_empty());
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    let bytes = changes(doc);
    let heads = guest.room_heads(room);
    let sibling = PeerOrigin::peer(&iroh::SecretKey::from_bytes(&[93; 32]).public());
    assert!(!guest.apply_room_changes(room, &bytes, &sibling).unwrap());
    assert_eq!(guest.room_heads(room), heads);
    assert!(guest.apply_room_changes(room, &bytes, &origin).unwrap());
    assert_eq!(
        media::parse(guest.room_docs.get(&room).unwrap()).unwrap(),
        Some(value)
    );
}

#[test]
fn persisted_invite_pin_restores_media_room_without_prior_room_document() {
    let (mut host, _unused_guest, room, origin) = fixture();
    let mut guest = WorkspaceTree::bootstrap_demo();
    accept_media_invite(&host, &mut guest, room);
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    let bytes = changes(doc);
    assert!(guest.apply_room_changes(room, &bytes, &origin).unwrap());
    let persisted = serde_json::to_vec(&guest).unwrap();
    let mut restored: WorkspaceTree = serde_json::from_slice(&persisted).unwrap();
    assert!(restored.room_docs.is_empty());
    assert_eq!(
        restored.room_authority(room),
        RoomAuthority::Peer(origin.clone())
    );
    // The persisted workspace owner still fences a re-rooting attempt even
    // though the admitted room document and its certificate are not durable.
    let endpoint = iroh::SecretKey::from_bytes(&[92; 32]).public().to_string();
    let other_root = ed25519_dalek::SigningKey::from_bytes(&[94; 32]);
    let mut rerooted = host.room_docs.get(&room).unwrap().clone();
    let identities = rerooted
        .get(ROOT, WorkspaceTree::ROOM_PEER_IDENTITIES_KEY)
        .unwrap()
        .unwrap()
        .1;
    let mut row = host.room_peer_identities[&room][&endpoint].clone();
    row.device_cert.user_public_key = other_root.verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut row.device_cert, &other_root).unwrap();
    rerooted
        .put(
            &identities,
            &endpoint,
            ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
        )
        .unwrap();
    rerooted
        .put(
            ROOT,
            "owner",
            hex::encode(other_root.verifying_key().as_bytes()),
        )
        .unwrap();
    let replaced = policy(&rerooted, room);
    put_policy(&mut rerooted, &replaced);
    assert!(
        !restored
            .apply_room_changes(room, &changes(&mut rerooted), &origin)
            .unwrap()
    );
    assert!(restored.room_docs.is_empty());
    assert!(restored.apply_room_changes(room, &bytes, &origin).unwrap());
    assert_eq!(
        media::parse(restored.room_docs.get(&room).unwrap()).unwrap(),
        Some(value)
    );
}

#[test]
fn admitted_policy_survives_projection_without_chat_cache() {
    let (mut host, mut guest, room, origin) = fixture();
    admit_legacy(&mut host, &mut guest, room, &origin);
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    assert!(
        guest
            .apply_room_changes(room, &changes(doc), &origin)
            .unwrap()
    );
    assert_eq!(
        media::parse(guest.room_docs.get(&room).unwrap()).unwrap(),
        Some(value.clone())
    );
    guest.sync_room_doc(room);
    let heads = guest.room_heads(room);
    assert!(
        guest
            .export_room_changes_since(room, &heads)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        media::parse(guest.room_docs.get(&room).unwrap()).unwrap(),
        Some(value)
    );
}

#[test]
fn malformed_pinned_candidate_and_conflict_leave_heads_and_certificates_unchanged() {
    let (mut host, mut guest, room, origin) = fixture();
    admit_legacy(&mut host, &mut guest, room, &origin);
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    guest
        .apply_room_changes(room, &changes(doc), &origin)
        .unwrap();
    let mut original = guest.room_docs.get(&room).unwrap().clone();
    let heads = guest.room_heads(room);
    let authority = guest.room_authority(room);
    let identities = guest.trusted_identity_projection(room);
    let mut malformed = original.clone();
    malformed.put(ROOT, media::FIELD, "not bytes").unwrap();
    let mut left = original.clone().fork();
    let mut right = original.fork();
    let mut restricted = value.clone();
    restricted.calls.get_mut(&[7; 32]).unwrap().allowed.publish = false;
    restricted.calls.get_mut(&[7; 32]).unwrap().epoch = 2;
    put_policy(&mut left, &restricted);
    restricted
        .calls
        .get_mut(&[7; 32])
        .unwrap()
        .allowed
        .subscribe = false;
    put_policy(&mut right, &restricted);
    left.merge_incoming(&mut right).unwrap();
    for mut rejected in [malformed, left] {
        assert!(
            !guest
                .apply_room_changes(room, &changes(&mut rejected), &origin)
                .unwrap()
        );
        assert_eq!(guest.room_heads(room), heads);
        assert_eq!(guest.room_authority(room), authority);
        assert_eq!(guest.trusted_identity_projection(room), identities);
    }
}

#[test]
fn non_governor_policy_rewrite_is_refused_but_unchanged_echo_is_retained() {
    let (mut host, mut guest, room, origin) = fixture();
    admit_legacy(&mut host, &mut guest, room, &origin);
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    guest
        .apply_room_changes(room, &changes(doc), &origin)
        .unwrap();
    let mut forged = guest.room_docs.get(&room).unwrap().clone();
    let mut changed = value.clone();
    changed.calls.get_mut(&[7; 32]).unwrap().allowed.publish = false;
    changed.calls.get_mut(&[7; 32]).unwrap().epoch = 2;
    put_policy(&mut forged, &changed);
    let heads = guest.room_heads(room);
    let other = PeerOrigin::peer(&iroh::SecretKey::from_bytes(&[93; 32]).public());
    assert!(
        !guest
            .apply_room_changes(room, &changes(&mut forged), &other)
            .unwrap()
    );
    assert_eq!(guest.room_heads(room), heads);
    let mut echo = guest.room_docs.get(&room).unwrap().clone();
    // An unrelated room label update makes the echoed batch observable even
    // if Automerge elides an equal-value policy put.
    echo.put(ROOT, "name", "Guest room label").unwrap();
    put_policy(&mut echo, &value);
    let echo_heads = echo.get_heads();
    assert_ne!(echo_heads, heads);
    guest
        .apply_room_changes(room, &changes(&mut echo), &other)
        .unwrap();
    let admitted = guest.room_docs.get_mut(&room).unwrap();
    assert!(
        echo_heads
            .iter()
            .all(|head| admitted.get_change_by_hash(head).is_some())
    );
    assert_eq!(
        media::parse(guest.room_docs.get(&room).unwrap()).unwrap(),
        Some(value)
    );
}

#[test]
fn local_security_projection_refreshes_revision_once_and_preserves_calls() {
    let (mut host, _guest, room, _origin) = fixture();
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    let authority = host.desktop.workspaces[0].owner().cloned().unwrap();
    let endpoint = iroh::SecretKey::from_bytes(&[92; 32]).public().to_string();
    assert!(host.name_peer(room, &authority, &endpoint, "Renamed governor"));
    assert_eq!(
        media::parse(host.room_docs.get(&room).unwrap()).unwrap(),
        Some(value.clone())
    );
    assert!(host.name_peer(room, &authority, &endpoint, ""));
    let updated = media::parse(host.room_docs.get(&room).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(updated.authorization_revision, 2);
    assert_eq!(updated.calls, value.calls);
    assert_ne!(updated.authorization_basis, value.authorization_basis);
    let heads = host.room_heads(room);
    assert!(
        host.export_room_changes_since(room, &heads)
            .unwrap()
            .is_empty()
    );
    assert_eq!(host.room_heads(room), heads);
}

#[test]
fn exhausted_revision_does_not_install_partial_projection() {
    let (mut host, _guest, room, _origin) = fixture();
    let doc = host.room_docs.get_mut(&room).unwrap();
    let mut value = policy(doc, room);
    value.authorization_revision = u64::MAX;
    put_policy(doc, &value);
    let heads = doc.get_heads();
    host.room_peer_names.get_mut(&room).unwrap().clear();
    host.sync_room_doc(room);
    assert_eq!(host.room_docs.get_mut(&room).unwrap().get_heads(), heads);
    assert_eq!(
        media::parse(host.room_docs.get(&room).unwrap()).unwrap(),
        Some(value)
    );
    assert!(host.export_room_changes_since(room, &[]).is_none());
}

#[test]
fn malformed_local_policy_is_not_repaired_or_exported() {
    let (mut host, _guest, room, _origin) = fixture();
    let doc = host.room_docs.get_mut(&room).unwrap();
    doc.put(ROOT, media::FIELD, "not bytes").unwrap();
    doc.commit();
    let heads = doc.get_heads();
    host.desktop.workspaces[0].name = "Changed locally".into();
    host.sync_room_doc(room);
    assert_eq!(host.room_docs.get_mut(&room).unwrap().get_heads(), heads);
    assert!(host.room_heads(room).is_empty());
    assert!(host.export_room_changes_since(room, &[]).is_none());
}

#[test]
fn restart_projection_does_not_recreate_policy_from_workspace_cache() {
    let (mut host, _guest, room, _origin) = fixture();
    let doc = host.room_docs.get_mut(&room).unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    host.room_docs.clear();
    host.sync_room_doc(room);
    assert_eq!(
        media::parse(host.room_docs.get(&room).unwrap()).unwrap(),
        None
    );
    assert!(host.export_room_changes_since(room, &[]).is_some());
}

#[test]
fn media_requires_exact_admitted_device_certificate_and_current_validity() {
    for case in ["hub", "local", "sibling", "renewed", "expired", "revoked"] {
        let (mut host, mut guest, room, origin) = fixture();
        admit_legacy(&mut host, &mut guest, room, &origin);
        let endpoint = iroh::SecretKey::from_bytes(&[92; 32]).public().to_string();
        let governor_root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let mut candidate = host.room_docs.get(&room).unwrap().clone();
        let identities = candidate
            .get(ROOT, WorkspaceTree::ROOM_PEER_IDENTITIES_KEY)
            .unwrap()
            .unwrap()
            .1;
        let mut row = host.room_peer_identities[&room][&endpoint].clone();
        match case {
            "hub" => {
                guest
                    .room_authority
                    .insert(room, RoomAuthority::Peer(PeerOrigin::hub(1)));
            }
            "local" => {
                guest
                    .room_authority
                    .insert(room, RoomAuthority::Peer(PeerOrigin::local()));
            }
            "sibling" => {
                let sibling = iroh::SecretKey::from_bytes(&[93; 32]).public();
                guest
                    .room_authority
                    .insert(room, RoomAuthority::Peer(PeerOrigin::peer(&sibling)));
            }
            "renewed" => {
                row.device_cert.issued_at_ns = 2;
                tilo_user_identity::sign_device_cert(&mut row.device_cert, &governor_root).unwrap();
                candidate
                    .put(
                        &identities,
                        &endpoint,
                        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
                    )
                    .unwrap();
            }
            "expired" | "revoked" => {
                if case == "expired" {
                    row.device_cert.expires_at_ns = Some(2);
                    tilo_user_identity::sign_device_cert(&mut row.device_cert, &governor_root)
                        .unwrap();
                } else {
                    let mut revocation = tilo_user_identity::RevocationEntry {
                        user_public_key: row.device_cert.user_public_key,
                        revoked_device_public_key: row.device_cert.device_public_key,
                        revoked_at_ns: 2,
                        reason: tilo_user_identity::RevocationReason::Retired,
                        signature: [0; 64],
                    };
                    tilo_user_identity::sign_revocation_entry(&mut revocation, &governor_root)
                        .unwrap();
                    row.revocations.push(revocation);
                }
                candidate
                    .put(
                        &identities,
                        &endpoint,
                        ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
                    )
                    .unwrap();
                // Existing normal certificate admission can retain historical evidence;
                // media must separately check that authority is still live now.
                // Close this transaction before cloning so the later policy write
                // extends the same committed identity history on both replicas.
                candidate.commit();
                guest.room_docs.insert(room, candidate.clone());
            }
            _ => unreachable!(),
        }
        let value = policy(&candidate, room);
        put_policy(&mut candidate, &value);
        let heads = guest.room_heads(room);
        assert!(
            heads
                .iter()
                .all(|head| candidate.get_change_by_hash(head).is_some()),
            "{case}: the policy candidate must extend the admitted identity history"
        );
        let pinned = guest.room_authority(room);
        assert!(
            !guest
                .apply_room_changes(room, &changes(&mut candidate), &origin)
                .unwrap(),
            "{case}"
        );
        assert_eq!(guest.room_heads(room), heads, "{case}");
        assert_eq!(guest.room_authority(room), pinned, "{case}");
        assert_eq!(
            media::parse(guest.room_docs.get(&room).unwrap()).unwrap(),
            None,
            "{case}"
        );
    }
}

#[test]
fn projection_preserves_native_admission_fields_outside_shell_cache() {
    let (mut host, _guest, room, _origin) = fixture();
    let endpoint = iroh::SecretKey::from_bytes(&[92; 32]).public().to_string();
    let governor_root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let doc = host.room_docs.get_mut(&room).unwrap();
    let identities = doc
        .get(ROOT, WorkspaceTree::ROOM_PEER_IDENTITIES_KEY)
        .unwrap()
        .unwrap()
        .1;
    let row = host.room_peer_identities[&room][&endpoint].clone();
    let mut native = serde_json::to_value(&row).unwrap();
    native["admission"] = serde_json::json!({
        "invite_id": vec![4u8; 32],
        "user_public_key": governor_root.verifying_key().to_bytes(),
        "device_public_key": row.device_cert.device_public_key,
        "request_id": vec![5u8; 32],
        "role": "Editor",
    });
    let native_bytes = serde_json::to_vec(&native).unwrap();
    doc.put(
        &identities,
        &endpoint,
        ScalarValue::Bytes(native_bytes.clone()),
    )
    .unwrap();
    let value = policy(doc, room);
    put_policy(doc, &value);
    host.sync_room_doc(room);
    let projected = host.room_docs.get(&room).unwrap();
    assert_eq!(media::parse(projected).unwrap(), Some(value));
    assert!(matches!(projected.get(&identities, &endpoint).unwrap(),
        Some((automerge::Value::Scalar(bytes), _)) if matches!(bytes.as_ref(), ScalarValue::Bytes(bytes) if bytes == &native_bytes)));
}
