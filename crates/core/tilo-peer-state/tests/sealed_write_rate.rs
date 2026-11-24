//! A device writes a long run of sealed changes to one record at the rate a person or an agent
//! makes them: each drafted, sealed, signed and admitted through the owner's write path, and kept
//! until a link carries it. Its three thousandth write does the work its first did on the frames
//! it keeps, whether a link carries each write as it is made or the device is offline and keeps
//! them all, so a run never slows towards the end of the freshness window a write is admitted in.
//! Writes to many records at once share their syncs, and each one answered is there when the
//! device starts again.
//!
//! Each test reports the rate it ran at. The `sealed-write-rate` check in t.toml runs them in a
//! release build, the build a device runs, where a device takes a thousand writes a second.

use automerge::{ActorId, AutoCommit, ROOT, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use collab_types::{CommitNamed as _, ContentRetention, EncryptionMetadata, SignedOp};
use key_group::ContentKeys;
use std::sync::Arc;
use std::time::Instant;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{
    APP, WORKSPACE, finalize, frames_touched, member_key, person_of, raw_fixture_at,
};
use tilo_peer_state::owner::{LocalWrite, NativeWorkspaceOwner, SealedHistoryKeys, open_file};
use web_time::SystemTime;

/// Writes in a run: thousands, most of the bound of sealed writes one record holds before its
/// device compacts it.
const WRITES: usize = 3_000;
/// The writes at each end of a run whose work is compared.
const ENDS: usize = 500;
/// Records written at once, and the writes to each.
const RECORDS: usize = 8;
const EACH: usize = 100;

/// The record's content keys, as the device holding them compacts with.
struct Held(ContentKeys);

impl SealedHistoryKeys for Held {
    fn open(&self, _: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String> {
        let ciphertext =
            collab_types::sealed_ciphertext(write).map_err(|error| error.to_string())?;
        self.0
            .open_content(
                write.artifact,
                &write.encryption.ok_or("not sealed")?,
                &ciphertext,
            )
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string())
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

/// A member device enrolled into a governed workspace at `now`, its owner kept in a file store
/// under `directory`, holding the workspace channel's keys.
fn enrolled(
    directory: &std::path::Path,
    now: SystemTime,
) -> (NativeWorkspaceOwner, WorkspaceId, ContentKeys) {
    let (intent, bootstrap, messages, governor, now) = raw_fixture_at(now);
    let owner = open_file(&directory.join("joined-workspaces.json")).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedServerDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    let keys = key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("governor").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    owner.keep_sealed_histories(Arc::new(Held(keys.clone())));
    (owner, workspace, keys)
}

/// `typist`'s next change to the record, as the changes a write carries.
fn typed(typist: &mut AutoCommit, person: &[u8; 32], key: &str) -> Vec<u8> {
    let before = typist.get_heads();
    typist.put(ROOT, key, key).unwrap();
    typist.commit_edit("fixture", "topic", person);
    typist.save_after(&before)
}

/// Write [`WRITES`] sealed changes to one record as one device, a link carrying each write as it
/// is made when `linked`, none while the device is offline: the most frames one of its first
/// [`ENDS`] writes copied or compared to keep them, and the most one of its last did.
async fn run(linked: bool) -> (u64, u64) {
    let directory = tempfile::tempdir().unwrap();
    let (owner, workspace, keys) = enrolled(directory.path(), SystemTime::now());
    let device = member_key().verifying_key().to_bytes();
    let signer: Arc<dyn DeviceSigner> = Arc::new(member_key());
    let (base, person) = owner
        .with_current(workspace, |view| {
            Ok((
                view.peer().document(APP, "").ok_or("no chat")?,
                person_of(view.peer(), device),
            ))
        })
        .unwrap();
    let mut typist = AutoCommit::load(&base).unwrap();
    typist.set_actor(ActorId::from([1; 16]));

    let began = Instant::now();
    let (mut first, mut last) = (0, 0);
    for n in 0..WRITES {
        let key = format!("k{n}");
        let changes = typed(&mut typist, &person, &key);
        let write = LocalWrite {
            app: APP,
            document: "",
            readers: None,
            changes: &changes,
            founding: false,
            snapshot: false,
            sealed: Some(&keys),
            commit: None,
            now: SystemTime::now(),
        };
        let before = frames_touched();
        owner
            .write_local(workspace, device, Arc::clone(&signer), write)
            .await
            .unwrap_or_else(|refusal| panic!("{key} is taken: {refusal}"));
        if linked {
            for frame in owner.frames(workspace) {
                owner.sent(workspace, &frame).unwrap();
            }
        }
        let touched = frames_touched() - before;
        if n < ENDS {
            first = first.max(touched);
        } else if n >= WRITES - ENDS {
            last = last.max(touched);
        }
    }
    let rate = WRITES as f64 / began.elapsed().as_secs_f64();
    let kept = owner.frames(workspace).len();
    assert_eq!(
        kept,
        if linked { 0 } else { WRITES },
        "every write is kept until sent"
    );
    eprintln!(
        "{WRITES} sealed writes to one record, {}: {rate:.0} a second; a write touched at most \
         {first} frames among the first {ENDS}, {last} among the last",
        if linked { "linked" } else { "offline" }
    );
    (first, last)
}

#[tokio::test]
async fn a_linked_device_does_the_same_work_for_its_last_sealed_write_as_its_first() {
    let (first, last) = run(true).await;
    assert_eq!(
        last, first,
        "a linked device's last writes touch as many kept frames as its first"
    );
}

#[tokio::test]
async fn an_offline_device_does_the_same_work_for_its_last_sealed_write_as_its_first() {
    let (first, last) = run(false).await;
    assert_eq!(
        last, first,
        "an offline device's last writes touch as many kept frames as its first, however many \
         it keeps"
    );
}

/// Writers each writing a record of their own at once, as a file's chunks are synced, each write
/// answered only once it is durable: the writes share their publications, and every write answered
/// is in the record when the device starts again from what it made durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_to_many_records_at_once_are_each_there_after_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, workspace, keys) = enrolled(directory.path(), SystemTime::now());
    let device = member_key().verifying_key().to_bytes();
    let signer: Arc<dyn DeviceSigner> = Arc::new(member_key());
    let person = owner
        .with_current(workspace, |view| Ok(person_of(view.peer(), device)))
        .unwrap();

    let began = Instant::now();
    let writers = (0..RECORDS).map(|writer| {
        let (owner, signer, keys) = (owner.clone(), Arc::clone(&signer), keys.clone());
        tokio::spawn(async move {
            let record = format!("chunk-{writer}");
            let mut typist = AutoCommit::new();
            typist.set_actor(ActorId::from([2 + u8::try_from(writer).unwrap(); 16]));
            for n in 0..EACH {
                let key = format!("k{n}");
                let snapshot = n == 0;
                let changes = if snapshot {
                    typist.put(ROOT, &key, &*key).unwrap();
                    typist.commit_edit("fixture", "chunk", &person);
                    typist.save()
                } else {
                    typed(&mut typist, &person, &key)
                };
                let write = LocalWrite {
                    app: APP,
                    document: &record,
                    readers: None,
                    changes: &changes,
                    founding: false,
                    snapshot,
                    sealed: Some(&keys),
                    commit: None,
                    now: SystemTime::now(),
                };
                owner
                    .write_local(workspace, device, Arc::clone(&signer), write)
                    .await
                    .unwrap_or_else(|refusal| panic!("{record} {key} is taken: {refusal}"));
            }
        })
    });
    for writer in writers.collect::<Vec<_>>() {
        writer.await.unwrap();
    }
    let rate = (RECORDS * EACH) as f64 / began.elapsed().as_secs_f64();
    eprintln!(
        "{} sealed writes to {RECORDS} records at once: {rate:.0} a second",
        RECORDS * EACH
    );
    let held = |owner: &NativeWorkspaceOwner| {
        let records = (0..RECORDS)
            .map(|writer| {
                owner
                    .with_current(workspace, |view| {
                        view.peer()
                            .document(APP, &format!("chunk-{writer}"))
                            .ok_or_else(|| format!("chunk-{writer} is not held"))
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        (records, owner.frames(workspace))
    };
    let answered = held(&owner);
    assert_eq!(
        answered.1.len(),
        RECORDS * EACH,
        "every write is kept until sent"
    );
    drop(owner);

    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    assert!(
        held(&owner) == answered,
        "every write answered is there after the restart, as it was answered"
    );
}
