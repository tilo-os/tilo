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
        .write(workspace, draft, &member_key(), true, now)
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
    let owed = custody(owner, workspace, now);
    assert!(
        owed.iter().any(|(_, message)| matches!(message,
            NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)),
        "the device compacted while offline"
    );
    // Meanwhile the peer writes at the host.
    let mut peer = Typist::over(&base(owner, workspace), 2);
    let peers: Vec<NeighbourMsg> = keys_typed('b', 5)
        .map(|key| {
            host.publication_mut()
                .publish_sealed_incremental(
                    APP,
                    "",
                    peer.change(&key),
                    &mut seal_delta(&keys),
                    &governor_key,
                    now,
                )
                .unwrap()
        })
        .collect();

    // Linked again: what each side holds reaches the other.
    let sent = answer(owner, workspace, now, &mut order);
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
