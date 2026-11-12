use crate::KeptRecord;
use crate::TransportOrigin;
use crate::owner::testing::{APP, WORKSPACE, raw_fixture, record_on_disk};
use crate::owner::{
    DocumentRead, History, Holding, HostAnswer, NativeWorkspaceOwner, PendingEnrollmentId,
    RecordWrites, WorkspaceId, open_file,
};
use crate::owner_commit::{CommitOperation, OwnerCommitRequest};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

#[path = "busy_workspace_tests.rs"]
mod busy_workspace;
#[path = "changes_read_tests.rs"]
mod changes_read;
#[path = "create_tests.rs"]
mod create;
#[path = "vouched_tests.rs"]
mod vouched;

struct Enrolled {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
    replay: NeighbourMsg,
}

/// Enroll through the file store and admit the governor's history.
fn enrolled() -> Enrolled {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let owner = open_file(&path).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = crate::owner::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let replay = messages
        .iter()
        .rev()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("fixture has signed app state")
        .clone();
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    Enrolled {
        _directory: directory,
        path,
        owner,
        workspace,
        now,
        replay,
    }
}

/// The file one workspace's record is in, as this device holds it.
fn workspace_file(path: &std::path::Path, workspace: WorkspaceId) -> PathBuf {
    let name = workspace.to_string();
    std::fs::read_dir(crate::owner::file_store::parts_of(path))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|file| {
            file.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&name))
        })
        .expect("the record holds the workspace's file")
}

/// One workspace's record as its own file holds it.
fn workspace_json(path: &std::path::Path, workspace: WorkspaceId) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(workspace_file(path, workspace)).unwrap()).unwrap()
}

/// The record as a build before the split wrote it: JSON a file holds whole.
fn whole_record(path: &std::path::Path) -> serde_json::Value {
    serde_json::to_value(crate::owner::read_file(path).unwrap()).unwrap()
}

/// Every file under `root`, looking into its directories.
fn files_under(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    files
}

fn member() -> SigningKey {
    SigningKey::from_bytes(&[63; 32])
}

fn edit(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
) -> OwnerCommitRequest {
    edit_with(owner, workspace, now, "written while joined")
}

fn edit_with(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
    topic: &str,
) -> OwnerCommitRequest {
    owner
        .with_current(workspace, |view| edit_from_view(view, now, topic))
        .unwrap()
}

fn edit_from_view(
    view: &crate::owner::NativeCommittedView,
    now: SystemTime,
    topic: &str,
) -> Result<OwnerCommitRequest, String> {
    let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
    let before = chat.get_heads();
    chat.put(ROOT, "topic", topic).unwrap();
    chat.commit_edit("fixture", "topic");
    let message = view.peer().clone().publication_mut().publish_incremental(
        APP,
        "",
        chat.save_after(&before),
        &crate::owner::testing::member_key(),
        now,
    )?;
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("test edit was not a signed document write".into());
    };
    Ok(OwnerCommitRequest {
        workspace: WORKSPACE.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope,
        },
    })
}

#[test]
fn a_joined_workspace_is_kept_in_its_own_private_file_and_reopens() {
    let joined = enrolled();
    let policy_calls = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(joined.workspace, |view| {
                Ok(view
                    .peer()
                    .validated_media_policy()?
                    .ok_or("missing admitted policy")?
                    .calls
                    .len())
            })
            .unwrap()
    };
    assert_eq!(policy_calls(&joined.owner), 1);
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(policy_calls(&reopened), 1);
    assert_eq!(reopened.workspaces(), vec![joined.workspace]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let parts = crate::owner::file_store::parts_of(&joined.path);
        let mode = std::fs::metadata(&parts).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the record's directory is owner-only");
        for (file, _) in record_on_disk(&joined.path) {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is owner-only", file.display());
        }
    }
}

#[test]
fn a_local_write_is_durable_with_its_outgoing_custody_before_its_receipt() {
    let joined = enrolled();
    assert_eq!(joined.owner.unadmitted_writes(joined.workspace).unwrap(), 0);
    let request = edit(&joined.owner, joined.workspace, joined.now);
    let receipt = joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let committed = record_on_disk(&joined.path);
    let retry = joined.owner.commit_document(request, joined.now).unwrap();
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(
        record_on_disk(&joined.path),
        committed,
        "an exact retry does not rewrite"
    );
    let outgoing = |owner: &NativeWorkspaceOwner| {
        owner
            .with_outgoing(joined.workspace, joined.now, |_, outgoing| {
                Ok(outgoing.entries().count())
            })
            .unwrap()
    };
    assert_eq!(outgoing(&joined.owner), 1);
    assert_eq!(
        joined.owner.unadmitted_writes(joined.workspace).unwrap(),
        1,
        "the write waits for the governor's admission"
    );
    drop(joined.owner);
    assert_eq!(outgoing(&open_file(&joined.path).unwrap()), 1);
}

