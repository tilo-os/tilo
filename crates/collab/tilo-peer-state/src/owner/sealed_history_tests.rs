//! A device keeps the sealed histories of its records within their bound
//! wherever it writes: alone, offline or linked, so a change is never refused
//! for how long its record's history grew.
use super::*;
use crate::owner::testing::{governed_fixture_at, member_key};
use crate::owner::{NativeCommittedView, SealedHistoryKeys};
use automerge::{ActorId, ChangeHash};
use channel_crypto::ChannelContentKeys;
use collab_types::{ContentRetention, DocumentMutation, EncryptionMetadata, OpKind, SignedOp};
use std::collections::BTreeSet;

/// The chat record's channel keys, as a member holding them compacts with.
pub(super) struct Held(pub(super) ChannelContentKeys);

impl SealedHistoryKeys for Held {
    fn open(&self, _: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String> {
        open(&self.0, write)
    }

    fn seal_checkpoint(
        &self,
        _: WorkspaceId,
        app: &str,
        record: &str,
        snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(
                collab_types::document_artifact_id(app, WORKSPACE, record),
                ContentRetention::Checkpoint,
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

fn open(keys: &ChannelContentKeys, write: &SignedOp) -> Result<Vec<u8>, String> {
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
    keys: &ChannelContentKeys,
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
        let before: Vec<ChangeHash> = self.0.get_heads();
        self.0.put(ROOT, key, key).unwrap();
        self.0.commit_edit("fixture", "topic");
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
    keys: &ChannelContentKeys,
    changes: Vec<u8>,
    now: SystemTime,
) -> Result<usize, String> {
    let frame = DocumentMutation::Incremental(changes).encode().unwrap();
    let author = member_key().verifying_key().to_bytes();
    let draft = |view: &NativeCommittedView| {
        let op = view.peer().draft_write(
            (APP, ""),
            &frame,
            Some(&mut seal_delta(keys)),
            author,
            0,
            now,
        )?;
        Ok(vec![OwnerCommitRequest {
            workspace: WORKSPACE.into(),
            operation: CommitOperation::Document {
                app: APP.into(),
                record: String::new(),
                envelope: Box::new(op),
            },
        }])
    };
    owner
        .write(workspace, draft, &member_key(), now)
        .await
        .map(|committed| committed.len())
}

/// What custody holds, in the order it sends it.
fn custody(
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

/// The host keeps all of custody durably: what it was sent, in order.
fn answer(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
    order: &mut u64,
) -> Vec<NeighbourMsg> {
    let held = custody(owner, workspace, now);
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
    keys: &ChannelContentKeys,
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

/// Everything custody holds, as the host takes it from its head until it owes nothing: the
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

fn opened(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    keys: &ChannelContentKeys,
) -> AutoCommit {
    owner
        .with_current(workspace, |view| Ok(opened_in(view.peer(), keys)))
        .unwrap()
}

fn opened_in(peer: &crate::StatePeer, keys: &ChannelContentKeys) -> AutoCommit {
    let record = peer
        .publication()
        .opened_document(APP, "", &mut |write| open(keys, write))
        .unwrap()
        .expect("the chat is held");
    assert!(record.unmerged.is_empty(), "every write merges");
    AutoCommit::load(&record.document).unwrap()
}

fn history(peer: &crate::StatePeer, keys: &ChannelContentKeys) -> Vec<(u64, bool)> {
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
            host.receive_from(TransportOrigin::RelayDelta(member), message, now)
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
        host.receive_from(TransportOrigin::RelayDelta(member), message, now)
            .unwrap();
    }
    for message in peers.clone() {
        owner
            .receive(
                workspace,
                TransportOrigin::RelayDelta(governor),
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

/// Writes a device makes while its host is away: custody keeps every one of them, past what
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
            host.receive_from(TransportOrigin::RelayDelta(member), message.clone(), now)
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
    assert!(owed >= AWAY, "custody owes every write: {owed}");
    drop(joined.owner);
    let owner = open_file(&path).unwrap();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    assert_eq!(
        owner.unadmitted_writes(workspace).unwrap(),
        owed,
        "custody keeps them all through a restart"
    );
    // Meanwhile another member writes at the host.
    let peers = written_at(
        &mut host,
        &base(&owner, workspace),
        &keys,
        &governor_key,
        now,
    );

    // Back: custody goes to the host in its order until it owes nothing.
    for message in drain(&owner, workspace, now, &mut order).await {
        host.receive_from(TransportOrigin::RelayDelta(member), message.clone(), now)
            .unwrap();
        relayed.push(message);
    }
    assert_eq!(owner.unadmitted_writes(workspace).unwrap(), 0);
    for message in peers.clone() {
        owner
            .receive(
                workspace,
                TransportOrigin::RelayDelta(governor),
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

/// `member` takes `messages` as its host relays them.
fn relay(member: &Enrolled, messages: impl IntoIterator<Item = NeighbourMsg>) {
    for message in messages {
        let NeighbourMsg::AppDocument { envelope, .. } = &message else {
            panic!("a document write");
        };
        let author = TransportOrigin::RetainedRelayDelta(*envelope.author.as_bytes());
        member
            .owner
            .receive(member.workspace, author, message, member.now)
            .unwrap();
    }
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
    keys: &ChannelContentKeys,
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
                TransportOrigin::RetainedRelayDelta(author),
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
    keys: &ChannelContentKeys,
    governor_key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let held = Held(keys.clone());
    let mut op = host
        .publication()
        .draft_sealed_compaction(
            (APP, ""),
            None,
            &mut |write| open(keys, write),
            &mut |snapshot| {
                held.seal_checkpoint(WorkspaceId::parse(WORKSPACE).unwrap(), APP, "", snapshot)
            },
            governor_key.verifying_key().to_bytes(),
            now,
        )
        .unwrap();
    collab_types::sign_op(&mut op, governor_key).unwrap();
    host.publication_mut()
        .admit_sealed_compaction(APP, "", op, now)
        .unwrap()
}

/// The host's side of the link: the history it holds, what it relayed in its order, and the
/// most writes its history of the chat ever held.
struct Host {
    peer: crate::StatePeer,
    relayed: Vec<NeighbourMsg>,
    longest: usize,
}

impl Host {
    /// Take `message` as the host does, or refuse it.
    fn take(&mut self, message: &NeighbourMsg, now: SystemTime) -> bool {
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("a document write");
        };
        let origin = TransportOrigin::RelayDelta(*envelope.author.as_bytes());
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

/// Custody goes to the host one write at a time, each answered as the host took it, until it
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
        let Some((id, message)) = custody(owner, workspace, now).into_iter().next() else {
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
        if until_refused && !refused.is_empty() {
            return refused;
        }
    }
    panic!(
        "custody still owes {} writes, {} refused",
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
    keys: &ChannelContentKeys,
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
/// record's bound. Answers the compactions the host refused.
async fn returns(away: Away, mut refuse: impl FnMut(&NeighbourMsg) -> bool) -> Vec<NeighbourMsg> {
    let (fixture, host, governor_key) = governed_fixture_at(tilo_clock::wall());
    let mut host = Host {
        peer: host,
        relayed: Vec::new(),
        longest: 0,
    };
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
                            TransportOrigin::RelayDelta(governor),
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
    refused
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
        returns(away, |_| false).await.is_empty(),
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
        returns(away, |_| false).await.is_empty(),
        "the host takes every compaction"
    );
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
    let raced = returns(away, |_| false).await;
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
    let refused = returns(away, |message| {
        is_compaction(message) && std::mem::take(&mut first)
    })
    .await;
    assert_eq!(refused.len(), 1, "the host refused one compaction");
}
