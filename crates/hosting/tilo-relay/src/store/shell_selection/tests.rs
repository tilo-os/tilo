//! Shell selection regressions over public real-owner and signed synthetic inputs.
#![allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::*;
use crate::store::{
    replay::{PublicationCursor, ReplayBudget, ReplayId, ReplayLimits, ReplaySnapshot},
    retained::{CheckpointStage, RetainBuild},
    shell_proof_tests::{binding, fixture, governed_sequence, metadata, parts, superseded_route},
};
use crate::transport::Envelope;
use automerge::transaction::Transactable;
use tilo_peer_state::{StatePeer, TransportOrigin};

// ---- shared drivers -------------------------------------------------------

fn finish<T>(mut step: impl FnMut() -> Step<T>) -> T {
    loop {
        if let Step::Done(value) = step() {
            return value;
        }
    }
}
fn one_entry() -> WorkLimit {
    WorkLimit {
        entries: 1,
        ..Default::default()
    }
}
fn scratch(store: &WorkspaceStore) -> PathBuf {
    store.root.join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ))
}
fn selection(store: &WorkspaceStore) -> Selection {
    Selection::new(store, scratch(store), None).unwrap()
}
fn select_all(store: &WorkspaceStore) {
    let mut selector = selection(store);
    finish(|| selector.step(WorkLimit::default()).unwrap());
}
fn retain(store: &WorkspaceStore, bytes: &[u8]) {
    let Envelope::Delta { delta, .. } = crate::transport::decode(&bytes[4..]).unwrap() else {
        panic!()
    };
    match CoeditPayload::decode(&delta).unwrap() {
        CoeditPayload::Checkpoint { channel, .. } => {
            store.retain_checkpoint(&channel, bytes).unwrap();
        }
        CoeditPayload::RetainedDelta { channel, .. } => {
            store.retain_delta(&channel, 1, bytes).unwrap();
        }
    }
}
fn checkpoint_channel(store: &WorkspaceStore, bytes: &[u8]) -> [u8; 32] {
    let (_, CoeditPayload::Checkpoint { channel, .. }) =
        retained::payload(bytes, store.workspace_id()).unwrap()
    else {
        panic!("checkpoint fixture contains a retained delta")
    };
    channel
}
fn retain_build(store: &WorkspaceStore, bytes: &[u8]) -> RetainBuild {
    store
        .start_retain(&checkpoint_channel(store, bytes), None, bytes.to_vec())
        .unwrap()
}
fn retain_outcome(mut build: RetainBuild, limit: WorkLimit) -> RetainOutcome {
    finish(|| build.step(limit).unwrap())
}
/// Stages each checkpoint as a projected lineage generation under a fresh
/// work root, asserting `eligible` (when given) for every staged input.
fn stage_checkpoints(
    store: &WorkspaceStore,
    frames: impl IntoIterator<Item = Vec<u8>>,
    eligible: Option<&str>,
) -> (PathBuf, Vec<CheckpointStage>, Vec<[u8; 32]>) {
    let root = scratch(store);
    fs::create_dir(&root).unwrap();
    let mut stages = Vec::new();
    let mut incoming = Vec::new();
    for (index, bytes) in frames.into_iter().enumerate() {
        let channel = checkpoint_channel(store, &bytes);
        incoming.push(incoming_identity(&bytes, store.workspace_id()).unwrap());
        let mut stage = store
            .start_staged_checkpoint(&channel, bytes, root.join(format!("input-{index:02}")))
            .unwrap();
        let projectable = finish(|| stage.step(WorkLimit::default()).unwrap());
        if let Some(message) = eligible {
            assert!(projectable, "{message}");
        }
        stages.push(stage);
    }
    (root, stages, incoming)
}
fn projected_selection(
    store: &WorkspaceStore,
    root: &Path,
    incoming: Vec<[u8; 32]>,
    stages: &[CheckpointStage],
) -> Selection {
    let overrides = stages
        .iter()
        .map(|stage| {
            (
                stage.lineage_name().unwrap().to_os_string(),
                stage.projected_generation().to_path_buf(),
            )
        })
        .collect();
    Selection::new_projected(store, root.join("selection"), incoming, overrides).unwrap()
}
fn selected_current(store: &WorkspaceStore) -> Option<PathBuf> {
    retained::current(&store.root.join(DIRECTORY)).unwrap()
}
fn assert_selected_complete(store: &WorkspaceStore) {
    let mut selected = SelectedRead::new(store).unwrap().unwrap();
    while matches!(selected.step().unwrap(), Step::More) {}
    assert!(selected.basis.complete());
}

// ---- replay and admission -------------------------------------------------