#[test]
fn a_write_its_relay_host_refused_leaves_the_replica_and_every_answer_is_counted() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let topic = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| {
                let chat = AutoCommit::load(&view.peer().document(APP, "").ok_or("no chat")?)
                    .map_err(|error| error.to_string())?;
                Ok(chat
                    .get(ROOT, "topic")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)))
            })
            .unwrap()
    };
    let newest = |owner: &NativeWorkspaceOwner| {
        owner
            .with_outgoing(workspace, now, |_, outgoing| Ok(outgoing.queued().last()))
            .unwrap()
            .unwrap()
    };
    owner
        .commit_document(edit_with(owner, workspace, now, "kept"), now)
        .unwrap();
    owner
        .retire_answered(
            workspace,
            &BTreeMap::from([(newest(owner), HostAnswer::Durable(7))]),
        )
        .unwrap();
    let refused = edit_with(owner, workspace, now, "refused");
    let crate::owner_commit::CommitOperation::Document { envelope, .. } = refused.operation.clone()
    else {
        panic!("a document write");
    };
    owner.commit_document(refused, now).unwrap();
    assert_eq!(topic(owner).as_deref(), Some("refused"));
    owner
        .retire_answered(
            workspace,
            &BTreeMap::from([(newest(owner), HostAnswer::Refused)]),
        )
        .unwrap();
    let answered = RecordWrites {
        pending: 0,
        durable: 1,
        refused: 1,
        last_order: Some(7),
    };
    assert_eq!(topic(owner).as_deref(), Some("kept"));
    assert!(
        owner
            .with_current(workspace, |view| {
                view.peer()
                    .lookup_exact_local_document(APP, "", &envelope, now)
                    .map_err(|error| error.to_string())
            })
            .unwrap()
            .is_none(),
        "no evidence of the refused write stays to wait for"
    );
    assert_eq!(
        owner
            .record_writes(workspace)
            .get(&(APP.to_owned(), String::new())),
        Some(&answered)
    );
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(topic(&reopened).as_deref(), Some("kept"));
    assert_eq!(
        reopened
            .record_writes(workspace)
            .get(&(APP.to_owned(), String::new())),
        Some(&answered)
    );
}

#[tokio::test]
async fn prepared_local_write_uses_current_sequence_after_an_intervening_commit() {
    let joined = enrolled();
    let stale = edit_with(&joined.owner, joined.workspace, joined.now, "stale edit");
    let first = edit_with(
        &joined.owner,
        joined.workspace,
        joined.now,
        "first committed edit",
    );
    joined.owner.commit_document(first, joined.now).unwrap();
    assert_eq!(
        joined.owner.commit_document(stale, joined.now).unwrap_err(),
        "signed operation conflicts with exact admitted sequence"
    );

    let receipt = joined
        .owner
        .write(
            joined.workspace,
            |view| {
                edit_from_view(view, joined.now, "edit from current owner turn")
                    .map(|edit| vec![edit])
            },
            &crate::owner::testing::member_key(),
            true,
            joined.now,
        )
        .await
        .unwrap()
        .pop()
        .unwrap()
        .1;
    assert!(receipt.generation > 1);
    joined
        .owner
        .with_current(joined.workspace, |view| {
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            assert_eq!(
                chat.get(ROOT, "topic").unwrap().unwrap().0.as_str(),
                Some("edit from current owner turn")
            );
            Ok(())
        })
        .unwrap();
}

/// A write signed outside the owner turn the way every SDK document signer
/// signs it: with the process-wide unordered sequence, not `floor + 1`.
fn unordered_edit(joined: &Enrolled, topic: &str) -> OwnerCommitRequest {
    use collab_types::{
        DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
        document_attestation_id, next_unordered_document_sequence, sign_op,
    };
    let changes = joined
        .owner
        .with_current(joined.workspace, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            chat.put(ROOT, "topic", topic).unwrap();
            chat.commit_edit("fixture", "topic");
            Ok(chat.save_after(&before))
        })
        .unwrap();
    let key = member();
    let mutation = DocumentMutation::Incremental(changes);
    let mut envelope = SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        document_artifact_id(APP, WORKSPACE, ""),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        document_attestation_id(APP, WORKSPACE, ""),
        OpKind::Change,
        joined.now,
        next_unordered_document_sequence(joined.now),
    );
    sign_op(&mut envelope, &key).unwrap();
    OwnerCommitRequest {
        workspace: WORKSPACE.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope: Box::new(envelope),
        },
    }
}

#[test]
fn a_write_signed_outside_the_owner_turn_commits_with_exact_admission_evidence() {
    let joined = enrolled();
    let request = unordered_edit(&joined, "unordered edit");
    joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let CommitOperation::Document { envelope, .. } = &request.operation else {
        panic!("the unordered edit is a document write")
    };
    joined
        .owner
        .with_current(joined.workspace, |view| {
            assert!(
                view.peer()
                    .lookup_exact_local_document(APP, "", envelope, joined.now)
                    .map_err(|error| error.to_string())?
                    .is_some(),
                "the caller-signed write must leave exact admission evidence"
            );
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            assert_eq!(
                chat.get(ROOT, "topic").unwrap().unwrap().0.as_str(),
                Some("unordered edit")
            );
            Ok(())
        })
        .unwrap();
}

