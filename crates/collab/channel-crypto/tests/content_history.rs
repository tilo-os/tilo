//! Full history for members who join later, with real multi-member MLS
//! groups: content keys are distributed as a key history on join, the
//! current state arrives as one snapshot, and older ops are paged in lazily
//! from a history host and decrypted locally.

use std::time::SystemTime;

use channel_crypto::{ChannelGroup, Error, MemberKey, VaultKey};
use collab_types::history::{HistoryPager, page_before};
use collab_types::{
    ArtifactId, AttestationId, ChannelId, ContentRetention, OpClass, SignedOp, generate_keypair,
    sign_op, verify_op,
};

/// The history host (broker/relay): stores signed ciphertext, never keys.
#[derive(Default)]
struct Host {
    ops: Vec<SignedOp>,
    seq: u64,
}

impl Host {
    fn total_bytes(&self) -> usize {
        self.ops.iter().map(|op| op.payload.len()).sum()
    }

    fn latest_checkpoint(&self, artifact: ArtifactId) -> Option<&SignedOp> {
        self.ops.iter().rev().find(|op| {
            op.artifact == artifact
                && op.encryption.and_then(|meta| meta.retention())
                    == Some(ContentRetention::Checkpoint)
        })
    }
}

struct Writer {
    identity: ed25519_dalek::SigningKey,
    public: collab_types::Ed25519PublicKey,
}

impl Writer {
    fn new() -> Self {
        let (identity, public) = generate_keypair();
        Self { identity, public }
    }

    fn publish(
        &self,
        host: &mut Host,
        group: &ChannelGroup,
        artifact: ArtifactId,
        retention: ContentRetention,
        plaintext: &[u8],
    ) {
        let (metadata, ciphertext) = group.seal_content(artifact, retention, plaintext).unwrap();
        host.seq += 1;
        let mut op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            self.public,
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            host.seq,
            metadata,
        );
        sign_op(&mut op, &self.identity).unwrap();
        host.ops.push(op);
    }
}

/// The document the ops build: one line per op. A snapshot is its lines.
fn line(index: usize) -> Vec<u8> {
    format!("line {index}\n").into_bytes()
}

fn open(group: &mut ChannelGroup, op: &SignedOp) -> channel_crypto::Result<Vec<u8>> {
    verify_op(op).unwrap();
    group
        .open_content(op.artifact, op.encryption.as_ref().unwrap(), &op.payload)
        .map(|plaintext| plaintext.to_vec())
}

fn member(name: &str) -> MemberKey {
    MemberKey::generate(name).unwrap()
}

/// Alice and Bob write `ops` lines, with a member (Dave) added and removed
/// half-way so the history spans several content-key epochs, then Alice
/// publishes a snapshot of the current document.
fn populated_channel(
    ops: usize,
) -> (
    ChannelId,
    ArtifactId,
    Host,
    ChannelGroup,
    ChannelGroup,
    Vec<u8>,
) {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (alice_writer, bob_writer) = (Writer::new(), Writer::new());
    let mut host = Host::default();
    let mut alice = ChannelGroup::create(channel, member("alice")).unwrap();
    let bob_key = member("bob");
    let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
    let mut bob = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        bob_key,
    )
    .unwrap();
    let mut document = Vec::new();
    for index in 0..ops {
        if index == ops / 2 {
            let dave_key = member("dave");
            let addition = alice.admit(dave_key.fresh_key_package().unwrap()).unwrap();
            bob.process_commit(addition.commit.as_ref().unwrap())
                .unwrap();
            let mut dave = ChannelGroup::process_welcome_with_history(
                channel,
                &addition.welcome,
                &addition.key_history,
                dave_key,
            )
            .unwrap();
            let removal = alice.remove_member(2).unwrap();
            bob.process_commit(&removal).unwrap();
            dave.process_commit(&removal).unwrap();
        }
        let (writer, group) = if index % 2 == 0 {
            (&alice_writer, &alice)
        } else {
            (&bob_writer, &bob)
        };
        writer.publish(
            &mut host,
            group,
            artifact,
            ContentRetention::Delta,
            &line(index),
        );
        document.extend(line(index));
    }
    alice_writer.publish(
        &mut host,
        &alice,
        artifact,
        ContentRetention::Checkpoint,
        &document,
    );
    (channel, artifact, host, alice, bob, document)
}