fn drain_replay(
    store: &WorkspaceStore,
    snapshot: &mut ReplaySnapshot,
    budget: &ReplayBudget,
) -> Vec<Envelope> {
    while matches!(
        store
            .prepare_replay_step(snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let mut result = Vec::new();
    loop {
        let mut batch = store
            .read_replay_batch(snapshot, ReplayLimits::default(), budget)
            .unwrap();
        while let Some(frame) = batch.front() {
            result.push(frame.clone());
            batch.pop_written();
        }
        if batch.eof {
            return result;
        }
    }
}
fn capture(store: &WorkspaceStore, limit: WorkLimit) -> ReplaySnapshot {
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    finish(|| store.capture_replay_step(&mut build, limit).unwrap())
}
pub(crate) fn replay_frames(store: &WorkspaceStore) -> Vec<Envelope> {
    let mut snapshot = capture(store, one_entry());
    let result = drain_replay(store, &mut snapshot, &ReplayBudget::new(256 * 1024 * 1024));
    let mut discard = snapshot.into_discard().unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    result
}
fn encoded(frames: &[Envelope]) -> Vec<Vec<u8>> {
    frames
        .iter()
        .map(|frame| crate::transport::encode(frame).unwrap())
        .collect()
}
fn receive(
    peer: &mut StatePeer,
    frame: Envelope,
    retained: bool,
    now: web_time::SystemTime,
) -> Result<tilo_peer_state::ReceiveOutcome, String> {
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = frame
    else {
        return Err("expected a delta frame".to_string());
    };
    let (_, body, _) = parts(CoeditPayload::decode(&delta).unwrap());
    let origin = if retained {
        TransportOrigin::RetainedRelayDelta(author_device_public_key)
    } else {
        TransportOrigin::RelayDelta(author_device_public_key)
    };
    peer.receive_from(origin, postcard::from_bytes(&body).unwrap(), now)
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
        .checked_add(web_time::Duration::from_nanos(
            meta["now_ns"].as_u64().unwrap(),
        ))
        .unwrap();
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
        let outcome = receive(&mut peer, frame, retained, now)
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
fn active_retained_count(store: &WorkspaceStore) -> usize {
    fs::read_dir(store.root.join("retained/v2"))
        .unwrap()
        .map(|lineage| {
            retained::current(&lineage.unwrap().path())
                .unwrap()
                .map_or(0, |active| fs::read_dir(active).unwrap().count())
        })
        .sum()
}

// ---- real-owner producer fixture ------------------------------------------

pub(crate) fn producer_fixture() -> PathBuf {
    fixture().parent().unwrap().join("shell-selection")
}
pub(crate) fn producer_metadata() -> serde_json::Value {
    serde_json::from_slice(&fs::read(producer_fixture().join("manifest.json")).unwrap()).unwrap()
}
fn producer_authority(meta: &serde_json::Value) -> RetentionAuthority {
    RetentionAuthority::new(
        meta["room"].as_str().unwrap(),
        serde_json::from_value(meta["authority"].clone()).unwrap(),
    )
    .unwrap()
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
fn producer_frames(meta: &serde_json::Value, group: &str, time: u64) -> Vec<Vec<u8>> {
    meta[group]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| producer_frame(meta, row, time))
        .collect()
}
/// A store bound to `authority` at its current workspace id.
fn bound_store(authority: RetentionAuthority) -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::tempdir().unwrap();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    (tmp, store)
}
/// A bound store holding the producer baseline checkpoint.
fn baseline_store(meta: &serde_json::Value) -> (tempfile::TempDir, WorkspaceStore) {
    let (tmp, store) = bound_store(producer_authority(meta));
    for bytes in producer_frames(meta, "baseline", 1) {
        retain(&store, &bytes);
    }
    (tmp, store)
}
/// Baseline selected, then the next checkpoint admitted durable before any selection.
fn setup() -> (tempfile::TempDir, WorkspaceStore) {
    let meta = producer_metadata();
    let (tmp, store) = baseline_store(&meta);
    // Trusted fixture import models admitted bytes durable before selection.
    let raw = WorkspaceStore::open(tmp.path(), store.workspace_id, true).unwrap();
    for bytes in producer_frames(&meta, "checkpoint2", 2) {
        retain(&raw, &bytes);
    }
    (tmp, store)
}
fn selected_baseline_store(meta: &serde_json::Value) -> (tempfile::TempDir, WorkspaceStore) {
    let (tmp, store) = baseline_store(meta);
    select_all(&store);
    (tmp, store)
}
fn reopen_bound(tmp: &Path, id: u64, authority: RetentionAuthority) -> WorkspaceStore {
    let mut store = WorkspaceStore::open(tmp, id, true).unwrap();
    store.bind_shell_authority(authority).unwrap();
    store
}

// ---- signed synthetic owner -----------------------------------------------

const SYNTHETIC_ROOM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

fn synthetic_cert(
    root: u8,
    key: &ed25519_dalek::SigningKey,
    label: String,
) -> tilo_user_identity::DeviceCert {
    let root = ed25519_dalek::SigningKey::from_bytes(&[root; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label,
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    cert
}
fn seeded_owner(
    room: &str,
    cert: &tilo_user_identity::DeviceCert,
    key: &ed25519_dalek::SigningKey,
    documents: Vec<tilo_peer_state::SeedDocument>,
    now: web_time::SystemTime,
) -> StatePeer {
    let mut peer = StatePeer::new(
        room,
        cert.clone(),
        vec![],
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        tilo_peer_state::SeedState {
            room_changes: tilo_peer_state::seed_room(room, cert, std::slice::from_ref(cert))
                .unwrap(),
            documents,
        },
        key,
        now,
    )
    .unwrap();
    peer
}
fn synthetic_owner() -> (
    ed25519_dalek::SigningKey,
    RetentionAuthority,
    StatePeer,
    web_time::SystemTime,
) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let cert = synthetic_cert(31, &key, "selector fixture".into());
    let now = web_time::SystemTime::UNIX_EPOCH
        .checked_add(web_time::Duration::from_secs(2000))
        .unwrap();
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "value", "baseline").unwrap();
    let document = tilo_peer_state::SeedDocument {
        app_id: "selector.app".into(),
        record_id: "one".into(),
        kind: "test".into(),
        bytes: doc.save(),
    };
    let peer = seeded_owner(SYNTHETIC_ROOM, &cert, &key, vec![document], now);
    (
        key,
        RetentionAuthority::new(SYNTHETIC_ROOM, cert).unwrap(),
        peer,
        now,
    )
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper keeps the call shape of the fixtures it builds"
)]
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
        peer: tilo_peer_state::peer_handle(&source),
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
    let channel = tilo_peer_state::transport::retention(msg, checkpoint)
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
fn retain_synthetic(
    store: &WorkspaceStore,
    authority: &RetentionAuthority,
    messages: &[NeighbourMsg],
    checkpoint: bool,
) {
    for msg in messages {
        retain(store, &synthetic_frame(authority, msg, checkpoint));
    }
}
fn document_seq(msg: &NeighbourMsg) -> u64 {
    let NeighbourMsg::AppDocument { envelope, .. } = msg else {
        panic!()
    };
    envelope.seq
}
fn changes_since(branch: &mut automerge::AutoCommit, heads: &[automerge::ChangeHash]) -> Vec<u8> {
    branch
        .get_changes(heads)
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect()
}
fn incremental(
    key: &ed25519_dalek::SigningKey,
    authority: &RetentionAuthority,
    bytes: Vec<u8>,
    seq: u64,
    now: web_time::SystemTime,
) -> NeighbourMsg {
    synthetic_signed(
        key,
        authority.room(),
        collab_types::DocumentMutation::Incremental(bytes),
        seq,
        now,
    )
}
/// Re-encodes a checkpoint frame with one identity-relevant field changed.
fn changed_snapshot_frame(raw: &[u8], change: &str) -> Vec<u8> {
    let mut envelope = crate::transport::decode(&raw[4..]).unwrap();
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = &mut envelope
    else {
        panic!()
    };
    let CoeditPayload::Checkpoint {
        mut channel,
        mut body,
    } = CoeditPayload::decode(delta).unwrap()
    else {
        panic!()
    };
    match change {
        "source" => author_device_public_key[0] ^= 1,
        "channel" => channel[0] ^= 1,
        "body" => {
            // Preserve the route, invalidate the signature.
            let mut message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
            let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
                panic!()
            };
            envelope.seq = envelope.seq.saturating_add(1);
            body = postcard::to_allocvec(&message).unwrap();
        }
        "kind" => {}
        _ => unreachable!(),
    }
    *delta = if change == "kind" {
        CoeditPayload::RetainedDelta { channel, body }
    } else {
        CoeditPayload::Checkpoint { channel, body }
    }
    .encode()
    .unwrap();
    crate::transport::encode(&envelope).unwrap()
}

// ---- captured shell replay ------------------------------------------------

#[test]
fn captured_shell_store_selection_replays_exact_current_pairs() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = binding();
    let workspace = crate::room_workspace_id(authority.room()).unwrap();
    let store = reopen_bound(tmp.path(), workspace, authority);
    // Trusted test-store import of the public authenticated-ingress capture.
    copy_tree(&fixture().join("retained"), &store.root.join("retained"));
    let frames = replay_frames(&store);
    assert!(frames.iter().all(|frame| matches!(
        frame,
        Envelope::Delta { workspace_id, .. } if *workspace_id == workspace
    )));
    let mutations = frames
        .iter()
        .filter(|frame| {
            let Envelope::Delta { delta, .. } = frame else {
                return false;
            };
            let (_, body, _) = parts(CoeditPayload::decode(delta).unwrap());
            let message: NeighbourMsg = postcard::from_bytes(&body).unwrap();
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                return false;
            };
            collab_types::DocumentMutation::decode(&envelope.payload).unwrap();
            true
        })
        .count();
    assert!(mutations > 0, "fixture contained no document mutations");
    let peer = admit_frames_with_origin(
        &metadata(),
        &fixture().join("initial-peer.postcard"),
        frames,
        true,
    );
    let (app, record) = superseded_route();
    let receipt = peer.durable_coverage(&app, &record).unwrap().unwrap();
    assert_eq!(receipt.coverage.sequence, governed_sequence());
    assert_eq!(
        peer.document_heads(&app, &record)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        receipt.coverage.heads
    );
}

