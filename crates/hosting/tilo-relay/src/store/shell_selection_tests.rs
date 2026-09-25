//! Store/replay regressions over public real-owner and signed synthetic inputs.
use super::shell_proof_tests::{binding, fixture, metadata, parts};
use super::*;
use crate::{coedit::CoeditPayload, transport::Envelope};
use qos_peer_state::{NeighbourMsg, StatePeer, retained_proof::*};

pub(crate) fn replay_frames(store: &WorkspaceStore) -> Vec<Envelope> {
    use super::replay::*;
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    let mut snapshot = loop {
        if let Step::Done(snapshot) = store
            .capture_replay_step(
                &mut build,
                WorkLimit {
                    entries: 1,
                    ..Default::default()
                },
            )
            .unwrap()
        {
            break snapshot;
        }
    };
    while matches!(
        store
            .prepare_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let mut result = Vec::new();
    loop {
        let mut batch = store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        while let Some(frame) = batch.front() {
            result.push(frame.clone());
            batch.pop_written();
        }
        if batch.eof {
            break;
        }
    }
    let mut discard = snapshot.into_discard().unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    result
}
pub(crate) fn admit_frames(
    meta: &serde_json::Value,
    initial: &Path,
    frames: Vec<Envelope>,
) -> StatePeer {
    admit_frames_with_origin(meta, initial, frames, false)
}
fn admit_frames_with_origin(
    meta: &serde_json::Value,
    initial: &Path,
    frames: Vec<Envelope>,
    retained: bool,
) -> StatePeer {
    let now = web_time::SystemTime::UNIX_EPOCH
        + web_time::Duration::from_nanos(meta["now_ns"].as_u64().unwrap());
    let mut peer = StatePeer::restore(
        &fs::read(initial).unwrap(),
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
        serde_json::from_value(meta["certificates"].clone()).unwrap(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    for (index, frame) in frames.into_iter().enumerate() {
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = frame
        else {
            panic!()
        };
        let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        let msg = postcard::from_bytes(&body).unwrap();
        let outcome = peer
            .receive_from(
                if retained {
                    qos_peer_state::TransportOrigin::RetainedRelayDelta(author_device_public_key)
                } else {
                    qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key)
                },
                msg,
                now,
            )
            .unwrap_or_else(|e| panic!("replay frame {index}: {e}"));
        assert_eq!(outcome.rejected, 0, "deferred rejection at {index}");
    }
    peer
}
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
#[test]
fn captured_shell_store_selection_replays_exact_current_pairs() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = binding();
    let [current_workspace, legacy_workspace] =
        crate::room_workspace_ids(authority.room()).unwrap();
    assert_ne!(current_workspace, legacy_workspace);
    let mut store = WorkspaceStore::open(tmp.path(), legacy_workspace, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    // Trusted test-store import of the public authenticated-ingress capture.
    copy_tree(&fixture().join("retained"), &store.root.join("retained"));
    let frames = replay_frames(&store);
    assert!(frames.iter().all(|frame| matches!(
        frame,
        Envelope::Delta { workspace_id, .. } if *workspace_id == legacy_workspace
    )));
    let versions = frames
        .iter()
        .filter_map(|frame| {
            let Envelope::Delta { delta, .. } = frame else {
                return None;
            };
            let (_, body, _) = parts(CoeditPayload::decode(delta).unwrap());
            let message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                return None;
            };
            Some(
                collab_types::DocumentMutation::decode_versioned(&envelope.payload)
                    .unwrap()
                    .version,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !versions.is_empty(),
        "fixture contained no document mutations"
    );
    assert!(
        versions
            .iter()
            .all(|version| *version == collab_types::DocumentWireVersion::QosV1),
        "captured fixture changed durable wire versions: {versions:?}"
    );
    let peer = admit_frames_with_origin(
        &metadata(),
        &fixture().join("initial-peer.postcard"),
        frames,
        true,
    );
    let record = "tab:8b11000000000004";
    let receipt = peer
        .durable_coverage("dev.qos.notes", record)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.coverage.sequence, 7);
    assert_eq!(
        peer.document_heads("dev.qos.notes", record)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        receipt.coverage.heads
    );
}
pub(crate) fn producer_fixture() -> PathBuf {
    fixture().parent().unwrap().join("shell-selection")
}
pub(crate) fn producer_metadata() -> serde_json::Value {
    serde_json::from_slice(&fs::read(producer_fixture().join("manifest.json")).unwrap()).unwrap()
}
pub(crate) fn producer_frame(
    meta: &serde_json::Value,
    row: &serde_json::Value,
    time: u64,
) -> Vec<u8> {
    let channel = hex::decode(row["channel"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let body = fs::read(producer_fixture().join(row["file"].as_str().unwrap())).unwrap();
    let payload = if row["kind"] == "Checkpoint" {
        CoeditPayload::Checkpoint { channel, body }
    } else {
        CoeditPayload::RetainedDelta { channel, body }
    };
    crate::transport::encode(&Envelope::Delta {
        workspace_id: crate::room_workspace_id(meta["room"].as_str().unwrap()).unwrap(),
        author_device_public_key: serde_json::from_value::<tilo_user_identity::DeviceCert>(
            meta["authority"].clone(),
        )
        .unwrap()
        .device_public_key,
        authored_at_ns: time,
        signature: [0; 64],
        delta: payload.encode().unwrap(),
    })
    .unwrap()
}
fn retain_frame(store: &WorkspaceStore, bytes: &[u8]) {
    let Envelope::Delta { delta, .. } = crate::transport::decode(&bytes[4..]).unwrap() else {
        panic!()
    };
    match CoeditPayload::decode(&delta).unwrap() {
        CoeditPayload::Checkpoint { channel, .. } => {
            store.retain_checkpoint(&channel, bytes).unwrap()
        }
        CoeditPayload::RetainedDelta { channel, .. } => {
            store.retain_delta(&channel, 1, bytes).unwrap()
        }
    }
}

#[test]
fn legacy_qos_fixture_replays_through_real_retained_admission() {
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retain_frame(&store, &producer_frame(&meta, row, 1));
    }
    for row in &meta["tail"].as_array().unwrap()[..2] {
        retain_frame(&store, &producer_frame(&meta, row, 2));
    }

    let peer = admit_frames_with_origin(
        &meta,
        &producer_fixture().join("baseline-peer.postcard"),
        replay_frames(&store),
        true,
    );
    for row in meta["rows"].as_array().unwrap() {
        let app = row["app_id"].as_str().unwrap();
        let record = row["record_id"].as_str().unwrap();
        assert!(
            peer.durable_coverage(app, record).unwrap().is_some(),
            "legacy retained checkpoint lost durable coverage for {app}/{record}"
        );
        assert_eq!(
            peer.document_heads(app, record)
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            serde_json::from_value::<Vec<String>>(row["heads"].clone()).unwrap()
        );
    }
}

#[test]
fn shell_selection_missing_half_both_orders_keeps_previous_receipts() {
    let meta = producer_metadata();
    for room_first in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let authority = RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let mut store = WorkspaceStore::open(
            tmp.path(),
            crate::room_workspace_id(authority.room()).unwrap(),
            true,
        )
        .unwrap();
        store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            retain_frame(&store, &producer_frame(&meta, row, 1));
        }
        let before = replay_frames(&store);
        let (first, second) = if room_first { (0, 1) } else { (1, 0) };
        retain_frame(&store, &producer_frame(&meta, &meta["tail"][first], 2));
        assert!(
            replay_frames(&store) == before,
            "an incomplete pair must remain staged, room_first={room_first}"
        );
        retain_frame(&store, &producer_frame(&meta, &meta["tail"][second], 3));
        let peer = admit_frames(
            &meta,
            &producer_fixture().join("baseline-peer.postcard"),
            replay_frames(&store),
        );
        let receipts = meta["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                peer.durable_coverage(
                    row["app_id"].as_str().unwrap(),
                    row["record_id"].as_str().unwrap(),
                )
                .unwrap()
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts[0].generation, receipts[1].generation);
        assert!(
            receipts[2].generation > receipts[0].generation,
            "selective publication keeps mixed route generations"
        );
    }
}