/// Two local writers of this device sign their writes before the owner's
/// turn orders them. The write that reaches the turn second, though signed
/// first, is taken all the same, signed again past the record's floor; one
/// still past the floor, and an exact retry of a taken write, are taken as
/// they are.
#[tokio::test]
async fn a_write_another_local_writer_passed_is_sequenced_in_the_owners_turn() {
    let joined = enrolled();
    let key = member();
    let envelope = |request: &OwnerCommitRequest| match &request.operation {
        CommitOperation::Document { envelope, .. } => (**envelope).clone(),
        _ => panic!("a document write"),
    };
    let earlier = unordered_edit(&joined, "signed first");
    let later = unordered_edit(&joined, "signed second");
    assert!(envelope(&earlier).seq < envelope(&later).seq);
    let (taken, _) = joined
        .owner
        .commit_local_document(later.clone(), &key, joined.now)
        .await
        .unwrap();
    assert_eq!(
        envelope(&taken),
        envelope(&later),
        "a write past the floor is taken as it is"
    );
    assert_eq!(
        joined
            .owner
            .commit_document(earlier.clone(), joined.now)
            .unwrap_err(),
        "native local document signed sequence is stale",
        "the passed write as it was signed is a replay"
    );
    let (taken, receipt) = joined
        .owner
        .commit_local_document(earlier.clone(), &key, joined.now)
        .await
        .unwrap();
    receipt.validate_local_document(&earlier, &taken).unwrap();
    assert_ne!(envelope(&taken), envelope(&earlier));
    assert!(envelope(&taken).seq > envelope(&later).seq);
    let (again, _) = joined
        .owner
        .commit_local_document(taken.clone(), &key, joined.now)
        .await
        .unwrap();
    assert_eq!(
        envelope(&again),
        envelope(&taken),
        "an exact retry is the write already taken"
    );
    joined
        .owner
        .with_current(joined.workspace, |view| {
            for write in [&later, &taken] {
                assert!(
                    view.peer()
                        .lookup_exact_local_document(APP, "", &envelope(write), joined.now)
                        .map_err(|error| error.to_string())?
                        .is_some(),
                    "both writes are the record's"
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_uncertain_write_fences_every_workspace_until_reopened() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let joined = enrolled();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _subscription = joined.owner.observe_fences(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let request = edit(&joined.owner, joined.workspace, joined.now);
    assert!(
        with_failure(Checkpoint::AfterPublish, || joined
            .owner
            .commit_document(request.clone(), joined.now))
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(joined.owner.health().is_err());
    assert!(joined.owner.contains_workspace(joined.workspace));
    assert!(joined.owner.commit_document(request, joined.now).is_err());
    drop(joined.owner);
    open_file(&joined.path).unwrap().health().unwrap();
}

#[test]
fn a_pending_enrollment_without_a_slot_survives_restart_and_cancels() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let (intent, _, _, _, now) = raw_fixture();
    let id = open_file(&path)
        .unwrap()
        .prepare_enrollment_into(&intent, None, now)
        .unwrap();
    let owner = open_file(&path).unwrap();
    assert!(owner.contains_workspace(id.workspace()));
    let invitation =
        crate::join::encode_invitation_text(intent.verified_join().signed_invitation());
    assert_eq!(
        owner
            .pending_enrollment_for_invitation(&invitation)
            .unwrap()
            .map(|pending| pending.id().clone()),
        Some(id.clone()),
        "a host without slots finds its retained proof by the invitation it redeems"
    );
    assert!(
        owner
            .pending_enrollment_for_invitation("another invitation")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        owner.prepare_enrollment_into(&intent, None, now).unwrap(),
        id,
        "a retry reuses the exact persisted proof"
    );
    assert!(owner.cancel_enrollment(id.workspace(), &id).unwrap());
    assert!(
        open_file(&path)
            .unwrap()
            .pending_enrollment(id.workspace())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_member_reads_the_admitted_document_and_a_stranger_does_not() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let read = |reader, record: &str, known, maximum| {
        joined.owner.with_current(joined.workspace, |view| {
            view.read_document(reader, APP, record, known, maximum, joined.now)
        })
    };
    let Ok(DocumentRead::Captured(document)) = read(member, "", None, usize::MAX) else {
        panic!("a member reads what the workspace admitted");
    };
    let doc = AutoCommit::load(&document.bytes).unwrap();
    assert!(doc.get(ROOT, "channels").unwrap().is_some());
    assert!(matches!(
        read(member, "", Some(document.revision), usize::MAX),
        Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
    ));
    assert!(
        read(member, "", None, document.bytes.len() - 1).is_err(),
        "a document larger than the reader can hold is refused"
    );
    assert!(
        read(stranger, "", None, usize::MAX).is_err(),
        "a device outside the workspace reads nothing"
    );
    assert!(read(member, "uuid:absent", None, usize::MAX).is_err());
}

/// A governor checkpoint reaches this owner as several signed frames. A read
/// between them hands out nothing and says the checkpoint is pending, so the
/// reader asks again; a refusal would end an app's collaboration for good.
/// It still refuses a device that may not read the record.
#[test]
fn a_read_between_the_frames_of_one_checkpoint_is_pending_not_refused() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let covered = edit_with(&joined.owner, joined.workspace, joined.now, "covered next");
    joined.owner.commit_document(covered, joined.now).unwrap();
    let mut remote = joined
        .owner
        .with_current(joined.workspace, |view| Ok(view.peer().clone()))
        .unwrap();
    let frames = remote
        .publication_mut()
        .durable_bootstrap_messages(&governor_key, joined.now, 600)
        .unwrap();
    let read = |reader| {
        joined.owner.with_current(joined.workspace, |view| {
            view.read_document(reader, APP, "", None, usize::MAX, joined.now)
        })
    };
    let mut pending = 0;
    for frame in frames {
        joined
            .owner
            .receive(
                joined.workspace,
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                frame,
                joined.now,
            )
            .unwrap();
        match read(member) {
            Ok(DocumentRead::Captured(_)) => {}
            Ok(DocumentRead::CheckpointPending) => {
                pending += 1;
                assert!(
                    read(stranger).is_err(),
                    "a pending checkpoint is reported only to a reader of the record"
                );
            }
            Ok(DocumentRead::Unchanged(_) | DocumentRead::Changes(_)) => {
                panic!("no revision was offered")
            }
            Err(error) => panic!("a read between checkpoint frames was refused: {error}"),
        }
    }
    assert!(
        pending > 0,
        "the checkpoint's frames leave the workspace between cuts"
    );
    let Ok(DocumentRead::Captured(document)) = read(member) else {
        panic!("the completed checkpoint is read again");
    };
    let doc = AutoCommit::load(&document.bytes).unwrap();
    assert_eq!(
        doc.get(ROOT, "topic")
            .unwrap()
            .map(|(value, _)| value.to_string()),
        Some("\"covered next\"".into())
    );
}

#[test]
fn an_unchanged_committed_read_does_not_capture_the_document() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let read = |reader, known| {
        joined.owner.with_current(joined.workspace, |view| {
            view.read_document(reader, APP, "", known, usize::MAX, joined.now)
        })
    };
    crate::owner::document_read::reset_captures();
    let Ok(DocumentRead::Captured(document)) = read(member, None) else {
        panic!("the first read admits the document");
    };
    assert!(
        crate::owner::document_read::captures() > 0,
        "a first read captures the document so the counter can see a later skip"
    );
    let captures = crate::owner::document_read::captures();
    assert!(matches!(
        read(member, Some(document.revision)),
        Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
    ));
    assert_eq!(
        crate::owner::document_read::captures(),
        captures,
        "an idle reread of the same revision must not capture the document"
    );
    assert!(
        read(stranger, Some(document.revision)).is_err(),
        "a device outside the workspace cannot reuse a revision it was never given"
    );
}

#[test]
fn a_held_revision_is_checked_from_heads_with_the_read_permission() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let Ok(DocumentRead::Captured(document)) =
        joined.owner.with_current(joined.workspace, |view| {
            view.read_document(member, APP, "", None, usize::MAX, joined.now)
        })
    else {
        panic!("the first read admits the document");
    };
    let holds = |reader, known| {
        joined.owner.with_current(joined.workspace, |view| {
            let history = History {
                open: &mut |_| Err("the record is not sealed".into()),
                kept: &mut KeptRecord::default(),
            };
            view.document_revision(reader, (APP, ""), joined.now, history)
                .map(|revision| revision.map(|revision| revision == known))
        })
    };
    crate::owner::document_read::reset_captures();
    assert_eq!(holds(member, document.revision), Ok(Some(true)));
    assert_eq!(
        holds(member, [0; 32]),
        Ok(Some(false)),
        "another revision is not held"
    );
    assert_eq!(
        crate::owner::document_read::captures(),
        0,
        "answered from the admitted heads, without capturing the document again"
    );
    assert!(
        holds(stranger, document.revision).is_err(),
        "the read permission is checked as a read checks it"
    );
}

/// What a sealed write says, which no file of the keyless owner may hold.
const SECRET: &str = "only key holders read this";

/// A channel's key holder, and its content keys before and after a rotation.
fn rotated_channel() -> (
    channel_crypto::ChannelContentKeys,
    channel_crypto::ChannelContentKeys,
) {
    let channel = collab_types::document_channel_id(APP, WORKSPACE, "");
    let mut group = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("governor").unwrap(),
    )
    .unwrap();
    let before = group.content_keys().unwrap();
    group
        .add_member(
            channel_crypto::MemberKey::generate("reader")
                .unwrap()
                .fresh_key_package()
                .unwrap(),
        )
        .unwrap();
    (before, group.content_keys().unwrap())
}

/// Commit a write sealed under `keys`, as an SDK app seals it, at `now`.
fn commit_sealed(
    joined: &Enrolled,
    keys: &channel_crypto::ChannelContentKeys,
) -> collab_types::SignedOp {
    commit_sealed_as(joined, keys, collab_types::ContentRetention::Delta)
}

/// Commit a write sealed under `keys` with the signed storage class `class`.
fn commit_sealed_as(
    joined: &Enrolled,
    keys: &channel_crypto::ChannelContentKeys,
    class: collab_types::ContentRetention,
) -> collab_types::SignedOp {
    let OwnerCommitRequest {
        operation: CommitOperation::Document { mut envelope, .. },
        ..
    } = edit_with(&joined.owner, joined.workspace, joined.now, SECRET)
    else {
        panic!("a document write")
    };
    let (metadata, sealed) = keys
        .seal_content(envelope.artifact, class, &envelope.payload)
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, &member()).unwrap();
    joined
        .owner
        .commit_document(
            OwnerCommitRequest {
                workspace: WORKSPACE.into(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    record: String::new(),
                    envelope: envelope.clone(),
                },
            },
            joined.now,
        )
        .unwrap();
    *envelope
}

fn sent(owner: &NativeWorkspaceOwner, workspace: WorkspaceId, now: SystemTime) -> Vec<Vec<u8>> {
    owner
        .with_outgoing(workspace, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect())
        })
        .unwrap()
}

