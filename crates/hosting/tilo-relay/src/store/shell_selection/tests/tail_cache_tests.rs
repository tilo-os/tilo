//! Plain route tails against a committed selection (TIL-623): no per-keystroke
//! selection, and exactly the publication a full selection would choose.
use super::*;
use automerge::ReadDoc;

/// An owner with `routes` text records `r0..`, its first checkpoint, and the
/// record `r0` as an editable branch at that checkpoint.
struct TailFixture {
    key: ed25519_dalek::SigningKey,
    authority: RetentionAuthority,
    owner: StatePeer,
    /// The owner as of the first checkpoint: a member that replay brings
    /// forward.
    baseline: StatePeer,
    now: web_time::SystemTime,
    checkpoint: Vec<NeighbourMsg>,
    branch: automerge::AutoCommit,
    next_seq: u64,
}
impl TailFixture {
    fn new(routes: usize) -> Self {
        let key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let cert = synthetic_cert(31, &key, "tail fixture".into());
        let now = web_time::SystemTime::UNIX_EPOCH
            .checked_add(web_time::Duration::from_secs(2000))
            .unwrap();
        let documents = (0..routes)
            .map(|index| {
                let mut doc = automerge::AutoCommit::new();
                let text = doc
                    .put_object(automerge::ROOT, "text", automerge::ObjType::Text)
                    .unwrap();
                doc.splice_text(&text, 0, 0, &"x".repeat(1024)).unwrap();
                tilo_peer_state::SeedDocument {
                    app_id: "selector.app".into(),
                    record_id: format!("r{index}"),
                    kind: "test".into(),
                    bytes: doc.save(),
                }
            })
            .collect();
        let mut owner = seeded_owner(SYNTHETIC_ROOM, &cert, &key, documents, now);
        let checkpoint = owner.durable_bootstrap_messages(&key, now, 1).unwrap();
        let next_seq = r0_seq(&checkpoint) + 1;
        let branch =
            automerge::AutoCommit::load(&owner.document("selector.app", "r0").unwrap()).unwrap();
        Self {
            key,
            authority: RetentionAuthority::new(SYNTHETIC_ROOM, cert).unwrap(),
            baseline: owner.clone(),
            owner,
            now,
            checkpoint,
            branch,
            next_seq,
        }
    }
    fn checkpoint_frames(&self) -> Vec<Vec<u8>> {
        self.checkpoint
            .iter()
            .map(|msg| synthetic_frame(&self.authority, msg, true))
            .collect()
    }
    /// One typed character on `r0`, as its retained tail frame and sequence.
    fn keystroke(&mut self) -> (Vec<u8>, u64) {
        let heads = self.branch.get_heads();
        let (_, text) = self.branch.get(automerge::ROOT, "text").unwrap().unwrap();
        self.branch.splice_text(&text, 0, 0, "k").unwrap();
        self.branch.commit();
        let bytes = changes_since(&mut self.branch, &heads);
        let seq = self.next_seq;
        self.next_seq += 1;
        let msg = synthetic_signed_record(
            &self.key,
            self.authority.room(),
            "r0",
            &collab_types::DocumentMutation::Incremental(bytes),
            (seq, self.now),
        );
        (synthetic_frame(&self.authority, &msg, false), seq)
    }
    /// A second checkpoint whose `r0` snapshot contains every keystroke so far.
    fn covering_checkpoint(&mut self) -> Vec<Vec<u8>> {
        let next = synthetic_signed_record(
            &self.key,
            self.authority.room(),
            "r0",
            &collab_types::DocumentMutation::Snapshot(self.branch.save()),
            (self.next_seq, self.now),
        );
        self.next_seq += 1;
        let NeighbourMsg::AppDocument { envelope, .. } = next else {
            panic!()
        };
        self.owner
            .admit_local_app_document("selector.app", "r0", *envelope, self.now)
            .unwrap();
        let messages = self
            .owner
            .durable_bootstrap_messages(&self.key, self.now, 2)
            .unwrap();
        // The checkpoint re-signs `r0`; later keystrokes follow its sequence.
        self.next_seq = r0_seq(&messages) + 1;
        messages
            .iter()
            .map(|msg| synthetic_frame(&self.authority, msg, true))
            .collect()
    }
    /// `r0`'s heads at a fresh receiver after subscription replay of `store`.
    fn replayed_heads(&self, store: &WorkspaceStore) -> Vec<automerge::ChangeHash> {
        let mut receiver = self.baseline.clone();
        for frame in replay_frames(store) {
            let outcome = receive(&mut receiver, frame, false, self.now).unwrap();
            assert_eq!(outcome.rejected, 0);
        }
        receiver.document_heads("selector.app", "r0").unwrap()
    }
}

/// The signed sequence of the `r0` snapshot a checkpoint carries.
fn r0_seq(checkpoint: &[NeighbourMsg]) -> u64 {
    checkpoint
        .iter()
        .find_map(|msg| match msg {
            NeighbourMsg::AppDocument {
                record_id,
                envelope,
                ..
            } if record_id == "r0" => Some(envelope.seq),
            _ => None,
        })
        .unwrap()
}

