use super::*;
use crate::peer_delivery_cursor::CursorIdentity;
use crate::peer_journal::Journal;
use crate::peer_storage_batch::PeerStorageBatches;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use tilo_relay::delivery_v3::{
    commit::CommitRef,
    cursor::{CursorIssue, CursorToken},
    state::canonical_item_digest,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use ui::portable_storage::{PendingWrite, PortableStorage, StorageKey};
use web_time::{Duration, UNIX_EPOCH};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn time() -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(1_000_000_000_000)
}
fn cert(root: &SigningKey, key: &SigningKey) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "native cut fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}
/// The native owner: a governor that publishes a room and one document.
struct Native {
    key: SigningKey,
    governor: DeviceCert,
    state: StatePeer,
}
impl Native {
    fn new(seed: u8) -> Self {
        let root = SigningKey::from_bytes(&[seed; 32]);
        let key = SigningKey::from_bytes(&[seed + 1; 32]);
        let governor = cert(&root, &key);
        let roster = vec![governor.clone()];
        let mut state = StatePeer::new(
            ROOM,
            governor.clone(),
            roster.clone(),
            RevocationList::new(),
            time(),
        )
        .unwrap();
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", "native v1").unwrap();
        doc.commit();
        state
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: qos_peer_state::seed_room(ROOM, &governor, &roster).unwrap(),
                    documents: vec![qos_peer_state::SeedDocument {
                        app_id: APP.into(),
                        record_id: RECORD.into(),
                        kind: "test".into(),
                        bytes: doc.save(),
                    }],
                },
                &key,
                time(),
            )
            .unwrap();
        Self {
            key,
            governor,
            state,
        }
    }
    /// The complete cut a fresh consumer receives: room governance plus documents.
    fn baseline(&mut self) -> Vec<(EventKind, Vec<u8>)> {
        self.state
            .bootstrap_messages(&self.key, time())
            .unwrap()
            .iter()
            .map(|message| {
                let kind = match message {
                    NeighbourMsg::SyncResp { .. } => EventKind::Governance,
                    _ => EventKind::State,
                };
                (kind, postcard::to_stdvec(message).unwrap())
            })
            .collect()
    }
    /// One later committed suffix: an incremental document publication.
    fn edit(&mut self, value: &str) -> Vec<(EventKind, Vec<u8>)> {
        let mut doc =
            automerge::AutoCommit::load(&self.state.document(APP, RECORD).unwrap()).unwrap();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "value", value).unwrap();
        doc.commit();
        let message = self
            .state
            .publish_incremental(APP, RECORD, doc.save_after(&before), &self.key, time())
            .unwrap();
        vec![(EventKind::State, postcard::to_stdvec(&message).unwrap())]
    }
    fn consumer(&self) -> StatePeer {
        StatePeer::new(
            ROOM,
            self.governor.clone(),
            vec![self.governor.clone()],
            RevocationList::new(),
            time(),
        )
        .unwrap()
    }
}
fn budget() -> IncomingBudget {
    IncomingBudget::new(64 * 1024 * 1024).unwrap()
}
fn admit(
    state: &StatePeer,
    incoming: &IncomingBudget,
    events: &[(EventKind, Vec<u8>)],
) -> Result<StatePeer, String> {
    let mut cut = CutAdmission::begin(state, incoming.clone(), time())?;
    for (ordinal, (kind, bytes)) in events.iter().enumerate() {
        cut.push(
            CompleteEvent {
                ordinal: ordinal as u32,
                kind: *kind,
                digest: canonical_item_digest(bytes),
                bytes: incoming.copy(bytes)?,
            },
            time(),
        )?;
    }
    cut.finish_complete(time())
}
fn value(state: &StatePeer) -> Option<String> {
    let doc = automerge::AutoCommit::load(&state.document(APP, RECORD)?).ok()?;
    use automerge::ReadDoc;
    doc.get(automerge::ROOT, "value")
        .ok()
        .flatten()
        .map(|(value, _)| value.to_str().unwrap_or_default().to_owned())
}