/// Native publication classifies through the one host classifier browser
/// peers use, so a sealed checkpoint is a checkpoint to it there too. A
/// member still never replaces the record's retained checkpoint under its own
/// relay author: the sealed checkpoint leaves as a delta after it, while a
/// cleartext snapshot stays a checkpoint the member never releases.
#[test]
fn native_sealed_checkpoint_is_classified_like_browser() {
    use neighbour_wire::{Retention, retention};
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    let envelope = commit_sealed_as(&joined, &keys, collab_types::ContentRetention::Checkpoint);
    let [bytes] = sent(&joined.owner, joined.workspace, joined.now)
        .try_into()
        .expect("the committed sealed checkpoint is released");
    let message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope: sent, .. } = &message else {
        panic!("a document write")
    };
    assert_eq!(**sent, envelope);

    let (channel, class) = retention(&message, false).expect("sealed state is retained");
    assert_eq!(class, Retention::Checkpoint, "the browser peer's class");
    let (_, recorded, incremental) = crate::owner::publication::metadata(&message).unwrap();
    assert_eq!(recorded, channel, "the same host channel as a browser peer");
    assert!(
        incremental,
        "a member's sealed checkpoint is published as a delta"
    );
    let published = joined
        .owner
        .with_outgoing(joined.workspace, joined.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| (entry.channel(), entry.retention()))
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(published, [(channel, Retention::Delta)]);

    let mut snapshot = message.clone();
    let mut single_use = message;
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut snapshot {
        envelope.encryption = None;
        envelope.payload = collab_types::DocumentMutation::Snapshot(vec![1])
            .encode()
            .unwrap();
    }
    assert_eq!(
        retention(&snapshot, false).map(|(_, class)| class),
        Some(Retention::Checkpoint)
    );
    assert!(
        !crate::owner::publication::metadata(&snapshot).unwrap().2,
        "a cleartext snapshot stays the governor's checkpoint"
    );
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut single_use {
        envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
            0,
            collab_types::document_channel_id(APP, WORKSPACE, ""),
        ));
    }
    assert!(
        crate::owner::publication::metadata(&single_use).is_err(),
        "a single-use MLS message has no retention class"
    );
}