#[test]
fn shell_selection_staging_disposition_is_bound_to_incoming_source_and_route() {
    use super::{
        bounded::{Step, WorkLimit},
        shell_selection::Publication,
    };
    let meta = producer_metadata();
    for foreign_source in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let authority = RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let mut store = WorkspaceStore::open(
            tmp.path(),
            crate::room_workspace_id(authority.room()).unwrap(),
            true,
        )
        .unwrap();
        store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            retain_frame(&store, &producer_frame(&meta, row, 1));
        }
        let staged = producer_frame(&meta, &meta["tail"][1], 2);
        retain_frame(&store, &staged);
        let mut input = crate::transport::decode(&staged[4..]).unwrap();
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = &mut input
        else {
            panic!()
        };
        let CoeditPayload::Checkpoint { mut channel, body } = CoeditPayload::decode(delta).unwrap()
        else {
            panic!()
        };
        if foreign_source {
            *author_device_public_key = [77; 32];
        } else {
            channel = [78; 32];
        }
        *delta = CoeditPayload::Checkpoint { channel, body }
            .encode()
            .unwrap();
        let bytes = crate::transport::encode(&input).unwrap();
        let mut build = store.start_retain(&channel, None, bytes.clone()).unwrap();
        let outcome = loop {
            if let Step::Done(outcome) = build.step(WorkLimit::default()).unwrap() {
                break outcome;
            }
        };
        assert_eq!(
            outcome.publication,
            Publication::Original,
            "a staged authentic body cannot suppress a different source or route, foreign_source={foreign_source}"
        );
        assert!(outcome.written > 0);
        assert!(
            store
                .load_retained()
                .unwrap()
                .iter()
                .any(|frame| frame == &bytes)
        );
    }
}