#[test]
fn fresh_owner_admits_complete_native_cut_without_mutating_the_live_owner() {
    let mut native = Native::new(1);
    let incoming = budget();
    let credit = incoming.available();
    let owner = native.consumer();
    let before = owner.durable().unwrap();
    let candidate = admit(&owner, &incoming, &native.baseline()).unwrap();
    assert_eq!(value(&candidate).as_deref(), Some("native v1"));
    assert_eq!(
        owner.durable().unwrap(),
        before,
        "owner changes only on install"
    );
    assert!(owner.document(APP, RECORD).is_none());
    // The suffix applies to the admitted candidate through the same checked path.
    let next = admit(&candidate, &incoming, &native.edit("native v2")).unwrap();
    assert_eq!(value(&next).as_deref(), Some("native v2"));
    assert_eq!(incoming.available(), credit, "no credit retained");
}

#[test]
fn duplicate_or_overlapping_replay_after_lost_ack_is_idempotent() {
    let mut native = Native::new(1);
    let incoming = budget();
    let baseline = native.baseline();
    let suffix = native.edit("native v2");
    let once = admit(&native.consumer(), &incoming, &baseline).unwrap();
    let once = admit(&once, &incoming, &suffix).unwrap();
    // A lost ACK or aborted commit replays from the older committed cursor.
    let twice = admit(&once, &incoming, &baseline).unwrap();
    let twice = admit(&twice, &incoming, &suffix).unwrap();
    assert_eq!(value(&twice).as_deref(), Some("native v2"));
    assert_eq!(twice.rows(), once.rows());
    assert_eq!(
        twice.document_heads(APP, RECORD),
        once.document_heads(APP, RECORD)
    );
}

#[test]
fn refused_item_consumes_the_candidate_and_leaves_the_owner_untouched() {
    let mut native = Native::new(1);
    let incoming = budget();
    let credit = incoming.available();
    let owner = native.consumer();
    let before = owner.durable().unwrap();
    let baseline = native.baseline();
    // Corrupt signed document bytes.
    let mut tampered = baseline.clone();
    let (_, bytes) = tampered
        .iter_mut()
        .find(|(kind, _)| *kind == EventKind::State)
        .unwrap();
    let last = bytes.len() - 8;
    bytes[last] ^= 0x55;
    assert!(admit(&owner, &incoming, &tampered).is_err());
    // A family label cannot relabel governance as document state.
    let mut relabelled = baseline.clone();
    relabelled[0].0 = EventKind::State;
    assert!(
        admit(&owner, &incoming, &relabelled)
            .err()
            .is_some_and(|error| error.contains("family"))
    );
    // Another governor's publications are not this workspace's authority.
    let mut other = Native::new(9);
    assert!(admit(&owner, &incoming, &other.baseline()).is_err());
    // A used cut cannot continue after refusal.
    let mut cut = CutAdmission::begin(&owner, incoming.clone(), time()).unwrap();
    let bad = CompleteEvent {
        ordinal: 0,
        kind: EventKind::Governance,
        digest: [0; 32],
        bytes: incoming.copy(&[0xff; 4]).unwrap(),
    };
    assert!(cut.push(bad, time()).is_err());
    assert!(cut.finish(time()).is_err());
    assert_eq!(owner.durable().unwrap(), before);
    assert_eq!(incoming.available(), credit);
    assert!(admit(&owner, &incoming, &baseline).is_ok());
}

fn identity() -> CursorIdentity {
    CursorIdentity {
        workspace: 9,
        room: ROOM.into(),
        workspace_root: [1; 32],
        device: [2; 32],
        device_root: [3; 32],
        host: host().verifying_key().to_bytes(),
        host_root: [4; 32],
    }
}
fn host() -> SigningKey {
    SigningKey::from_bytes(&[10; 32])
}
fn cursor(sequence: u64, digest: u8, generation: u8) -> StoredDeliveryCursor {
    let identity = identity();
    let committed = CommitRef {
        sequence,
        digest: [digest; 32],
    };
    let token = CursorToken::issue(
        CursorIssue {
            room: identity.room.clone(),
            workspace: identity.workspace,
            workspace_root: identity.workspace_root,
            host_device: identity.host,
            host_root: identity.host_root,
            consumer_device: identity.device,
            consumer_root: identity.device_root,
            consumer: [5; 16],
            store_generation: [generation; 16],
            authorization_view: [7; 32],
            committed,
        },
        &host(),
    )
    .unwrap();
    StoredDeliveryCursor {
        version: 1,
        identity,
        consumer: vec![5; 16],
        authorization_view: vec![7; 32],
        store_generation: vec![generation; 16],
        token: token.as_bytes().to_vec(),
    }
}