/// A member's sealed compaction is the one checkpoint it publishes as itself:
/// the host replaces that member's retained history of the record with it,
/// where a sealed snapshot would only be appended after the checkpoint a
/// member never releases.
#[test]
fn a_members_sealed_compaction_is_published_as_a_checkpoint() {
    use neighbour_wire::{Retention, retention};
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    let sealed = commit_sealed_as(&joined, &keys, collab_types::ContentRetention::Checkpoint);
    let [bytes] = sent(&joined.owner, joined.workspace, joined.now)
        .try_into()
        .expect("the committed sealed checkpoint is released");
    let mut message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope: op, .. } = &mut message else {
        panic!("a document write")
    };
    op.kind = collab_types::OpKind::Compaction;
    collab_types::sign_op(op, &member()).unwrap();
    assert_eq!(
        op.seq, sealed.seq,
        "the same write, published as a compaction"
    );

    assert_eq!(
        retention(&message, false).map(|(_, class)| class),
        Some(Retention::Checkpoint)
    );
    let (_, _, incremental) = crate::owner::publication::metadata(&message).unwrap();
    assert!(
        !incremental,
        "a member's sealed compaction replaces the record's retained history"
    );
}

#[tokio::test]
async fn a_member_with_no_sealed_history_past_its_bound_compacts_nothing() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let (mut opened, mut sealed) = (0, 0);
    let compacted = joined
        .owner
        .compact_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            |_| {
                opened += 1;
                Err("no history is due".to_owned())
            },
            |_, _, _| {
                sealed += 1;
                Err("no history is due".to_owned())
            },
            joined.now,
        )
        .await
        .unwrap();
    assert_eq!(compacted, 0);
    assert_eq!((opened, sealed), (0, 0), "nothing is opened or sealed");
    assert_eq!(sent(&joined.owner, joined.workspace, joined.now).len(), 1);
}