#[test]
fn shell_selection_interrupted_gc_preserves_historical_proof_until_retirement_is_recoverable() {
    use super::replay::*;
    let tmp = tempfile::tempdir().unwrap();
    let authority = binding();
    let [current_workspace, legacy_workspace] =
        crate::room_workspace_ids(authority.room()).unwrap();
    assert_ne!(current_workspace, legacy_workspace);
    let id = legacy_workspace;
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    copy_tree(&fixture().join("retained"), &store.root.join("retained"));
    let room = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode(authority.governor().device_public_key),
        hex::encode(blake3::hash(b"shell-tree").as_bytes())
    ));
    let active = retained::current(&room).unwrap().unwrap();
    assert!(fs::read_dir(&active).unwrap().count() > 0);
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    let mut interrupted = false;
    for _ in 0..20_000 {
        match store
            .capture_replay_step(
                &mut build,
                WorkLimit {
                    entries: 1,
                    ..Default::default()
                },
            )
            .unwrap()
        {
            Step::More => {}
            Step::Done(_) => panic!("capture finished before the retirement interruption"),
        }
        if fs::read_dir(&active).unwrap().count() == 0 {
            interrupted = true;
            break;
        }
    }
    assert!(interrupted, "capture never began exact legacy retirement");
    let remaining_before_reopen = fs::read_dir(store.root.join("retained/v2"))
        .unwrap()
        .map(|lineage| {
            retained::current(&lineage.unwrap().path())
                .unwrap()
                .map(|active| fs::read_dir(active).unwrap().count())
                .unwrap_or(0)
        })
        .sum::<usize>();
    assert!(
        remaining_before_reopen > 0,
        "fixture did not interrupt retirement between exact inputs"
    );
    let selected = retained::current(&store.root.join(super::shell_selection::DIRECTORY))
        .unwrap()
        .expect("retirement began before selected proof publication");
    drop(build);
    drop(store);
    let mut reopened = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    reopened.bind_shell_authority(authority).unwrap();
    assert_eq!(
        retained::current(&reopened.root.join(super::shell_selection::DIRECTORY))
            .unwrap()
            .unwrap(),
        selected
    );
    let peer = admit_frames_with_origin(
        &metadata(),
        &fixture().join("initial-peer.postcard"),
        replay_frames(&reopened),
        true,
    );
    assert_eq!(
        peer.durable_coverage("dev.qos.notes", "tab:8b11000000000004")
            .unwrap()
            .unwrap()
            .coverage
            .sequence,
        7
    );
    assert_eq!(retained::current(&room).unwrap().unwrap(), active);
    let mut remaining = 0;
    for lineage in fs::read_dir(reopened.root.join("retained/v2")).unwrap() {
        if let Some(active) = retained::current(&lineage.unwrap().path()).unwrap() {
            remaining += fs::read_dir(active).unwrap().count();
        }
    }
    assert_eq!(
        remaining, 0,
        "restart must finish proven retirement after old originals were unlinked"
    );
}