#[test]
fn shell_selection_interrupted_gc_preserves_historical_proof_until_retirement_is_recoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let authority = binding();
    let id = crate::room_workspace_id(authority.room()).unwrap();
    let store = reopen_bound(tmp.path(), id, authority.clone());
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
        match store.capture_replay_step(&mut build, one_entry()).unwrap() {
            Step::More => {}
            Step::Done(_) => panic!("capture finished before the retirement interruption"),
        }
        if fs::read_dir(&active).unwrap().count() == 0 {
            interrupted = true;
            break;
        }
    }
    assert!(interrupted, "capture never began exact retirement");
    assert!(
        active_retained_count(&store) > 0,
        "fixture did not interrupt retirement between exact inputs"
    );
    let selected =
        selected_current(&store).expect("retirement began before selected proof publication");
    drop(build);
    drop(store);
    let reopened = reopen_bound(tmp.path(), id, authority);
    assert_eq!(selected_current(&reopened).unwrap(), selected);
    let peer = admit_frames_with_origin(
        &metadata(),
        &fixture().join("initial-peer.postcard"),
        replay_frames(&reopened),
        true,
    );
    let (app, record) = superseded_route();
    assert_eq!(
        peer.durable_coverage(&app, &record)
            .unwrap()
            .unwrap()
            .coverage
            .sequence,
        governed_sequence()
    );
    assert_eq!(retained::current(&room).unwrap().unwrap(), active);
    assert_eq!(
        active_retained_count(&reopened),
        0,
        "restart must finish proven retirement after old originals were unlinked"
    );
}

// ---- producer fixture selection -------------------------------------------

