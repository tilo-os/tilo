use crate::KeptDocument;
use crate::TransportOrigin;
use crate::owner::testing::{APP, WORKSPACE, document_on_disk, raw_fixture};
use crate::owner::{
    DocumentRead, DocumentWrites, History, Holding, HostAnswer, NativeWorkspaceOwner,
    PendingEnrollmentId, WorkspaceId, open_file,
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
#[path = "refused_change_tests.rs"]
mod refused_change;
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
    enrolled_with(raw_fixture())
}

/// [`enrolled`] into the workspace `fixture` holds.
fn enrolled_with(
    (intent, bootstrap, messages, governor, now): crate::owner::testing::RawFixture,
) -> Enrolled {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let owner = open_file(&path).unwrap();
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
                TransportOrigin::RetainedServerDelta(governor),
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

/// The file one workspace's record is in, as this device holds it: the
/// snapshot the index names (`<workspace>.<revision>.json`), not the journal
/// beside it (`<workspace>.<revision>.log`) nor a run of its outgoing ownership
/// (named by the workspace and the hash of its bytes), whose names begin the same way and hold
/// bytes that are no document.
fn workspace_file(path: &std::path::Path, workspace: WorkspaceId) -> PathBuf {
    let name = format!("{workspace}.");
    std::fs::read_dir(crate::owner::file_store::parts_of(path))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|file| {
            file.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&name))
                && file
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .expect("the record holds the workspace's file")
}

/// One workspace's record as its own file holds it.
fn workspace_json(path: &std::path::Path, workspace: WorkspaceId) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(workspace_file(path, workspace)).unwrap()).unwrap()
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
    chat.commit_edit("fixture", "topic", &crate::owner::testing::member_person());
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
            document: String::new(),
            envelope,
            readers: None,
        },
    })
}