fn synthetic_owner() -> (
    ed25519_dalek::SigningKey,
    RetentionAuthority,
    StatePeer,
    web_time::SystemTime,
) {
    use automerge::transaction::Transactable;
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "selector fixture".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let now = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(2000);
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut peer = StatePeer::new(
        room,
        cert.clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "value", "baseline").unwrap();
    peer.install_seed(
        qos_peer_state::SeedState {
            room_changes: qos_peer_state::seed_room(room, &cert, &[cert.clone()]).unwrap(),
            documents: vec![qos_peer_state::SeedDocument {
                app_id: "selector.app".into(),
                record_id: "one".into(),
                kind: "test".into(),
                bytes: doc.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    (key, RetentionAuthority::new(room, cert).unwrap(), peer, now)
}
fn synthetic_signed(
    key: &ed25519_dalek::SigningKey,
    room: &str,
    mutation: collab_types::DocumentMutation,
    seq: u64,
    now: web_time::SystemTime,
) -> NeighbourMsg {
    let source = key.verifying_key().to_bytes();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id("selector.app", room, "one"),
        collab_types::Ed25519PublicKey::from_bytes(source),
        collab_types::document_attestation_id("selector.app", room, "one"),
        collab_types::OpClass::Splice,
        now,
        seq,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: qos_peer_state::peer_handle(&source),
        app_id: "selector.app".into(),
        cluster_id: room.into(),
        record_id: "one".into(),
        envelope: Box::new(envelope),
    }
}
fn synthetic_frame(
    authority: &RetentionAuthority,
    msg: &NeighbourMsg,
    checkpoint: bool,
) -> Vec<u8> {
    let channel = qos_peer_state::transport::retention(msg, checkpoint)
        .unwrap()
        .0;
    let body = postcard::to_allocvec(msg).unwrap();
    let payload = if checkpoint {
        CoeditPayload::Checkpoint { channel, body }
    } else {
        CoeditPayload::RetainedDelta { channel, body }
    };
    let source = match msg {
        NeighbourMsg::AppDocument { envelope, .. } => *envelope.author.as_bytes(),
        _ => authority.governor().device_public_key,
    };
    crate::transport::encode(&Envelope::Delta {
        workspace_id: crate::room_workspace_id(authority.room()).unwrap(),
        author_device_public_key: source,
        authored_at_ns: 1,
        signature: [0; 64],
        delta: payload.encode().unwrap(),
    })
    .unwrap()
}

#[test]
fn subscription_replay_falls_back_to_current_when_current_tail_is_a_prerequisite() {
    use super::replay::*;
    use automerge::transaction::Transactable;

    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &cp1[1] else {
        panic!()
    };
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let baseline_heads = branch.get_heads();
    branch.put(automerge::ROOT, "first", "T1").unwrap();
    branch.commit();
    let t1_heads = branch.get_heads();
    let t1_bytes = branch
        .get_changes(&baseline_heads)
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect();
    let t1 = synthetic_signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Incremental(t1_bytes),
        envelope.seq + 1,
        now,
    );
    branch.put(automerge::ROOT, "second", "T2").unwrap();
    branch.commit();
    let t2_bytes = branch
        .get_changes(&t1_heads)
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect();
    let t2 = synthetic_signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Incremental(t2_bytes),
        envelope.seq + 2,
        now,
    );
    let tmp = tempfile::tempdir().unwrap();
    let workspace_id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    for msg in &cp1 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let t1_frame = synthetic_frame(&authority, &t1, false);
    retain_frame(&store, &t1_frame);
    let prior_frames = replay_frames(&store);
    assert!(
        prior_frames
            .iter()
            .any(|frame| { crate::transport::encode(frame).unwrap() == t1_frame })
    );
    let selected_before = retained::current(&store.root.join("shell-selected-v1"))
        .unwrap()
        .unwrap();
    let mut removed_original = false;
    for lineage in fs::read_dir(store.root.join("retained/v2")).unwrap() {
        let lineage = lineage.unwrap().path();
        let Some(active) = retained::current(&lineage).unwrap() else {
            continue;
        };
        for entry in fs::read_dir(active).unwrap() {
            let path = entry.unwrap().path();
            if fs::read(&path).unwrap() == t1_frame {
                fs::remove_file(path).unwrap();
                removed_original = true;
            }
        }
    }
    assert!(removed_original, "fixture did not isolate T1 in CURRENT");
    drop(store);

    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
    let cp2_frames = cp2
        .iter()
        .map(|msg| synthetic_frame(&authority, msg, true))
        .collect::<Vec<_>>();
    let t2_frame = synthetic_frame(&authority, &t2, false);

    let mut reopened = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    for frame in &cp2_frames {
        retain_frame(&reopened, frame);
    }
    retain_frame(&reopened, &t2_frame);
    reopened.bind_shell_authority(authority).unwrap();
    let pending = crate::transport::encode(&Envelope::Presence {
        workspace_id,
        payload: b"pending subscription frame".to_vec(),
    })
    .unwrap();
    reopened.enqueue_pending(&[2; 32], 1, &pending).unwrap();
    let mut build = reopened
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    let mut snapshot = loop {
        match reopened
            .capture_replay_step(&mut build, WorkLimit::default())
            .unwrap()
        {
            Step::More => (),
            Step::Done(snapshot) => break snapshot,
        }
    };
    while matches!(
        reopened
            .prepare_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let mut replayed = Vec::new();
    loop {
        let mut batch = reopened
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        while let Some(frame) = batch.front() {
            replayed.push(crate::transport::encode(frame).unwrap());
            batch.pop_written();
        }
        if batch.eof {
            break;
        }
    }
    assert!(
        replayed.contains(&t1_frame),
        "CURRENT tail was not replayed"
    );
    assert!(
        replayed.contains(&pending),
        "pending frame was not replayed"
    );
    assert!(
        !replayed.contains(&t2_frame),
        "unresolved tail was replayed"
    );
    assert_eq!(
        retained::current(&reopened.root.join("shell-selected-v1"))
            .unwrap()
            .unwrap(),
        selected_before
    );
    let stored = reopened.load_shared().unwrap();
    for frame in cp2_frames.iter().chain(std::iter::once(&t2_frame)) {
        assert!(
            stored.contains(frame),
            "unresolved original was not retained"
        );
    }
    let work = reopened.root.join("store-work-v1");
    let mut replay_work = fs::read_dir(&work)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("replay-")
        })
        .collect::<Vec<_>>();
    assert_eq!(replay_work.len(), 1);
    let work_root = replay_work.pop().unwrap();
    let fallback_sort = work_root.join("extras-sort-fallback");
    assert!(fs::read_dir(&fallback_sort).unwrap().next().is_some());
    let mut discard = snapshot.into_discard().unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(!work_root.exists(), "fallback sort survived direct discard");

    let abandoned = work.join("replay-11111111111111111111111111111111");
    let abandoned_sort = abandoned.join("extras-sort-fallback");
    fs::create_dir_all(&abandoned_sort).unwrap();
    fs::write(
        abandoned_sort.join("0000000000000000-0000000000000000.run"),
        b"sorted metadata",
    )
    .unwrap();
    let mut reclaimer = reopened.start_work_reclamation().unwrap();
    while matches!(reclaimer.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(
        !abandoned.exists(),
        "fallback sort survived startup reclamation"
    );
    drop(reopened);

    let unbound = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    assert!(replay_frames(&unbound).starts_with(&prior_frames));
}

#[test]
fn shell_selection_uncovered_tail_blocks_new_floor_until_complete_successor_contains_it() {
    use automerge::transaction::Transactable;
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let baseline = owner.clone();
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let old_heads = branch.get_heads();
    branch.put(automerge::ROOT, "uncovered", "T2").unwrap();
    branch.commit();
    let tail_bytes = branch
        .get_changes(&old_heads)
        .iter()
        .flat_map(|c| c.raw_bytes().to_vec())
        .collect();
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &cp2[1] else {
        panic!()
    };
    let tail = synthetic_signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Incremental(tail_bytes),
        envelope.seq,
        now,
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    for msg in &cp1 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let raw_tail = synthetic_frame(&authority, &tail, false);
    retain_frame(&store, &raw_tail);
    for msg in &cp2 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let mut receiver = baseline.clone();
    for frame in replay_frames(&store) {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        let result = receiver
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                now,
            )
            .unwrap();
        assert_eq!(result.rejected, 0);
    }
    assert_eq!(
        receiver.document_heads("selector.app", "one").unwrap(),
        branch.get_heads(),
        "CP2 floor must not suppress uncovered T2"
    );
    let next = synthetic_signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Snapshot(branch.save()),
        100,
        now,
    );
    let NeighbourMsg::AppDocument { envelope, .. } = next else {
        panic!()
    };
    owner
        .admit_local_app_document("selector.app", "one", *envelope, now)
        .unwrap();
    let cp4 = owner.durable_bootstrap_messages(&key, now, 4).unwrap();
    for msg in &cp4 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let frames = replay_frames(&store);
    assert_eq!(
        frames.len(),
        2,
        "CP4 retires the proven contained tail and obsolete exact pairs"
    );
    let mut receiver = baseline;
    for frame in frames {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        receiver
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                now,
            )
            .unwrap();
    }
    assert_eq!(
        receiver.document_heads("selector.app", "one").unwrap(),
        owner.document_heads("selector.app", "one").unwrap()
    );
}