/// One retain through the owner's production driver: its outcome, whether it
/// ran a selection, and how many retained records it read.
fn retain_frame(store: &WorkspaceStore, bytes: &[u8], seq: u64) -> (RetainOutcome, bool, u64) {
    try_retain_frame(store, bytes, seq).unwrap()
}
fn try_retain_frame(
    store: &WorkspaceStore,
    bytes: &[u8],
    seq: u64,
) -> Result<(RetainOutcome, bool, u64), StoreError> {
    let (_, payload) = retained::payload(bytes, store.workspace_id()).unwrap();
    let (channel, checkpoint) = match payload {
        CoeditPayload::Checkpoint { channel, .. } => (channel, true),
        CoeditPayload::RetainedDelta { channel, .. } => (channel, false),
    };
    let mut build = store.start_retain(&channel, (!checkpoint).then_some(seq), bytes.to_vec())?;
    let outcome = loop {
        if let Step::Done(outcome) = build.step(WorkLimit::default())? {
            break outcome;
        }
    };
    let selected = build.selection_stats().is_some();
    let reads = build.retention_stats().preparation_reads;
    Ok((outcome, selected, reads))
}

#[test]
fn tail_delta_retain_is_constant_in_history() {
    let mut fixture = TailFixture::new(32);
    let (_tmp, store) = bound_store(fixture.authority.clone());
    let selections = track_selection_constructions(store.root());
    let constructed = || selections.load(std::sync::atomic::Ordering::SeqCst);
    for bytes in fixture.checkpoint_frames() {
        let (_, selected, _) = retain_frame(&store, &bytes, 0);
        assert!(selected, "checkpoints still select on the commit path");
    }
    // The route's first tail selects once and leaves its cursor behind.
    let (bytes, seq) = fixture.keystroke();
    let (outcome, selected, _) = retain_frame(&store, &bytes, seq);
    assert!(selected);
    assert_eq!(outcome.publication, Publication::Original);

    let before = constructed();
    let mut reads = Vec::new();
    for _ in 0..200 {
        let (bytes, seq) = fixture.keystroke();
        let (outcome, selected, read) = retain_frame(&store, &bytes, seq);
        assert!(!selected, "a plain tail must not select");
        assert_eq!(outcome.publication, Publication::Original);
        assert!(outcome.written > 0);
        reads.push(read);
    }
    assert_eq!(
        constructed(),
        before,
        "no Selection, and so no retained/v2 scan, for a plain tail"
    );
    assert!(
        reads.iter().all(|read| *read == 0),
        "a tail reads no retained record, however long its history: {reads:?}"
    );

    // Replay selects before it serves, so every unlisted tail still arrives.
    assert_eq!(fixture.replayed_heads(&store), fixture.branch.get_heads());

    // A checkpoint covering them selects once per frame and retires every tail.
    let before = constructed();
    let frames = fixture.covering_checkpoint();
    let count = frames.len();
    for bytes in frames {
        let (_, selected, _) = retain_frame(&store, &bytes, 0);
        assert!(selected);
    }
    assert_eq!(constructed() - before, count);
    assert_eq!(
        replay_frames(&store).len(),
        33,
        "the room and one snapshot per route remain; every covered tail retires"
    );
    assert_eq!(fixture.replayed_heads(&store), fixture.branch.get_heads());
    selection_constructions(store.root());
}

#[test]
fn tail_cache_publishes_what_a_full_selection_would() {
    let mut fixture = TailFixture::new(4);
    let (_fast_tmp, fast) = bound_store(fixture.authority.clone());
    // `full` forgets its cursors before every write, so each retain selects.
    let (_full_tmp, full) = bound_store(fixture.authority.clone());
    let mut fast_selections = 0;
    let mut retains = 0;
    let mut retain_both = |bytes: &[u8], seq: u64| {
        retains += 1;
        let (fast_outcome, selected, _) = try_retain_frame(&fast, bytes, seq)
            .unwrap_or_else(|error| panic!("retain {retains} (seq {seq}) with cursors: {error}"));
        fast_selections += usize::from(selected);
        full.tail_cache.invalidate();
        let (full_outcome, _, _) = try_retain_frame(&full, bytes, seq)
            .unwrap_or_else(|error| panic!("retain {retains} (seq {seq}) selecting: {error}"));
        assert_eq!(
            std::mem::discriminant(&fast_outcome.publication),
            std::mem::discriminant(&full_outcome.publication)
        );
        assert_eq!(fast_outcome.written, full_outcome.written);
        fast_outcome.publication
    };
    let first = fixture.checkpoint_frames();
    let mut checkpoint_frames = first.len();
    for bytes in first {
        retain_both(&bytes, 0);
    }
    let mut sent = Vec::new();
    for _ in 0..6 {
        let (bytes, seq) = fixture.keystroke();
        assert_eq!(retain_both(&bytes, seq), Publication::Original);
        sent.push((bytes, seq));
    }
    // A retransmission is a duplicate and an older tail sorts before the
    // last accepted one: both leave the fast path for the full selection.
    for index in [5, 2] {
        let (bytes, seq) = sent[index].clone();
        retain_both(&bytes, seq);
    }
    let (bytes, seq) = fixture.keystroke();
    assert_eq!(retain_both(&bytes, seq), Publication::Original);
    let second = fixture.covering_checkpoint();
    checkpoint_frames += second.len();
    for bytes in second {
        retain_both(&bytes, 0);
    }
    // A tail the new checkpoint covers is obsolete on both paths.
    let (bytes, seq) = sent[0].clone();
    assert_eq!(retain_both(&bytes, seq), Publication::Suppress);
    for _ in 0..3 {
        let (bytes, seq) = fixture.keystroke();
        assert_eq!(retain_both(&bytes, seq), Publication::Original);
    }
    assert_eq!(
        encoded(&replay_frames(&fast)),
        encoded(&replay_frames(&full))
    );
    assert_eq!(fixture.replayed_heads(&fast), fixture.branch.get_heads());
    // Every checkpoint frame, the route's first tail, and the duplicate and
    // older retains selected; the obsolete tail and the nine plain tails after
    // the first did not.
    assert_eq!(fast_selections, checkpoint_frames + 3);
}
