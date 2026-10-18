//! Private extraction must not masquerade as ordinary room compaction.
use super::*;
use automerge::{ObjType, ROOT, ScalarValue, transaction::Transactable};

const SECRET: &str = "legacy native instruction retained only in private custody";

fn legacy_room(dir: &Path) -> (GovernedRoom, SigningKey, DeviceCert) {
    legacy_room_with(dir, false, false)
}

fn legacy_room_with(
    dir: &Path,
    title_only: bool,
    deleted: bool,
) -> (GovernedRoom, SigningKey, DeviceCert) {
    let (key, cert) = tests::governor();
    let now = SystemTime::now();
    let mut room = GovernedRoom::create(dir, "Legacy instructions", &cert, &key, now).unwrap();
    // Deliberately reconstruct a pre-baseline room. Production creation now
    // establishes title provenance before admitting the first task.
    let mut legacy = StatePeer::new(
        room.room(),
        cert.clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    legacy
        .install_seed(
            empty_workspace(
                room.room(),
                &cert,
                std::slice::from_ref(&cert),
                "Legacy instructions",
            )
            .unwrap(),
            &key,
            now,
        )
        .unwrap();
    prepare_governance(&mut legacy, &key, now).unwrap();
    room.record.peer = room.parts.stage(&legacy).unwrap();
    room.record.outgoing = legacy
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap()
        .iter()
        .map(|message| publication(message, true).unwrap())
        .collect();
    room.peer = legacy;
    let mut doc = room.peer().admitted_room().unwrap().clone();
    let tasks = doc.put_object(ROOT, "tasks_v1", ObjType::Map).unwrap();
    doc.put(
        &tasks,
        "legacy-model",
        ScalarValue::Bytes(
            serde_json::to_vec(&serde_json::json!({
                "route": "model", "title": SECRET,
                "audit": [tilo_desktop_model::WorkspaceTaskAuditEvent::new(
                    7,
                    "agent:scribe",
                    "finish",
                    [("outcome", tilo_actions::Scalar::text("done"))],
                ).noting(SECRET)]
            }))
            .unwrap(),
        ),
    )
    .unwrap();
    if !title_only {
        let controls = doc
            .put_object(ROOT, "task_control_v1", ObjType::Map)
            .unwrap();
        doc.put(
            &controls,
            "legacy-model",
            ScalarValue::Bytes(
                serde_json::to_vec(&serde_json::json!({
                    "instruction": SECRET, "directions": []
                }))
                .unwrap(),
            ),
        )
        .unwrap();
        if deleted {
            doc.commit();
            doc.delete(controls, "legacy-model").unwrap();
        }
    }
    if deleted {
        doc.commit();
        doc.delete(tasks, "legacy-model").unwrap();
    }
    let seed = tilo_peer_state::SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        documents: vec![],
    };
    let mut candidate = room.peer().clone();
    candidate.install_seed(seed, &key, now).unwrap();
    let collaboration = room.collaboration().clone();
    room.commit(candidate, &key, now, |_, _| Ok(collaboration))
        .unwrap();
    (room, key, cert)
}