#[test]
fn selected_shell_unbound_reopen_preserves_readable_committed_basis_without_gc_authority() {
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retain_frame(&store, &producer_frame(&meta, row, 1));
    }
    let expected = replay_frames(&store);
    drop(store);
    let reopened = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    assert!(reopened.shell_authority().is_none());
    assert!(
        replay_frames(&reopened) == expected,
        "absence of local authority cannot hide already committed immutable replay references"
    );
    assert!(
        reopened.load_shared().unwrap().len() >= expected.len(),
        "compatibility readers include the committed selection"
    );
}

#[test]
fn selected_shell_replay_pins_survive_later_selection_and_activation_budget_is_owned() {
    use super::replay::*;
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority).unwrap();
    for row in meta["baseline"].as_array().unwrap() {
        retain_frame(&store, &producer_frame(&meta, row, 1));
    }
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    let mut pinned = loop {
        if let Step::Done(s) = store
            .capture_replay_step(
                &mut build,
                WorkLimit {
                    entries: 1,
                    ..Default::default()
                },
            )
            .unwrap()
        {
            break s;
        }
    };
    for row in meta["checkpoint2"].as_array().unwrap() {
        retain_frame(&store, &producer_frame(&meta, row, 2));
    }
    while matches!(
        store
            .prepare_replay_step(&mut pinned, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let mut frames = Vec::new();
    loop {
        let mut batch = store
            .read_replay_batch(&mut pinned, ReplayLimits::default(), &budget)
            .unwrap();
        while let Some(frame) = batch.front() {
            frames.push(frame.clone());
            batch.pop_written()
        }
        if batch.eof {
            break;
        }
    }
    assert_eq!(frames.len(), 4);
    let peer = admit_frames(
        &meta,
        &producer_fixture().join("baseline-peer.postcard"),
        frames,
    );
    assert_eq!(
        peer.durable_coverage("dev.qos.word-processor", "tab:0000000000000001")
            .unwrap()
            .unwrap()
            .coverage
            .sequence,
        3
    );
    let selected = retained::current(&store.root.join("shell-selected-v1"))
        .unwrap()
        .unwrap();
    let mut cursor = PublicationCursor::open(&selected).unwrap();
    assert!(cursor.step(&ReplayBudget::new(1)).is_err());
    let Step::Done(Some(frame)) = cursor.step(&budget).unwrap() else {
        panic!("output rejection must not consume activation metadata")
    };
    let held = std::sync::Arc::new(frame);
    let duplicate = held.clone();
    let used = budget.used();
    assert!(used > 0);
    drop(held);
    assert_eq!(budget.used(), used);
    drop(duplicate);
    assert_eq!(budget.used(), 0);
}

#[test]
fn shell_selection_preserves_invalid_foreign_and_unknown_records() {
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    for msg in &cp1 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let mut invalid = cp1[1].clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut invalid else {
        panic!()
    };
    envelope.seq += 100;
    let invalid = synthetic_frame(&authority, &invalid, true);
    retain_frame(&store, &invalid);
    let foreign_key = ed25519_dalek::SigningKey::from_bytes(&[37; 32]);
    let foreign = synthetic_signed(
        &foreign_key,
        authority.room(),
        collab_types::DocumentMutation::Snapshot(owner.document("selector.app", "one").unwrap()),
        1,
        now,
    );
    let foreign = synthetic_frame(&authority, &foreign, true);
    retain_frame(&store, &foreign);
    let unknown = crate::transport::encode(&Envelope::Delta {
        workspace_id: store.workspace_id,
        author_device_public_key: [99; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [88; 32],
            body: vec![7; 50],
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    retain_frame(&store, &unknown);
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
    for msg in &cp2 {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    let frames = replay_frames(&store)
        .into_iter()
        .map(|f| crate::transport::encode(&f).unwrap())
        .collect::<Vec<_>>();
    for original in [invalid, foreign, unknown] {
        assert!(
            frames.contains(&original),
            "unproven or foreign bytes must remain in replay"
        );
    }
}

#[test]
fn initial_shell_basis_replays_independent_reversed_author_tails_in_exact_sequence_order() {
    use automerge::transaction::Transactable;
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &cp[1] else {
        panic!()
    };
    let first_seq = envelope.seq + 1;
    let mut tails = Vec::new();
    let mut expected =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    for i in 0..2u8 {
        let mut branch =
            automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
        branch.set_actor(automerge::ActorId::from(vec![i + 90]));
        let heads = branch.get_heads();
        branch
            .put(automerge::ROOT, format!("tail-{i}"), i as u64)
            .unwrap();
        branch.commit();
        let bytes: Vec<u8> = branch
            .get_changes(&heads)
            .iter()
            .flat_map(|c| c.raw_bytes().to_vec())
            .collect();
        expected.load_incremental(&bytes).unwrap();
        tails.push(synthetic_signed(
            &key,
            authority.room(),
            collab_types::DocumentMutation::Incremental(bytes),
            first_seq + u64::from(i),
            now,
        ));
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut store = WorkspaceStore::open(
        tmp.path(),
        crate::room_workspace_id(authority.room()).unwrap(),
        true,
    )
    .unwrap();
    for msg in &cp {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    for msg in tails.iter().rev() {
        retain_frame(&store, &synthetic_frame(&authority, msg, false));
    }
    store.bind_shell_authority(authority).unwrap();
    let frames = replay_frames(&store);
    assert_eq!(
        frames.len(),
        4,
        "first selection contains complete snapshot plus both uncovered tails"
    );
    for frame in frames {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        let outcome = owner
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                now,
            )
            .unwrap();
        assert_eq!(outcome.rejected, 0);
    }
    assert_eq!(
        owner.document_heads("selector.app", "one").unwrap(),
        expected.get_heads(),
        "reverse receipt order must not silently lose the lower signed sequence"
    );
}

#[test]
fn reopened_shell_basis_replays_dependent_tails_cumulatively_in_signed_sequence_order() {
    use automerge::{ReadDoc, transaction::Transactable};
    let (key, authority, owner, now) = synthetic_owner();
    let cp = owner
        .clone()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &cp[1] else {
        panic!()
    };
    let first_seq = envelope.seq + 1;
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let mut tails = Vec::new();
    for (offset, (field, value)) in [("first", "T1"), ("second", "T2")].into_iter().enumerate() {
        let heads = branch.get_heads();
        branch.put(automerge::ROOT, field, value).unwrap();
        branch.commit();
        let bytes = branch
            .get_changes(&heads)
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        tails.push(synthetic_signed(
            &key,
            authority.room(),
            collab_types::DocumentMutation::Incremental(bytes),
            first_seq + u64::try_from(offset).unwrap(),
            now,
        ));
    }

    let tmp = tempfile::tempdir().unwrap();
    let workspace_id = crate::room_workspace_id(authority.room()).unwrap();
    let store = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    for msg in &cp {
        retain_frame(&store, &synthetic_frame(&authority, msg, true));
    }
    for msg in tails.iter().rev() {
        retain_frame(&store, &synthetic_frame(&authority, msg, false));
    }
    drop(store);

    let mut reopened = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    reopened.bind_shell_authority(authority).unwrap();
    let frames = replay_frames(&reopened);
    assert_eq!(frames.len(), 4);
    let mut receiver = owner;
    for frame in frames {
        let Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } = frame
        else {
            panic!()
        };
        let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
        let outcome = receiver
            .receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key),
                postcard::from_bytes(&body).unwrap(),
                now,
            )
            .unwrap();
        assert_eq!(outcome.rejected, 0);
        assert!(!outcome.deferred);
        assert!(outcome.incomplete.is_none());
    }
    assert_eq!(
        receiver.document_heads("selector.app", "one").unwrap(),
        branch.get_heads()
    );
    let replayed =
        automerge::Automerge::load(&receiver.document("selector.app", "one").unwrap()).unwrap();
    assert_eq!(
        replayed
            .get(automerge::ROOT, "first")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("T1")
    );
    assert_eq!(
        replayed
            .get(automerge::ROOT, "second")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("T2")
    );
}

#[test]
fn initial_shell_conflict_or_t2_without_t1_remains_unselected() {
    use super::replay::*;
    use automerge::transaction::Transactable;
    for conflicting in [false, true] {
        let (key, authority, mut owner, now) = synthetic_owner();
        let cp = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = &cp[1] else {
            panic!()
        };
        let seq = envelope.seq + 1;
        let base = owner.document("selector.app", "one").unwrap();
        let mut branch = automerge::AutoCommit::load(&base).unwrap();
        let mut tails = Vec::new();
        for i in 0..2u8 {
            if conflicting {
                branch = automerge::AutoCommit::load(&base).unwrap();
                branch.set_actor(automerge::ActorId::from(vec![i + 70]));
            }
            let heads = branch.get_heads();
            branch
                .put(automerge::ROOT, format!("case-{i}"), i as u64)
                .unwrap();
            branch.commit();
            let bytes = branch
                .get_changes(&heads)
                .iter()
                .flat_map(|c| c.raw_bytes().to_vec())
                .collect();
            tails.push(synthetic_signed(
                &key,
                authority.room(),
                collab_types::DocumentMutation::Incremental(bytes),
                seq + if conflicting { 0 } else { u64::from(i) },
                now,
            ));
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(
            tmp.path(),
            crate::room_workspace_id(authority.room()).unwrap(),
            true,
        )
        .unwrap();
        let retained_tails = if conflicting {
            tails.as_slice()
        } else {
            &tails[1..]
        };
        let originals = cp
            .iter()
            .map(|msg| synthetic_frame(&authority, msg, true))
            .chain(
                retained_tails
                    .iter()
                    .map(|msg| synthetic_frame(&authority, msg, false)),
            )
            .collect::<Vec<_>>();
        if !conflicting {
            assert_eq!(originals.len(), cp.len() + 1, "only T2 is retained");
        }
        for bytes in &originals {
            retain_frame(&store, bytes);
        }
        store.bind_shell_authority(authority).unwrap();
        let mut build = store
            .start_replay_capture(ReplayId(rand::random()), [2; 32])
            .unwrap();
        let error = loop {
            match store.capture_replay_step(&mut build, WorkLimit::default()) {
                Err(error) => break error,
                Ok(Step::More) => (),
                Ok(Step::Done(_)) => panic!("unproven tail ordering became selected"),
            }
        };
        assert!(error.to_string().contains(if conflicting {
            "conflicting signed tail sequence"
        } else {
            "unresolved shell tail"
        }));
        assert!(
            retained::current(&store.root.join("shell-selected-v1"))
                .unwrap()
                .is_none()
        );
        let stored = store.load_shared().unwrap();
        for original in originals {
            assert!(stored.contains(&original));
        }
    }
}

fn shell_selection_preserves_unproved_wrapper(outer: bool) {
    use super::{
        bounded::{Step, WorkLimit},
        shell_selection::Publication,
    };
    let meta = producer_metadata();
    let authority = RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority.clone()).unwrap();
    let raw = producer_frame(&meta, &meta["baseline"][0], 1);
    let unproved = retained::tests::wrapper_suffix(&raw, outer);
    retain_frame(&store, &unproved);
    for group in ["baseline", "checkpoint2"] {
        for row in meta[group].as_array().unwrap() {
            retain_frame(&store, &producer_frame(&meta, row, 2));
        }
    }
    assert!(
        store.load_retained().unwrap().contains(&unproved),
        "selection cannot retire an unproved wrapper"
    );
    let incoming =
        retained::tests::wrapper_suffix(&producer_frame(&meta, &meta["baseline"][0], 3), outer);
    let Envelope::Delta { delta, .. } = crate::transport::decode(&incoming[4..]).unwrap() else {
        panic!()
    };
    let (channel, _, _) = parts(CoeditPayload::decode(&delta).unwrap());
    let mut build = store
        .start_retain(&channel, None, incoming.clone())
        .unwrap();
    let outcome = loop {
        if let Step::Done(outcome) = build.step(WorkLimit::default()).unwrap() {
            break outcome;
        }
    };
    assert_eq!(
        outcome.publication,
        Publication::Original,
        "selection cannot suppress an unproved incoming wrapper"
    );
    assert!(outcome.written > 0);
    drop(build);
    drop(store);
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    let replay = replay_frames(&store);
    assert!(replay.contains(&crate::transport::decode(&unproved[4..]).unwrap()));
    assert!(replay.contains(&crate::transport::decode(&incoming[4..]).unwrap()));
    let retained = store.load_retained().unwrap();
    assert!(retained.contains(&unproved));
    assert!(retained.contains(&incoming));
}

#[test]
fn shell_selection_preserves_unproved_outer_wrapper() {
    shell_selection_preserves_unproved_wrapper(true);
}
#[test]
fn shell_selection_preserves_unproved_coedit_wrapper() {
    shell_selection_preserves_unproved_wrapper(false);
}
#[test]
fn retained_authority_accepts_only_its_exact_current_or_legacy_room_binding() {
    let authority = binding();
    let room = uuid::Uuid::parse_str(authority.room()).unwrap();
    let hash = blake3::derive_key("qos-room-transport-v1", room.to_string().as_bytes());
    let mut first = [0; 8];
    first.copy_from_slice(&hash[..8]);
    let legacy = u64::from_le_bytes(first).max(1);
    let current = crate::room_workspace_id(authority.room()).unwrap();
    assert_ne!(legacy, current);
    for workspace in [legacy, current] {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), workspace, true).unwrap();
        store.bind_shell_authority(authority.clone()).unwrap();
        assert_eq!(store.workspace_id(), workspace);
    }
    let tmp = tempfile::tempdir().unwrap();
    let unrelated = crate::room_workspace_id(&uuid::Uuid::new_v4().to_string()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), unrelated, true).unwrap();
    assert!(store.bind_shell_authority(authority).is_err());
}
