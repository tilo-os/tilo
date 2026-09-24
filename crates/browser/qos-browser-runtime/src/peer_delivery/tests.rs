use super::*;
use crate::peer_delivery_cursor::CursorIdentity;
use ed25519_dalek::SigningKey;
use tilo_relay::delivery_v3::{
    commit::CommitRef,
    cursor::{CursorIssue, CursorToken},
    state::{
        CommitBoundary, EventChunk, EventKind, EventSource, canonical_item_digest,
        visible_order_digest,
    },
};
const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
fn fixture() -> (CursorIdentity, SigningKey, CommitRef) {
    let key = SigningKey::from_bytes(&[10; 32]);
    (
        CursorIdentity {
            workspace: 9,
            room: ROOM.into(),
            workspace_root: [1; 32],
            device: [2; 32],
            device_root: [3; 32],
            host: key.verifying_key().to_bytes(),
            host_root: [4; 32],
        },
        key,
        CommitRef {
            sequence: 7,
            digest: [8; 32],
        },
    )
}
fn chunk(
    commit: &CommitRef,
    ordinal: u32,
    bytes: &[u8],
    range: std::ops::Range<usize>,
) -> EventChunk {
    EventChunk::new(
        commit.clone(),
        ordinal,
        canonical_item_digest(bytes),
        EventSource::Native,
        EventKind::State,
        bytes.len() as u64,
        range.start as u64,
        bytes[range].to_vec(),
    )
    .unwrap()
}
fn boundary(
    identity: &CursorIdentity,
    key: &SigningKey,
    commit: &CommitRef,
    events: &[(u32, [u8; 32])],
) -> CommitBoundary {
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
            store_generation: [6; 16],
            authorization_view: [7; 32],
            committed: commit.clone(),
        },
        key,
    )
    .unwrap();
    CommitBoundary::new(
        [5; 16],
        [7; 32],
        [6; 16],
        commit.clone(),
        events.len() as u32,
        visible_order_digest(events).unwrap(),
        token,
    )
    .unwrap()
}
#[test]
fn replay_reassembles_exact_ranges_then_checks_signed_complete_cut() {
    let (identity, key, commit) = fixture();
    let bytes = b"canonical bytes";
    let mut batch = ReplayBatch::new(identity.clone(), 11, 1024).unwrap();
    batch.push(11, chunk(&commit, 0, bytes, 0..4)).unwrap();
    batch
        .push(11, chunk(&commit, 0, bytes, 4..bytes.len()))
        .unwrap();
    let cut = batch
        .finish(
            11,
            &boundary(
                &identity,
                &key,
                &commit,
                &[(0, canonical_item_digest(bytes))],
            ),
        )
        .unwrap();
    assert_eq!(cut.events.len(), 1);
    assert_eq!(cut.events[0].bytes.bytes(), bytes);
    assert_eq!(cut.cursor.identity, identity);
    // An authenticated cut alone does not offer a network ACK API.
    assert_eq!(
        cut.cursor.token,
        boundary(
            &identity,
            &key,
            &commit,
            &[(0, canonical_item_digest(bytes))]
        )
        .cursor()
        .as_bytes()
    );
}
#[test]
fn replay_rejects_overlaps_gaps_digest_substitution_and_stale_generation() {
    let (identity, _, commit) = fixture();
    let bytes = b"abcdef";
    for bad in [
        chunk(&commit, 0, bytes, 0..2),
        chunk(&commit, 0, bytes, 4..6),
        chunk(&commit, 0, b"abcdeg", 3..6),
    ] {
        let mut batch = ReplayBatch::new(identity.clone(), 11, 1024).unwrap();
        batch.push(11, chunk(&commit, 0, bytes, 0..3)).unwrap();
        assert!(batch.push(11, bad).is_err());
    }
    let mut batch = ReplayBatch::new(identity, 11, 1024).unwrap();
    assert!(batch.push(12, chunk(&commit, 0, bytes, 0..6)).is_err());
}
#[test]
fn replay_boundary_cannot_skip_pending_bytes_change_scope_or_lie_about_order() {
    let (identity, key, commit) = fixture();
    let bytes = b"abcdef";
    let make = || {
        let mut b = ReplayBatch::new(identity.clone(), 11, 1024).unwrap();
        b.push(11, chunk(&commit, 0, bytes, 0..6)).unwrap();
        b
    };
    let mut partial = ReplayBatch::new(identity.clone(), 11, 1024).unwrap();
    partial.push(11, chunk(&commit, 0, bytes, 0..3)).unwrap();
    let valid = boundary(
        &identity,
        &key,
        &commit,
        &[(0, canonical_item_digest(bytes))],
    );
    assert!(partial.finish(11, &valid).is_err());
    assert!(make().finish(12, &valid).is_err());
    assert!(
        make()
            .finish(11, &boundary(&identity, &key, &commit, &[]))
            .is_err()
    );
    assert!(
        make()
            .finish(
                11,
                &boundary(
                    &identity,
                    &key,
                    &commit,
                    &[(1, canonical_item_digest(bytes))]
                )
            )
            .is_err()
    );
    let mut foreign = identity.clone();
    foreign.device = [33; 32];
    assert!(
        make()
            .finish(
                11,
                &boundary(
                    &foreign,
                    &key,
                    &commit,
                    &[(0, canonical_item_digest(bytes))]
                )
            )
            .is_err()
    );
    assert!(
        make()
            .finish(
                11,
                &boundary(
                    &identity,
                    &SigningKey::from_bytes(&[44; 32]),
                    &commit,
                    &[(0, canonical_item_digest(bytes))]
                )
            )
            .is_err()
    );
}
#[test]
fn replay_checks_declared_bytes_before_allocating_and_bounds_aggregate() {
    let (identity, _, commit) = fixture();
    let bytes = b"abcdef";
    let mut batch = ReplayBatch::new(identity.clone(), 11, 5).unwrap();
    assert!(batch.push(11, chunk(&commit, 0, bytes, 0..1)).is_err());
    let mut batch = ReplayBatch::new(identity, 11, 10).unwrap();
    batch.push(11, chunk(&commit, 0, bytes, 0..6)).unwrap();
    assert!(batch.push(11, chunk(&commit, 1, bytes, 0..6)).is_err());
}

