//! A device keeps the sealed histories of its records within their bound
//! wherever it writes: alone, offline or linked, so a change is never refused
//! for how long its record's history grew.
use super::*;
use crate::owner::testing::{governed_fixture_at, member_key, member_person};
use crate::owner::{NativeCommittedView, SealedHistoryKeys, WriteRefusal};
use automerge::{ActorId, ChangeHash};
use collab_types::{ContentRetention, DocumentMutation, EncryptionMetadata, OpKind, SignedOp};
use key_group::ContentKeys;
use std::collections::BTreeSet;

/// The chat document's keys, as a member holding them compacts with.
pub(super) struct Held(pub(super) ContentKeys);

impl SealedHistoryKeys for Held {
    fn open(&self, _: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String> {
        open(&self.0, write)
    }

    fn seal_checkpoint(
        &self,
        _: WorkspaceId,
        app: &str,
        record: &str,
        _named: Option<&str>,
        snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(
                collab_types::document_artifact_id(app, WORKSPACE, record),
                ContentRetention::of_frame(snapshot),
                snapshot,
            )
            .map_err(|error| error.to_string())
    }

    fn renew(
        &self,
        _: WorkspaceId,
        write: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let plaintext = open(&self.0, write)?;
        self.0
            .seal_content(write.artifact, ContentRetention::Delta, &plaintext)
            .map_err(|error| error.to_string())
    }
}

fn open(keys: &ContentKeys, write: &SignedOp) -> Result<Vec<u8>, String> {
    let ciphertext = collab_types::sealed_ciphertext(write).map_err(|error| error.to_string())?;
    keys.open_content(
        write.artifact,
        &write.encryption.ok_or("not sealed")?,
        &ciphertext,
    )
    .map(|bytes| bytes.to_vec())
    .map_err(|error| error.to_string())
}

fn seal_delta(
    keys: &ContentKeys,
) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + '_ {
    |plaintext| {
        keys.seal_content(
            collab_types::document_artifact_id(APP, WORKSPACE, ""),
            ContentRetention::Delta,
            plaintext,
        )
        .map_err(|error| error.to_string())
    }
}

/// Someone typing into the chat record: one change per key, each after the
/// last.
struct Typist(AutoCommit);

impl Typist {
    fn over(base: &[u8], actor: u8) -> Self {
        let mut document = AutoCommit::load(base).unwrap();
        document.set_actor(ActorId::from([actor; 16]));
        Self(document)
    }

    fn change(&mut self, key: &str) -> Vec<u8> {
        self.put(key, key)
    }

    fn put(&mut self, key: &str, value: &str) -> Vec<u8> {
        let before: Vec<ChangeHash> = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.0.commit_edit("fixture", "topic", &member_person());
        self.0.save_after(&before)
    }
}

fn base(owner: &NativeWorkspaceOwner, workspace: WorkspaceId) -> Vec<u8> {
    owner
        .with_current(workspace, |view| {
            view.peer()
                .document(APP, "")
                .ok_or_else(|| "no chat".into())
        })
        .unwrap()
}

/// Write `changes` sealed under `keys` as this device does: drafted and
/// sealed in the owner's turn, signed by its key outside it.
async fn write(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    changes: Vec<u8>,
    now: SystemTime,
) -> Result<usize, String> {
    write_all(owner, workspace, keys, vec![changes], now)
        .await
        .map_err(String::from)
}

/// Write every one of `changes` in one owner turn, as a session commits the writes it held.
async fn write_all(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    changes: Vec<Vec<u8>>,
    now: SystemTime,
) -> Result<usize, WriteRefusal> {
    let frames: Vec<Vec<u8>> = changes
        .into_iter()
        .map(|changes| DocumentMutation::Incremental(changes).encode().unwrap())
        .collect();
    let author = member_key().verifying_key().to_bytes();
    let draft = |view: &NativeCommittedView| {
        frames
            .iter()
            .zip(0..)
            .map(|(frame, drafted)| {
                let op = view.peer().draft_write(
                    (APP, ""),
                    None,
                    frame,
                    Some(&mut seal_delta(keys)),
                    author,
                    drafted,
                    now,
                )?;
                Ok(OwnerCommitRequest {
                    workspace: WORKSPACE.into(),
                    operation: CommitOperation::Document {
                        app: APP.into(),
                        document: String::new(),
                        envelope: Box::new(op),
                        readers: None,
                    },
                })
            })
            .collect()
    };
    owner
        .write(workspace, draft, &member_key(), now)
        .await
        .map(|committed| committed.len())
}

#[tokio::test]
async fn a_named_store_document_keeps_its_reader_scope_through_local_admission() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let readers = owner
        .with_current(workspace, |view| {
            Ok(
                collab_types::Readers::people([view.device().user_public_key])
                    .unwrap()
                    .text(),
            )
        })
        .unwrap();
    let document = format!("{}/notes.page#0", collab_types::reader_set_name(&readers));
    let scope = neighbour_wire::KeyScope::document(workspace, APP, &document, Some(&readers))
        .expect("the document binds the named reader set");
    let keys = key_group::KeyGroup::create(
        scope.group(),
        key_group::MemberKey::generate("member").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let mut initial = AutoCommit::new();
    initial.put(ROOT, "w", readers.as_str()).unwrap();
    initial.put(ROOT, "title", "Restricted notes").unwrap();
    let frame = DocumentMutation::Snapshot(initial.save()).encode().unwrap();
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, &document);
    let author = member_key().verifying_key().to_bytes();

    let draft = |view: &NativeCommittedView| {
        let envelope = view.peer().draft_write(
            (APP, &document),
            Some(&readers),
            &frame,
            Some(&mut |plaintext| {
                keys.seal_content(artifact, ContentRetention::Checkpoint, plaintext)
                    .map_err(|error| error.to_string())
            }),
            author,
            0,
            now,
        )?;
        Ok(vec![OwnerCommitRequest {
            workspace: WORKSPACE.into(),
            operation: CommitOperation::Document {
                app: APP.into(),
                document: document.clone(),
                envelope: Box::new(envelope),
                readers: Some(readers.clone()),
            },
        }])
    };
    assert_eq!(
        owner
            .write(workspace, draft, &member_key(), now)
            .await
            .unwrap()
            .len(),
        1
    );
    let messages = ownership(owner, workspace, now);
    assert!(messages.iter().any(|(_, message)| matches!(
        message,
        NeighbourMsg::AppDocument { document_id, readers: Some(named), envelope, .. }
            if document_id == &document && named == &readers && open(&keys, envelope).as_deref() == Ok(frame.as_slice())
    )));
}

/// What ownership holds, in the order it sends it.
fn ownership(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
) -> Vec<([u8; 32], NeighbourMsg)> {
    owner
        .with_outgoing(workspace, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| (entry.id(), postcard::from_bytes(entry.bytes()).unwrap()))
                .collect())
        })
        .unwrap()
}