#[test]
fn producer_fixture_replays_through_real_retained_admission() {
    let meta = producer_metadata();
    let (_tmp, store) = baseline_store(&meta);
    for bytes in &producer_frames(&meta, "tail", 2)[..2] {
        retain(&store, bytes);
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
            "retained checkpoint lost durable coverage for {app}/{record}"
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
        let (_tmp, store) = baseline_store(&meta);
        let before = replay_frames(&store);
        let (first, second) = if room_first { (0, 1) } else { (1, 0) };
        retain(&store, &producer_frame(&meta, &meta["tail"][first], 2));
        assert!(
            replay_frames(&store) == before,
            "an incomplete pair must remain staged, room_first={room_first}"
        );
        retain(&store, &producer_frame(&meta, &meta["tail"][second], 3));
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
    let meta = producer_metadata();
    for foreign_source in [true, false] {
        let (_tmp, store) = baseline_store(&meta);
        let staged = producer_frame(&meta, &meta["tail"][1], 2);
        retain(&store, &staged);
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
        let outcome = retain_outcome(
            store.start_retain(&channel, None, bytes.clone()).unwrap(),
            WorkLimit::default(),
        );
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
fn selected_shell_unbound_reopen_preserves_readable_committed_basis_without_gc_authority() {
    let meta = producer_metadata();
    let (tmp, store) = baseline_store(&meta);
    let id = store.workspace_id;
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
    let meta = producer_metadata();
    let (_tmp, store) = baseline_store(&meta);
    let mut pinned = capture(&store, one_entry());
    for bytes in producer_frames(&meta, "checkpoint2", 2) {
        retain(&store, &bytes);
    }
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let frames = drain_replay(&store, &mut pinned, &budget);
    assert_eq!(frames.len(), 4);
    let peer = admit_frames(
        &meta,
        &producer_fixture().join("baseline-peer.postcard"),
        frames,
    );
    assert_eq!(
        peer.durable_coverage("so.tilo.word-processor", "tab:0000000000000001")
            .unwrap()
            .unwrap()
            .coverage
            .sequence,
        3
    );
    let selected = selected_current(&store).unwrap();
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

fn shell_selection_preserves_unproved_wrapper(outer: bool) {
    let meta = producer_metadata();
    let authority = producer_authority(&meta);
    let (tmp, store) = bound_store(authority.clone());
    let id = store.workspace_id;
    let raw = producer_frame(&meta, &meta["baseline"][0], 1);
    let unproved = retained::tests::wrapper_suffix(&raw, outer);
    retain(&store, &unproved);
    for group in ["baseline", "checkpoint2"] {
        for bytes in producer_frames(&meta, group, 2) {
            retain(&store, &bytes);
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
    let outcome = retain_outcome(
        store
            .start_retain(&channel, None, incoming.clone())
            .unwrap(),
        WorkLimit::default(),
    );
    assert_eq!(
        outcome.publication,
        Publication::Original,
        "selection cannot suppress an unproved incoming wrapper"
    );
    assert!(outcome.written > 0);
    drop(store);
    let store = reopen_bound(tmp.path(), id, authority);
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
fn retained_authority_accepts_only_its_exact_room_binding() {
    let authority = binding();
    let workspace = crate::room_workspace_id(authority.room()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = reopen_bound(tmp.path(), workspace, authority.clone());
    assert_eq!(store.workspace_id(), workspace);
    let tmp = tempfile::tempdir().unwrap();
    let unrelated = crate::room_workspace_id(&uuid::Uuid::new_v4().to_string()).unwrap();
    let mut store = WorkspaceStore::open(tmp.path(), unrelated, true).unwrap();
    assert!(store.bind_shell_authority(authority).is_err());
}

// ---- selector internals over the producer fixture -------------------------

#[test]
fn unchanged_basis_does_not_repeat_snapshot_verification() {
    let (_tmp, store) = setup();
    select_all(&store);
    let selected_before = replay_frames(&store);

    let mut selector = selection(&store);
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(selector.basis.complete());
    assert_eq!(selector.basis.routes.len(), 3);
    assert_eq!(
        selector.snapshot_verifications,
        (3, 0),
        "Pair verifies each possible route once; Check reuses only the identical proved input"
    );
    assert_eq!(replay_frames(&store), selected_before);
}

#[test]
fn fallback_original_proof_reuse_is_limited_to_exact_proved_incoming_slots() {
    let meta = producer_metadata();
    for change in ["unchanged", "source", "channel", "kind", "body", "unproved"] {
        let (tmp, store) = baseline_store(&meta);
        let id = store.workspace_id;
        let raw_store = WorkspaceStore::open(tmp.path(), id, true).unwrap();
        let mut apps = Vec::new();
        for mut raw in producer_frames(&meta, "checkpoint2", 2) {
            let (_, CoeditPayload::Checkpoint { body, .. }) = retained::payload(&raw, id).unwrap()
            else {
                panic!()
            };
            if matches!(
                postcard::from_bytes::<NeighbourMsg>(&body).unwrap(),
                NeighbourMsg::AppDocument { .. }
            ) {
                if apps.len() == 2 {
                    continue;
                }
                if apps.is_empty() && change == "unproved" {
                    raw = changed_snapshot_frame(&raw, "source");
                }
                apps.push(raw.clone());
            }
            retain(&raw_store, &raw);
        }
        assert_eq!(apps.len(), 2);
        let incoming = incoming_identity(&apps[0], id).unwrap();
        let mut selector = Selection::new_batch(&store, scratch(&store), vec![incoming]).unwrap();
        while !matches!(selector.phase, Phase::Check) {
            selector.step_projection(WorkLimit::default()).unwrap();
        }
        assert!(selector.fallback && selector.basis.room.is_some());
        assert_eq!(selector.incoming_verified, vec![change != "unproved"]);
        let mut inventory = File::open(selector.inventory.as_ref().unwrap()).unwrap();
        let path = loop {
            let meta = read_meta(&mut inventory)
                .unwrap()
                .expect("incoming retained frame");
            let path = store.root.join(meta.source);
            if frame(&path, id)
                .unwrap()
                .is_some_and(|value| value.identity() == incoming)
            {
                break path;
            }
        };
        let expected = if matches!(change, "unchanged" | "unproved") {
            apps[0].clone()
        } else {
            changed_snapshot_frame(&apps[0], change)
        };
        fs::write(&path, &expected).unwrap();
        let outcome = finish(|| selector.step_projection(WorkLimit::default()).unwrap());
        assert_eq!(
            outcome.dispositions,
            vec![IncomingDisposition::Original],
            "{change}"
        );
        assert_eq!(outcome.publication, Publication::Original, "{change}");
        assert_eq!(
            selector.snapshot_verifications.1,
            if change == "unchanged" { 1 } else { 2 },
            "nonincoming and unproved/changed inputs must keep full verification: {change}"
        );
        assert_eq!(
            fs::read(path).unwrap(),
            expected,
            "Original was retired: {change}"
        );
    }
}

#[test]
fn changed_snapshot_input_cannot_reuse_a_paired_proof() {
    for change in ["source", "channel", "kind", "body"] {
        let (_tmp, store) = setup();
        select_all(&store);
        let mut selector = selection(&store);
        while !matches!(selector.phase, Phase::Check) {
            selector.step(WorkLimit::default()).unwrap();
        }
        let route = selector.basis.routes.values().next().unwrap();
        assert!(route.verified_input.is_some());
        let path = store.root.join(&route.file.as_ref().unwrap().source);
        fs::write(
            &path,
            changed_snapshot_frame(&fs::read(&path).unwrap(), change),
        )
        .unwrap();

        // Drive the actual Check path. Unchanged records may precede the edited
        // one, but none needs a second proof. The changed input always does.
        while selector.snapshot_verifications.1 == 0 {
            assert!(matches!(selector.phase, Phase::Check), "{change}");
            selector.step(WorkLimit::default()).unwrap();
        }
        assert_eq!(selector.snapshot_verifications.1, 1, "{change}");
        assert_eq!(selector.blocked, change != "kind", "{change}");
        assert!(
            path.exists(),
            "an uncommitted proof check retired its input"
        );
    }
}

#[test]
fn replacement_room_does_not_inherit_old_snapshot_proofs_before_pairing() {
    let (_tmp, store) = setup();
    let mut selector = selection(&store);
    while !matches!(selector.phase, Phase::Pair) {
        selector.step(WorkLimit::default()).unwrap();
    }
    assert!(selector.old.complete());
    assert!(
        selector
            .old
            .routes
            .values()
            .all(|route| route.verified_input.is_some())
    );
    assert_ne!(
        selector.old.room.as_ref().unwrap().heads(),
        selector.basis.room.as_ref().unwrap().heads()
    );
    assert!(!selector.basis.routes.is_empty());
    assert!(
        selector
            .basis
            .routes
            .values()
            .all(|route| { route.file.is_none() && route.verified_input.is_none() })
    );
}

#[test]
fn serial_incomplete_checkpoint_pairs_wait_then_publish_each_route_once() {
    fn contains_original(path: &Path, expected: &[u8]) -> bool {
        fs::read_dir(path).unwrap().any(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                contains_original(&entry.path(), expected)
            } else if entry.path().extension().is_some_and(|ext| ext == "delta") {
                replay::frame_bytes(&entry.path()).unwrap().as_deref() == Some(expected)
            } else {
                false
            }
        })
    }
    let meta = producer_metadata();
    let authority = producer_authority(&meta);
    let (tmp, mut store) = bound_store(authority.clone());
    for (generation, name) in [(1, "baseline"), (2, "checkpoint2")] {
        let frames = producer_frames(&meta, name, generation);
        assert_eq!(
            frames.len(),
            4,
            "real owner fixture is room plus three document routes"
        );
        let mut originals = Vec::new();
        for (index, bytes) in frames.iter().enumerate() {
            originals.push(bytes.clone());
            let outcome = retain_outcome(retain_build(&store, bytes), one_entry());
            if index + 1 < frames.len() {
                // Reopen and complete selected GC: suppression must not discard the
                // only durable copy while the rest of the checkpoint is absent.
                store = reopen_bound(tmp.path(), store.workspace_id(), authority.clone());
                let mut selected = SelectedRead::new(&store).unwrap().unwrap();
                while matches!(selected.step().unwrap(), Step::More) {}
                for original in &originals {
                    assert!(
                        contains_original(&store.root.join("retained"), original),
                        "suppressed input lost durable retained custody"
                    );
                }
                assert_eq!(
                    outcome.publication,
                    Publication::Suppress,
                    "{name} input {index} is a verified incomplete pair, not an opaque original"
                );
            } else {
                let Publication::Activate(path) = outcome.publication else {
                    panic!("complete {name} must activate one coherent basis")
                };
                let mut cursor = PublicationCursor::open(&path).unwrap();
                let mut delivered = Vec::new();
                loop {
                    match cursor.step(crate::persistence::output_budget()).unwrap() {
                        Step::More => {}
                        Step::Done(None) => break,
                        Step::Done(Some(mut batch)) => {
                            while let Some(frame) = batch.front() {
                                delivered.push(crate::transport::encode(frame).unwrap());
                                batch.pop_written();
                            }
                        }
                    }
                }
                originals.sort();
                delivered.sort();
                assert_eq!(
                    delivered, originals,
                    "each exact canonical frame must publish once"
                );
            }
        }
    }
}

#[test]
fn serial_checkpoint_refresh_reference_repeats_global_selection_per_frame() {
    let meta = producer_metadata();
    let (_tmp, store) = selected_baseline_store(&meta);
    let mut global_selections = 0;
    let mut global_reads = 0;
    for bytes in producer_frames(&meta, "checkpoint2", 2) {
        let mut build = retain_build(&store, &bytes);
        while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
        if let Some(stats) = build.selection_stats() {
            global_selections += 1;
            global_reads += stats.preparation_reads;
        }
    }

    assert_eq!(global_selections, 4);
    assert_eq!(global_reads, 100);
}

#[test]
fn projected_checkpoint_prefix_classifies_every_input_in_one_selection() {
    let (_tmp, store) = setup();
    let meta = producer_metadata();
    let incoming = producer_frames(&meta, "checkpoint2", 2)
        .iter()
        .map(|bytes| incoming_identity(bytes, store.workspace_id()).unwrap())
        .collect::<Vec<_>>();
    let mut selector = Selection::new_batch(&store, scratch(&store), incoming).unwrap();
    let outcome = finish(|| selector.step(WorkLimit::default()).unwrap());
    let SelectionOutcome::BatchPublished {
        supplemental_recovery,
        publication,
        dispositions,
    } = outcome
    else {
        panic!("coherent checkpoint projection did not finish")
    };
    assert!(supplemental_recovery.is_none());
    assert!(matches!(publication, Publication::Activate(_)));
    assert_eq!(dispositions.len(), 4);
    assert!(
        dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original),
        "a coherent checkpoint projection left an input unrepresented: {dispositions:?}"
    );
    assert!(selector.stats().preparation_reads < 40);
}

#[test]
fn unchanged_all_covered_projected_prefix_suppresses_and_switches_current() {
    let (_tmp, store) = setup();
    let meta = producer_metadata();
    select_all(&store);
    let before = selected_current(&store).unwrap();

    // These older signed shell checkpoints are opaque to raw-Automerge
    // retention. Production CheckpointStage therefore preserves the active
    // checkpoint2 generation and appends each exact baseline input. The
    // unchanged selected checkpoint2 basis proves every appended input Covered.
    let (root, mut stages, incoming) = stage_checkpoints(
        &store,
        producer_frames(&meta, "baseline", 3),
        Some("opaque exact history must remain projectable"),
    );
    let mut selector = projected_selection(&store, &root, incoming, &stages);
    let projected = finish(|| selector.step_projection(WorkLimit::default()).unwrap());
    assert_eq!(projected.publication, Publication::Suppress);
    assert_eq!(projected.reason, BatchProjectionReason::Represented);
    assert!(
        projected
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Covered),
        "unchanged projected inputs were not all proven Covered: {:?}",
        projected.dispositions
    );
    assert_eq!(
        selected_current(&store).unwrap(),
        before,
        "projection switched selected CURRENT before lineage promotion"
    );

    for stage in &mut stages {
        stage.promote().unwrap();
    }
    let committed = finish(|| selector.step(WorkLimit::default()).unwrap());
    let SelectionOutcome::BatchPublished {
        supplemental_recovery,
        publication,
        dispositions,
    } = committed
    else {
        panic!("projected batch did not publish a batch outcome")
    };
    assert_eq!(supplemental_recovery, projected.supplemental_recovery);
    assert_eq!(publication, Publication::Suppress);
    assert_eq!(dispositions, projected.dispositions);
    assert_ne!(
        selected_current(&store).unwrap(),
        before,
        "unchanged projected cut did not durably switch selected CURRENT"
    );
    for stage in &mut stages {
        while matches!(stage.finish_gc(WorkLimit::default()).unwrap(), Step::More) {}
    }
    assert_selected_complete(&store);
}

#[test]
fn successful_old_basis_fallback_keeps_newer_checkpoint_original() {
    rejected_checkpoint_fixture(false);
}

#[test]
fn serial_suppressed_prefix_rejected_cut_requires_coherent_release_or_replay() {
    rejected_checkpoint_fixture(true);
}

fn rejected_checkpoint_fixture(serial: bool) {
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let next_sequence = document_seq(&cp1[1]).saturating_add(1);

    // The prospective checkpoint admits branch A at next_sequence. A signed
    // concurrent branch-B tail at that same sequence is below its floor and
    // therefore blocks the prospective cut, while the old CP1 basis can still
    // append it in sequence. This makes fallback complete rather than return an
    // unresolved outcome.
    let baseline = owner.document("selector.app", "one").unwrap();
    let mut branch_a = automerge::AutoCommit::load(&baseline).unwrap();
    branch_a.set_actor(automerge::ActorId::from(vec![71]));
    branch_a.put(automerge::ROOT, "candidate", "A").unwrap();
    branch_a.commit();
    let candidate = synthetic_signed(
        &key,
        authority.room(),
        collab_types::DocumentMutation::Snapshot(branch_a.save()),
        next_sequence,
        now,
    );
    let NeighbourMsg::AppDocument { envelope, .. } = candidate else {
        panic!()
    };
    owner
        .admit_local_app_document("selector.app", "one", *envelope, now)
        .unwrap();
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();

    let mut branch_b = automerge::AutoCommit::load(&baseline).unwrap();
    branch_b.set_actor(automerge::ActorId::from(vec![72]));
    let baseline_heads = branch_b.get_heads();
    branch_b.put(automerge::ROOT, "fallback", "B").unwrap();
    branch_b.commit();
    let tail = incremental(
        &key,
        &authority,
        changes_since(&mut branch_b, &baseline_heads),
        next_sequence,
        now,
    );

    let (_tmp, store) = bound_store(authority.clone());
    retain_synthetic(&store, &authority, &cp1, true);
    select_all(&store);
    retain(&store, &synthetic_frame(&authority, &tail, false));
    let selected_before = selected_current(&store).unwrap();
    let cp2_frames = cp2
        .iter()
        .map(|message| synthetic_frame(&authority, message, true));

    if serial {
        for (index, bytes) in cp2_frames.enumerate() {
            let outcome = retain_outcome(retain_build(&store, &bytes), one_entry());
            if index == 0 {
                assert_eq!(outcome.publication, Publication::Suppress);
            } else {
                assert!(
                    matches!(outcome.publication, Publication::ReplayRequired(_)),
                    "previously suppressed serial prerequisite requires the complete fallback, got {:?}",
                    outcome.publication
                );
            }
        }
        return;
    }

    let (root, stages, incoming) = stage_checkpoints(
        &store,
        cp2_frames,
        Some("signed checkpoint fixture must be projectable"),
    );
    let mut selector = projected_selection(&store, &root, incoming, &stages);
    let mut saw_provisional = false;
    let mut saw_cleared_fallback = false;
    let outcome = loop {
        let was_fallback = selector.fallback;
        let result = selector.step_projection(one_entry()).unwrap();
        if !selector.fallback
            && selector
                .incoming_dispositions
                .iter()
                .all(|value| *value == Some(IncomingDisposition::Covered))
        {
            saw_provisional = true;
        }
        if !was_fallback && selector.fallback {
            saw_cleared_fallback = selector.incoming_dispositions.iter().all(Option::is_none);
        }
        if let Step::Done(outcome) = result {
            break outcome;
        }
    };
    assert!(
        saw_provisional,
        "prospective basis never classified the prefix Covered"
    );
    assert!(
        saw_cleared_fallback,
        "prospective dispositions survived the switch back to the old basis"
    );
    assert!(
        matches!(selector.phase, Phase::Commit),
        "fixture returned unresolved instead of completing the old-basis fallback"
    );
    assert_eq!(outcome.publication, Publication::Original);
    assert!(
        outcome.supplemental_recovery.is_none(),
        "full rejected batch has no external prefix"
    );
    assert_eq!(outcome.reason, BatchProjectionReason::FinalBasisOmission);
    assert!(
        outcome
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Original),
        "newer checkpoint was suppressed even though the old-basis cut omits it: {:?}",
        outcome.dispositions
    );
    assert_eq!(
        selected_current(&store).unwrap(),
        selected_before,
        "projection switched selected CURRENT before a serial retry"
    );
}

#[test]
fn projected_checkpoint_prefix_exposes_the_route_count_cutoff() {
    // The relay caps a checkpoint prefix at a fixed frame count; selection
    // treats every prefix size alike, so a small prefix exposes the same
    // cutoff at a fraction of the signing and retention work.
    const PREFIX: usize = 4;
    fn outcome(document_count: usize) -> BatchSelectionOutcome {
        let key =
            ed25519_dalek::SigningKey::from_bytes(&[u8::try_from(document_count).unwrap(); 32]);
        let cert = synthetic_cert(41, &key, format!("{document_count} route prefix fixture"));
        let now = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(3000);
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let documents = (0..document_count)
            .map(|index| {
                let mut document = automerge::AutoCommit::new();
                document
                    .put(automerge::ROOT, "index", index as u64)
                    .unwrap();
                tilo_peer_state::SeedDocument {
                    app_id: "threshold.app".into(),
                    record_id: format!("record-{index:02}"),
                    kind: "test".into(),
                    bytes: document.save(),
                }
            })
            .collect();
        let mut owner = seeded_owner(room, &cert, &key, documents, now);
        let authority = RetentionAuthority::new(room, cert).unwrap();
        let baseline = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
        let next = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
        assert_eq!(baseline.len(), document_count + 1);
        assert_eq!(next.len(), document_count + 1);

        let (_tmp, store) = bound_store(authority.clone());
        retain_synthetic(&store, &authority, &baseline, true);
        select_all(&store);

        // Native publication commits the newly signed room recovery checkpoint
        // before the capped run of per-record checkpoints reaches the relay.
        let room_frame = synthetic_frame(&authority, &next[0], true);
        retain(&store, &room_frame);
        let mut room_selection = Selection::new(
            &store,
            scratch(&store),
            incoming_identity(&room_frame, store.workspace_id()),
        )
        .unwrap();
        let room_publication = loop {
            match room_selection.step(WorkLimit::default()).unwrap() {
                Step::More => {}
                Step::Done(SelectionOutcome::Published(publication)) => break publication,
                Step::Done(other) => panic!("unexpected room checkpoint result: {other:?}"),
            }
        };
        assert_eq!(room_publication, Publication::Suppress);

        let (root, stages, incoming) = stage_checkpoints(
            &store,
            next.iter()
                .skip(1)
                .take(PREFIX)
                .map(|message| synthetic_frame(&authority, message, true)),
            None,
        );
        let mut selector = projected_selection(&store, &root, incoming, &stages);
        let outcome = finish(|| selector.step_projection(WorkLimit::default()).unwrap());
        if document_count > PREFIX {
            assert_eq!(
                selector.snapshot_verifications.1, 0,
                "a proved newer Original needs no repeated signature proof against the older fallback room"
            );
        }
        outcome
    }

    let complete = outcome(PREFIX);
    assert_eq!(complete.reason, BatchProjectionReason::Represented);
    assert!(
        complete
            .dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original)
    );
    for document_count in [PREFIX + 1, PREFIX + 2] {
        let truncated = outcome(document_count);
        assert_eq!(
            truncated.reason,
            BatchProjectionReason::FinalBasisOmission,
            "{document_count} records unexpectedly fit in the {PREFIX}-frame projection"
        );
        assert!(
            truncated
                .dispositions
                .iter()
                .all(|value| *value == IncomingDisposition::Original),
            "{document_count} record fallback was not the observed all-Original cut: {:?}",
            truncated.dispositions
        );
    }
}

#[test]
fn exact_history_checkpoint_prefix_stays_inactive_before_projected_selection() {
    fn lineage_currents(store: &WorkspaceStore) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
        fs::read_dir(store.root.join("retained/v2"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name(),
                    fs::read(entry.path().join("CURRENT")).unwrap(),
                )
            })
            .collect()
    }
    let meta = producer_metadata();
    let (_tmp, store) = selected_baseline_store(&meta);
    let selected_before = fs::read(store.root.join(DIRECTORY).join("CURRENT")).unwrap();
    let before = lineage_currents(&store);

    let (_root, stages, incoming) = stage_checkpoints(
        &store,
        producer_frames(&meta, "checkpoint2", 2),
        Some("exact history checkpoint must be projectable"),
    );
    assert_eq!(incoming.len(), stages.len());
    assert_eq!(
        lineage_currents(&store),
        before,
        "projection switched a lineage CURRENT"
    );
    assert_eq!(
        fs::read(store.root.join(DIRECTORY).join("CURRENT")).unwrap(),
        selected_before,
        "projection switched selected CURRENT"
    );
}