#[tokio::test]
async fn an_expired_sealed_write_is_renewed_through_its_key_holder_and_replaces_its_original() {
    let joined = enrolled();
    let (before, current) = rotated_channel();
    let original = commit_sealed(&joined, &before);
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    assert!(sent(&joined.owner, joined.workspace, later).is_empty());

    let mut asked = 0;
    joined
        .owner
        .renew_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            |op| {
                asked += 1;
                let metadata = op.encryption.ok_or("not sealed")?;
                let plaintext = current
                    .open_content(op.artifact, &metadata, &op.payload)
                    .map_err(|e| e.to_string())?;
                current
                    .seal_content(
                        op.artifact,
                        collab_types::ContentRetention::Delta,
                        &plaintext,
                    )
                    .map_err(|e| e.to_string())
            },
            later,
        )
        .await
        .unwrap();
    assert_eq!(asked, 1);
    let status = joined
        .owner
        .outgoing_status(joined.workspace, later)
        .unwrap();
    assert_eq!(status.pending, 1, "the renewal replaces its original");
    let [bytes] = sent(&joined.owner, joined.workspace, later)
        .try_into()
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = postcard::from_bytes(&bytes).unwrap() else {
        panic!("a document write")
    };
    collab_types::verify_op(&envelope).unwrap();
    assert_eq!(envelope.seq, original.seq.checked_add(1).unwrap());
    assert_eq!(envelope.timestamp, later);
    let metadata = envelope.encryption.unwrap();
    assert_eq!(metadata.epoch, current.epoch());
    assert_eq!(
        current
            .open_current_content(envelope.artifact, &metadata, &envelope.payload)
            .unwrap()
            .as_slice(),
        before
            .open_content(
                original.artifact,
                &original.encryption.unwrap(),
                &original.payload
            )
            .unwrap()
            .as_slice(),
        "the renewal carries the same content under the current epoch"
    );

    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(sent(&reopened, joined.workspace, later), vec![bytes]);
    for (file, contents) in files_under(joined.path.parent().unwrap()) {
        assert!(
            !contents
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "the keyless owner's files hold the plaintext in {}",
            file.display()
        );
    }
}

#[tokio::test]
async fn a_refused_renewal_leaves_the_sealed_write_in_custody_as_it_was() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let committed = record_on_disk(&joined.path);
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    joined
        .owner
        .renew_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            |_| Err("this device holds no current key".into()),
            later,
        )
        .await
        .unwrap();
    assert_eq!(record_on_disk(&joined.path), committed);
    assert_eq!(
        joined
            .owner
            .outgoing_status(joined.workspace, later)
            .unwrap()
            .pending,
        1
    );
    assert!(sent(&joined.owner, joined.workspace, later).is_empty());
    assert!(
        joined
            .owner
            .renew_outgoing(
                joined.workspace,
                &SigningKey::from_bytes(&[99; 32]),
                |_| panic!("another signer's renewal asked the key holder"),
                later,
            )
            .await
            .is_err()
    );
}

#[test]
fn a_workspace_file_names_its_host_under_the_stored_key() {
    let joined = enrolled();
    let host = joined
        .owner
        .with_current(joined.workspace, |view| Ok(view.workspace_host().clone()))
        .unwrap();
    drop(joined.owner);
    let value = workspace_json(&joined.path, joined.workspace);
    let stored = value.as_object().expect("a stored workspace");
    assert!(
        stored.contains_key("room_host"),
        "one key, one spelling: {}",
        serde_json::to_string_pretty(&value).unwrap()
    );
    assert!(
        !stored.contains_key("workspace_host"),
        "one key, one spelling"
    );
    let reopened = open_file(&joined.path).expect("the file this build wrote reads");
    assert_eq!(
        reopened
            .with_current(joined.workspace, |view| Ok(view.workspace_host().clone()))
            .unwrap(),
        host,
        "the stored key names the workspace's host"
    );
}

#[test]
fn a_host_written_under_each_renamed_key_reads() {
    for key in ["governor", "workspace_host"] {
        let joined = enrolled();
        let host = joined
            .owner
            .with_current(joined.workspace, |view| Ok(view.workspace_host().clone()))
            .unwrap();
        drop(joined.owner);
        let file = workspace_file(&joined.path, joined.workspace);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let stored = value.as_object_mut().expect("a stored workspace");
        let named_as_the_host = stored
            .remove("room_host")
            .expect("the record names the host");
        stored.insert(key.into(), named_as_the_host);
        std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        let reopened = open_file(&joined.path).expect("a file written under that key reads");
        assert_eq!(
            reopened
                .with_current(joined.workspace, |view| Ok(view.workspace_host().clone()))
                .unwrap(),
            host,
            "the {key} field is the workspace's host"
        );
    }
}

#[test]
fn an_owner_file_written_while_the_map_was_named_for_workspaces_restores() {
    let joined = enrolled();
    drop(joined.owner);
    let mut value = whole_record(&joined.path);
    let workspaces = value
        .as_object_mut()
        .expect("a storage object")
        .remove("rooms")
        .expect("the storage names its workspaces");
    value["workspaces"] = workspaces;
    // That build's file, where the split layout this build keeps is not.
    std::fs::remove_dir_all(crate::owner::file_store::parts_of(&joined.path)).unwrap();
    std::fs::write(&joined.path, serde_json::to_vec(&value).unwrap()).unwrap();
    let reopened = open_file(&joined.path).expect("a file written by that build reads");
    assert_eq!(
        reopened.workspaces(),
        vec![joined.workspace],
        "the workspaces read under the key it was written under"
    );
}

