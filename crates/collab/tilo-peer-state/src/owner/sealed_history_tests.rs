//! A device keeps the sealed histories of its records within their bound
//! wherever it writes: alone, offline or linked, so a change is never refused
//! for how long its record's history grew.
use super::*;
use crate::owner::SealedHistoryKeys;
use crate::owner::testing::member_key;
use automerge::{ActorId, ChangeHash};
use collab_types::{ContentRetention, EncryptionMetadata};
use key_group::ContentKeys;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The chat document's keys, as a member holding them compacts with.
pub(super) struct Held(pub(super) ContentKeys);

impl SealedHistoryKeys for Held {
    fn open(&self, _: WorkspaceId, write: &collab_types::SignedOp) -> Result<Vec<u8>, String> {
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
}

fn open(keys: &ContentKeys, write: &collab_types::SignedOp) -> Result<Vec<u8>, String> {
    let ciphertext = collab_types::sealed_ciphertext(write).map_err(|error| error.to_string())?;
    keys.open_content(
        write.artifact,
        &write.encryption.ok_or("not sealed")?,
        &ciphertext,
    )
    .map(|bytes| bytes.to_vec())
    .map_err(|error| error.to_string())
}

/// Someone typing into the chat record: one change per key, each after the
/// last, made by `person`, the person the writing device acts as.
struct Typist(AutoCommit, [u8; 32]);

impl Typist {
    fn over(base: &[u8], actor: u8, person: [u8; 32]) -> Self {
        let mut document = AutoCommit::load(base).unwrap();
        document.set_actor(ActorId::from([actor; 16]));
        Self(document, person)
    }

    fn change(&mut self, key: &str) -> Vec<u8> {
        self.put(key, key)
    }