#[test]
fn atomic_checkpoint_prefix_commit_matches_serial_replay() {
    let meta = producer_metadata();
    let (_serial_tmp, serial) = selected_baseline_store(&meta);
    for bytes in producer_frames(&meta, "checkpoint2", 2) {
        retain_outcome(retain_build(&serial, &bytes), WorkLimit::default());
    }
    let serial_replay = replay_frames(&serial);

    let (_batch_tmp, batch) = selected_baseline_store(&meta);
    let (root, mut stages, incoming) = stage_checkpoints(
        &batch,
        producer_frames(&meta, "checkpoint2", 2),
        Some("exact history checkpoint must be projectable"),
    );
    for (index, stage) in stages.iter_mut().enumerate() {
        stage.promote().unwrap();
        let mut selector = Selection::new(
            &batch,
            root.join(format!("serial-{index:02}")),
            Some(incoming[index]),
        )
        .unwrap();
        finish(|| selector.step(WorkLimit::default()).unwrap());
        while matches!(stage.finish_gc(WorkLimit::default()).unwrap(), Step::More) {}
    }
    assert_eq!(replay_frames(&batch), serial_replay);
}

#[test]
fn promoted_prefix_repairs_on_reopen_and_suffix_republish_completes_the_cut() {
    let meta = producer_metadata();
    let prefix = producer_frames(&meta, "checkpoint2", 2)[..2].to_vec();
    let (_expected_tmp, expected) = selected_baseline_store(&meta);
    for bytes in &prefix {
        retain_outcome(retain_build(&expected, bytes), WorkLimit::default());
    }
    let complete = replay_frames(&expected);

    let (recovery_tmp, recovery) = selected_baseline_store(&meta);
    let id = recovery.workspace_id;
    let channels = prefix
        .iter()
        .map(|bytes| checkpoint_channel(&recovery, bytes))
        .collect::<Vec<_>>();
    let recovery_root = recovery.root.clone();
    let lineage = |channel: [u8; 32]| {
        recovery_root.join("retained/v2").join(format!(
            "{}-{}",
            hex::encode(
                serde_json::from_value::<tilo_user_identity::DeviceCert>(meta["authority"].clone())
                    .unwrap()
                    .device_public_key
            ),
            hex::encode(channel)
        ))
    };
    let before = channels
        .iter()
        .map(|channel| retained::current(&lineage(*channel)).unwrap().unwrap())
        .collect::<Vec<_>>();
    let (_root, mut staged, _incoming) = stage_checkpoints(&recovery, prefix.clone(), None);
    staged[0].promote().unwrap();
    drop(staged);
    drop(recovery);

    let reopened = reopen_bound(recovery_tmp.path(), id, producer_authority(&meta));
    let repaired = replay_frames(&reopened);
    assert!(
        !repaired.is_empty(),
        "promoted prefix repairs into a replayable cut"
    );
    assert_ne!(
        retained::current(&lineage(channels[0])).unwrap().unwrap(),
        before[0]
    );
    assert_eq!(
        retained::current(&lineage(channels[1])).unwrap().unwrap(),
        before[1],
        "unpromoted suffix must not switch its lineage CURRENT"
    );

    retain_outcome(retain_build(&reopened, &prefix[1]), WorkLimit::default());
    assert_eq!(
        replay_frames(&reopened),
        complete,
        "current checkpoint republish is accepted or semantically suppressed after repair"
    );
}