#[test]
fn cursor_continuity_never_skips_back_and_only_snapshots_reset_scope() {
    let one = cursor(1, 1, 6);
    let two = cursor(2, 2, 6);
    assert_eq!(cursor_step(None, &two, false).unwrap(), CursorStep::Suffix);
    assert_eq!(cursor_step(None, &two, true).unwrap(), CursorStep::Reset);
    assert_eq!(
        cursor_step(Some(&one), &two, false).unwrap(),
        CursorStep::Suffix
    );
    // An exact duplicate boundary is an idempotent no-op step.
    assert_eq!(
        cursor_step(Some(&two), &two, false).unwrap(),
        CursorStep::Suffix
    );
    assert!(cursor_step(Some(&two), &one, false).is_err(), "regression");
    assert!(
        cursor_step(Some(&two), &one, true).is_err(),
        "stale snapshot"
    );
    assert!(
        cursor_step(Some(&two), &cursor(2, 9, 6), false).is_err(),
        "fork"
    );
    let reset = cursor(1, 1, 8);
    assert!(cursor_step(Some(&two), &reset, false).is_err());
    assert_eq!(
        cursor_step(Some(&two), &reset, true).unwrap(),
        CursorStep::Reset
    );
    let mut forged = two.clone();
    forged.token[40] ^= 1;
    assert!(cursor_step(Some(&one), &forged, false).is_err());
    let mut foreign = two.clone();
    foreign.identity.workspace = 10;
    assert!(cursor_step(Some(&one), &foreign, true).is_err());
}

#[test]
fn cursor_advance_keeps_descriptor_scope_and_confirm_requires_exact_commit() {
    let storage = JournalStorage::default();
    let next = advance_cursor(&storage, cursor(1, 1, 6)).unwrap();
    assert_eq!(next.format, JournalFormat::V3);
    assert_eq!(next.delivery, Some(cursor(1, 1, 6)));
    let mut foreign = cursor(2, 2, 6);
    foreign.identity.room = "other".into();
    assert!(advance_cursor(&next, foreign).is_err());
    assert!(confirm_commit(&cursor(2, 2, 6), None).is_err());
    assert!(confirm_commit(&cursor(2, 2, 6), Some(&cursor(1, 1, 6))).is_err());
    confirm_commit(&cursor(2, 2, 6), Some(&cursor(2, 2, 6))).unwrap();
}

fn record() -> StorageKey {
    StorageKey::Record {
        package: APP.into(),
        workspace: ROOM.into(),
        record: RECORD.into(),
    }
}
fn journal_key() -> StorageKey {
    StorageKey::Admission {
        workspace: ROOM.into(),
    }
}
/// Mirror peer.rs install_admitted: record bytes plus one Admission value that
/// carries state, outgoing work and the cut cursor.
fn stage(
    cache: &PortableStorage,
    state: &StatePeer,
    outbox: &[NeighbourMsg],
    storage: &JournalStorage,
) {
    if let Some(bytes) = state.document(APP, RECORD) {
        cache.put(record(), bytes, Some(vec![1; 32])).unwrap();
    }
    let mut journal = Journal::from_state(state, outbox.to_vec()).unwrap();
    journal.storage = storage.clone();
    cache
        .put(journal_key(), journal.encode().unwrap(), None)
        .unwrap();
}
fn capture(
    batches: &mut PeerStorageBatches,
    cache: &PortableStorage,
) -> (String, Vec<PendingWrite>) {
    batches
        .capture(
            cache,
            &[record()],
            1,
            |v| v,
            |id, writes| Ok((id.to_owned(), writes.to_vec())),
        )
        .unwrap()
}
fn committed(cache: &PortableStorage, native: &Native) -> (StatePeer, Journal) {
    let bytes = cache
        .status(&journal_key())
        .unwrap()
        .committed
        .unwrap()
        .1
        .bytes;
    reload(&bytes, native)
}
fn reload(bytes: &[u8], native: &Native) -> (StatePeer, Journal) {
    let journal = Journal::decode(bytes).unwrap();
    let state = StatePeer::restore(
        &journal.state,
        ROOM,
        native.governor.clone(),
        vec![native.governor.clone()],
        RevocationList::new(),
        time(),
    )
    .unwrap();
    (state, journal)
}