/// The host keeps all of ownership durably: what it was sent, in order.
fn answer(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
    order: &mut u64,
) -> Vec<NeighbourMsg> {
    let held = ownership(owner, workspace, now);
    let answers = held
        .iter()
        .map(|(id, _)| {
            *order = order.checked_add(1).unwrap();
            (*id, HostAnswer::Durable(*order))
        })
        .collect();
    owner.retire_answered(workspace, &answers).unwrap();
    held.into_iter().map(|(_, message)| message).collect()
}

/// Five changes another member writes at `host` over `base`.
fn written_at(
    host: &mut crate::StatePeer,
    base: &[u8],
    keys: &ContentKeys,
    governor_key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> Vec<NeighbourMsg> {
    let mut peer = Typist::over(base, 2);
    keys_typed('b', 5)
        .map(|key| {
            host.publication_mut()
                .publish_sealed_incremental(
                    APP,
                    "",
                    peer.change(&key),
                    &mut seal_delta(keys),
                    governor_key,
                    now,
                )
                .unwrap()
        })
        .collect()
}

/// Everything ownership holds, as the host takes it from its head until it owes nothing: the
/// device drafts what it sends next before each pass, as its session does.
async fn drain(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
    order: &mut u64,
) -> Vec<NeighbourMsg> {
    let mut sent = Vec::new();
    loop {
        owner
            .compact_outgoing(workspace, &member_key(), now)
            .await
            .unwrap();
        let more = answer(owner, workspace, now, order);
        if more.is_empty() {
            return sent;
        }
        sent.extend(more);
    }
}

fn opened(owner: &NativeWorkspaceOwner, workspace: WorkspaceId, keys: &ContentKeys) -> AutoCommit {
    owner
        .with_current(workspace, |view| Ok(opened_in(view.peer(), keys)))
        .unwrap()
}

fn opened_in(peer: &crate::StatePeer, keys: &ContentKeys) -> AutoCommit {
    let record = peer
        .publication()
        .opened_document(APP, "", &mut |write| open(keys, write))
        .unwrap()
        .expect("the chat is held");
    assert!(record.unmerged.is_empty(), "every write merges");
    AutoCommit::load(&record.document).unwrap()
}

fn history(peer: &crate::StatePeer, keys: &ContentKeys) -> Vec<(u64, bool)> {
    let opened = peer
        .publication()
        .opened_history(APP, "", &mut |write| open(keys, write))
        .unwrap();
    opened
        .writes
        .iter()
        .map(|write| (write.seq, write.mutation.is_ok()))
        .collect()
}

fn typed(document: &AutoCommit) -> BTreeSet<String> {
    document
        .keys(ROOT)
        .filter(|key| key.starts_with('k') || key.starts_with('b'))
        .collect()
}

fn keys_typed(prefix: char, count: usize) -> impl Iterator<Item = String> {
    (0..count).map(move |key| format!("{prefix}{key}"))
}

/// Changes a device alone writes: past this crate's bound of sealed writes
/// per record four times over.
const ALONE: usize = 4 * 256 + 100;
/// Changes that reach the host before the device goes offline, and those it
/// writes offline, compacting once it holds three quarters of the bound.
const LINKED: usize = 180;
const OFFLINE: usize = 40;

#[tokio::test]
async fn a_device_alone_takes_every_change_to_one_sealed_record_past_its_bound() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;
    for key in keys_typed('k', ALONE) {
        let changes = typist.change(&key);
        assert_eq!(
            write(owner, workspace, &keys, changes, now).await,
            Ok(1),
            "{key} is taken"
        );
        answer(owner, workspace, now, &mut order);
    }
    let held = owner
        .with_current(workspace, |view| Ok(history(view.peer(), &keys)))
        .unwrap();
    assert!(
        held.len() < 256,
        "the history stays within its bound: {}",
        held.len()
    );

    let path = joined.path.clone();
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        typed(&opened(&reopened, workspace, &keys)),
        keys_typed('k', ALONE).collect(),
        "every change reads after a restart"
    );
}