// ---- selected generation durability ---------------------------------------

#[test]
fn selected_switch_failure_keeps_previous_durable_basis_and_retires_no_input() {
    let (_tmp, store) = setup();
    let before = selected_current(&store);
    let mut selector = selection(&store);
    while !matches!(selector.phase, Phase::Commit) {
        assert!(matches!(selector.step(one_entry()).unwrap(), Step::More));
    }
    let moved = selector.staged.with_extension("unavailable");
    fs::rename(&selector.staged, &moved).unwrap();
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "required directory sync fails before selector switch"
    );
    assert_eq!(selected_current(&store), before);
    let mut plan = File::open(moved.join("gc.manifest")).unwrap();
    let mut count = 0;
    while let Some(meta) = read_meta(&mut plan).unwrap() {
        assert!(store.root.join(meta.source).exists());
        count += 1;
    }
    assert!(count > 0);
    fs::rename(moved, &selector.staged).unwrap();
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_ne!(selected_current(&store), before);
}

#[test]
fn durable_retirement_plan_preserves_reused_path_and_selected_references_survive_gc() {
    let (_tmp, store) = setup();
    let mut selector = selection(&store);
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let mut plan = File::open(selector.staged.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let path = store.root.join(target.source);
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &bytes).unwrap();
    while matches!(selector.step(one_entry()).unwrap(), Step::More) {}
    assert_eq!(
        fs::read(path).unwrap(),
        bytes,
        "new inode at a retired name is not the captured target"
    );
    assert_selected_complete(&store);
}