#[test]
fn state_journal_and_cursor_commit_atomically_across_abort_lost_ack_and_reload() {
    let mut native = Native::new(1);
    let incoming = budget();
    let cache = PortableStorage::new("certified:room", "device").unwrap();
    let mut batches = PeerStorageBatches::new(1);
    batches.bind_lease("lease").unwrap();
    // Pending local work that must survive every outcome below.
    let pending = vec![NeighbourMsg::SyncReq {
        room: "original-signed-operation".into(),
        since: vec![],
        invite_id: None,
    }];

    // First complete cut: fresh startup snapshot.
    let owner = native.consumer();
    let first = admit(&owner, &incoming, &native.baseline()).unwrap();
    let storage = advance_cursor(&JournalStorage::default(), cursor(1, 1, 6)).unwrap();
    stage(&cache, &first, &pending, &storage);
    let (id, _) = capture(&mut batches, &cache);
    batches.settle(&id, "lease", Ok(())).unwrap();
    confirm_commit(&cursor(1, 1, 6), batches.committed_cursor(1)).unwrap();

    // Second cut: a suffix whose transaction aborts (quota).
    let suffix = native.edit("native v2");
    let second = admit(&first, &incoming, &suffix).unwrap();
    let storage = advance_cursor(&storage, cursor(2, 2, 6)).unwrap();
    stage(&cache, &second, &pending, &storage);
    let (id, _) = capture(&mut batches, &cache);
    batches
        .settle(&id, "lease", Err("storage-quota".into()))
        .unwrap();
    // No ACK is permitted and the committed pair stays the older cut.
    assert!(confirm_commit(&cursor(2, 2, 6), batches.committed_cursor(1)).is_err());
    let (durable, journal) = committed(&cache, &native);
    assert_eq!(journal.storage.delivery, Some(cursor(1, 1, 6)));
    assert_eq!(value(&durable).as_deref(), Some("native v1"));
    assert_eq!(journal.outbox, pending);
    assert!(
        cache
            .pending_writes()
            .iter()
            .any(|write| write.key == journal_key()),
        "aborted work stays pending for retry"
    );

    // Reload after the abort: replay from the committed cursor is idempotent
    // and reaches the same candidate without skipping the suffix.
    let reloaded = PortableStorage::new("certified:room", "device").unwrap();
    for key in [journal_key(), record()] {
        let (_, saved) = cache.status(&key).unwrap().committed.unwrap();
        reloaded.hydrate(key, saved.bytes, saved.actor).unwrap();
    }
    let (restored, journal) = committed(&reloaded, &native);
    assert_eq!(journal.storage.delivery, Some(cursor(1, 1, 6)));
    let replayed = admit(&restored, &incoming, &suffix).unwrap();
    assert_eq!(value(&replayed).as_deref(), Some("native v2"));
    assert_eq!(journal.into_unretired_outbox(&restored), pending);

    // The retry commits state, pending journal work and cursor together.
    let (id, _) = capture(&mut batches, &cache);
    batches.settle(&id, "lease", Ok(())).unwrap();
    confirm_commit(&cursor(2, 2, 6), batches.committed_cursor(1)).unwrap();
    let (durable, journal) = committed(&cache, &native);
    assert_eq!(journal.storage.delivery, Some(cursor(2, 2, 6)));
    assert_eq!(value(&durable).as_deref(), Some("native v2"));
    assert_eq!(journal.outbox, pending);
    assert!(cache.status(&record()).unwrap().durable);

    // Lost storage acknowledgement: the transaction outcome is unknown when the
    // page reloads. Either durable outcome is a self-consistent state/cursor pair.
    let third = admit(&second, &incoming, &native.edit("native v3")).unwrap();
    let storage = advance_cursor(&storage, cursor(3, 3, 6)).unwrap();
    stage(&cache, &third, &pending, &storage);
    let (_, writes) = capture(&mut batches, &cache);
    let old = cache
        .status(&journal_key())
        .unwrap()
        .committed
        .unwrap()
        .1
        .bytes;
    let new = writes
        .iter()
        .find(|write| write.key == journal_key())
        .unwrap()
        .value
        .bytes
        .clone();
    for (bytes, expected, text) in [
        (old, cursor(2, 2, 6), "native v2"),
        (new, cursor(3, 3, 6), "native v3"),
    ] {
        let (state, journal) = reload(&bytes, &native);
        assert_eq!(journal.storage.delivery, Some(expected));
        assert_eq!(value(&state).as_deref(), Some(text));
        assert_eq!(journal.outbox, pending);
    }
    // Nothing from the unsettled batch reached the ACK source.
    assert_eq!(batches.committed_cursor(1), Some(&cursor(2, 2, 6)));
}