/// Page the whole history back from the host, newest first, and decrypt
/// every op with the joiner's local keys only.
fn read_all_history(
    group: &mut ChannelGroup,
    host: &Host,
    artifact: ArtifactId,
) -> (Vec<Vec<u8>>, usize) {
    let mut pager = HistoryPager::new(artifact, 128, 16);
    pager.anchor(host.seq + 1);
    let mut decrypted_pages = Vec::new();
    let mut requests = 0;
    while let Some(request) = pager.on_viewport(0) {
        requests += 1;
        let (page, exhausted) =
            page_before(&host.ops, request.artifact, request.before_seq, request.max);
        let mut decrypted = Vec::new();
        for op in &page {
            decrypted.push(open(group, op).unwrap());
        }
        decrypted_pages.push(decrypted);
        pager.on_page_ops(&page, exhausted);
    }
    decrypted_pages.reverse();
    (decrypted_pages.concat(), requests)
}

/// A member added after 1000 ops reads every op and the full document; joining transfers state
/// and keys, not history; a joiner decrypts fetched history after restart with nobody online;
/// scrolling back fetches only the pages it reaches.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn a_joiner_reads_history_on_demand_from_transferred_keys() {
    member_added_after_1000_ops_reads_every_op_and_the_full_document();
    joining_transfers_the_current_state_and_keys_not_the_history();
    joiner_decrypts_fetched_history_after_restart_with_no_other_member_online();
    scrolling_back_fetches_only_the_pages_it_reaches();
}

fn member_added_after_1000_ops_reads_every_op_and_the_full_document() {
    let (channel, artifact, host, mut alice, mut bob, document) = populated_channel(1000);
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    bob.process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    let mut carol = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        carol_key,
    )
    .unwrap();

    // Current state: one snapshot.
    let snapshot = host.latest_checkpoint(artifact).unwrap();
    assert_eq!(open(&mut carol, snapshot).unwrap(), document);

    // Full history, every op, spanning every epoch before the join.
    let (history, _) = read_all_history(&mut carol, &host, artifact);
    assert_eq!(history.len(), 1001);
    let replayed: Vec<u8> = history[..1000].concat();
    assert_eq!(replayed, document);
    assert!(carol.keyring().len() >= 5, "{:?}", carol.keyring());

    // Content-key ciphertext can be opened again (unlike MLS messages).
    assert_eq!(open(&mut carol, &host.ops[0]).unwrap(), line(0));
    assert_eq!(open(&mut carol, &host.ops[0]).unwrap(), line(0));
}

fn joining_transfers_the_current_state_and_keys_not_the_history() {
    let join_cost = |ops: usize| {
        let (channel, artifact, host, mut alice, _bob, document) = populated_channel(ops);
        let carol_key = member("carol");
        let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
        let snapshot = host.latest_checkpoint(artifact).unwrap();
        let mut carol = ChannelGroup::process_welcome_with_history(
            channel,
            &addition.welcome,
            &addition.key_history,
            carol_key,
        )
        .unwrap();
        assert_eq!(open(&mut carol, snapshot).unwrap(), document);
        (
            addition.key_history.len(),
            addition.welcome.len() + addition.key_history.len() + snapshot.payload.len(),
            document.len(),
            host.total_bytes(),
        )
    };
    let (small_keys, _, _, _) = join_cost(10);
    let (large_keys, join_bytes, state_bytes, history_bytes) = join_cost(1000);
    // Key material depends on the membership epochs, not on the op count.
    assert_eq!(small_keys, large_keys);
    // The join is O(current state + key material): the snapshot plus a
    // Welcome and a few keys, a fraction of the op history.
    assert!(
        join_bytes < state_bytes + 16 * 1024,
        "join {join_bytes} for state {state_bytes}"
    );
    assert!(
        join_bytes * 5 < history_bytes,
        "join {join_bytes} vs history {history_bytes}"
    );
}