#[test]
fn private_staging_preserves_predecessor_and_outgoing_until_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let (room, key, cert) = legacy_room(dir.path());
    let peer = room.record.peer.clone();
    let outgoing = postcard::to_stdvec(&room.record.outgoing).unwrap();
    let generation = room.record.generation;
    let baseline = room
        .stage_private_task_baseline(&key, SystemTime::now())
        .unwrap()
        .unwrap();
    room.peer()
        .clone()
        .admit_sanitized_room_baseline(&baseline, SystemTime::now())
        .unwrap();
    assert_eq!(room.record.peer, peer);
    assert_eq!(
        postcard::to_stdvec(&room.record.outgoing).unwrap(),
        outgoing
    );
    assert_eq!(room.record.generation, generation);
    let restored = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    let ledger =
        std::fs::read_to_string(restored.path.with_extension("private-task-text-v1.json")).unwrap();
    assert!(
        ledger.contains(SECRET),
        "private custody must survive restart before retirement"
    );
    let historical: BTreeMap<String, Vec<serde_json::Value>> = super::private_tasks::read_private(
        &restored.path.with_extension("private-task-history-v1.json"),
    )
    .unwrap()
    .unwrap();
    assert!(
        historical["legacy-model"]
            .iter()
            .any(|event| event["note"] == SECRET)
    );
    assert!(restored.load_task_reports().unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&room.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

fn retained_frame(workspace: u64, author: [u8; 32], channel: [u8; 32], body: Vec<u8>) -> Vec<u8> {
    let delta = crate::coedit::CoeditPayload::Checkpoint { channel, body }
        .encode()
        .unwrap();
    crate::frame::encode(&crate::transport::Envelope::Delta {
        workspace_id: workspace,
        author_device_public_key: author,
        authored_at_ns: 1,
        delta,
        signature: [0; 64],
    })
    .unwrap()
}

#[test]
fn retirement_scrubs_every_room_author_and_pin_but_keeps_resource_channels() {
    let dir = tempfile::tempdir().unwrap();
    let (mut room, key, cert) = legacy_room(&dir.path().join("rooms"));
    let workspace = room.workspace_id();
    let store = crate::store::WorkspaceStore::open(dir.path(), workspace, true).unwrap();
    let room_channel = *blake3::hash(b"shell-tree").as_bytes();
    let resource_channel = neighbour_wire::app_document_channel("chat", "resource");
    let resource = retained_frame(
        workspace,
        [7; 32],
        resource_channel,
        b"independent resource ciphertext".to_vec(),
    );
    store
        .retain_checkpoint(&resource_channel, &resource)
        .unwrap();
    let old = retained_frame(
        workspace,
        cert.device_public_key,
        room_channel,
        room.record.outgoing.last().unwrap().bytes.clone(),
    );
    store.retain_checkpoint(&room_channel, &old).unwrap();
    let foreign = retained_frame(workspace, [8; 32], room_channel, SECRET.as_bytes().to_vec());
    store.retain_checkpoint(&room_channel, &foreign).unwrap();
    store.enqueue_pending(&[9; 32], 1, &foreign).unwrap();
    store.enqueue_pending(&[9; 32], 2, &resource).unwrap();
    store.save_snapshot(SECRET.as_bytes()).unwrap();
    let pins = store.root().join("store-work-v1/replay-retirement/pins");
    std::fs::create_dir_all(&pins).unwrap();
    std::fs::write(pins.join("0"), &old).unwrap();
    std::fs::write(pins.join("1"), &resource).unwrap();
    let selected = store.root().join("shell-selected-v1/unselected");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::write(selected.join("old.delta"), &old).unwrap();
    std::fs::write(selected.join("resource.delta"), &resource).unwrap();
    let baseline = room
        .stage_private_task_baseline(&key, SystemTime::now())
        .unwrap()
        .unwrap();
    // The production boundary authenticates the admitted predecessor first.
    room.peer()
        .clone()
        .admit_sanitized_room_baseline(&baseline, SystemTime::now())
        .unwrap();
    store.retire_room_history(&baseline).unwrap();
    room.install_private_task_baseline(&baseline).unwrap();
    let retained = store.load_retained().unwrap();
    assert!(retained.contains(&resource));
    assert!(!retained.contains(&old));
    assert!(!retained.contains(&foreign));
    assert!(store.retain_checkpoint(&room_channel, &old).is_err());
    assert!(store.retain_checkpoint(&room_channel, &foreign).is_err());
    assert_eq!(store.load_retained().unwrap(), retained);
    assert!(store.enqueue_pending(&[9; 32], 3, &old).is_err());
    let update = future_room_frame(
        &baseline,
        room.room(),
        workspace,
        cert.device_public_key,
        room_channel,
    );
    store.retain_checkpoint(&room_channel, &update).unwrap();
    let retained = store.load_retained().unwrap();
    assert!(retained.contains(&resource));
    assert!(retained.contains(&update));
    assert!(!retained.contains(&old));
    let pending = store.load_pending(&[9; 32]).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].bytes, resource);
    assert!(!pins.exists());
    assert!(!selected.exists());
    assert_eq!(store.load_snapshot().unwrap().unwrap(), baseline.document);
    let reopened = crate::store::WorkspaceStore::open(dir.path(), workspace, true).unwrap();
    assert_eq!(
        reopened.sanitized_room_anchor().unwrap(),
        Some(baseline.anchor())
    );
    assert_eq!(reopened.load_retained().unwrap(), retained);
    let restored = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    assert_eq!(
        restored.peer().sanitized_room_generation().unwrap(),
        baseline.generation
    );
    assert!(
        restored
            .stage_private_task_baseline(&key, SystemTime::now())
            .unwrap()
            .is_none(),
        "restarting a sanitized chain must not manufacture another replacement"
    );
}

fn future_room_frame(
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
    room: &str,
    workspace: u64,
    author: [u8; 32],
    room_channel: [u8; 32],
) -> Vec<u8> {
    let mut current = tilo_automerge_isolate::load(&baseline.document).unwrap();
    let heads = current.get_heads();
    current.put(ROOT, "future_epoch_value", true).unwrap();
    current.commit();
    let updates = current
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let message = neighbour_wire::NeighbourMsg::SyncResp {
        room: room.into(),
        changes: updates,
    };
    retained_frame(
        workspace,
        author,
        room_channel,
        postcard::to_stdvec(&message).unwrap(),
    )
}

fn has_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(SECRET.len())
        .any(|part| part == SECRET.as_bytes())
}