tilo_test_env::scenario! {
    fn an_owners_files_are_private_named_and_compact;

    phase a_joined_workspace_is_kept_in_its_own_private_file_and_reopens;
    {
        let joined = enrolled();
        let basis = |owner: &NativeWorkspaceOwner| {
            owner
                .with_current(joined.workspace, |view| {
                    view.peer()
                        .security_basis()
                        .ok_or("missing admitted workspace")?
                        .map_err(|error| error.to_string())
                })
                .unwrap()
        };
        let held = basis(&joined.owner);
        drop(joined.owner);
        let reopened = open_file(&joined.path).unwrap();
        assert_eq!(basis(&reopened), held, "the reopened file holds the same governance");
        assert_eq!(reopened.workspaces(), vec![joined.workspace]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let parts = crate::owner::file_store::parts_of(&joined.path);
            let mode = std::fs::metadata(&parts).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the record's directory is owner-only");
            for (file, _) in document_on_disk(&joined.path) {
                let mode = std::fs::metadata(&file).unwrap().permissions().mode();
                assert_eq!(mode & 0o077, 0, "{} is owner-only", file.display());
            }
        }
    }

    phase a_workspace_file_names_its_host_under_the_stored_key;
    {
        let joined = enrolled();
        let host = joined
            .owner
            .with_current(joined.workspace, |view| Ok(view.workspace_host().clone()))
            .unwrap();
        drop(joined.owner);
        let value = workspace_json(&joined.path, joined.workspace);
        let stored = value.as_object().expect("a stored workspace");
        assert!(
            stored.contains_key("workspace_host"),
            "the stored key names the workspace's host: {}",
            serde_json::to_string_pretty(&value).unwrap()
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

    phase owner_files_hold_byte_fields_as_compact_base64;
    {
        let joined = enrolled();
        // The journals beside the files are binary.
        for (file, bytes) in document_on_disk(&joined.path)
            .into_iter()
            .filter(|(file, _)| {
                file.extension()
                    .is_some_and(|extension| extension == "json")
            })
        {
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
}

/// A device owes a workspace nothing until it writes to it, and nothing again once the write is
/// answered: what its session reads to send nothing, without judging the device's standing.
#[test]
fn a_device_owes_a_workspace_nothing_until_it_writes_and_once_it_is_answered() {
    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    assert!(
        owner.owes_nothing(workspace).unwrap(),
        "a device that only entered"
    );
    owner
        .commit_document(edit(owner, workspace, now), now)
        .unwrap();
    assert!(!owner.owes_nothing(workspace).unwrap(), "its write waits");
    let written = owner
        .with_outgoing(workspace, now, |_, outgoing| Ok(outgoing.queued().last()))
        .unwrap()
        .unwrap();
    owner
        .retire_answered(
            workspace,
            &BTreeMap::from([(written, HostAnswer::Durable(1))]),
        )
        .unwrap();
    assert!(
        owner.owes_nothing(workspace).unwrap(),
        "its write was answered"
    );
}

tilo_test_env::scenario! {
    fn a_local_write_is_committed_durably_or_not_at_all;

    phase a_local_write_is_durable_with_its_outgoing_ownership_before_its_receipt;
    {
        let joined = enrolled();
        assert_eq!(joined.owner.unadmitted_writes(joined.workspace).unwrap(), 0);
        let request = edit(&joined.owner, joined.workspace, joined.now);
        let receipt = joined
            .owner
            .commit_document(request.clone(), joined.now)
            .unwrap();
        let committed = document_on_disk(&joined.path);
        let retry = joined.owner.commit_document(request, joined.now).unwrap();
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(
            document_on_disk(&joined.path),
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

    phase a_write_its_server_host_refused_leaves_the_replica_and_every_answer_is_counted;
    {
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
        let answered = DocumentWrites {
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
                .document_writes(workspace)
                .get(&(APP.to_owned(), String::new())),
            Some(&answered)
        );
        let reopened = open_file(&joined.path).unwrap();
        assert_eq!(topic(&reopened).as_deref(), Some("kept"));
        assert_eq!(
            reopened
                .document_writes(workspace)
                .get(&(APP.to_owned(), String::new())),
            Some(&answered)
        );
    }

    phase a_write_signed_outside_the_owner_turn_commits_with_exact_admission_evidence;
    {
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

    phase an_uncertain_write_fences_every_workspace_until_reopened;
    {
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

    phase retained_replay_reuses_exact_committed_publication;
    {
        let joined = enrolled();
        let governor = joined
            .owner
            .with_current(joined.workspace, |view| {
                Ok(view.workspace_host().device_public_key)
            })
            .unwrap();
        let message = joined.replay.clone();
        let before = document_on_disk(&joined.path);
        let generation = joined
            .owner
            .with_current(joined.workspace, |view| Ok(view.generation()))
            .unwrap();
        let report = joined
            .owner
            .receive(
                joined.workspace,
                TransportOrigin::RetainedServerDelta(governor),
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
            document_on_disk(&joined.path),
            before,
            "an unchanged authenticated replay must not rewrite durable owner state"
        );
    }
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
            chat.commit_edit("fixture", "topic", &crate::owner::testing::member_person());
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
            document: String::new(),
            envelope: Box::new(envelope),
            readers: None,
        },
    }
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
    let (taken_later, _) = joined
        .owner
        .commit_local_document(later.clone(), &key, joined.now)
        .await
        .unwrap();
    let heads = joined
        .owner
        .with_current(joined.workspace, |view| {
            Ok(view.peer().governance().heads())
        })
        .unwrap();
    let heads: Vec<[u8; 4]> = heads.iter().map(collab_types::governance_head).collect();
    let (taken_op, later_op) = (envelope(&taken_later), envelope(&later));
    assert_eq!(
        (taken_op.seq, &taken_op.payload, &taken_op.governance),
        (later_op.seq, &later_op.payload, &heads),
        "a write past the floor is taken under its own number, naming the governance it is \
         taken under"
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
            for write in [&taken_later, &taken] {
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

tilo_test_env::scenario! {
    fn a_pending_enrollment_stays_until_it_is_installed_or_cancelled;

    phase a_pending_enrollment_without_a_slot_survives_restart_and_cancels;
    {
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

    phase a_pending_enrollment_handle_names_its_workspace;
    {
        let id = PendingEnrollmentId::new(WorkspaceId::parse(WORKSPACE).unwrap(), 7, [3; 32]);
        let value = serde_json::to_value(&id).unwrap();
        let object = value.as_object().expect("a handle object");
        assert!(object.contains_key("workspace"));
        assert_eq!(
            serde_json::from_value::<PendingEnrollmentId>(value).unwrap(),
            id
        );
    }

    phase a_worker_crash_while_installing_leaves_the_join_pending;
    // A worker that crashed while installing the host's governance says nothing
    // about the enrollment: nothing is installed, and the join stays where it is
    // for the next attempt.
    {
        let (intent, mut bootstrap, _, _, now) = raw_fixture();
        bootstrap.governance_messages = vec![
            postcard::to_stdvec(&NeighbourMsg::SyncResp {
                changes: vec![hex::decode(CRASHING_CHANGE).unwrap()],
                workspace: WORKSPACE.into(),
            })
            .unwrap(),
        ];
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path = directory.path().join("joined-workspaces.json");
        let owner = open_file(&path).unwrap();
        let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        let enrollment = crate::owner::testing::finalize(intent, pending, bootstrap).unwrap();
        let error = owner
            .install(&enrollment, now)
            .expect_err("the crashed worker fails this install");
        assert!(
            error.is_device_failure(),
            "the crash is this device failing, not a refusal: {error:?}"
        );
        assert!(
            owner.with_current(workspace, |_| Ok(())).is_err(),
            "nothing was installed"
        );
        let held = owner
            .pending_enrollment(workspace)
            .unwrap()
            .expect("the join stays pending");
        assert_eq!(
            held.status(),
            crate::owner::PendingEnrollmentStatus::Active,
            "the next attempt finds the same join"
        );
    }
}

tilo_test_env::scenario! {
    fn a_committed_document_is_read_by_members_at_the_revision_they_hold;

    phase a_member_reads_the_admitted_document_and_a_stranger_does_not;
    {
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
        assert!(doc.get(ROOT, "tl").unwrap().is_some());
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

    phase a_read_between_the_frames_of_one_checkpoint_is_pending_not_refused;
    // A governor checkpoint reaches this owner as several signed frames. A read
    // between them hands out nothing and says the checkpoint is pending, so the
    // reader asks again; a refusal would end an app's collaboration for good.
    // It still refuses a device that may not read the record.
    {
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
                    TransportOrigin::ServerDelta(governor_key.verifying_key().to_bytes()),
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
                Ok(DocumentRead::Arriving) => {
                    panic!("a document outside any key group waits for no write it knows of")
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

    phase an_unchanged_committed_read_does_not_capture_the_document;
    {
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

    phase a_held_revision_is_checked_from_heads_with_the_read_permission;
    {
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
                    kept: &mut KeptDocument::default(),
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
}

/// What a sealed write says, which no file of the keyless owner may hold.
const SECRET: &str = "only key holders read this";

/// A channel's key holder, and its content keys before and after a rotation.
fn rotated_channel() -> (key_group::ContentKeys, key_group::ContentKeys) {
    let channel = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let mut group =
        key_group::KeyGroup::create(channel, key_group::MemberKey::generate("governor").unwrap())
            .unwrap();
    let before = group.content_keys().unwrap();
    group
        .add_member(
            key_group::MemberKey::generate("reader")
                .unwrap()
                .fresh_key_package()
                .unwrap(),
        )
        .unwrap();
    (before, group.content_keys().unwrap())
}

/// Commit a write sealed under `keys`, as an SDK app seals it, at `now`.
fn commit_sealed(joined: &Enrolled, keys: &key_group::ContentKeys) -> collab_types::SignedOp {
    commit_sealed_as(joined, keys, collab_types::ContentRetention::Delta)
}

/// Commit a write sealed under `keys` with the signed storage class `class`.
fn commit_sealed_as(
    joined: &Enrolled,
    keys: &key_group::ContentKeys,
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
                    document: String::new(),
                    envelope: envelope.clone(),
                    readers: None,
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

tilo_test_env::scenario! {
    fn sealed_history_is_published_and_read_through_its_keys;

    phase native_sealed_checkpoint_is_classified_like_browser;
    // Native publication classifies through the one host classifier browser
    // peers use, so a sealed checkpoint is a checkpoint to it there too. A
    // member still never replaces the record's retained checkpoint under its own
    // server author: the sealed checkpoint leaves as a delta after it, while a
    // cleartext snapshot stays a checkpoint the member never releases.
    {
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
            envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(0));
        }
        assert!(
            crate::owner::publication::metadata(&single_use).is_err(),
            "a single-use MLS message has no retention class"
        );
    }

    phase a_members_sealed_compaction_is_published_as_a_checkpoint;
    // A member's sealed compaction is the one checkpoint it publishes as itself:
    // the host replaces that member's retained history of the record with it,
    // where a sealed snapshot would only be appended after the checkpoint a
    // member never releases.
    {
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

    phase a_kept_sealed_record_opens_each_write_once;
    // A reader that keeps a sealed record opened opens each write once: a read again opens nothing,
    // and a reader holding the revision is told it is unchanged.
    {
        let joined = enrolled();
        let reader = member().verifying_key().to_bytes();
        let (keys, _) = rotated_channel();
        commit_sealed(&joined, &keys);
        let mut decryptions = 0;
        let mut kept = KeptDocument::default();
        let mut read = |holding: Holding<'_>, kept: &mut KeptDocument| {
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

    phase encrypted_committed_history_reads_through_held_keys_without_mutating_the_owner;
    {
        let joined = enrolled();
        let reader = member().verifying_key().to_bytes();
        let (keys, _) = rotated_channel();
        commit_sealed(&joined, &keys);
        let before = document_on_disk(&joined.path);
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
                    "the plaintext-only path must retain its encrypted refusal"
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
                            kept: &mut KeptDocument::default(),
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
                        kept: &mut KeptDocument::default(),
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
        assert_eq!(document_on_disk(&joined.path), before);
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
                kept: &mut KeptDocument::default(),
            })
        }).unwrap(), DocumentRead::Unchanged(revision) if revision == captured.revision));
        assert_eq!(document_on_disk(&path), before);
    }
}

#[tokio::test]
async fn a_member_with_no_sealed_history_past_its_bound_compacts_nothing() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    joined
        .owner
        .keep_sealed_histories(Arc::new(sealed_history::Held(keys)));
    let compacted = joined
        .owner
        .compact_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            joined.now,
        )
        .await
        .unwrap();
    assert_eq!(compacted, 0);
    assert_eq!(sent(&joined.owner, joined.workspace, joined.now).len(), 1);
}

/// Channel keys that renew sealed writes as `0` answers, and hold no history to compact.
struct Renewing<F>(F);

impl<F> crate::owner::SealedHistoryKeys for Renewing<F>
where
    F: Fn(&collab_types::SignedOp) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String>
        + Send
        + Sync,
{
    fn open(&self, _: WorkspaceId, _: &collab_types::SignedOp) -> Result<Vec<u8>, String> {
        Err("no history keys".into())
    }

    fn seal_checkpoint(
        &self,
        _: WorkspaceId,
        _: &str,
        _: &str,
        _named: Option<&str>,
        _: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        Err("no history keys".into())
    }

    fn renew(
        &self,
        _: WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        (self.0)(write)
    }
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

    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let current_keys = current.clone();
    let counted = Arc::clone(&asked);
    joined
        .owner
        .keep_sealed_histories(Arc::new(Renewing(move |op: &collab_types::SignedOp| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let metadata = op.encryption.ok_or("not sealed")?;
            let plaintext = current_keys
                .open_content(op.artifact, &metadata, &op.payload)
                .map_err(|e| e.to_string())?;
            current_keys
                .seal_content(
                    op.artifact,
                    collab_types::ContentRetention::Delta,
                    &plaintext,
                )
                .map_err(|e| e.to_string())
        })));
    let renewed = joined
        .owner
        .renew_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            later,
        )
        .await
        .unwrap();
    assert_eq!(renewed, 1);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
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
async fn a_refused_renewal_leaves_the_sealed_write_in_ownership_as_it_was() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let committed = document_on_disk(&joined.path);
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    joined
        .owner
        .keep_sealed_histories(Arc::new(Renewing(|_: &collab_types::SignedOp| {
            Err("this device holds no current key".into())
        })));
    joined
        .owner
        .renew_outgoing(
            joined.workspace,
            &crate::owner::testing::member_key(),
            later,
        )
        .await
        .unwrap();
    assert_eq!(document_on_disk(&joined.path), committed);
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
            .renew_outgoing(joined.workspace, &SigningKey::from_bytes(&[99; 32]), later)
            .await
            .is_err()
    );
}

/// A change chunk that makes Automerge's decoder panic in the isolated worker.
const CRASHING_CHANGE: &str = "856f4a83a9cba9c000ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f010401800002017f02020002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103";

/// A write drafted in an owner turn, not signed yet, under a write the same
/// device sent meanwhile on another of its connections, as a device's app
/// publishes the document it opens: numbered past that write, signed then and
/// saved, its document as it wrote it.
#[tokio::test]
async fn a_draft_under_a_write_its_device_sent_meanwhile_is_saved() {
    for whole in [false, true] {
        let joined = enrolled();
        let author = member().verifying_key().to_bytes();
        let drafted = joined
            .owner
            .with_current(joined.workspace, |view| {
                let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
                let before = chat.get_heads();
                chat.put(ROOT, "topic", "drafted before").unwrap();
                chat.commit_edit("fixture", "topic", &crate::owner::testing::member_person());
                let mutation = if whole {
                    collab_types::DocumentMutation::Snapshot(chat.save())
                } else {
                    collab_types::DocumentMutation::Incremental(chat.save_after(&before))
                };
                let envelope = view.peer().draft_write(
                    (APP, ""),
                    None,
                    &mutation.encode().map_err(|error| error.to_string())?,
                    None,
                    author,
                    0,
                    joined.now,
                )?;
                Ok(OwnerCommitRequest {
                    workspace: WORKSPACE.into(),
                    operation: CommitOperation::Document {
                        app: APP.into(),
                        document: String::new(),
                        envelope: Box::new(envelope),
                        readers: None,
                    },
                })
            })
            .unwrap();
        let sent = edit_with(
            &joined.owner,
            joined.workspace,
            joined.now,
            "sent meanwhile",
        );
        joined.owner.commit_document(sent, joined.now).unwrap();

        joined
            .owner
            .write(
                joined.workspace,
                |_| Ok(vec![drafted.clone()]),
                &member(),
                joined.now,
            )
            .await
            .unwrap_or_else(|refusal| panic!("whole {whole}: {refusal}"));
        joined
            .owner
            .with_current(joined.workspace, |view| {
                let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
                assert!(
                    chat.get_all(ROOT, "topic")
                        .unwrap()
                        .iter()
                        .any(|(value, _)| value.as_str() == Some("drafted before")),
                    "whole {whole}: the drafted write is in the document"
                );
                Ok(())
            })
            .unwrap();
    }
}

/// An app's write, signed by the identity service before the owner's turn and
/// naming no governance, is admitted naming the governance its owner admitted
/// it under: a peer that has not heard of the role it was written under yet
/// holds it rather than refusing it, and takes it in once the role arrives.
#[tokio::test]
async fn an_apps_write_names_the_governance_its_owner_admitted_it_under() {
    let (fixture, mut governor_peer, governor_key) =
        crate::owner::testing::governed_fixture_at(tilo_clock::wall());
    let joined = enrolled_with(fixture);
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let person = SigningKey::from_bytes(&[64; 32]);
    let device = crate::owner::testing::certificate(&person, &member(), "Member");
    let governor = governor_key.verifying_key().to_bytes();

    // The member is made a reader, which is all the receiver hears of, then an editor again.
    let mut set_role = |role| {
        assert!(
            governor_peer
                .governance_mut()
                .set_member_role(person.verifying_key().to_bytes(), role, &governor_key, now)
                .unwrap()
        );
        governor_peer.clone()
    };
    let mut receiver = set_role(crate::join::Role::Viewer);
    let governor_peer = set_role(crate::join::Role::Editor);
    owner
        .receive(
            workspace,
            TransportOrigin::ServerDelta(governor),
            governor_peer.workspace_message(),
            now,
        )
        .unwrap();

    let mut request = edit_with(owner, workspace, now, "written by an app");
    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
        panic!("the edit is a document write")
    };
    envelope.governance = Vec::new();
    collab_types::sign_op(envelope, &member()).unwrap();
    let (committed, _) = owner
        .commit_local_document(request, &member(), now)
        .await
        .unwrap();
    let CommitOperation::Document { envelope, .. } = committed.operation else {
        panic!("the commit is a document write")
    };
    let heads = owner
        .with_current(workspace, |view| Ok(view.peer().governance().heads()))
        .unwrap();
    assert!(!heads.is_empty());
    let heads: Vec<[u8; 4]> = heads.iter().map(collab_types::governance_head).collect();
    assert_eq!(
        envelope.governance, heads,
        "it names its owner's governance"
    );
    collab_types::verify_op(&envelope).expect("signed for what it names");

    let topic = |peer: &crate::StatePeer| {
        AutoCommit::load(&peer.document(APP, "").unwrap())
            .unwrap()
            .get_all(ROOT, "topic")
            .unwrap()
            .iter()
            .any(|(value, _)| value.as_str() == Some("written by an app"))
    };
    let write = NeighbourMsg::AppDocument {
        peer: crate::peer_handle(&device.device_public_key),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: String::new(),
        envelope,
        readers: None,
    };
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(device.device_public_key),
            write,
            now,
        )
        .expect("held, not refused");
    assert!(!topic(&receiver), "nothing of it is taken in yet");
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(governor),
            governor_peer.workspace_message(),
            now,
        )
        .unwrap();
    assert!(topic(&receiver), "taken in once the role arrived");
}