#[test]
fn abandoned_snapshot_binding_never_blocks_the_retried_commit() {
    let mut native = Native::new(1);
    let incoming = budget();
    let cache = PortableStorage::new("certified:room", "device").unwrap();
    let mut batches = PeerStorageBatches::new(1);
    batches.bind_lease("lease").unwrap();
    let candidate = admit(&native.consumer(), &incoming, &native.baseline()).unwrap();
    let storage = advance_cursor(&JournalStorage::default(), cursor(1, 1, 6)).unwrap();
    let transfer = "0123456789abcdef0123456789abcdef";
    batches.bind_transfer(transfer, cursor(1, 1, 6), 1).unwrap();
    stage(&cache, &candidate, &[], &storage);
    // The staged cut's transaction fails while the delivery session stops.
    let id = batches
        .capture_transfer(
            &cache,
            &[record()],
            &[],
            1,
            |v| v,
            |id, _, tag| {
                assert_eq!(tag, Some(transfer));
                Ok(id.to_owned())
            },
        )
        .unwrap();
    batches.abandon_transfer();
    batches
        .settle(&id, "lease", Err("storage-quota".into()))
        .unwrap();
    assert!(batches.committed_cursor(1).is_none());
    // Retry commits the same state and cursor without the vanished stage.
    let id = batches
        .capture_transfer(
            &cache,
            &[record()],
            &[],
            1,
            |v| v,
            |id, _, tag| {
                assert_eq!(tag, None);
                Ok(id.to_owned())
            },
        )
        .unwrap();
    batches.settle(&id, "lease", Ok(())).unwrap();
    confirm_commit(&cursor(1, 1, 6), batches.committed_cursor(1)).unwrap();
    // A later snapshot can bind again; an uncaptured binding is dropped at once.
    batches.bind_transfer(transfer, cursor(2, 2, 6), 1).unwrap();
    batches.abandon_transfer();
    batches.bind_transfer(transfer, cursor(2, 2, 6), 1).unwrap();
}

// ---- Lane W: a record-only batch inside the publication window.

/// Hydrate a fresh cache from exactly what the last transactions committed.
fn reload_cache(cache: &PortableStorage) -> PortableStorage {
    let reloaded = PortableStorage::new("certified:room", "device").unwrap();
    for key in [journal_key(), record()] {
        let (_, saved) = cache.status(&key).unwrap().committed.unwrap();
        reloaded.hydrate(key, saved.bytes, saved.actor).unwrap();
    }
    reloaded
}
/// The canonical Writer owner commits a keystroke to its record at once; the
/// peer journal is only restaged when the publication window elapses.
fn local_edit(cache: &PortableStorage, text: &str) {
    let mut doc = automerge::AutoCommit::load(&cache.get(&record()).unwrap().bytes).unwrap();
    doc.put(automerge::ROOT, "value", text).unwrap();
    doc.commit();
    cache
        .put_document(record(), &mut doc, Some(vec![1; 32]))
        .unwrap();
}
/// A cursor-bearing journal committed with the admitted record at H0.
fn committed_owner(native: &mut Native) -> (PortableStorage, PeerStorageBatches, JournalStorage) {
    let incoming = budget();
    let cache = PortableStorage::new("certified:room", "device").unwrap();
    let mut batches = PeerStorageBatches::new(1);
    batches.bind_lease("lease").unwrap();
    let state = admit(&native.consumer(), &incoming, &native.baseline()).unwrap();
    let storage = advance_cursor(&JournalStorage::default(), cursor(1, 1, 6)).unwrap();
    stage(&cache, &state, &[], &storage);
    let (id, _) = capture(&mut batches, &cache);
    batches.settle(&id, "lease", Ok(())).unwrap();
    (cache, batches, storage)
}