#[test]
fn a_pending_enrollment_handle_is_written_under_the_stored_key() {
    let id = PendingEnrollmentId::new(WorkspaceId::parse(WORKSPACE).unwrap(), 7, [3; 32]);
    let value = serde_json::to_value(&id).unwrap();
    let object = value.as_object().expect("a handle object");
    assert!(
        object.contains_key("room"),
        "a build any image can roll back to reads this key"
    );
    assert!(!object.contains_key("workspace"), "one key, one spelling");
    assert_eq!(
        serde_json::from_value::<PendingEnrollmentId>(value).unwrap(),
        id
    );
}

#[test]
fn a_pending_enrollment_handle_written_under_the_workspace_key_reads() {
    let id = PendingEnrollmentId::new(WorkspaceId::parse(WORKSPACE).unwrap(), 7, [3; 32]);
    let mut value = serde_json::to_value(&id).unwrap();
    let object = value.as_object_mut().expect("a handle object");
    let workspace = object
        .remove("room")
        .expect("the handle names its workspace");
    object.insert("workspace".into(), workspace);
    assert_eq!(
        serde_json::from_value::<PendingEnrollmentId>(value).unwrap(),
        id,
        "the handle reads under the key it was written under"
    );
}

#[test]
fn an_owner_file_from_before_the_join_version_bump_restores() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    let joined = enrolled();
    drop(joined.owner);
    let now_ns = u64::try_from(
        joined
            .now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let revocations = tilo_user_identity::RevocationList::new();
    // The record's invitation, re-signed as the device that issued it by the
    // build before the version bump.
    let mut value = whole_record(&joined.path);
    let stored = value["rooms"][joined.workspace.to_string()]
        .as_object_mut()
        .expect("a stored workspace");
    let invitation = STANDARD
        .decode(stored["invitation"].as_str().expect("base64 invitation"))
        .unwrap();
    let verified =
        crate::join::decode_and_verify_invitation(&invitation, None, &revocations, now_ns).unwrap();
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let governor = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[62; 32]),
        &governor_key,
        "Governor",
    );
    let previous_invitation = crate::join::issue_previous_invitation(
        verified.grant(),
        verified.inviter(),
        &governor,
        &governor_key,
    )
    .unwrap();
    // The proof the same device signed then: the challenge it answered in the
    // layout before the bump, with the authority the invitation named.
    let device_key = SigningKey::from_bytes(&[63; 32]);
    let device = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[64; 32]),
        &device_key,
        "Member",
    );
    let previous_verified =
        crate::join::decode_and_verify_invitation(&previous_invitation, None, &revocations, now_ns)
            .unwrap();
    let challenge = crate::join::JoinChallenge::new(
        &previous_verified,
        &device,
        [66; 32],
        [67; 32],
        now_ns.checked_add(30_000_000_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let complete =
        crate::join::complete_previous_layout(&challenge, &governor, &device_key).unwrap();
    let previous_intent = crate::owner::VerifiedNativeEnrollmentIntent::previous_layout_bytes(
        complete.clone(),
        device,
        0,
        joined.now,
    )
    .unwrap();
    stored.insert(
        "invitation".into(),
        serde_json::Value::String(STANDARD.encode(&previous_invitation)),
    );
    stored.insert(
        "enrollment_intent".into(),
        serde_json::Value::String(STANDARD.encode(&previous_intent)),
    );
    // That build's file, where the split layout this build keeps is not.
    std::fs::remove_dir_all(crate::owner::file_store::parts_of(&joined.path)).unwrap();
    std::fs::write(&joined.path, serde_json::to_vec(&value).unwrap()).unwrap();

    let reopened = open_file(&joined.path).expect("a file from before the join version bump reads");
    assert_eq!(reopened.workspaces(), vec![joined.workspace]);

    // Opened once, the file is written in this build's form; the proof keeps
    // the bytes it was signed as.
    let value = whole_record(&joined.path);
    let stored_intent = STANDARD
        .decode(
            value["rooms"][joined.workspace.to_string()]["enrollment_intent"]
                .as_str()
                .expect("base64 intent"),
        )
        .unwrap();
    let intent = crate::owner::VerifiedNativeEnrollmentIntent::restore(&stored_intent).unwrap();
    assert_eq!(
        intent.durable_bytes().unwrap(),
        stored_intent,
        "the record is in the form this build writes"
    );
    assert_eq!(
        intent.complete_bytes(),
        complete,
        "the stored proof is still the one signed"
    );
}

#[test]
fn owner_files_hold_byte_fields_as_compact_base64() {
    let joined = enrolled();
    for (file, bytes) in record_on_disk(&joined.path) {
        assert!(!bytes.contains(&b'\n'), "{} is compact", file.display());
    }
    let file = workspace_file(&joined.path, joined.workspace);
    let bytes = std::fs::read(&file).unwrap();
    assert!(!bytes.contains(&b'\n'), "the workspace's file is compact");
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for field in ["peer", "invitation", "enrollment_intent"] {
        assert!(value[field].is_string(), "{field} is base64 text");
    }
}

#[test]
fn retained_replay_reuses_exact_committed_publication() {
    let joined = enrolled();
    let governor = joined
        .owner
        .with_current(joined.workspace, |view| {
            Ok(view.workspace_host().device_public_key)
        })
        .unwrap();
    let message = joined.replay.clone();
    let before = record_on_disk(&joined.path);
    let generation = joined
        .owner
        .with_current(joined.workspace, |view| Ok(view.generation()))
        .unwrap();
    let report = joined
        .owner
        .receive(
            joined.workspace,
            TransportOrigin::RetainedRelayDelta(governor),
            message,
            joined.now,
        )
        .unwrap();
    assert!(matches!(
        report.primary(),
        crate::InputDisposition::Duplicate(_)
    ));
    assert_eq!(
        joined
            .owner
            .with_current(joined.workspace, |view| Ok(view.generation()))
            .unwrap(),
        generation,
        "an unchanged authenticated replay must reuse the actual committed generation"
    );
    assert_eq!(
        record_on_disk(&joined.path),
        before,
        "an unchanged authenticated replay must not rewrite durable owner state"
    );
}

/// A reader that keeps a sealed record opened opens each write once: a read again opens nothing,
/// and a reader holding the revision is told it is unchanged.
#[test]
fn a_kept_sealed_record_opens_each_write_once() {
    let joined = enrolled();
    let reader = member().verifying_key().to_bytes();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let mut decryptions = 0;
    let mut kept = KeptRecord::default();
    let mut read = |holding: Holding<'_>, kept: &mut KeptRecord| {
        joined.owner.with_current(joined.workspace, |view| {
            let open = &mut |op: &collab_types::SignedOp| {
                decryptions += 1;
                let ciphertext =
                    collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
                keys.open_content(
                    op.artifact,
                    &op.encryption.ok_or("not sealed")?,
                    &ciphertext,
                )
                .map(|bytes| bytes.to_vec())
                .map_err(|error| error.to_string())
            };
            view.read_document_with_history(
                reader,
                (APP, ""),
                holding,
                1024 * 1024,
                joined.now,
                History { open, kept },
            )
        })
    };
    let Ok(DocumentRead::Captured(first)) = read(Holding::default(), &mut kept) else {
        panic!("a first read captures the history");
    };
    let holding = Holding {
        revision: Some(first.revision),
        heads: &first.heads,
    };
    assert!(
        matches!(read(holding, &mut kept), Ok(DocumentRead::Unchanged(revision)) if revision == first.revision)
    );
    assert!(
        matches!(read(Holding::default(), &mut kept), Ok(DocumentRead::Captured(again)) if again.bytes == first.bytes)
    );
    assert_eq!(decryptions, 1, "the kept record opened its one write once");
}