#[test]
fn staged_stream_reads_split_headers_and_one_canonical_item_at_a_time() {
    use tilo_relay::delivery_v3::state::{TransferKind, TransferRecordHeader};
    let mut stream = Vec::new();
    let items = [
        (3_u32, b"governance".as_slice()),
        (8, b"original member document".as_slice()),
    ];
    for (ordinal, bytes) in items {
        stream.extend(
            TransferRecordHeader::new(
                ordinal,
                TransferKind::State,
                bytes.len() as u64,
                canonical_item_digest(bytes),
            )
            .unwrap()
            .encode(),
        );
        stream.extend(bytes);
    }
    let mut decoder = StreamDecoder::new(2, stream.len() as u64, 1024).unwrap();
    let mut actual = Vec::new();
    for part in stream.chunks(7) {
        decoder
            .push(part, |event| {
                actual.push((event.ordinal, event.bytes.bytes().to_vec()));
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        decoder.finish().unwrap(),
        visible_order_digest(
            &items.map(|(ordinal, bytes)| (ordinal, canonical_item_digest(bytes)))
        )
        .unwrap()
    );
    assert_eq!(
        actual,
        items.map(|(ordinal, bytes)| (ordinal, bytes.to_vec()))
    );
}
#[test]
fn staged_stream_rejects_incomplete_corrupt_oversized_and_out_of_order_items() {
    use tilo_relay::delivery_v3::state::{TransferKind, TransferRecordHeader};
    let header =
        TransferRecordHeader::new(4, TransferKind::State, 6, canonical_item_digest(b"abcdef"))
            .unwrap()
            .encode();
    let mut decoder = StreamDecoder::new(1, 51, 5).unwrap();
    assert!(decoder.push(&header, |_| panic!("must not admit")).is_err());
    let mut decoder = StreamDecoder::new(1, 51, 6).unwrap();
    decoder
        .push(&header[..10], |_| panic!("must not admit"))
        .unwrap();
    assert!(decoder.finish().is_err());
    let mut decoder = StreamDecoder::new(1, 51, 6).unwrap();
    decoder.push(&header, |_| panic!("must not admit")).unwrap();
    assert!(
        decoder
            .push(b"abcdeg", |_| panic!("corrupt bytes must not admit"))
            .is_err()
    );
    let mut decoder = StreamDecoder::new(2, 102, 6).unwrap();
    decoder.push(&header, |_| Ok(())).unwrap();
    decoder.push(b"abcdef", |_| Ok(())).unwrap();
    assert!(decoder.push(&header, |_| Ok(())).is_err());
}

#[test]
fn pinned_transfer_checks_scope_every_chunk_final_order_generation_and_actual_deadline() {
    use super::transfer::PinnedTransfer;
    use state::{
        ManifestIssue, SnapshotChunk, SnapshotKind, SnapshotManifest, TransferKind,
        TransferRecordHeader,
    };
    let (identity, key, commit) = fixture();
    let bytes = b"original";
    let digest = canonical_item_digest(bytes);
    let mut stream = TransferRecordHeader::new(4, TransferKind::State, bytes.len() as u64, digest)
        .unwrap()
        .encode()
        .to_vec();
    stream.extend(bytes);
    let boundary = boundary(&identity, &key, &commit, &[(4, digest)]);
    let scope = boundary
        .cursor()
        .authenticate(&key.verifying_key())
        .unwrap()
        .issue()
        .clone();
    let issue = ManifestIssue {
        scope,
        pin: [9; 16],
        kind: SnapshotKind::Baseline,
        predecessor: None,
        baseline_generation: 2,
        expires_at_ns: 1_000_000_000,
        item_count: 1,
        stream_bytes: stream.len() as u64,
        chunk_size: 240 * 1024,
        chunk_digests: vec![canonical_item_digest(&stream)],
    };
    let manifest = SnapshotManifest::issue(issue.clone(), &key, 10).unwrap();
    let mut transfer =
        PinnedTransfer::open(identity.clone(), 11, manifest.as_bytes(), None, 10).unwrap();
    let chunk = SnapshotChunk::new([9; 16], manifest.digest(), 0, &stream).unwrap();
    transfer.validate_chunk(11, &chunk, 20).unwrap();
    assert!(
        transfer
            .current_at(
                11,
                20,
                web_time::Instant::now() + std::time::Duration::from_secs(5)
            )
            .is_err(),
        "a wall clock rollback cannot revive a transfer after its original monotonic budget"
    );
    assert!(transfer.validate_chunk(12, &chunk, 20).is_err());
    assert!(transfer.validate_chunk(11, &chunk, 1_000_000_000).is_err());
    transfer.validate_staged(11, 0, &stream, 20).unwrap();
    let mut corrupt = stream.clone();
    corrupt[46] ^= 1;
    assert!(transfer.validate_staged(11, 0, &corrupt, 20).is_err());
    assert!(transfer.validate_staged(11, 1, &stream, 20).is_err());
    assert!(
        transfer
            .finish(11, boundary.visible_order_digest(), 20)
            .is_err()
    );
    transfer.set_boundary(11, boundary.clone(), 20).unwrap();
    assert!(transfer.finish(11, [0; 32], 20).is_err());
    assert_eq!(
        transfer
            .finish(11, boundary.visible_order_digest(), 20)
            .unwrap()
            .token,
        boundary.cursor().as_bytes()
    );
    let mut changed = issue.clone();
    changed.scope.consumer_device = [33; 32];
    let foreign = SnapshotManifest::issue(changed, &key, 10).unwrap();
    assert!(PinnedTransfer::open(identity.clone(), 11, foreign.as_bytes(), None, 10).is_err());
    let mut renewed = issue.clone();
    renewed.expires_at_ns += 1_000_000_000;
    let renewal = SnapshotManifest::issue(renewed.clone(), &key, 20).unwrap();
    transfer.renew(11, renewal.as_bytes(), 20).unwrap();
    assert_eq!(transfer.digest(), renewal.digest());
    transfer.renew(11, renewal.as_bytes(), 21).expect(
        "a lost renewal receipt may return the exact current manifest without extending it",
    );
    renewed.chunk_digests[0] = [44; 32];
    renewed.expires_at_ns += 1;
    let changed = SnapshotManifest::issue(renewed, &key, 20).unwrap();
    assert!(transfer.renew(11, changed.as_bytes(), 20).is_err());
    // Renewed metadata changes the wire digest, but previously stored bytes
    // remain valid only because every immutable signed claim matched.
    transfer
        .validate_staged(11, 0, &stream, 1_000_000_000)
        .unwrap();
}

#[test]
fn incoming_budget_is_shared_across_requests_raw_items_and_actual_bounded_dto_decode() {
    use super::incoming::IncomingBudget;
    use qos_peer_state::resources::{BoundedPeerMessage, OuterLimits};
    let budget = IncomingBudget::new(64 * 1024).unwrap();
    let free = budget.available();
    let request = budget.reserve(40 * 1024).unwrap();
    assert!(budget.clone().reserve(30 * 1024).is_err());
    let bytes = postcard::to_allocvec(&qos_peer_state::NeighbourMsg::SyncResp {
        changes: vec![vec![1; 32]],
        room: "room".into(),
    })
    .unwrap();
    let raw = budget.copy(&bytes).unwrap();
    assert_eq!(raw.bytes(), bytes);
    assert!(
        BoundedPeerMessage::decode(
            raw.bytes(),
            OuterLimits {
                allocation_bytes: 32 * 1024,
                collection_entries: 64
            },
            budget.allowance()
        )
        .is_err()
    );
    drop(request);
    let dto = BoundedPeerMessage::decode(
        raw.bytes(),
        OuterLimits {
            allocation_bytes: 32 * 1024,
            collection_entries: 64,
        },
        budget.allowance(),
    )
    .unwrap();
    assert!(budget.reserve(40 * 1024).is_err());
    drop(dto);
    drop(raw);
    assert_eq!(budget.available(), free);
    assert!(budget.copy(&vec![0; 64 * 1024]).is_err());
    assert_eq!(budget.available(), free);
}

#[test]
fn staged_parser_retains_shared_raw_credit_until_the_consuming_admission_drops_it() {
    use super::incoming::IncomingBudget;
    use state::{TransferKind, TransferRecordHeader};
    let budget = IncomingBudget::new(64 * 1024).unwrap();
    let before = budget.available();
    let header = TransferRecordHeader::new(
        1,
        TransferKind::State,
        32 * 1024,
        canonical_item_digest(&vec![3; 32 * 1024]),
    )
    .unwrap()
    .encode();
    let mut decoder =
        StreamDecoder::with_incoming(1, 32 * 1024 + 45, 32 * 1024, budget.clone()).unwrap();
    decoder.push(&header, |_| unreachable!()).unwrap();
    assert!(budget.reserve(33 * 1024).is_err());
    let mut retained = None;
    decoder
        .push(&vec![3; 32 * 1024], |event| {
            retained = Some(event);
            Ok(())
        })
        .unwrap();
    decoder.finish().unwrap();
    assert!(budget.reserve(33 * 1024).is_err());
    drop(retained);
    assert_eq!(budget.available(), before);
}

// ---- Lane D2: multi-group poll pages (MULTI_GROUP_REPLAY_V1).

fn group_frames(
    identity: &CursorIdentity,
    key: &SigningKey,
    commit: &CommitRef,
    items: &[&[u8]],
    page: &mut Vec<u8>,
) {
    use tilo_relay::delivery_v3::framing::{self, MessageType};
    let mut order = Vec::new();
    for (ordinal, bytes) in items.iter().enumerate() {
        let event = chunk(commit, ordinal as u32, bytes, 0..bytes.len());
        framing::append_frame(
            page,
            MessageType::EventChunk,
            &event.encode().unwrap(),
            1 << 20,
        )
        .unwrap();
        order.push((ordinal as u32, canonical_item_digest(bytes)));
    }
    let boundary = boundary(identity, key, commit, &order);
    framing::append_frame(
        page,
        MessageType::CommitBoundary,
        &boundary.encode().unwrap(),
        1 << 20,
    )
    .unwrap();
}
fn commit_at(sequence: u64) -> CommitRef {
    CommitRef {
        sequence,
        digest: [sequence as u8; 32],
    }
}
const MULTI: (usize, usize) = (128, 512);
const SINGLE: (usize, usize) = (1, 64);

#[test]
fn a_multi_group_page_yields_every_complete_group_in_commit_order() {
    let (identity, key, _) = fixture();
    let incoming = incoming::IncomingBudget::new(16 * 1024 * 1024).unwrap();
    let mut page = Vec::new();
    group_frames(&identity, &key, &commit_at(7), &[b"a1", b"a2"], &mut page);
    group_frames(&identity, &key, &commit_at(8), &[], &mut page);
    group_frames(&identity, &key, &commit_at(12), &[b"c1"], &mut page);
    let PageContents::Cuts(cuts) = collect_page(&identity, 11, &incoming, &page, MULTI).unwrap()
    else {
        panic!("expected cuts")
    };
    assert_eq!(
        cuts.iter()
            .map(|cut| cut.committed.sequence)
            .collect::<Vec<_>>(),
        [7, 8, 12]
    );
    assert_eq!(cuts[0].events.len(), 2);
    assert_eq!(cuts[2].events[0].bytes.bytes(), b"c1");
    // A client that did not negotiate the extension accepts exactly one group.
    assert!(collect_page(&identity, 11, &incoming, &page, SINGLE).is_err());
    let mut single = Vec::new();
    group_frames(&identity, &key, &commit_at(7), &[b"a1"], &mut single);
    assert!(matches!(
        collect_page(&identity, 11, &incoming, &single, SINGLE).unwrap(),
        PageContents::Cuts(cuts) if cuts.len() == 1
    ));
}

#[test]
fn a_multi_group_page_refuses_reordered_partial_or_oversized_groups() {
    let (identity, key, _) = fixture();
    let incoming = incoming::IncomingBudget::new(16 * 1024 * 1024).unwrap();
    // Non-increasing commit order.
    let mut page = Vec::new();
    group_frames(&identity, &key, &commit_at(9), &[b"x"], &mut page);
    group_frames(&identity, &key, &commit_at(9), &[b"x"], &mut page);
    assert!(collect_page(&identity, 11, &incoming, &page, MULTI).is_err());
    // A trailing partial group.
    let mut page = Vec::new();
    group_frames(&identity, &key, &commit_at(9), &[b"x"], &mut page);
    let event = chunk(&commit_at(10), 0, b"y", 0..1);
    tilo_relay::delivery_v3::framing::append_frame(
        &mut page,
        tilo_relay::delivery_v3::framing::MessageType::EventChunk,
        &event.encode().unwrap(),
        1 << 20,
    )
    .unwrap();
    assert!(collect_page(&identity, 11, &incoming, &page, MULTI).is_err());
    // More groups than negotiated.
    let mut page = Vec::new();
    for sequence in 1..=3 {
        group_frames(&identity, &key, &commit_at(sequence), &[], &mut page);
    }
    assert!(collect_page(&identity, 11, &incoming, &page, (2, 512)).is_err());
    assert!(collect_page(&identity, 11, &incoming, &page, (3, 2)).is_err());
    assert!(collect_page(&identity, 11, &incoming, &page, (3, 3)).is_ok());
    // A group's events cannot be relabelled under another group's boundary.
    let mut page = Vec::new();
    let event = chunk(&commit_at(4), 0, b"z", 0..1);
    tilo_relay::delivery_v3::framing::append_frame(
        &mut page,
        tilo_relay::delivery_v3::framing::MessageType::EventChunk,
        &event.encode().unwrap(),
        1 << 20,
    )
    .unwrap();
    group_frames(&identity, &key, &commit_at(5), &[b"z"], &mut page);
    assert!(collect_page(&identity, 11, &incoming, &page, MULTI).is_err());
}
