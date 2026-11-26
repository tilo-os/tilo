use crate::KeptDocument;
use crate::TransportOrigin;
use crate::owner::testing::{APP, WORKSPACE, document_on_disk, raw_fixture};
use crate::owner::{
    DeviceWorkspaces, DocumentRead, History, Holding, PendingEnrollmentId, WorkspaceDocumentWrite,
    WorkspaceId, WriteRefusal, open_file,
};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{path::PathBuf, time::SystemTime};

#[path = "busy_workspace_tests.rs"]
mod busy_workspace;
#[path = "changes_read_tests.rs"]
mod changes_read;
#[path = "refused_change_tests.rs"]
mod refused_change;
#[path = "sealed_history_tests.rs"]
mod sealed_history;
#[path = "vouched_tests.rs"]
mod vouched;

/// A write whose merge makes the automerge worker die: this device failing to
/// decide it, never the workspace refusing it.
const CRASHING_CHANGE: &str = "856f4a83a9cba9c000ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f010401800002017f02020002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103";

struct Enrolled {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: DeviceWorkspaces,
    workspace: WorkspaceId,
    now: SystemTime,
}

/// Join through the file store and admit the host's history.
fn enrolled() -> Enrolled {
    enrolled_with(raw_fixture())
}

/// [`enrolled`] into the workspace `fixture` holds.
fn enrolled_with(
    (intent, bootstrap, messages, owner_copy, now): crate::owner::testing::RawFixture,
) -> Enrolled {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let owner = open_file(&path).unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = crate::owner::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    // What the workspace holds reaches this device as the answer's replay.
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedServerDelta(owner_copy),
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

fn member() -> SigningKey {
    SigningKey::from_bytes(&[63; 32])
}

fn edit_with(
    owner: &DeviceWorkspaces,
    workspace: WorkspaceId,
    now: SystemTime,
    topic: &str,
) -> collab_types::SignedOp {
    owner
        .with_current(workspace, |view| edit_from_view(view, now, topic))
        .unwrap()
}

/// Admit `write`, this device's signed write to the fixture's record, and make
/// it durable.
fn commit(
    owner: &DeviceWorkspaces,
    workspace: WorkspaceId,
    envelope: collab_types::SignedOp,
    now: SystemTime,
) -> Result<(), WriteRefusal> {
    owner.admit_writes(
        workspace,
        vec![WorkspaceDocumentWrite {
            app: APP.into(),
            document: String::new(),
            readers: None,
            envelope,
        }],
        now,
    )
}

fn edit_from_view(
    view: &crate::owner::NativeCommittedView,
    now: SystemTime,
    topic: &str,
) -> Result<collab_types::SignedOp, String> {
    let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
    let before = chat.get_heads();
    chat.put(ROOT, "topic", topic).unwrap();
    // The member's device writes as its own person, never as the workspace's owner.
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
    Ok(*envelope)
}

tilo_test_env::scenario! {
    fn an_owners_files_are_private_named_and_compact;

    phase a_joined_workspace_is_kept_in_its_own_private_file_and_reopens;
    {
        let joined = enrolled();
        let basis = |owner: &DeviceWorkspaces| {
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

    phase a_read_between_the_frames_of_one_checkpoint_is_pending_not_refused;
    // A host's checkpoint reaches this owner as several signed frames. A read
    // between them hands out nothing and says the checkpoint is pending, so the
    // reader asks again; a refusal would end an app's collaboration for good.
    // It still refuses a device that may not read the record.
    {
        let joined = enrolled();
        let member = member().verifying_key().to_bytes();
        let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
        let owner_copy_key = SigningKey::from_bytes(&[61; 32]);
        let covered = edit_with(&joined.owner, joined.workspace, joined.now, "covered next");
        commit(&joined.owner, joined.workspace, covered, joined.now).unwrap();
        let mut remote = joined
            .owner
            .with_current(joined.workspace, |view| Ok(view.peer().clone()))
            .unwrap();
        let frames = remote
            .publication_mut()
            .bootstrap_messages(&owner_copy_key, joined.now)
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
                    crate::fixture::origin(&owner_copy_key),
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

    phase an_unchanged_committed_read_answers_unchanged_to_a_member_and_is_refused_a_stranger;
    {
        let joined = enrolled();
        let member = member().verifying_key().to_bytes();
        let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
        let read = |reader, known| {
            joined.owner.with_current(joined.workspace, |view| {
                view.read_document(reader, APP, "", known, usize::MAX, joined.now)
            })
        };
        let Ok(DocumentRead::Captured(document)) = read(member, None) else {
            panic!("the first read admits the document");
        };
        assert!(matches!(
            read(member, Some(document.revision)),
            Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
        ));
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
        assert_eq!(holds(member, document.revision), Ok(Some(true)));
        assert_eq!(
            holds(member, [0; 32]),
            Ok(Some(false)),
            "another revision is not held"
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
        key_group::KeyGroup::create(channel, key_group::MemberKey::generate("host").unwrap())
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
    let mut envelope = edit_with(&joined.owner, joined.workspace, joined.now, SECRET);
    let (metadata, sealed) = keys
        .seal_content(envelope.artifact, class, &envelope.payload)
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, &member()).unwrap();
    commit(
        &joined.owner,
        joined.workspace,
        envelope.clone(),
        joined.now,
    )
    .unwrap();
    envelope
}

tilo_test_env::scenario! {
    fn sealed_history_is_read_through_its_keys;

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

/// A shortcut with a step is committed through the workspace's membership write
/// and read back the same: its step keeps the kind it carries flattened.
#[tokio::test]
async fn a_shortcut_with_a_step_is_committed_and_read_back() {
    use crate::workspace_membership::SettingsChange;
    use tilo_automation::ShortcutChange;

    let joined = enrolled();
    let (owner, workspace, now) = (&joined.owner, joined.workspace, joined.now);
    let certificate = owner
        .with_current(workspace, |view| Ok(view.device().clone()))
        .unwrap();
    let key = crate::owner::testing::member_key();
    let mut shortcut: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [{
            "id": "list",
            "step": "invoke",
            "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
            "resource": {"fixed": "notes.folder:inbox"},
        }],
    }))
    .expect("a shortcut with a step");
    crate::workspace::sign_saved_shortcut(
        &workspace.to_string(),
        &mut shortcut,
        &certificate,
        &key,
    )
    .unwrap();
    let command = crate::workspace_membership::WorkspaceMembershipCommand::Settings(
        SettingsChange::Shortcuts(ShortcutChange::Save(Box::new(shortcut.clone()))),
    );
    assert!(
        owner
            .commit_membership(workspace, &certificate, &command, &key, now)
            .await
            .unwrap(),
        "the workspace takes the shortcut"
    );
    let held = owner
        .with_current(workspace, |view| {
            Ok(tilo_desktop_model::projection::workspace_shortcuts_of(
                view.peer().admitted_workspace().expect("the workspace"),
            ))
        })
        .unwrap();
    assert_eq!(
        tilo_automation::decode_saved(&held),
        vec![shortcut],
        "the workspace holds the step the person saved"
    );
}