#[test]
fn encrypted_committed_history_reads_through_held_keys_without_mutating_the_owner() {
    let joined = enrolled();
    let reader = member().verifying_key().to_bytes();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let before = record_on_disk(&joined.path);
    let mut decryptions = 0;
    let mut opener = |op: &collab_types::SignedOp| {
        decryptions += 1;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(
            op.artifact,
            &op.encryption.ok_or("not sealed")?,
            &ciphertext,
        )
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
    };
    let captured = joined
        .owner
        .with_current(joined.workspace, |view| {
            assert!(
                view.read_document(reader, APP, "", None, 1024 * 1024, joined.now)
                    .is_err(),
                "the plaintext-only door must retain its encrypted refusal"
            );
            assert!(
                view.read_document_with_history(
                    [0x77; 32],
                    (APP, ""),
                    Holding::default(),
                    1024 * 1024,
                    joined.now,
                    History {
                        open: &mut opener,
                        kept: &mut KeptRecord::default(),
                    },
                )
                .is_err(),
                "a stranger cannot open history"
            );
            view.read_document_with_history(
                reader,
                (APP, ""),
                Holding::default(),
                1024 * 1024,
                joined.now,
                History {
                    open: &mut opener,
                    kept: &mut KeptRecord::default(),
                },
            )
        })
        .unwrap();
    let DocumentRead::Captured(captured) = captured else {
        panic!("a first encrypted read captures history");
    };
    assert_eq!(
        decryptions, 1,
        "only the admitted member's read opens the write"
    );
    let doc = AutoCommit::load(&captured.bytes).unwrap();
    assert_eq!(
        doc.get(ROOT, "topic")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        SECRET
    );
    assert_eq!(record_on_disk(&joined.path), before);
    let path = joined.path.clone();
    let workspace = joined.workspace;
    let now = joined.now;
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    assert!(matches!(reopened.with_current(workspace, |view| {
        let holding = Holding { revision: Some(captured.revision), heads: &[] };
        view.read_document_with_history(reader, (APP, ""), holding, 1024 * 1024, now, History {
            open: &mut |op| {
                keys.open_content(op.artifact, &op.encryption.ok_or("not sealed")?,
                    &collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?)
                    .map(|bytes| bytes.to_vec()).map_err(|error| error.to_string())
            },
            kept: &mut KeptRecord::default(),
        })
    }).unwrap(), DocumentRead::Unchanged(revision) if revision == captured.revision));
    assert_eq!(record_on_disk(&path), before);
}
