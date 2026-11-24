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

/// `forged`, changes of the chat record, sealed under `keys` and signed by `signer` in an envelope
/// whose claims are the ones a device without the key decides it by: what a device that skips its
/// own check of a change it makes for a caller sends. Every envelope check passes on it.
fn sealed_forgery(
    host: &crate::StatePeer,
    keys: &ContentKeys,
    (signer, person): (&ed25519_dalek::SigningKey, [u8; 32]),
    forged: Vec<u8>,
    now: SystemTime,
) -> NeighbourMsg {
    use collab_types::DocumentMutation;
    let author = signer.verifying_key().to_bytes();
    let mut seal = |plaintext: &[u8]| {
        keys.seal_content(
            collab_types::document_artifact_id(APP, WORKSPACE, ""),
            ContentRetention::Delta,
            plaintext,
        )
        .map_err(|error| error.to_string())
    };
    // An envelope as the signer drafts one, for a change the workspace takes...
    let mut honest = Typist::over(&base_of(host), 3, person);
    let frame = DocumentMutation::Incremental(honest.change("honest"))
        .encode()
        .unwrap();
    let mut op = host
        .draft_write((APP, ""), None, &frame, Some(&mut seal), author, 0, now)
        .unwrap();
    // ...carrying the forged changes, claimed as they say themselves.
    let forged = DocumentMutation::Incremental(forged);
    let (metadata, sealed) = seal(&forged.encode().unwrap()).unwrap();
    op.payload = sealed;
    op.encryption = Some(metadata);
    op.claims = host.sealed_claims((APP, ""), &forged);
    collab_types::sign_op(&mut op, signer).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(&author),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: String::new(),
        readers: None,
        envelope: Box::new(op),
    }
}

fn base_of(peer: &crate::StatePeer) -> Vec<u8> {
    peer.document(APP, "").expect("the chat is here")
}

/// A device holding a sealed record's keys opens a change it receives and decides its contents by
/// the check every reader of the record runs. It keeps the change, as every device without the
/// keys does, and keeps its verdict: the change's author is told, and the record reads without
/// the change. No device's admission decides the record's history for the others: the server
/// holding the workspace keeps it as every member's device does.
#[tokio::test]
async fn a_device_with_the_keys_decides_a_sealed_change_it_receives_by_what_it_opens_to() {
    let (fixture, host, writer_key) =
        crate::owner::testing::governed_fixture_at(tilo_clock::wall());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let writer = writer_key.verifying_key().to_bytes();
    let (_, keys) = rotated_channel();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    // The writing device writes for its own person, as though that person had asked on it,
    // and keeps no invocation it signed: a device without the key takes the claim as it is.
    let person = host
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == writer)
        .expect("the writing device's certificate")
        .user_public_key;
    let forged = super::refused_change::forged_for_caller(&base_of(&host), person, writer);
    let forged_change = collab_types::parse_changes(&forged).unwrap()[0].hash().0;
    let message = sealed_forgery(&host, &keys, (&writer_key, person), forged, now);
    let NeighbourMsg::AppDocument { envelope, .. } = &message else {
        unreachable!("a document write")
    };
    let envelope = (**envelope).clone();

    // Kept, with the verdict beside it.
    let heard = owner.receive(
        workspace,
        TransportOrigin::ServerDelta(writer),
        message.clone(),
        now,
    );
    let refused = heard
        .as_ref()
        .expect("a device keeps the change")
        .opened_refusal()
        .expect("the report says the workspace's rules refuse what it opens to")
        .clone();
    assert!(
        refused.changes.contains(&forged_change),
        "the refusal names the change: {:?}",
        refused.changes
    );
    owner
        .keep_verdicts(workspace, &[(&message, &heard)], &member_key(), now)
        .await
        .unwrap();
    let verdicts = owner.write_refusals(workspace);
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert_eq!(
        verdicts[0].author, writer,
        "the verdict is about the change"
    );
    assert_eq!(
        verdicts[0].by,
        member_key().verifying_key().to_bytes(),
        "the verdict is this device's own"
    );
    assert!(verdicts[0].changes.contains(&forged_change));
    let (kept, read) = owner
        .with_current(workspace, |view| {
            let peer = view.peer();
            let read = peer
                .publication()
                .opened_document(APP, "", &mut |write| open(&keys, write))
                .unwrap()
                .expect("the chat is here");
            Ok((peer.publication().holds_sealed(APP, "", &envelope), read))
        })
        .unwrap();
    assert!(kept, "the sealed write is kept");
    assert!(
        matches!(
            read.unmerged.as_slice(),
            [crate::UnmergedWrite { author, reason: crate::UnmergedReason::Refused { .. }, .. }]
                if *author == writer
        ),
        "the record reads without the refused change, and names it: {:?}",
        read.unmerged
    );
    assert_ne!(
        AutoCommit::load(&read.document)
            .unwrap()
            .get(ROOT, "topic")
            .unwrap()
            .and_then(|(value, _)| value.to_str().map(str::to_owned)),
        Some("forged".to_owned()),
        "nothing of the refused change reads"
    );
}