#[test]
fn shell_selection_spills_history_metadata_and_preserves_unknown_files() {
    let (_tmp, store) = setup();
    let raw = WorkspaceStore::open(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        true,
    )
    .unwrap();
    let bytes = crate::transport::encode(&Envelope::Delta {
        workspace_id: store.workspace_id,
        author_device_public_key: [77; 32],
        authored_at_ns: 1,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [88; 32],
            body: vec![9; 64],
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    raw.retain_delta(&[88; 32], 1, &bytes).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([77; 32]),
        hex::encode([88; 32])
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    for i in 2..602 {
        fs::write(active.join(format!("{i:016x}.delta")), &bytes).unwrap();
    }
    let mut selector = selection(&store);
    let mut steps = 0;
    loop {
        let result = selector.step(one_entry()).unwrap();
        assert!(selector.stats().last_step_work <= 1);
        steps += 1;
        if matches!(result, Step::Done(_)) {
            break;
        }
    }
    assert!(steps > 600);
    assert!(selector.stats().peak_metadata_entries <= 128);
    assert!(selector.stats().peak_metadata_bytes <= 512 * 1024);
    assert_eq!(
        fs::read_dir(active).unwrap().count(),
        601,
        "opaque foreign history has no retirement proof"
    );
}

#[test]
fn retirement_pin_survives_failed_source_barrier_on_retry_and_reopen() {
    let (_tmp, store) = setup();
    let mut selector = selection(&store);
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let generation = selector.staged.clone();
    let mut plan = File::open(generation.join("gc.manifest")).unwrap();
    let target = read_meta(&mut plan).unwrap().unwrap();
    let source = store.root.join(target.source);
    let pin = generation.join(format!("gc-{:016x}.delta", target.pin));
    let fault = crate::store::test_barrier::fail(source.parent().unwrap(), "sync-dir");
    assert!(selector.step(WorkLimit::default()).is_err());
    assert!(
        !source.exists(),
        "injected barrier failure follows the real source unlink"
    );
    assert!(
        pin.exists(),
        "failed source barrier retains its durable witness"
    );
    assert!(
        selector.step(WorkLimit::default()).is_err(),
        "NotFound retry still requires source barrier"
    );
    assert!(
        pin.exists(),
        "retry must not retire witness without source barrier"
    );
    drop(selector);
    let reopened_store = reopen_bound(
        store.root.parent().unwrap().parent().unwrap(),
        store.workspace_id,
        store.shell_authority().unwrap().clone(),
    );
    let mut reopened = SelectedRead::new(&reopened_store).unwrap().unwrap();
    loop {
        match reopened.step() {
            Ok(Step::More) => (),
            Ok(Step::Done(())) => panic!("reopen bypassed failed source barrier"),
            Err(_) => break,
        }
    }
    assert!(
        pin.exists(),
        "reopen must retain witness while source barrier fails"
    );
    drop(fault);
    while matches!(reopened.step().unwrap(), Step::More) {}
    assert!(!pin.exists());
    assert!(!source.exists());
    assert!(reopened.basis.complete());
}

#[test]
fn retirement_confirms_immutable_selection_once_for_the_whole_plan() {
    let (_tmp, store) = setup();
    let mut selector = selection(&store);
    while !matches!(selector.phase, Phase::Gc) {
        selector.step(WorkLimit::default()).unwrap();
    }
    let generation = selector.staged.clone();
    let mut gc = SelectedGc::new(store.root.clone(), generation.clone()).unwrap();
    let selected_dir = store.root.join(DIRECTORY);
    let fault = crate::store::test_barrier::fail(&selected_dir, "sync-dir");
    assert!(
        gc.step().is_err(),
        "the first retirement requires a durable selection"
    );
    drop(fault);
    assert!(matches!(gc.step().unwrap(), Step::More));

    // The owner serializes this immutable plan. Further records must not force
    // the already-confirmed CURRENT selection to disk again for every unlink.
    // Source/pin directory barriers still run for each actual retirement.
    let fault = crate::store::test_barrier::fail(&selected_dir, "sync-dir");
    while matches!(
        gc.step().expect("selection barrier repeated per record"),
        Step::More
    ) {}
    drop(fault);
    let mut plan = File::open(generation.join("gc.manifest")).unwrap();
    let mut retired = 0;
    while let Some(meta) = read_meta(&mut plan).unwrap() {
        assert!(
            !generation
                .join(format!("gc-{:016x}.delta", meta.pin))
                .exists()
        );
        retired += 1;
    }
    assert!(retired > 1, "exercise a multi-record plan");
    assert_selected_complete(&store);
}

/// Drives a fresh selection to `Commit`, then fails `barrier` on its staged
/// generation and checks that no source or pin was retired.
fn staged_commit_barrier_keeps_every_input(barrier: &'static str) -> (Selection, usize) {
    let (tmp, store) = setup();
    let mut selector = selection(&store);
    let before = selected_current(&store);
    while !matches!(selector.phase, Phase::Commit) {
        selector.step(WorkLimit::default()).unwrap();
    }
    assert!(
        selector.ordinal > 3,
        "exercise multiple staged records and retirement pins"
    );
    let fault = crate::store::test_barrier::fail(&selector.staged, barrier);
    assert!(selector.step(WorkLimit::default()).is_err());
    assert_eq!(selected_current(&store), before);
    let mut pins = File::open(selector.staged.join("gc.manifest")).unwrap();
    let mut retired = 0_usize;
    while let Some(meta) = read_meta(&mut pins).unwrap() {
        assert!(
            store.root.join(meta.source).exists(),
            "failed {barrier} must not retire source"
        );
        assert!(
            selector
                .staged
                .join(format!("gc-{:016x}.delta", meta.pin))
                .exists()
        );
        retired = retired.saturating_add(1);
    }
    drop(fault);
    while matches!(selector.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert_selected_complete(&store);
    drop(tmp);
    (selector, retired)
}

#[test]
fn selected_generation_batches_full_drive_flushes_before_publication() {
    let (selector, _) = staged_commit_barrier_keeps_every_input("selection-full-flush");
    assert_eq!(
        selector.stats().staging_full_flushes,
        3,
        "drive-wide flush count must depend on commit boundaries, not retained history size"
    );
}

#[test]
fn selected_generation_defers_hard_link_durability_to_the_commit_barrier() {
    let (_, retired) = staged_commit_barrier_keeps_every_input("sync-dir");
    assert!(
        retired > 1,
        "exercise multiple retirements after the staged barrier"
    );
}

#[test]
fn recovery_cursor_rejects_other_workspace_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, true).unwrap();
    let generation = store.root().join("shell-selected-v1/test");
    fs::create_dir_all(&generation).unwrap();
    let frame = Envelope::Delta {
        workspace_id: 8,
        author_device_public_key: [1; 32],
        authored_at_ns: 1,
        delta: vec![255],
        signature: [0; 64],
    };
    let bytes = crate::transport::encode(&frame).unwrap();
    let source = generation.join("0.delta");
    fs::write(&source, &bytes).unwrap();
    let mut manifest = File::create(generation.join("basis.manifest")).unwrap();
    write_meta(
        &mut manifest,
        &Meta {
            key: vec!["extra".into()],
            source: source.strip_prefix(store.root()).unwrap().into(),
            pin: 0,
            len: bytes.len() as u64,
            pending: false,
        },
    )
    .unwrap();
    drop(manifest);
    let mut cursor = PublicationCursor::open_recovery(&generation).unwrap();
    assert!(
        cursor.step(&ReplayBudget::new(1024 * 1024)).is_err(),
        "recovery emitted an envelope from another workspace"
    );
}

// ---- signed synthetic selection -------------------------------------------

#[test]
fn subscription_replay_falls_back_to_current_when_current_tail_is_a_prerequisite() {
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let seq = document_seq(&cp1[1]);
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let baseline_heads = branch.get_heads();
    branch.put(automerge::ROOT, "first", "T1").unwrap();
    branch.commit();
    let t1_heads = branch.get_heads();
    let t1_bytes = changes_since(&mut branch, &baseline_heads);
    let t1 = incremental(&key, &authority, t1_bytes, seq + 1, now);
    branch.put(automerge::ROOT, "second", "T2").unwrap();
    branch.commit();
    let t2_bytes = changes_since(&mut branch, &t1_heads);
    let t2 = incremental(&key, &authority, t2_bytes, seq + 2, now);
    let (tmp, store) = bound_store(authority.clone());
    let workspace_id = store.workspace_id;
    retain_synthetic(&store, &authority, &cp1, true);
    let t1_frame = synthetic_frame(&authority, &t1, false);
    retain(&store, &t1_frame);
    let prior_frames = replay_frames(&store);
    assert!(
        prior_frames
            .iter()
            .any(|frame| { crate::transport::encode(frame).unwrap() == t1_frame })
    );
    let selected_before = selected_current(&store).unwrap();
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
        retain(&reopened, frame);
    }
    retain(&reopened, &t2_frame);
    reopened.bind_shell_authority(authority).unwrap();
    let pending = crate::transport::encode(&Envelope::Presence {
        workspace_id,
        payload: b"pending subscription frame".to_vec(),
    })
    .unwrap();
    reopened.enqueue_pending(&[2; 32], 1, &pending).unwrap();
    let mut snapshot = capture(&reopened, WorkLimit::default());
    let replayed = encoded(&drain_replay(
        &reopened,
        &mut snapshot,
        &ReplayBudget::new(256 * 1024 * 1024),
    ));
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
    assert_eq!(selected_current(&reopened).unwrap(), selected_before);
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
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let baseline = owner.clone();
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let old_heads = branch.get_heads();
    branch.put(automerge::ROOT, "uncovered", "T2").unwrap();
    branch.commit();
    let tail_bytes = changes_since(&mut branch, &old_heads);
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
    let tail = incremental(&key, &authority, tail_bytes, document_seq(&cp2[1]), now);
    let (_tmp, store) = bound_store(authority.clone());
    retain_synthetic(&store, &authority, &cp1, true);
    retain(&store, &synthetic_frame(&authority, &tail, false));
    retain_synthetic(&store, &authority, &cp2, true);
    let mut receiver = baseline.clone();
    for frame in replay_frames(&store) {
        let result = receive(&mut receiver, frame, false, now).unwrap();
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
    retain_synthetic(&store, &authority, &cp4, true);
    let frames = replay_frames(&store);
    assert_eq!(
        frames.len(),
        2,
        "CP4 retires the proven contained tail and obsolete exact pairs"
    );
    let mut receiver = baseline;
    for frame in frames {
        receive(&mut receiver, frame, false, now).unwrap();
    }
    assert_eq!(
        receiver.document_heads("selector.app", "one").unwrap(),
        owner.document_heads("selector.app", "one").unwrap()
    );
}

#[test]
fn shell_selection_preserves_invalid_foreign_and_unknown_records() {
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp1 = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let (_tmp, store) = bound_store(authority.clone());
    retain_synthetic(&store, &authority, &cp1, true);
    let mut invalid = cp1[1].clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut invalid else {
        panic!()
    };
    envelope.seq += 100;
    let invalid = synthetic_frame(&authority, &invalid, true);
    retain(&store, &invalid);
    let foreign_key = ed25519_dalek::SigningKey::from_bytes(&[37; 32]);
    let foreign = synthetic_signed(
        &foreign_key,
        authority.room(),
        collab_types::DocumentMutation::Snapshot(owner.document("selector.app", "one").unwrap()),
        1,
        now,
    );
    let foreign = synthetic_frame(&authority, &foreign, true);
    retain(&store, &foreign);
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
    retain(&store, &unknown);
    let cp2 = owner.durable_bootstrap_messages(&key, now, 2).unwrap();
    retain_synthetic(&store, &authority, &cp2, true);
    let frames = encoded(&replay_frames(&store));
    for original in [invalid, foreign, unknown] {
        assert!(
            frames.contains(&original),
            "unproven or foreign bytes must remain in replay"
        );
    }
}

#[test]
fn initial_shell_basis_replays_independent_reversed_author_tails_in_exact_sequence_order() {
    let (key, authority, mut owner, now) = synthetic_owner();
    let cp = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
    let first_seq = document_seq(&cp[1]) + 1;
    let mut tails = Vec::new();
    let mut expected =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    for i in 0..2u8 {
        let mut branch =
            automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
        branch.set_actor(automerge::ActorId::from(vec![i + 90]));
        let heads = branch.get_heads();
        branch
            .put(automerge::ROOT, format!("tail-{i}"), u64::from(i))
            .unwrap();
        branch.commit();
        let bytes = changes_since(&mut branch, &heads);
        tilo_automerge::ApplyIncoming::load_incoming(&mut expected, &bytes).unwrap();
        tails.push(incremental(
            &key,
            &authority,
            bytes,
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
    retain_synthetic(&store, &authority, &cp, true);
    for msg in tails.iter().rev() {
        retain(&store, &synthetic_frame(&authority, msg, false));
    }
    store.bind_shell_authority(authority).unwrap();
    let frames = replay_frames(&store);
    assert_eq!(
        frames.len(),
        4,
        "first selection contains complete snapshot plus both uncovered tails"
    );
    for frame in frames {
        let outcome = receive(&mut owner, frame, false, now).unwrap();
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
    use automerge::ReadDoc;
    let (key, authority, owner, now) = synthetic_owner();
    let cp = owner
        .clone()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    let first_seq = document_seq(&cp[1]) + 1;
    let mut branch =
        automerge::AutoCommit::load(&owner.document("selector.app", "one").unwrap()).unwrap();
    let mut tails = Vec::new();
    for (offset, (field, value)) in [("first", "T1"), ("second", "T2")].into_iter().enumerate() {
        let heads = branch.get_heads();
        branch.put(automerge::ROOT, field, value).unwrap();
        branch.commit();
        let bytes = changes_since(&mut branch, &heads);
        tails.push(incremental(
            &key,
            &authority,
            bytes,
            first_seq + u64::try_from(offset).unwrap(),
            now,
        ));
    }

    let tmp = tempfile::tempdir().unwrap();
    let workspace_id = crate::room_workspace_id(authority.room()).unwrap();
    let store = WorkspaceStore::open(tmp.path(), workspace_id, true).unwrap();
    retain_synthetic(&store, &authority, &cp, true);
    for msg in tails.iter().rev() {
        retain(&store, &synthetic_frame(&authority, msg, false));
    }
    drop(store);

    let reopened = reopen_bound(tmp.path(), workspace_id, authority);
    let frames = replay_frames(&reopened);
    assert_eq!(frames.len(), 4);
    let mut receiver = owner;
    for frame in frames {
        let outcome = receive(&mut receiver, frame, false, now).unwrap();
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
    for (field, value) in [("first", "T1"), ("second", "T2")] {
        assert_eq!(
            replayed
                .get(automerge::ROOT, field)
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some(value)
        );
    }
}

#[test]
fn initial_shell_conflict_or_t2_without_t1_remains_unselected() {
    for conflicting in [false, true] {
        let (key, authority, mut owner, now) = synthetic_owner();
        let cp = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
        let seq = document_seq(&cp[1]) + 1;
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
                .put(automerge::ROOT, format!("case-{i}"), u64::from(i))
                .unwrap();
            branch.commit();
            let bytes = changes_since(&mut branch, &heads);
            tails.push(incremental(
                &key,
                &authority,
                bytes,
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
            retain(&store, bytes);
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
        assert!(selected_current(&store).is_none());
        let stored = store.load_shared().unwrap();
        for original in originals {
            assert!(stored.contains(&original));
        }
    }
}