    fn put(&mut self, key: &str, value: &str) -> Vec<u8> {
        let before: Vec<ChangeHash> = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.0.commit_edit("fixture", "topic", &self.1);
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

/// Write `changes` sealed under `keys` as this device does: drafted, sealed and
/// signed in one turn, and compacted when the history is due.
async fn write(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    keys: &ContentKeys,
    changes: Vec<u8>,
    now: SystemTime,
) -> Result<(), WriteRefusal> {
    owner
        .write_local(
            workspace,
            member_key().verifying_key().to_bytes(),
            Arc::new(member_key()) as Arc<dyn collab_types::signer::DeviceSigner>,
            crate::owner::LocalWrite {
                app: APP,
                document: "",
                readers: None,
                changes: &changes,
                founding: false,
                snapshot: false,
                sealed: Some(keys),
                now,
            },
        )
        .await
        .map(drop)
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

/// Changes a device alone writes: past this crate's bound of sealed writes per
/// record.
const ALONE: usize = 300;

#[tokio::test]
async fn a_device_alone_takes_every_change_to_one_sealed_record_past_its_bound() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let invoker = owner
        .with_current(workspace, |view| {
            Ok(crate::owner::testing::person_of(
                view.peer(),
                member_key().verifying_key().to_bytes(),
            ))
        })
        .unwrap();
    let mut typist = Typist::over(&base(owner, workspace), 1, invoker);
    for key in keys_typed('k', ALONE) {
        let changes = typist.change(&key);
        write(owner, workspace, &keys, changes, now)
            .await
            .unwrap_or_else(|refusal| panic!("{key} is taken: {refusal}"));
    }
    let held = owner
        .with_current(workspace, |view| Ok(history(view.peer(), &keys)))
        .unwrap();
    assert!(
        held.len() < 256,
        "the history stays within its bound: {}",
        held.len()
    );
    let kinds: Vec<collab_types::OpKind> = owner
        .frames(workspace)
        .iter()
        .filter_map(|frame| match NeighbourMsg::decode(frame) {
            Ok(NeighbourMsg::AppDocument { envelope, .. }) => Some(envelope.kind),
            _ => None,
        })
        .collect();
    assert!(
        kinds.contains(&collab_types::OpKind::Compaction),
        "the device compacted its own history: {kinds:?}"
    );
    let path = joined.path.clone();
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    let document = reopened
        .with_current(workspace, |view| {
            let mut opener = |write: &collab_types::SignedOp| open(&keys, write);
            let record = view
                .peer()
                .publication()
                .opened_document(APP, "", &mut opener)
                .unwrap()
                .expect("the chat is held");
            assert!(record.unmerged.is_empty(), "every write merges");
            Ok(AutoCommit::load(&record.document).unwrap())
        })
        .unwrap();
    assert_eq!(
        typed(&document),
        keys_typed('k', ALONE).collect(),
        "every change reads after a restart"
    );
}

/// A device given no keys to compact with does not rewrite a full history: its
/// writes wait, and say so; given the keys, its history is compacted and the
/// write past its bound lands and reads after a restart.
#[tokio::test]
async fn without_compaction_keys_a_full_sealed_history_waits() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    let invoker = owner
        .with_current(workspace, |view| {
            Ok(crate::owner::testing::person_of(
                view.peer(),
                member_key().verifying_key().to_bytes(),
            ))
        })
        .unwrap();
    let mut typist = Typist::over(&base(owner, workspace), 1, invoker);
    let mut refused = None;
    for key in keys_typed('k', 300) {
        let changes = typist.change(&key);
        match write(owner, workspace, &keys, changes.clone(), now).await {
            Ok(()) => {}
            Err(WriteRefusal::HistoryFull { .. }) => {
                refused = Some((key, changes));
                break;
            }
            Err(error) => panic!("{key}: {error}"),
        }
    }
    let (refused_key, refused_changes) =
        refused.expect("a device that cannot compact waits for its key holder");
    // The keys are here now: the history is compacted and the write it held back lands.
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    write(owner, workspace, &keys, refused_changes, now)
        .await
        .expect("a write past the bound lands once its history compacts");
    write(owner, workspace, &keys, typist.change("k-after"), now)
        .await
        .expect("the device goes on writing after the compaction");
    let path = joined.path.clone();
    let held = owner
        .with_current(workspace, |view| Ok(history(view.peer(), &keys)))
        .unwrap();
    assert!(
        held.len() < 256,
        "the history is compacted within its bound: {}",
        held.len()
    );
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    let document = reopened
        .with_current(workspace, |view| {
            let mut opener = |write: &collab_types::SignedOp| open(&keys, write);
            let record = view
                .peer()
                .publication()
                .opened_document(APP, "", &mut opener)
                .unwrap()
                .expect("the chat is held");
            Ok(AutoCommit::load(&record.document).unwrap())
        })
        .unwrap();
    let typed_keys = typed(&document);
    assert!(typed_keys.contains(&refused_key), "the held write landed");
    assert!(typed_keys.contains("k-after"), "the next one landed");
}

/// The frame a write is kept as carries the record's signed write, sealed.
#[tokio::test]
async fn a_write_is_kept_as_the_frame_a_link_carries() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    let invoker = owner
        .with_current(workspace, |view| {
            Ok(crate::owner::testing::person_of(
                view.peer(),
                member_key().verifying_key().to_bytes(),
            ))
        })
        .unwrap();
    let mut typist = Typist::over(&base(owner, workspace), 1, invoker);
    let changes = typist.change("k0");
    write(owner, workspace, &keys, changes.clone(), now)
        .await
        .unwrap();
    let frames = owner.frames(workspace);
    assert_eq!(frames.len(), 1);
    let NeighbourMsg::AppDocument { envelope, .. } = NeighbourMsg::decode(&frames[0]).unwrap()
    else {
        panic!("a write is a document write")
    };
    assert!(envelope.encryption.is_some(), "the frame is sealed");
    let frame = collab_types::DocumentMutation::Incremental(changes)
        .encode()
        .unwrap();
    assert_eq!(open(&keys, &envelope).unwrap(), frame);
}