fn assert_no_public_marker(path: &Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            assert_no_public_marker(&entry.path());
        } else {
            assert!(
                !has_marker(&std::fs::read(entry.path()).unwrap()),
                "{:?}",
                entry.path()
            );
        }
    }
}

fn resource_tail(workspace: u64, route: [u8; 32]) -> Vec<u8> {
    let delta = crate::coedit::CoeditPayload::RetainedDelta {
        channel: route,
        body: b"resource tail".to_vec(),
    }
    .encode()
    .unwrap();
    crate::frame::encode(&crate::transport::Envelope::Delta {
        workspace_id: workspace,
        author_device_public_key: [7; 32],
        authored_at_ns: 2,
        delta,
        signature: [0; 64],
    })
    .unwrap()
}

#[test]
fn retirement_recovers_after_every_destructive_operation_without_old_manifests() {
    let mut saw_dangling_manifest = false;
    // Each fixture is fresh: inject after the Nth deletion, then let the real
    // startup path finish it. Stop only once every deletion boundary was tried.
    for crash_at in 1.. {
        let dir = tempfile::tempdir().unwrap();
        let (room, key, cert) = legacy_room(&dir.path().join("rooms"));
        let workspace = room.workspace_id();
        let store = crate::store::WorkspaceStore::open(dir.path(), workspace, true).unwrap();
        let channel = *blake3::hash(b"shell-tree").as_bytes();
        let route = neighbour_wire::app_document_channel("chat", "resource");
        let resource = retained_frame(workspace, [7; 32], route, b"resource checkpoint".to_vec());
        let tail = resource_tail(workspace, route);
        let old = retained_frame(
            workspace,
            cert.device_public_key,
            channel,
            SECRET.as_bytes().to_vec(),
        );
        store.retain_checkpoint(&channel, &old).unwrap();
        store.enqueue_pending(&[9; 32], 1, &old).unwrap();
        // These resource frames exist only in the selected cut/capture, not in
        // retained storage. Its manifest deliberately refers to the room frame
        // that the scrub deletes before removing the manifest itself.
        store.retirement_selected_fixture(&[old.clone(), resource.clone(), tail.clone()]);
        let pins = store.root().join("store-work-v1/replay-retirement/pins");
        std::fs::create_dir_all(&pins).unwrap();
        std::fs::write(pins.join("room"), &old).unwrap();
        std::fs::write(pins.join("resource"), &resource).unwrap();
        std::fs::create_dir_all(store.root().join("resume")).unwrap();
        std::fs::write(store.root().join("generation"), b"old").unwrap();
        let baseline = room
            .stage_private_task_baseline(&key, SystemTime::now())
            .unwrap()
            .unwrap();
        std::fs::write(
            store.root().join("room-retirement-pending-v1.json"),
            serde_json::to_vec(&baseline).unwrap(),
        )
        .unwrap();
        let mut deletions = 0;
        let result = store.finish_room_retirement_with(|| {
            deletions += 1;
            if deletions == crash_at {
                Err(std::io::Error::other("injected crash after deletion").into())
            } else {
                Ok(())
            }
        });
        let selected = store
            .root()
            .join("shell-selected-v1/00000000000000000000000000000001");
        saw_dangling_manifest |=
            selected.join("basis.manifest").exists() && !selected.join("0.delta").exists();
        if result.is_err() {
            assert_eq!(
                deletions, crash_at,
                "retirement failed before injected crash: {result:?}"
            );
            if store
                .root()
                .join("room-retirement-pending-v1.json")
                .exists()
            {
                assert!(store.load_retained().is_err());
                assert!(store.load_snapshot().is_err());
            }
        }
        drop(store);
        let recovered = crate::store::WorkspaceStore::open(dir.path(), workspace, true).unwrap();
        let retained = recovered.load_retained().unwrap();
        assert!(retained.contains(&resource));
        assert!(retained.contains(&tail));
        assert_eq!(retained.iter().filter(|raw| **raw == resource).count(), 1);
        assert_eq!(retained.iter().filter(|raw| **raw == tail).count(), 1);
        assert!(
            retained.iter().position(|raw| *raw == resource).unwrap()
                < retained.iter().position(|raw| *raw == tail).unwrap()
        );
        assert!(!retained.contains(&old));
        assert!(recovered.load_pending(&[9; 32]).unwrap().is_empty());
        assert_eq!(
            recovered.sanitized_room_anchor().unwrap(),
            Some(baseline.anchor())
        );
        assert_eq!(
            recovered.load_snapshot().unwrap().unwrap(),
            baseline.document
        );
        assert_no_public_marker(recovered.root());
        drop(recovered);
        let retried = crate::store::WorkspaceStore::open(dir.path(), workspace, true).unwrap();
        assert_eq!(retried.load_retained().unwrap(), retained);
        if result.is_ok() {
            assert!(saw_dangling_manifest);
            break;
        }
    }
}