#[test]
fn record_queued_inside_the_publication_window_waits_for_its_journal() {
    let mut native = Native::new(1);
    let (cache, mut batches, storage) = committed_owner(&mut native);
    let (state, _) = committed(&cache, &native);
    local_edit(&cache, "typed in window");
    // A persist inside the window: backpressure, nothing captured.
    let error = batches
        .capture_transfer(
            &cache,
            &[record()],
            &state.rows(),
            1,
            |v| v,
            |id, _, _| Ok(id.to_owned()),
        )
        .unwrap_err();
    assert!(error.starts_with("storage-pressure:"), "{error}");
    assert!(cache.status(&record()).unwrap().committed.is_some());
    // The publication stages the matching journal; both commit together.
    let mut published = state.clone();
    let mut doc = automerge::AutoCommit::load(&cache.get(&record()).unwrap().bytes).unwrap();
    let before = state.document_heads(APP, RECORD).unwrap();
    let message = published
        .publish_incremental(APP, RECORD, doc.save_after(&before), &native.key, time())
        .unwrap();
    let mut journal = Journal::from_state(&published, vec![message]).unwrap();
    journal.storage = storage;
    cache
        .put(journal_key(), journal.encode().unwrap(), None)
        .unwrap();
    let (id, writes) = batches
        .capture_transfer(
            &cache,
            &[record()],
            &published.rows(),
            1,
            |v| v,
            |id, writes, _| Ok((id.to_owned(), writes.to_vec())),
        )
        .unwrap();
    assert_eq!(writes.len(), 2);
    batches.settle(&id, "lease", Ok(())).unwrap();
    let (durable, _) = committed(&reload_cache(&cache), &native);
    assert_eq!(value(&durable).as_deref(), Some("typed in window"));
}

#[test]
fn torn_record_only_batch_boots_after_repair_with_the_edit_intact() {
    let mut native = Native::new(1);
    let (cache, mut batches, _) = committed_owner(&mut native);
    local_edit(&cache, "typed in window");
    // An older runtime's persist inside the window committed the record
    // alone: IndexedDB now holds record H1 with cursor journal H0.
    let (id, writes) = capture(&mut batches, &cache);
    assert!(writes.iter().all(|write| write.key == record()));
    batches.settle(&id, "lease", Ok(())).unwrap();
    let reloaded = reload_cache(&cache);
    let (restored, journal) = committed(&reloaded, &native);
    let storage = advance_cursor(&journal.storage, cursor(2, 2, 6)).unwrap();
    // Without repair, re-staging H0 wedges every capture (the field symptom).
    {
        let wedged = reload_cache(&cache);
        stage_journal_only(&wedged, &restored, &[], &storage);
        let mut batches = PeerStorageBatches::new(2);
        batches.bind_lease("lease-2").unwrap();
        let error = batches
            .capture(&wedged, &[record()], 1, |v| v, |id, _| Ok(id.to_owned()))
            .unwrap_err();
        assert_eq!(error, "cursor journal does not match exact record basis");
    }
    // Boot repair advances the journal to the verified descendant record.
    let mut state = restored.clone();
    let mut outbox = journal.into_unretired_outbox(&restored);
    let repaired = crate::admitted_records::repair_record_basis(
        &reloaded,
        &mut state,
        &mut outbox,
        &native.key,
        |_| true,
        time(),
    )
    .unwrap();
    assert_eq!(repaired, 1);
    assert_eq!(value(&state).as_deref(), Some("typed in window"));
    assert_eq!(outbox.len(), 1, "the edit is republished, not dropped");
    stage_journal_only(&reloaded, &state, &outbox, &storage);
    let mut batches = PeerStorageBatches::new(2);
    batches.bind_lease("lease-2").unwrap();
    let id = batches
        .capture_transfer(
            &reloaded,
            &[record()],
            &state.rows(),
            1,
            |v| v,
            |id, _, _| Ok(id.to_owned()),
        )
        .unwrap();
    batches.settle(&id, "lease-2", Ok(())).unwrap();
    // The next boot restores the edit from the journal and needs no repair.
    let again = reload_cache(&reloaded);
    let (mut restored, journal) = committed(&again, &native);
    assert_eq!(value(&restored).as_deref(), Some("typed in window"));
    assert_eq!(journal.outbox, outbox);
    let mut outbox = journal.outbox.clone();
    assert_eq!(
        crate::admitted_records::repair_record_basis(
            &again,
            &mut restored,
            &mut outbox,
            &native.key,
            |_| true,
            time(),
        ),
        Ok(0)
    );
}