fn joiner_decrypts_fetched_history_after_restart_with_no_other_member_online() {
    let (channel, artifact, host, mut alice, mut bob, document) = populated_channel(300);
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    bob.process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    let carol = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        carol_key,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("carol.mls");
    carol
        .save_sealed(&path, &VaultKey::new([0x5c; 32]))
        .unwrap();
    // Everyone else goes away; Carol restarts from disk.
    drop((alice, bob, carol));
    let mut carol = ChannelGroup::load_sealed(&path, &VaultKey::new([0x5c; 32])).unwrap();

    let (history, requests) = read_all_history(&mut carol, &host, artifact);
    assert_eq!(history[..300].concat(), document);
    assert_eq!(requests, 3, "301 ops in pages of 128");
}

fn scrolling_back_fetches_only_the_pages_it_reaches() {
    let (channel, artifact, host, mut alice, _bob, _document) = populated_channel(1000);
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    let mut carol = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        carol_key,
    )
    .unwrap();
    let mut pager = HistoryPager::new(artifact, 50, 10);
    pager.anchor(host.seq + 1);
    let mut loaded: Vec<SignedOp> = Vec::new();
    let mut fetched_pages = 0;
    // The view shows the newest 30 ops, then the user scrolls up 120 rows.
    for rows_scrolled in (0..=120).step_by(10) {
        let above_viewport = loaded.len().saturating_sub(rows_scrolled);
        if let Some(request) = pager.on_viewport(above_viewport) {
            let (page, exhausted) =
                page_before(&host.ops, request.artifact, request.before_seq, request.max);
            for op in &page {
                open(&mut carol, op).unwrap();
            }
            fetched_pages += 1;
            pager.on_page_ops(&page, exhausted);
            loaded.splice(0..0, page);
        }
    }
    assert_eq!(
        fetched_pages, 3,
        "prefetched just ahead of the scroll position"
    );
    assert_eq!(loaded.len(), 150);
    assert!(!pager.is_exhausted());

    // Opening a version from the first ops pages straight back to it.
    let mut request = pager.ensure_loaded_back_to(5);
    while let Some(next) = request {
        let (page, exhausted) = page_before(&host.ops, next.artifact, next.before_seq, next.max);
        for op in &page {
            open(&mut carol, op).unwrap();
        }
        request = pager.on_page_ops(&page, exhausted);
    }
    assert!(pager.oldest_loaded().unwrap() <= 5);
}

/// A removed member cannot read content sealed after removal, and concurrent add and remove
/// serialise so every member agrees on keys.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn membership_changes_settle_one_agreed_keyring() {
    removed_member_cannot_read_content_sealed_after_removal();
    concurrent_add_and_remove_serialize_and_every_member_agrees_on_keys();
}

fn removed_member_cannot_read_content_sealed_after_removal() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let writer = Writer::new();
    let mut host = Host::default();
    let mut alice = ChannelGroup::create(channel, member("alice")).unwrap();
    let bob_key = member("bob");
    let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
    let mut bob = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        bob_key,
    )
    .unwrap();
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    bob.process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    let mut carol = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        carol_key,
    )
    .unwrap();

    writer.publish(
        &mut host,
        &alice,
        artifact,
        ContentRetention::Delta,
        b"before removal",
    );
    let removal = alice.remove_member(1).unwrap();
    carol.process_commit(&removal).unwrap();
    bob.process_commit(&removal).unwrap();
    writer.publish(
        &mut host,
        &alice,
        artifact,
        ContentRetention::Delta,
        b"after removal",
    );
    writer.publish(
        &mut host,
        &carol,
        artifact,
        ContentRetention::Checkpoint,
        b"state",
    );

    // Bob keeps what he legitimately saw.
    assert_eq!(open(&mut bob, &host.ops[0]).unwrap(), b"before removal");
    // Nothing sealed after the removal opens for him ...
    for op in &host.ops[1..] {
        let epoch = op.encryption.unwrap().epoch;
        assert!(matches!(open(&mut bob, op), Err(Error::MissingContentKey(e)) if e == epoch));
        // ... not even by trying every key he holds against the ciphertext.
        let (first, last) = bob.keyring().epoch_range().unwrap();
        for candidate in first..=last {
            let mut guessed = op.encryption.unwrap();
            guessed.epoch = candidate;
            assert!(
                bob.keyring()
                    .open(channel, op.artifact, &guessed, &op.payload)
                    .is_err()
            );
        }
    }
    // He cannot seal new content either, and cannot be handed history.
    assert!(
        bob.seal_content(artifact, ContentRetention::Delta, b"x")
            .is_err()
    );
    assert!(bob.current_key_history().is_err());
    // Survivors read everything.
    for op in &host.ops {
        open(&mut carol, op).unwrap();
    }
}