#[tokio::test]
async fn a_device_that_compacted_offline_converges_with_a_peer_that_wrote_meanwhile() {
    let (fixture, mut host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let late = enrolled_with(fixture.clone());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let member = member_key().verifying_key().to_bytes();
    let governor = governor_key.verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;

    // Linked: every change reaches the host.
    for key in keys_typed('k', LINKED) {
        let changes = typist.change(&key);
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        for message in answer(owner, workspace, now, &mut order) {
            host.receive_from(TransportOrigin::ServerDelta(member), message, now)
                .unwrap();
        }
    }
    // Offline: the device goes on writing and compacts its own copy.
    for key in keys_typed('k', LINKED + OFFLINE).skip(LINKED) {
        let changes = typist.change(&key);
        assert_eq!(
            write(owner, workspace, &keys, changes, now).await,
            Ok(1),
            "{key} is taken offline"
        );
    }
    // Meanwhile the peer writes at the host.
    let peers = written_at(
        &mut host,
        &base(owner, workspace),
        &keys,
        &governor_key,
        now,
    );

    // Linked again: what each side holds reaches the other.
    let sent = drain(owner, workspace, now, &mut order).await;
    assert!(
        sent.iter().any(|message| matches!(message,
            NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)),
        "the device compacted while offline"
    );
    for message in sent.clone() {
        host.receive_from(TransportOrigin::ServerDelta(member), message, now)
            .unwrap();
    }
    for message in peers.clone() {
        owner
            .receive(
                workspace,
                TransportOrigin::ServerDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    let everything: BTreeSet<String> = keys_typed('k', LINKED + OFFLINE)
        .chain(keys_typed('b', 5))
        .collect();
    let mut device = opened(owner, workspace, &keys);
    let mut hosted = opened_in(&host, &keys);
    assert_eq!(typed(&device), everything, "the device holds every change");
    assert_eq!(typed(&hosted), everything, "the host holds every change");
    let (mut device_heads, mut host_heads) = (device.get_heads(), hosted.get_heads());
    device_heads.sort();
    host_heads.sort();
    assert_eq!(device_heads, host_heads, "both converge on one document");
    assert!(
        history(&host, &keys).len() < LINKED,
        "the host took the device's compaction in place of what it covers"
    );

    joins_after(&late, &sent, peers, &keys, &everything);
}

/// A sealed history just short of three quarters of its bound, where a device compacts it before
/// a write, and the writes a session then commits at once, together past the bound.
const BEFORE_FULL: usize = 256 / 4 * 3 - 1;
const AT_ONCE: usize = 80;

#[tokio::test]
async fn writes_past_a_full_sealed_history_land_after_its_compaction() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let held = || {
        owner
            .with_current(workspace, |view| Ok(history(view.peer(), &keys).len()))
            .unwrap()
    };
    let mut written = 0;
    while held() < BEFORE_FULL {
        let changes = typist.change(&format!("k{written}"));
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        written += 1;
    }
    assert_eq!(held(), BEFORE_FULL, "no compaction yet");

    // Held while the device was away, the writes go in one turn: none is refused or dropped.
    let changes: Vec<Vec<u8>> = keys_typed('k', written + AT_ONCE)
        .skip(written)
        .map(|key| typist.change(&key))
        .collect();
    assert_eq!(
        write_all(owner, workspace, &keys, changes, now).await,
        Ok(AT_ONCE),
        "every held write is taken"
    );
    assert_eq!(
        typed(&opened(owner, workspace, &keys)),
        keys_typed('k', written + AT_ONCE).collect(),
        "every change reads"
    );
    assert!(held() <= 256, "the history stays within its bound");
    // Ownership sends the compaction before the writes it made workspace for.
    let sent = drain(owner, workspace, now, &mut 0).await;
    let compaction = sent
        .iter()
        .position(|message| {
            matches!(message,
            NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
        })
        .expect("the full history was compacted");
    assert_eq!(
        sent.len() - compaction - 1,
        AT_ONCE,
        "the held writes follow the compaction"
    );
}

/// Channel keys that have not reached this device yet.
struct NotYet;

impl SealedHistoryKeys for NotYet {
    fn open(&self, _: WorkspaceId, _: &SignedOp) -> Result<Vec<u8>, String> {
        Err("this device holds no group keys yet".into())
    }

    fn seal_checkpoint(
        &self,
        _: WorkspaceId,
        _: &str,
        _: &str,
        _named: Option<&str>,
        _: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        Err("this device holds no group keys yet".into())
    }

    fn renew(&self, _: WorkspaceId, _: &SignedOp) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        Err("this device holds no group keys yet".into())
    }
}

/// A write that overflows a history not yet due waits for its compaction while the keys are on
/// their way, and lands with the next write once they are here: a compaction that failed for
/// want of them does not keep the record full until its history is due, which a full record
/// never becomes.
#[tokio::test]
async fn a_full_history_whose_first_compaction_failed_is_compacted_once_the_keys_are_here() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let held = || {
        owner
            .with_current(workspace, |view| Ok(history(view.peer(), &keys).len()))
            .unwrap()
    };
    let mut written = 0;
    while held() < BEFORE_FULL {
        let changes = typist.change(&format!("k{written}"));
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        written += 1;
    }
    let changes: Vec<Vec<u8>> = keys_typed('k', written + AT_ONCE)
        .skip(written)
        .map(|key| typist.change(&key))
        .collect();
    owner.keep_sealed_histories(Arc::new(NotYet));
    assert!(
        matches!(
            write_all(owner, workspace, &keys, changes.clone(), now).await,
            Err(WriteRefusal::HistoryFull { .. })
        ),
        "without the keys the history stays full"
    );
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    assert_eq!(
        write_all(owner, workspace, &keys, changes, now).await,
        Ok(AT_ONCE),
        "with the keys here, the history is compacted and the writes land"
    );
    assert!(held() <= 256, "the history stays within its bound");
}

/// A sealed write that reaches a peer after one of a later number is still history that peer
/// lacks: the document keeps it, so a compaction its author drafts over its own history covers
/// exactly what the peer holds.
#[tokio::test]
async fn a_sealed_write_that_arrives_after_a_later_one_is_kept() {
    let (fixture, mut peer, _) = governed_fixture_at(tilo_clock::wall());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;
    let mut sent = Vec::new();
    for key in keys_typed('k', 3) {
        let changes = typist.change(&key);
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        sent.extend(answer(owner, workspace, now, &mut order));
    }
    // The peer takes the first, then the third, then the second: a sealed history is a log, and
    // a replica catching up can hand it a write after one of a later number.
    assert!(taken(&mut peer, &sent[0], now));
    assert!(taken(&mut peer, &sent[2], now));
    assert!(taken(&mut peer, &sent[1], now));
    assert_eq!(
        peer.publication().sealed_history(APP, "").len(),
        3,
        "a write that reached the peer after a later one is still kept"
    );
    // The device compacts the history, and the peer takes the compaction: it covers exactly
    // the writes the peer holds.
    assert_eq!(
        owner
            .compact_document(workspace, (APP, ""), &member_key(), now)
            .await,
        Ok(1)
    );
    for message in answer(owner, workspace, now, &mut order) {
        assert!(
            taken(&mut peer, &message, now),
            "the peer takes the compaction"
        );
    }
    assert_eq!(
        opened(owner, workspace, &keys).get_heads(),
        opened_in(&peer, &keys).get_heads(),
        "the peer reads every change the device made"
    );
}

/// Whether `peer` takes `message` as a write of its author.
fn taken(peer: &mut crate::StatePeer, message: &NeighbourMsg, now: SystemTime) -> bool {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document write");
    };
    peer.receive_from(
        TransportOrigin::ServerDelta(*envelope.author.as_bytes()),
        message.clone(),
        now,
    )
    .is_ok()
}

/// Writes a device makes while its host is away: ownership keeps every one of them, past what
/// it keeps in its record (eight writes here) and the record's sealed history bound many times.
const AWAY: usize = 2_000;