#[test]
fn non_descendant_retained_record_is_refused_explicitly() {
    let mut native = Native::new(1);
    let (cache, mut batches, _) = committed_owner(&mut native);
    // A retained record that does not contain the admitted heads.
    let mut foreign = automerge::AutoCommit::new();
    foreign.put(automerge::ROOT, "value", "unrelated").unwrap();
    foreign.commit();
    cache
        .put_document(record(), &mut foreign, Some(vec![1; 32]))
        .unwrap();
    let (id, _) = capture(&mut batches, &cache);
    batches.settle(&id, "lease", Ok(())).unwrap();
    let reloaded = reload_cache(&cache);
    let (restored, journal) = committed(&reloaded, &native);
    let mut state = restored.clone();
    let mut outbox = journal.outbox.clone();
    let error = crate::admitted_records::repair_record_basis(
        &reloaded,
        &mut state,
        &mut outbox,
        &native.key,
        |_| true,
        time(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        format!(
            "retained record {APP}/{RECORD} does not descend from its admitted journal; local storage is kept for recovery"
        )
    );
    // Nothing was changed or dropped.
    assert_eq!(state.rows(), restored.rows());
    assert_eq!(outbox, journal.outbox);
    assert_eq!(
        reloaded.get(&record()).unwrap().bytes,
        cache.get(&record()).unwrap().bytes
    );
}
fn stage_journal_only(
    cache: &PortableStorage,
    state: &StatePeer,
    outbox: &[NeighbourMsg],
    storage: &JournalStorage,
) {
    let mut journal = Journal::from_state(state, outbox.to_vec()).unwrap();
    journal.storage = storage.clone();
    cache
        .put(journal_key(), journal.encode().unwrap(), None)
        .unwrap();
}

// ---- Lane D2: one page of several groups admits in order onto one candidate.

fn replay_cut(
    incoming: &IncomingBudget,
    events: &[(EventKind, Vec<u8>)],
    sequence: u64,
) -> crate::peer_delivery::ReplayCut {
    let stored = cursor(sequence, sequence as u8, 1);
    crate::peer_delivery::ReplayCut {
        events: events
            .iter()
            .enumerate()
            .map(|(ordinal, (kind, bytes))| CompleteEvent {
                ordinal: ordinal as u32,
                kind: *kind,
                digest: canonical_item_digest(bytes),
                bytes: incoming.copy(bytes).unwrap(),
            })
            .collect(),
        cursor: stored,
        committed: CommitRef {
            sequence,
            digest: [sequence as u8; 32],
        },
    }
}

#[test]
fn page_cuts_admit_in_order_and_a_later_refusal_keeps_the_admitted_prefix() {
    let mut native = Native::new(1);
    let incoming = budget();
    let baseline = native.baseline();
    let v2 = native.edit("native v2");
    let v3 = native.edit("native v3");
    let owner = native.consumer();
    let sequential = admit(&owner, &incoming, &baseline).unwrap();
    let sequential = admit(&sequential, &incoming, &v2).unwrap();
    let sequential = admit(&sequential, &incoming, &v3).unwrap();
    let cuts = vec![
        replay_cut(&incoming, &baseline, 1),
        replay_cut(&incoming, &v2, 2),
        replay_cut(&incoming, &v3, 3),
    ];
    page_cursor_steps(None, &cuts).unwrap();
    let (state, cursor, count) = admit_cuts(&owner, cuts, &incoming, time()).unwrap();
    assert_eq!(count, 3);
    assert_eq!(cursor, replay_cut(&incoming, &v3, 3).cursor);
    assert_eq!(value(&state).as_deref(), Some("native v3"));
    assert_eq!(state.rows(), sequential.rows());
    // A tampered later group: install the prefix with that prefix's cursor.
    let mut tampered = v3.clone();
    let last = tampered[0].1.len() - 8;
    tampered[0].1[last] ^= 0x55;
    let cuts = vec![
        replay_cut(&incoming, &baseline, 1),
        replay_cut(&incoming, &v2, 2),
        replay_cut(&incoming, &tampered, 3),
    ];
    let (state, cursor, count) = admit_cuts(&owner, cuts, &incoming, time()).unwrap();
    assert_eq!(count, 2);
    assert_eq!(cursor, replay_cut(&incoming, &v2, 2).cursor);
    assert_eq!(value(&state).as_deref(), Some("native v2"));
    // A refused first group refuses the page and leaves the owner untouched.
    let cuts = vec![replay_cut(&incoming, &tampered, 1)];
    assert!(admit_cuts(&native.consumer(), cuts, &incoming, time()).is_err());
    // Groups must continue each other's cursor within the page.
    let backwards = vec![replay_cut(&incoming, &v2, 2), replay_cut(&incoming, &v3, 1)];
    assert!(page_cursor_steps(None, &backwards).is_err());
}