fn concurrent_add_and_remove_serialize_and_every_member_agrees_on_keys() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let writer = Writer::new();
    let mut host = Host::default();
    let mut alice = ChannelGroup::create(channel, member("alice")).unwrap();
    let bob_key = member("bob");
    let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
    let mut bob = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        bob_key,
    )
    .unwrap();
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    bob.process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    let mut carol = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        carol_key,
    )
    .unwrap();
    writer.publish(
        &mut host,
        &carol,
        artifact,
        ContentRetention::Delta,
        b"carol was here",
    );

    // At the same epoch, Alice admits Dave while Bob removes Carol. The
    // broker orders governance: Alice's commit lands first, so Bob's
    // concurrent removal loses the race and is rolled back.
    let epoch = alice.epoch();
    let mut raced_commit = Vec::new();
    let lost = bob.remove_member_transactional(2, |_, commit, _| {
        raced_commit = commit.to_vec();
        Err(Error::Governance(
            "a concurrent commit was ordered first".into(),
        ))
    });
    assert!(lost.is_err());
    assert_eq!(bob.epoch(), epoch, "the losing commit left no trace");
    let dave_key = member("dave");
    let addition = alice.admit(dave_key.fresh_key_package().unwrap()).unwrap();
    bob.process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    carol
        .process_commit(addition.commit.as_ref().unwrap())
        .unwrap();
    let mut dave = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        dave_key,
    )
    .unwrap();
    // The stale commit is refused by members who moved on.
    assert!(alice.process_commit(&raced_commit).is_err());

    // Bob retries on top of the new epoch; everyone applies it.
    let removal = bob.remove_member(2).unwrap();
    alice.process_commit(&removal).unwrap();
    dave.process_commit(&removal).unwrap();
    carol.process_commit(&removal).unwrap();
    writer.publish(
        &mut host,
        &dave,
        artifact,
        ContentRetention::Delta,
        b"after carol left",
    );

    for group in [&alice, &bob, &dave] {
        assert_eq!(group.epoch(), alice.epoch());
        assert_eq!(group.member_count(), 3);
    }
    // Every survivor holds the same key for every epoch it knows, so any
    // of them reads what any other sealed — including Dave, who joined
    // after Carol's op, and reads it from history.
    for epoch in 0..=alice.epoch() {
        let reference = alice.keyring().get(epoch);
        assert!(reference.is_some(), "alice lacks epoch {epoch}");
        assert_eq!(dave.keyring().get(epoch), reference);
        assert_eq!(bob.keyring().get(epoch), reference);
    }
    for op in &host.ops {
        open(&mut dave, op).unwrap();
        open(&mut alice, op).unwrap();
    }
    // Carol, removed, reads her own op but not what followed.
    assert_eq!(open(&mut carol, &host.ops[0]).unwrap(), b"carol was here");
    assert!(open(&mut carol, &host.ops[1]).is_err());
}

/// The keyring survives restart for every member.
#[test]
fn keyring_survives_restart_for_every_member() {
    let (channel, artifact, host, alice, bob, document) = populated_channel(40);
    let dir = tempfile::tempdir().unwrap();
    for (name, group) in [("alice", alice), ("bob", bob)] {
        let path = dir.path().join(format!("{name}.mls"));
        let key = VaultKey::new([name.as_bytes()[0]; 32]);
        group.save_sealed(&path, &key).unwrap();
        let before = group.keyring().len();
        drop(group);
        let mut restored = ChannelGroup::load_sealed(&path, &key).unwrap();
        assert_eq!(restored.keyring().len(), before);
        assert_eq!(restored.channel_id(), channel);
        let (history, _) = read_all_history(&mut restored, &host, artifact);
        assert_eq!(history[..40].concat(), document);
        assert!(
            tilo_seal::envelope::is_envelope(&std::fs::read(&path).unwrap()),
            "state is written in the current format"
        );
    }
}