#[tokio::test]
async fn a_device_away_keeps_every_write_through_a_restart_and_all_converge_on_return() {
    let (fixture, mut host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let other = enrolled_with(fixture.clone());
    let joined = enrolled_with(fixture);
    let (workspace, now, path) = (joined.workspace, joined.now, joined.path.clone());
    let member = member_key().verifying_key().to_bytes();
    let governor = governor_key.verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    joined
        .owner
        .keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(&joined.owner, workspace), 1);
    let mut order = 0;
    // What the host took from the device, in its order.
    let mut relayed = Vec::new();
    for key in keys_typed('k', LINKED) {
        let changes = typist.change(&key);
        assert_eq!(
            write(&joined.owner, workspace, &keys, changes, now).await,
            Ok(1)
        );
        for message in answer(&joined.owner, workspace, now, &mut order) {
            host.receive_from(TransportOrigin::ServerDelta(member), message.clone(), now)
                .unwrap();
            relayed.push(message);
        }
    }
    // Away: every write is taken, and kept as it is taken.
    for key in keys_typed('k', LINKED + AWAY).skip(LINKED) {
        let changes = typist.change(&key);
        assert_eq!(
            write(&joined.owner, workspace, &keys, changes, now).await,
            Ok(1),
            "{key} is taken while the host is away"
        );
    }
    let owed = joined.owner.unadmitted_writes(workspace).unwrap();
    assert!(owed >= AWAY, "ownership owes every write: {owed}");
    drop(joined.owner);
    let owner = open_file(&path).unwrap();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    assert_eq!(
        owner.unadmitted_writes(workspace).unwrap(),
        owed,
        "ownership keeps them all through a restart"
    );
    // Meanwhile another member writes at the host.
    let peers = written_at(
        &mut host,
        &base(&owner, workspace),
        &keys,
        &governor_key,
        now,
    );

    // Back: ownership goes to the host in its order until it owes nothing.
    for message in drain(&owner, workspace, now, &mut order).await {
        host.receive_from(TransportOrigin::ServerDelta(member), message.clone(), now)
            .unwrap();
        relayed.push(message);
    }
    assert_eq!(owner.unadmitted_writes(workspace).unwrap(), 0);
    for message in peers.clone() {
        owner
            .receive(
                workspace,
                TransportOrigin::ServerDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    // Another device of the workspace takes what the host relays.
    relay(&other, relayed.into_iter().chain(peers));
    let everything: BTreeSet<String> = keys_typed('k', LINKED + AWAY)
        .chain(keys_typed('b', 5))
        .collect();
    converge(
        [
            ("the device", opened(&owner, workspace, &keys)),
            ("the host", opened_in(&host, &keys)),
            (
                "the other device",
                opened(&other.owner, other.workspace, &keys),
            ),
        ],
        &everything,
    );
}

/// How many of the documents its host relays a member's session keeps in one owner turn here.
const RELAYED_AT_ONCE: usize = 256;

/// `member` takes `messages` as its host relays them, the way its session keeps the documents
/// that arrived at once: one owner turn for a run of them
/// ([`NativeWorkspaceOwner::receive_batch`], which [`NativeWorkspaceOwner::receive`] is for a
/// run of one), each decided on its own, and every one of them taken. A compaction arrives
/// alone, as every document did when each was taken in a turn of its own: the member's record
/// is committed as the writes before the compaction left it, and again as the compaction left
/// it in place of them, before a write after it is taken.
fn relay(member: &Enrolled, messages: impl IntoIterator<Item = NeighbourMsg>) {
    let mut relayed = messages.into_iter().peekable();
    let mut taken = 0_usize;
    while let Some(first) = relayed.next() {
        let alone = is_compaction(&first);
        let mut run = vec![first];
        while !alone && run.len() < RELAYED_AT_ONCE {
            let Some(next) = relayed.next_if(|next| !is_compaction(next)) else {
                break;
            };
            run.push(next);
        }
        let sent = run.len();
        let arrived = run
            .into_iter()
            .map(|message| {
                let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                    panic!("a document write");
                };
                let author = TransportOrigin::RetainedServerDelta(*envelope.author.as_bytes());
                (author, message)
            })
            .collect();
        let reports = member
            .owner
            .receive_batch(member.workspace, arrived, member.now)
            .unwrap();
        assert_eq!(reports.len(), sent, "one answer for each document");
        for report in reports {
            if let Err(error) = report {
                panic!("the member took {taken} relayed documents and refused the next: {error:?}");
            }
            taken = taken.saturating_add(1);
        }
    }
}

/// A run of relayed documents taken in one owner turn is decided as each would be alone: the
/// answers come in the documents' order, a document the member refuses changes nothing of those
/// around it, and the member reads the record as one that took the same documents one at a time
/// does. The run is committed once and a document taken alone once; a run of none, and one of
/// nothing the member takes, are not committed at all.
#[tokio::test]
async fn a_run_of_relayed_documents_is_decided_as_each_would_be_alone() {
    let (fixture, mut host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let alone = enrolled_with(fixture.clone());
    let together = enrolled_with(fixture);
    let (_, keys) = rotated_channel();
    let origin = TransportOrigin::RetainedServerDelta(governor_key.verifying_key().to_bytes());
    let mut relayed = written_at(
        &mut host,
        &base(&alone.owner, alone.workspace),
        &keys,
        &governor_key,
        alone.now,
    );
    // The third write arrives damaged on its way, and then as its author signed it.
    let mut damaged = relayed[2].clone();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut damaged else {
        panic!("a document write");
    };
    envelope.signature.0[0] ^= 1;
    relayed.insert(2, damaged.clone());
    let generation = |member: &Enrolled| {
        member
            .owner
            .with_current(member.workspace, |view| Ok(view.generation()))
            .unwrap()
    };
    let (before_alone, before_together) = (generation(&alone), generation(&together));

    let none = together
        .owner
        .receive_batch(together.workspace, Vec::new(), together.now)
        .unwrap();
    assert!(none.is_empty(), "no document, no answer");
    let refused = together
        .owner
        .receive_batch(together.workspace, vec![(origin, damaged)], together.now)
        .unwrap();
    assert!(
        matches!(refused.as_slice(), [Err(_)]),
        "a damaged document is refused"
    );
    assert_eq!(
        generation(&together),
        before_together,
        "a run the member takes nothing of is not committed"
    );

    let one_by_one: Vec<bool> = relayed
        .iter()
        .map(|message| {
            alone
                .owner
                .receive(alone.workspace, origin, message.clone(), alone.now)
                .is_ok()
        })
        .collect();
    let run = relayed.into_iter().map(|message| (origin, message));
    let at_once: Vec<bool> = together
        .owner
        .receive_batch(together.workspace, run.collect(), together.now)
        .unwrap()
        .iter()
        .map(Result::is_ok)
        .collect();
    assert_eq!(
        one_by_one,
        [true, true, false, true, true, true],
        "only the damaged document is refused"
    );
    assert_eq!(
        at_once, one_by_one,
        "a run is answered document by document, in their order"
    );
    assert_eq!(
        generation(&alone),
        before_alone.checked_add(5).unwrap(),
        "each document taken alone is committed alone, the refused one not at all"
    );
    assert_eq!(
        generation(&together),
        before_together.checked_add(1).unwrap(),
        "the run is committed once"
    );
    let everything: BTreeSet<String> = keys_typed('b', 5).collect();
    let read = |member: &Enrolled| opened(&member.owner, member.workspace, &keys);
    converge(
        [
            ("the member that took each alone", read(&alone)),
            ("the member that took the run", read(&together)),
        ],
        &everything,
    );
}

/// Every one of `documents` holds every change of `everything`, and all are one document.
fn converge<const N: usize>(documents: [(&str, AutoCommit); N], everything: &BTreeSet<String>) {
    let mut heads = Vec::new();
    for (holder, mut document) in documents {
        assert_eq!(typed(&document), *everything, "{holder} holds every change");
        let mut held = document.get_heads();
        held.sort();
        heads.push(held);
    }
    assert!(
        heads.windows(2).all(|pair| pair[0] == pair[1]),
        "all converge on one document"
    );
}

/// A member that joins after the compaction is handed the compacted history,
/// `sent` from its compaction on and the `peers`' writes, and reads the whole
/// record.
fn joins_after(
    late: &Enrolled,
    sent: &[NeighbourMsg],
    peers: Vec<NeighbourMsg>,
    keys: &ContentKeys,
    everything: &BTreeSet<String>,
) {
    let compaction = sent
        .iter()
        .position(|message| {
            matches!(message,
            NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
        })
        .unwrap();
    for message in sent[compaction..].iter().cloned().chain(peers) {
        let NeighbourMsg::AppDocument { envelope, .. } = &message else {
            panic!("a document write");
        };
        let author = *envelope.author.as_bytes();
        late.owner
            .receive(
                late.workspace,
                TransportOrigin::RetainedServerDelta(author),
                message,
                late.now,
            )
            .unwrap();
    }
    assert_eq!(
        typed(&opened(&late.owner, late.workspace, keys)),
        *everything,
        "a member joining after the compaction reads the whole record"
    );
}

/// The record's writers' compaction of `host`'s history, as the workspace's governor
/// compacts it at the host.
fn compacted_at(
    host: &mut crate::StatePeer,
    keys: &ContentKeys,
    governor_key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let held = Held(keys.clone());
    let mut op = host
        .publication()
        .draft_sealed_compaction(
            (APP, ""),
            None,
            None,
            &mut |write| open(keys, write),
            &mut |snapshot| {
                held.seal_checkpoint(
                    WorkspaceId::parse(WORKSPACE).unwrap(),
                    APP,
                    "",
                    None,
                    snapshot,
                )
            },
            governor_key.verifying_key().to_bytes(),
            now,
        )
        .unwrap();
    for op in &mut op {
        collab_types::sign_op(op, governor_key).unwrap();
    }
    let [compaction]: [NeighbourMsg; 1] = host
        .publication_mut()
        .admit_sealed_compaction(APP, "", None, op, now)
        .unwrap()
        .try_into()
        .unwrap();
    compaction
}

/// The host's side of the link: the history it holds, what it relayed in its order, the most
/// writes its history of the chat ever held, and the most runs the device kept what it knows of
/// that history in.
struct Host {
    peer: crate::StatePeer,
    relayed: Vec<NeighbourMsg>,
    longest: usize,
    mirrored_runs: usize,
}

impl Host {
    fn new(peer: crate::StatePeer) -> Self {
        Self {
            peer,
            relayed: Vec::new(),
            longest: 0,
            mirrored_runs: 0,
        }
    }

    /// Take `message` as the host does, or refuse it.
    fn take(&mut self, message: &NeighbourMsg, now: SystemTime) -> bool {
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("a document write");
        };
        let origin = TransportOrigin::ServerDelta(*envelope.author.as_bytes());
        let taken = self.peer.receive_from(origin, message.clone(), now).is_ok();
        if taken {
            self.relayed.push(message.clone());
            self.longest = self
                .longest
                .max(self.peer.publication().sealed_history(APP, "").len());
        }
        taken
    }
}

/// Ownership goes to the host one write at a time, each answered as the host took it, until it
/// owes nothing; the device drafts what it sends next before each, as its session does, and
/// `refuse` picks writes the host refuses whatever they are. Answers what the host refused;
/// with `until_refused`, as soon as it refused one.
async fn delivered(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    host: &mut Host,
    (now, order, until_refused): (SystemTime, &mut u64, bool),
    mut refuse: impl FnMut(&NeighbourMsg) -> bool,
) -> Vec<NeighbourMsg> {
    let mut refused = Vec::new();
    for _ in 0..4096 {
        owner
            .compact_outgoing(workspace, &member_key(), now)
            .await
            .unwrap();
        let Some((id, message)) = ownership(owner, workspace, now).into_iter().next() else {
            return refused;
        };
        let answer = if !refuse(&message) && host.take(&message, now) {
            *order = order.checked_add(1).unwrap();
            HostAnswer::Durable(*order)
        } else {
            refused.push(message);
            HostAnswer::Refused
        };
        owner
            .retire_answered(workspace, &BTreeMap::from([(id, answer)]))
            .unwrap();
        let (in_resource, runs) = owner
            .with_current(workspace, |view| {
                Ok(crate::owner::publication::hosted_in_document(view))
            })
            .unwrap();
        assert!(
            in_resource < crate::owner::publication::MAX_ENTRIES,
            "the record holds {in_resource} of the host's writes itself"
        );
        host.mirrored_runs = host.mirrored_runs.max(runs);
        if until_refused && !refused.is_empty() {
            return refused;
        }
    }
    panic!(
        "ownership still owes {} writes, {} refused",
        owner.unadmitted_writes(workspace).unwrap(),
        refused.len()
    );
}

fn is_compaction(message: &NeighbourMsg) -> bool {
    matches!(message,
        NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
}

/// The device reads every key of `written`, with no write of its history left out.
fn holds_every_write(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    written: &BTreeSet<String>,
    when: &str,
) {
    let record = owner
        .with_current(workspace, |view| {
            Ok(view
                .peer()
                .publication()
                .opened_document(APP, "", &mut |write| open(keys, write))
                .unwrap()
                .unwrap())
        })
        .unwrap();
    assert!(
        record.unmerged.is_empty(),
        "{when}: {} writes left out, first {:?}",
        record.unmerged.len(),
        record.unmerged.first().map(ToString::to_string)
    );
    let held = typed(&AutoCommit::load(&record.document).unwrap());
    let lost: Vec<&String> = written.difference(&held).collect();
    assert!(lost.is_empty(), "{when}: the device lost {lost:?}");
}

/// The compactions the host refused, and the most runs the host's writes took beside the record.
type Returned = (Vec<NeighbourMsg>, usize);

/// How a device's time away from its host goes.
struct Away {
    /// Writes it makes while away.
    writes: usize,
    /// Whether another member compacts the host's history meanwhile.
    peer_compacts: bool,
    /// Whether the peer's writes and compaction reach the device before it sends again; else
    /// they reach it once the host refused something it sent.
    peers_first: bool,
}

/// A device writes linked, then `away.writes` while its host is away, while a peer writes (and
/// compacts) at the host; back, it sends everything, `refuse` picking writes the host refuses
/// whatever they are, then writes linked past the record's bound. Every member ends up reading
/// every change, and the host never refuses one of the device's writes nor holds more than the
/// record's bound, nor does the device's record hold the host's writes it keeps beside it. Answers
/// the compactions the host refused, and the most runs those writes took.
async fn returns(away: Away, mut refuse: impl FnMut(&NeighbourMsg) -> bool) -> Returned {
    let (fixture, host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let mut host = Host::new(host);
    let other = enrolled_with(fixture.clone());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let governor = governor_key.verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;
    let mut written = BTreeSet::new();
    let mut refused = Vec::new();
    let mut deliver = async |host: &mut Host, order: &mut u64, until_refused: bool| {
        let back = delivered(
            owner,
            workspace,
            host,
            (now, order, until_refused),
            &mut refuse,
        )
        .await;
        assert!(
            back.iter().all(is_compaction),
            "the host refuses none of the device's writes"
        );
        back
    };
    let after = LINKED.checked_add(away.writes).unwrap();
    for (index, key) in keys_typed('k', after.checked_add(2 * 256).unwrap()).enumerate() {
        if index == after {
            // Meanwhile a peer writes at the host, and compacts its history.
            let mut peers = written_at(
                &mut host.peer,
                &base(owner, workspace),
                &keys,
                &governor_key,
                now,
            );
            if away.peer_compacts {
                peers.push(compacted_at(&mut host.peer, &keys, &governor_key, now));
            }
            host.relayed.extend(peers.iter().cloned());
            let peers_reach = |owner: &NativeWorkspaceOwner, peers: Vec<NeighbourMsg>| {
                for message in peers {
                    owner
                        .receive(
                            workspace,
                            TransportOrigin::ServerDelta(governor),
                            message,
                            now,
                        )
                        .unwrap();
                }
            };
            if !away.peers_first {
                refused.extend(deliver(&mut host, &mut order, true).await);
            }
            peers_reach(owner, peers);
            refused.extend(deliver(&mut host, &mut order, false).await);
            written.extend(keys_typed('b', 5));
            holds_every_write(owner, workspace, &keys, &written, "back");
        }
        let changes = typist.change(&key);
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        written.insert(key);
        if !(LINKED..after).contains(&index) {
            refused.extend(deliver(&mut host, &mut order, false).await);
        }
    }
    assert!(
        host.longest < 256,
        "the host's history stays within its bound: {}",
        host.longest
    );
    relay(&other, host.relayed.clone());
    converge(
        [
            ("the device", opened(owner, workspace, &keys)),
            ("the host", opened_in(&host.peer, &keys)),
            (
                "the other device",
                opened(&other.owner, other.workspace, &keys),
            ),
        ],
        &written,
    );
    let path = joined.path.clone();
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        typed(&opened(&reopened, workspace, &keys)),
        written,
        "the device reads every change after a restart"
    );
    (refused, host.mirrored_runs)
}

/// Writes a device makes offline in the time a peer compacts the host's history: enough for
/// the device to compact its own copy meanwhile.
const OVERTAKEN: usize = 200;

#[tokio::test]
async fn a_device_whose_offline_compactions_a_peers_compaction_overtook_sends_them_drafted_anew() {
    let away = Away {
        writes: OVERTAKEN,
        peer_compacts: true,
        peers_first: true,
    };
    assert!(
        returns(away, |_| false).await.0.is_empty(),
        "the host takes every compaction"
    );
}

#[tokio::test]
async fn a_backlog_past_the_bound_after_a_peers_compaction_reaches_the_host_whole() {
    let away = Away {
        writes: 3 * 256,
        peer_compacts: true,
        peers_first: true,
    };
    assert!(
        returns(away, |_| false).await.0.is_empty(),
        "the host takes every compaction"
    );
}

#[tokio::test]
async fn a_compaction_refused_amid_a_backlog_goes_again_before_the_hosts_history_fills() {
    // The host refuses the second compaction of a backlog several times the record's bound:
    // the device's next one would come most of a bound later.
    let mut compactions = 0;
    let away = Away {
        writes: 3 * 256,
        peer_compacts: false,
        peers_first: true,
    };
    let (refused, _) = returns(away, |message| {
        is_compaction(message) && {
            compactions += 1;
            compactions == 2
        }
    })
    .await;
    assert_eq!(refused.len(), 1, "the host refused one compaction");
}

#[tokio::test]
async fn what_a_device_knows_of_its_hosts_history_stays_beside_its_record() {
    // The peer's writes at the host, before the device folded them, are what the device knows
    // of the host's history while its backlog drains.
    let away = Away {
        writes: 3 * 256,
        peer_compacts: false,
        peers_first: true,
    };
    let (refused, runs) = returns(away, |_| false).await;
    assert!(refused.is_empty(), "the host takes every compaction");
    assert!(runs > 0, "the host's writes went to runs beside the record");
}

#[tokio::test]
async fn a_compaction_the_host_refused_leaves_the_device_in_step() {
    // The peer's compaction reaches the device only after the device sent its own, drafted
    // against the history before it: the host refuses that one.
    let away = Away {
        writes: OVERTAKEN,
        peer_compacts: true,
        peers_first: false,
    };
    let (raced, _) = returns(away, |_| false).await;
    assert!(
        !raced.is_empty(),
        "the host refused a compaction drafted before the peer's"
    );
    // The host refuses the device's first compaction whatever it is.
    let mut first = true;
    let away = Away {
        writes: 0,
        peer_compacts: false,
        peers_first: true,
    };
    let (refused, _) = returns(away, |message| {
        is_compaction(message) && std::mem::take(&mut first)
    })
    .await;
    assert_eq!(refused.len(), 1, "the host refused one compaction");
}

/// A record's history holds one checkpoint however often its host asks for one: the member it
/// asks compacts the history, and every replica takes that in place of exactly the writes it
/// covers. Edited well past the bound of a sealed history, the host's stored copy stays a
/// fraction of it, takes every write, and a member that joins late reads the record with every
/// change ever made to it.
#[tokio::test]
async fn a_record_its_host_checkpoints_stays_bounded_past_its_bound_of_writes() {
    /// What a sealed history may hold where it is admitted.
    const BOUND: usize = 16 * 1024 * 1024;
    /// Writes of a value an app keeps rewriting, as a document's body is.
    const WRITES: usize = 1100;
    const VALUE: usize = 16 * 1024;
    /// Writes between two checkpoints the host asks for.
    const ASKED: usize = 50;
    let (fixture, mut host, _) = governed_fixture_at(tilo_clock::wall());
    let late = enrolled_with(fixture.clone());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let member = member_key().verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let (mut order, mut written, mut stored) = (0, 0, 0);
    // What the host serves a member joining: its last checkpoint and the writes since.
    let mut served: Vec<NeighbourMsg> = Vec::new();
    for index in 0..WRITES {
        let value = format!("{index:08}").repeat(VALUE / 8);
        let changes = typist.put(&format!("k{}", index % 8), &value);
        written += changes.len();
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
        let asked = index % ASKED == ASKED - 1;
        if asked {
            assert_eq!(
                owner
                    .compact_document(workspace, (APP, ""), &member_key(), now)
                    .await,
                Ok(1),
                "a member holding the keys compacts the record"
            );
        }
        for message in answer(owner, workspace, now, &mut order) {
            host.receive_from(TransportOrigin::ServerDelta(member), message.clone(), now)
                .unwrap_or_else(|error| panic!("the host takes write {index}: {error}"));
            if matches!(&message,
                NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
            {
                served.clear();
            }
            served.push(message);
        }
        if asked {
            stored = stored.max(host.durable().unwrap().len());
        }
    }
    assert!(written > BOUND, "{written} bytes written");
    assert!(
        stored < BOUND / 4,
        "the host stores {stored} bytes of a record written {written} bytes"
    );
    assert!(history(&host, &keys).len() <= ASKED + 1);
    for message in served {
        let NeighbourMsg::AppDocument { envelope, .. } = &message else {
            panic!("a document write");
        };
        let author = TransportOrigin::RetainedServerDelta(*envelope.author.as_bytes());
        late.owner
            .receive(late.workspace, author, message, late.now)
            .unwrap();
    }
    let mut read = opened(&late.owner, late.workspace, &keys);
    let held: BTreeSet<ChangeHash> = read
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    let every: BTreeSet<ChangeHash> = typist
        .0
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    assert!(
        every.is_subset(&held),
        "a member joining late holds every change ever made"
    );
    assert_eq!(read.get_heads(), typist.0.get_heads());
}

/// A write the host refused leaves the device's record, and the device's next write of it goes
/// out: it takes a sequence past every write of the device a peer's compaction folded, not one
/// the record decided already.
#[tokio::test]
async fn a_refused_write_after_a_peers_fold_leaves_the_next_write_free() {
    let (fixture, host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let mut host = Host::new(host);
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let governor = governor_key.verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;
    for key in keys_typed('k', 3) {
        let changes = typist.change(&key);
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    }
    let refused = delivered(
        owner,
        workspace,
        &mut host,
        (now, &mut order, false),
        |_| false,
    )
    .await;
    assert!(refused.is_empty());
    // The governor folds the record's history, the device's writes with it.
    let compaction = compacted_at(&mut host.peer, &keys, &governor_key, now);
    owner
        .receive(
            workspace,
            TransportOrigin::ServerDelta(governor),
            compaction,
            now,
        )
        .unwrap();
    // The host refuses the device's next write.
    let changes = typist.change("krefused");
    assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    let [(id, _)]: [_; 1] = ownership(owner, workspace, now).try_into().unwrap();
    owner
        .retire_answered(workspace, &BTreeMap::from([(id, HostAnswer::Refused)]))
        .unwrap();
    // The device goes on writing, and the host takes it.
    let mut typist = Typist::over(&opened(owner, workspace, &keys).save(), 1);
    let changes = typist.change("kafter");
    assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    let relayed = host.relayed.len();
    let refused = delivered(
        owner,
        workspace,
        &mut host,
        (now, &mut order, false),
        |_| false,
    )
    .await;
    assert!(refused.is_empty(), "the host refused {}", refused.len());
    assert_eq!(
        host.relayed.len(),
        relayed + 1,
        "the host took the next write"
    );

    // A record kept with the device's floor under the writes a compaction folded, as a refusal
    // left it before, opens again with its floor back at the fold, and takes the device's next
    // write.
    let compaction = compacted_at(&mut host.peer, &keys, &governor_key, now);
    owner
        .receive(
            workspace,
            TransportOrigin::ServerDelta(governor),
            compaction,
            now,
        )
        .unwrap();
    let changes = typist.change("kgone");
    assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    let [(id, _)]: [_; 1] = ownership(owner, workspace, now).try_into().unwrap();
    owner
        .retire_answered(workspace, &BTreeMap::from([(id, HostAnswer::Refused)]))
        .unwrap();
    let device = member_key().verifying_key().to_bytes();
    owner
        .replica(workspace)
        .unwrap()
        .lock()
        .forget_floor(APP, "", device);
    owner.persist(workspace).unwrap();
    let path = joined.path.clone();
    drop(joined.owner);
    let owner = &open_file(&path).unwrap();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&opened(owner, workspace, &keys).save(), 1);
    let changes = typist.change("kreopened");
    assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    let refused = delivered(
        owner,
        workspace,
        &mut host,
        (now, &mut order, false),
        |_| false,
    )
    .await;
    assert!(refused.is_empty(), "the host refused {}", refused.len());
    assert_eq!(
        host.relayed.len(),
        relayed + 2,
        "the host took the write after a restart"
    );
}

/// A write the device renewed before its server took it is not one the server holds: it leaves
/// the document the renewal takes its place in, so a compaction covers exactly what the server
/// has.
#[tokio::test]
async fn a_compaction_covers_no_write_the_server_has_not_taken() {
    let (fixture, served, _) = governed_fixture_at(tilo_clock::wall());
    let mut served = Host::new(served);
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let mut order = 0;
    let changes = typist.change("k0");
    assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    let refused = delivered(
        owner,
        workspace,
        &mut served,
        (now, &mut order, false),
        |_| false,
    )
    .await;
    assert!(refused.is_empty(), "the server refused {}", refused.len());
    // Two writes the device renews before its server takes them: their epoch closes first.
    for key in ["k1", "k2"] {
        let changes = typist.change(key);
        assert_eq!(write(owner, workspace, &keys, changes, now).await, Ok(1));
    }
    let later = now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    assert_eq!(
        owner
            .renew_outgoing(workspace, &member_key(), later)
            .await
            .unwrap(),
        2,
        "both writes closed before their server took them"
    );
    let peers = owner.replica(workspace).unwrap();
    let held: Vec<u64> = history(&peers.lock(), &keys)
        .into_iter()
        .map(|(seq, _)| seq)
        .collect();
    drop(peers);
    assert_eq!(held, vec![1, 4, 5], "the document holds no renewed write");
    let compacted = owner
        .compact_document(workspace, (APP, ""), &member_key(), later)
        .await
        .unwrap();
    assert_eq!(compacted, 1, "the document is compacted");
    let refused = delivered(
        owner,
        workspace,
        &mut served,
        (later, &mut order, false),
        |_| false,
    )
    .await;
    assert!(refused.is_empty(), "the server refused {}", refused.len());
    holds_every_write(
        owner,
        workspace,
        &keys,
        &keys_typed('k', 3).collect(),
        "after a renewal",
    );
}

#[tokio::test]
async fn a_write_the_holder_signed_and_admitted_is_published_once() {
    // A holder signs its device's write and admits it into the replica the owner keeps: what a
    // page does over its own owner. Publishing it takes it into ownership as it is, once.
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let key = member_key();
    let author = key.verifying_key().to_bytes();
    let mut document = AutoCommit::new();
    document.put(ROOT, "title", "Published once").unwrap();
    let frame = DocumentMutation::Snapshot(document.save())
        .encode()
        .expect("the founding frame encodes");
    let request = owner
        .with_current(workspace, |view| {
            let mut op = view
                .peer()
                .draft_write((APP, ""), None, &frame, None, author, 0, now)?;
            collab_types::sign_op(&mut op, &key).map_err(|error| error.to_string())?;
            view.replica().write(|peer| {
                peer.admit_new_local_document(APP, "", op.clone(), now)
                    .map_err(|refused| refused.to_string())
            })?;
            Ok(OwnerCommitRequest {
                workspace: workspace.to_string(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    document: String::new(),
                    envelope: Box::new(op),
                    readers: None,
                },
            })
        })
        .expect("the replica holds this device's write");
    let committed = owner
        .commit_admitted(workspace, vec![request.clone()], now)
        .expect("the write is published");
    assert_eq!(
        committed.len(),
        1,
        "the write is published as it was signed"
    );
    assert_eq!(
        ownership(owner, workspace, now).len(),
        1,
        "ownership holds it once"
    );
    // Published again, it is appended no second time: one write, one publication.
    let again = owner
        .commit_admitted(workspace, vec![request], now)
        .expect("publishing it again is harmless");
    assert!(again.is_empty(), "nothing is published twice");
    assert_eq!(
        ownership(owner, workspace, now).len(),
        1,
        "still one publication"
    );
}

/// A device holding its record's current epoch, and not the earlier epoch a compaction is
/// sealed under: `here` says the earlier key has arrived.
struct OlderEpoch {
    before: ContentKeys,
    after: ContentKeys,
    here: std::sync::atomic::AtomicBool,
}

impl OlderEpoch {
    fn open(&self, write: &SignedOp) -> Result<Vec<u8>, String> {
        if self.here.load(std::sync::atomic::Ordering::Relaxed)
            && let Ok(plaintext) = open(&self.before, write)
        {
            return Ok(plaintext);
        }
        open(&self.after, write)
    }
}

impl SealedHistoryKeys for OlderEpoch {
    fn open(&self, _: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String> {
        OlderEpoch::open(self, write)
    }

    fn seal_checkpoint(
        &self,
        _: WorkspaceId,
        app: &str,
        record: &str,
        _named: Option<&str>,
        snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        self.after
            .seal_content(
                collab_types::document_artifact_id(app, WORKSPACE, record),
                ContentRetention::of_frame(snapshot),
                snapshot,
            )
            .map_err(|error| error.to_string())
    }

    fn renew(
        &self,
        _: WorkspaceId,
        write: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let plaintext = OlderEpoch::open(self, write)?;
        self.after
            .seal_content(write.artifact, ContentRetention::Delta, &plaintext)
            .map_err(|error| error.to_string())
    }
}

/// A compaction sealed under an epoch the device holds no key for is no refusal: it is kept in
/// the record, survives a restart, and is taken when that epoch's key arrives.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the older epoch, the kept compaction, its restart and the key's arrival"
)]
async fn a_compaction_sealed_under_an_epoch_the_device_lacks_is_kept_and_taken_when_it_arrives() {
    let (fixture, mut host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let copy = enrolled_with(fixture);
    let (owner, workspace, now) = (&copy.owner, copy.workspace, copy.now);
    let channel = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let mut group =
        key_group::KeyGroup::create(channel, key_group::MemberKey::generate("governor").unwrap())
            .unwrap();
    let before = group.content_keys().unwrap();
    // A device let into the channel after it moved on: it holds the epoch it joined at, and no
    // older one.
    let joiner = key_group::MemberKey::generate("joiner").unwrap();
    let welcome = group
        .add_member(joiner.fresh_key_package().unwrap())
        .unwrap();
    let after = key_group::KeyGroup::process_welcome(channel, &welcome, joiner)
        .unwrap()
        .content_keys()
        .unwrap();
    assert_ne!(before.epoch(), after.epoch(), "the channel moved on");
    let public_key = governor_key.verifying_key().to_bytes();

    // The host's own device writes sealed under the older epoch, and compacts what it wrote.
    let older = written_at(
        &mut host,
        &base(owner, workspace),
        &before,
        &governor_key,
        now,
    );
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, "");
    let mut ops = host
        .publication()
        .draft_sealed_compaction(
            (APP, ""),
            None,
            None,
            &mut |write: &SignedOp| open(&before, write),
            &mut |frame: &[u8]| {
                before
                    .seal_content(artifact, ContentRetention::of_frame(frame), frame)
                    .map_err(|error| error.to_string())
            },
            public_key,
            now,
        )
        .unwrap();
    for op in &mut ops {
        collab_types::sign_op(op, &governor_key).unwrap();
    }
    let compaction = host
        .publication_mut()
        .admit_sealed_compaction(APP, "", None, ops, now)
        .unwrap()
        .into_iter()
        .next()
        .expect("a compaction message");
    assert!(
        older
            .iter()
            .all(|message| matches!(message, NeighbourMsg::AppDocument { .. })),
        "the writes it folds are document writes"
    );

    // The copy holds its record's current epoch and a write of its own, and not the older
    // epoch the compaction is sealed under.
    owner.keep_sealed_histories(Arc::new(OlderEpoch {
        before: before.clone(),
        after: after.clone(),
        here: std::sync::atomic::AtomicBool::new(false),
    }));
    let mut typist = Typist::over(&base(owner, workspace), 1);
    let current = typist.change("k0");
    assert_eq!(write(owner, workspace, &after, current, now).await, Ok(1));
    let report = owner
        .receive(
            workspace,
            TransportOrigin::RetainedServerDelta(public_key),
            compaction,
            now,
        )
        .expect("the compaction is kept, not refused");
    assert_eq!(
        report.primary(),
        &crate::InputDisposition::RetryLater(crate::MissingPrerequisite::PersistedMls),
        "the device holds it until the epoch's key is here"
    );

    // The copy restarts, and the kept compaction is still its record's.
    let path = copy.path.clone();
    drop(copy.owner);
    let reopened = open_file(&path).unwrap();
    // The earlier epoch's key arrives.
    reopened.keep_sealed_histories(Arc::new(OlderEpoch {
        before: before.clone(),
        after: after.clone(),
        here: std::sync::atomic::AtomicBool::new(true),
    }));
    reopened.retry_kept_compactions(workspace);
    let history = reopened
        .with_current(workspace, |view| {
            Ok(view.peer().publication().sealed_history(APP, ""))
        })
        .unwrap();
    assert_eq!(
        history.first().map(|op| op.kind),
        Some(OpKind::Compaction),
        "the compaction the record kept leads its history after the restart"
    );
    let document = reopened
        .with_current(workspace, |view| {
            let opened = view
                .peer()
                .publication()
                .opened_document(APP, "", &mut |write| {
                    open(&before, write).or_else(|_| open(&after, write))
                })?
                .expect("the chat is held");
            assert!(opened.unmerged.is_empty(), "every write merges");
            Ok(AutoCommit::load(&opened.document).unwrap())
        })
        .unwrap();
    let read: BTreeSet<String> = document
        .keys(ROOT)
        .filter(|key| key.starts_with('k') || key.starts_with('b'))
        .collect();
    let mut expected: BTreeSet<String> = keys_typed('b', 5).collect();
    expected.insert("k0".to_owned());
    assert_eq!(read, expected, "every covered write reads");
}
