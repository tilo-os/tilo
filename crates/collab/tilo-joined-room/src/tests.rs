use crate::testing::{APP, ROOM, raw_fixture};
use crate::{DocumentRead, NativeWorkspaceOwner, RoomId, open_file};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::TransportOrigin;

#[path = "busy_room_tests.rs"]
mod busy_room;
#[path = "changes_read_tests.rs"]
mod changes_read;
#[path = "member_initialization_tests.rs"]
mod member_initialization;

struct Enrolled {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    replay: NeighbourMsg,
}

/// Enroll through the file store and admit the governor's history.
fn enrolled() -> Enrolled {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-rooms.json");
    let owner = open_file(&path).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = crate::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let replay = messages
        .iter()
        .rev()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("fixture has signed app state")
        .clone();
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                room,
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
        room,
        now,
        replay,
    }
}

fn member() -> SigningKey {
    SigningKey::from_bytes(&[63; 32])
}

fn edit(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> OwnerCommitRequest {
    edit_with(owner, room, now, "written while joined")
}

fn edit_with(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    topic: &str,
) -> OwnerCommitRequest {
    owner
        .with_current(room, |view| edit_from_view(view, now, topic))
        .unwrap()
}

fn edit_from_view(
    view: &crate::NativeCommittedView,
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
        &member(),
        now,
    )?;
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("test edit was not a signed document write".into());
    };
    Ok(OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope,
        },
    })
}

#[test]
fn a_joined_room_is_kept_in_its_own_private_file_and_reopens() {
    let joined = enrolled();
    let policy_calls = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(joined.room, |view| {
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
    assert_eq!(reopened.rooms(), vec![joined.room]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&joined.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the record is owner-only");
    }
}

#[test]
fn a_local_write_is_durable_with_its_outgoing_custody_before_its_receipt() {
    let joined = enrolled();
    let request = edit(&joined.owner, joined.room, joined.now);
    let receipt = joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let committed = std::fs::read(&joined.path).unwrap();
    let retry = joined.owner.commit_document(request, joined.now).unwrap();
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(
        std::fs::read(&joined.path).unwrap(),
        committed,
        "an exact retry does not rewrite"
    );
    let outgoing = |owner: &NativeWorkspaceOwner| {
        owner
            .with_outgoing(joined.room, joined.now, |_, outgoing| {
                Ok(outgoing.entries().count())
            })
            .unwrap()
    };
    assert_eq!(outgoing(&joined.owner), 1);
    drop(joined.owner);
    assert_eq!(outgoing(&open_file(&joined.path).unwrap()), 1);
}

#[test]
fn prepared_local_write_uses_current_sequence_after_an_intervening_commit() {
    let joined = enrolled();
    let stale = edit_with(&joined.owner, joined.room, joined.now, "stale edit");
    let first = edit_with(
        &joined.owner,
        joined.room,
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
        .commit_document_with_current(
            joined.room,
            |view| edit_from_view(view, joined.now, "edit from current owner turn"),
            joined.now,
        )
        .unwrap();
    assert!(receipt.generation > 1);
    joined
        .owner
        .with_current(joined.room, |view| {
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            assert_eq!(
                chat.get(ROOT, "topic").unwrap().unwrap().0.as_str(),
                Some("edit from current owner turn")
            );
            Ok(())
        })
        .unwrap();
}

/// Commit one original edit that the governor has not yet admitted, then give
/// the route a fresh governor checkpoint that does not cover it, so the next
/// `reconcile_outgoing` signs a recovery projection at the dense `floor + 1`.
fn with_pending_original(joined: &Enrolled) {
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let mut remote = joined
        .owner
        .with_current(joined.room, |view| Ok(view.peer().clone()))
        .unwrap();
    let original = edit_with(
        &joined.owner,
        joined.room,
        joined.now,
        "original awaiting ack",
    );
    joined.owner.commit_document(original, joined.now).unwrap();
    for message in remote
        .publication_mut()
        .durable_bootstrap_messages(&governor_key, joined.now, 600)
        .unwrap()
    {
        joined
            .owner
            .receive(
                joined.room,
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                message,
                joined.now,
            )
            .unwrap();
    }
}

fn projections(joined: &Enrolled) -> usize {
    joined
        .owner
        .with_current(joined.room, |view| {
            Ok(crate::testing::projection(view)?.len())
        })
        .unwrap()
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
        .with_current(joined.room, |view| {
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
        document_artifact_id(APP, ROOM, ""),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        document_attestation_id(APP, ROOM, ""),
        OpKind::Change,
        joined.now,
        next_unordered_document_sequence(joined.now),
    );
    sign_op(&mut envelope, &key).unwrap();
    OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope: Box::new(envelope),
        },
    }
}

#[test]
fn dense_snapshot_sequence_is_stale_after_an_intervening_recovery_projection() {
    let joined = enrolled();
    with_pending_original(&joined);
    let dense = edit_with(&joined.owner, joined.room, joined.now, "dense edit");
    assert_eq!(projections(&joined), 0);
    joined
        .owner
        .reconcile_outgoing(joined.room, &member(), joined.now)
        .unwrap();
    assert_eq!(
        projections(&joined),
        1,
        "the owner's own turn must sign a recovery projection at floor + 1"
    );
    assert_eq!(
        joined.owner.commit_document(dense, joined.now).unwrap_err(),
        "native local document signed sequence is stale"
    );
}

#[test]
fn unordered_sequence_survives_an_intervening_recovery_projection() {
    let joined = enrolled();
    with_pending_original(&joined);
    let request = unordered_edit(&joined, "unordered edit");
    assert_eq!(projections(&joined), 0);
    joined
        .owner
        .reconcile_outgoing(joined.room, &member(), joined.now)
        .unwrap();
    assert_eq!(
        projections(&joined),
        1,
        "the owner's own turn must sign a recovery projection at floor + 1"
    );
    joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let CommitOperation::Document { envelope, .. } = &request.operation else {
        panic!("the unordered edit is a document write")
    };
    joined
        .owner
        .with_current(joined.room, |view| {
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

#[test]
fn an_uncertain_write_fences_every_room_until_reopened() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let joined = enrolled();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _subscription = joined.owner.observe_fences(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let request = edit(&joined.owner, joined.room, joined.now);
    assert!(
        with_failure(Checkpoint::AfterPublish, || joined
            .owner
            .commit_document(request.clone(), joined.now))
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(joined.owner.health().is_err());
    assert!(joined.owner.contains_room(joined.room));
    assert!(joined.owner.commit_document(request, joined.now).is_err());
    drop(joined.owner);
    open_file(&joined.path).unwrap().health().unwrap();
}

#[test]
fn a_pending_enrollment_without_a_slot_survives_restart_and_cancels() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-rooms.json");
    let (intent, _, _, _, now) = raw_fixture();
    let id = open_file(&path)
        .unwrap()
        .prepare_enrollment_into(&intent, None, now)
        .unwrap();
    let owner = open_file(&path).unwrap();
    assert!(owner.contains_room(id.room()));
    let invitation =
        tilo_peer_state::join::encode_invitation_text(intent.verified_join().signed_invitation());
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
    assert!(owner.cancel_enrollment(id.room(), &id).unwrap());
    assert!(
        open_file(&path)
            .unwrap()
            .pending_enrollment(id.room())
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
        joined.owner.with_current(joined.room, |view| {
            view.read_document(reader, APP, record, known, maximum, joined.now)
        })
    };
    let Ok(DocumentRead::Captured(document)) = read(member, "", None, usize::MAX) else {
        panic!("a member reads what the room admitted");
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
        "a device outside the room reads nothing"
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
    let covered = edit_with(&joined.owner, joined.room, joined.now, "covered next");
    joined.owner.commit_document(covered, joined.now).unwrap();
    let mut remote = joined
        .owner
        .with_current(joined.room, |view| Ok(view.peer().clone()))
        .unwrap();
    let frames = remote
        .publication_mut()
        .durable_bootstrap_messages(&governor_key, joined.now, 600)
        .unwrap();
    let read = |reader| {
        joined.owner.with_current(joined.room, |view| {
            view.read_document(reader, APP, "", None, usize::MAX, joined.now)
        })
    };
    let mut pending = 0;
    for frame in frames {
        joined
            .owner
            .receive(
                joined.room,
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
            Ok(DocumentRead::Unchanged(_)) => panic!("no revision was offered"),
            Err(error) => panic!("a read between checkpoint frames was refused: {error}"),
        }
    }
    assert!(
        pending > 0,
        "the checkpoint's frames leave the room between cuts"
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
fn an_unchanged_committed_read_does_not_restore_the_room() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let read = |reader, known| {
        joined.owner.with_current(joined.room, |view| {
            view.read_document(reader, APP, "", known, usize::MAX, joined.now)
        })
    };
    crate::document_read::reset_room_restores();
    let Ok(DocumentRead::Captured(document)) = read(member, None) else {
        panic!("the first read admits the document");
    };
    assert!(
        crate::document_read::room_restores() > 0,
        "a first read restores the room so the counter can see a later skip"
    );
    let restores = crate::document_read::room_restores();
    assert!(matches!(
        read(member, Some(document.revision)),
        Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
    ));
    assert_eq!(
        crate::document_read::room_restores(),
        restores,
        "an idle reread of the same revision must not restore the room"
    );
    assert!(
        read(stranger, Some(document.revision)).is_err(),
        "a device outside the room cannot reuse a revision it was never given"
    );
}

#[test]
fn a_held_revision_is_checked_from_heads_with_the_read_permission() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let Ok(DocumentRead::Captured(document)) = joined.owner.with_current(joined.room, |view| {
        view.read_document(member, APP, "", None, usize::MAX, joined.now)
    }) else {
        panic!("the first read admits the document");
    };
    let holds = |reader, known| {
        joined.owner.with_current(joined.room, |view| {
            view.holds_revision(reader, APP, "", known, joined.now)
        })
    };
    crate::document_read::reset_room_restores();
    assert_eq!(holds(member, document.revision), Ok(Some(true)));
    assert_eq!(
        holds(member, [0; 32]),
        Ok(Some(false)),
        "another revision is not held"
    );
    assert_eq!(
        crate::document_read::room_restores(),
        0,
        "answered from the admitted heads, without restoring the room"
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
    let channel = collab_types::document_channel_id(APP, ROOM, "");
    let mut group = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("steward").unwrap(),
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
    } = edit_with(&joined.owner, joined.room, joined.now, SECRET)
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
                room: ROOM.into(),
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

fn sent(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> Vec<Vec<u8>> {
    owner
        .with_outgoing(room, now, |_, outgoing| {
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
    let [bytes] = sent(&joined.owner, joined.room, joined.now)
        .try_into()
        .expect("the committed sealed checkpoint is released");
    let message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope: sent, .. } = &message else {
        panic!("a document write")
    };
    assert_eq!(**sent, envelope);

    let (channel, class) = retention(&message, false).expect("sealed state is retained");
    assert_eq!(class, Retention::Checkpoint, "the browser peer's class");
    let (_, recorded, incremental) = crate::publication::metadata(&message).unwrap();
    assert_eq!(recorded, channel, "the same host channel as a browser peer");
    assert!(
        incremental,
        "a member's sealed checkpoint is published as a delta"
    );
    let published = joined
        .owner
        .with_outgoing(joined.room, joined.now, |_, outgoing| {
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
        !crate::publication::metadata(&snapshot).unwrap().2,
        "a cleartext snapshot stays the governor's checkpoint"
    );
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut single_use {
        envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
            0,
            collab_types::document_channel_id(APP, ROOM, ""),
        ));
    }
    assert!(
        crate::publication::metadata(&single_use).is_err(),
        "a single-use MLS message has no retention class"
    );
}

#[test]
fn an_expired_sealed_write_is_renewed_through_its_key_holder_and_replaces_its_original() {
    let joined = enrolled();
    let (before, current) = rotated_channel();
    let original = commit_sealed(&joined, &before);
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    assert!(sent(&joined.owner, joined.room, later).is_empty());

    let mut asked = 0;
    joined
        .owner
        .renew_outgoing(
            joined.room,
            &member(),
            &mut |op| {
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
        .unwrap();
    assert_eq!(asked, 1);
    let status = joined.owner.outgoing_status(joined.room, later).unwrap();
    assert_eq!(status.pending, 1, "the renewal replaces its original");
    let [bytes] = sent(&joined.owner, joined.room, later).try_into().unwrap();
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
    assert_eq!(sent(&reopened, joined.room, later), vec![bytes]);
    for file in std::fs::read_dir(joined.path.parent().unwrap()).unwrap() {
        let contents = std::fs::read(file.unwrap().path()).unwrap();
        assert!(
            !contents
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "the keyless owner's files hold the plaintext"
        );
    }
}

#[test]
fn a_refused_renewal_leaves_the_sealed_write_in_custody_as_it_was() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let committed = std::fs::read(&joined.path).unwrap();
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    joined
        .owner
        .renew_outgoing(
            joined.room,
            &member(),
            &mut |_| Err("this device holds no current key".into()),
            later,
        )
        .unwrap();
    assert_eq!(std::fs::read(&joined.path).unwrap(), committed);
    assert_eq!(
        joined
            .owner
            .outgoing_status(joined.room, later)
            .unwrap()
            .pending,
        1
    );
    assert!(sent(&joined.owner, joined.room, later).is_empty());
    assert!(
        joined
            .owner
            .renew_outgoing(
                joined.room,
                &SigningKey::from_bytes(&[99; 32]),
                &mut |_| panic!("another signer's renewal asked the key holder"),
                later,
            )
            .is_err()
    );
}

#[test]
fn owner_file_holds_byte_fields_as_compact_base64() {
    let joined = enrolled();
    let current = std::fs::read(&joined.path).unwrap();
    assert!(!current.contains(&b'\n'), "the owner file is compact");
    let value: serde_json::Value = serde_json::from_slice(&current).unwrap();
    let room = &value["rooms"][joined.room.to_string()];
    for field in ["peer", "invitation", "enrollment_intent"] {
        assert!(room[field].is_string(), "{field} is base64 text");
    }
}

#[test]
fn retained_replay_reuses_exact_committed_publication() {
    let joined = enrolled();
    let governor = joined
        .owner
        .with_current(joined.room, |view| Ok(view.governor().device_public_key))
        .unwrap();
    let message = joined.replay.clone();
    let before = std::fs::read(&joined.path).unwrap();
    let generation = joined
        .owner
        .with_current(joined.room, |view| Ok(view.generation()))
        .unwrap();
    let report = joined
        .owner
        .receive(
            joined.room,
            TransportOrigin::RetainedRelayDelta(governor),
            message,
            joined.now,
        )
        .unwrap();
    assert!(matches!(
        report.primary(),
        tilo_peer_state::InputDisposition::Duplicate(_)
    ));
    assert_eq!(
        joined
            .owner
            .with_current(joined.room, |view| Ok(view.generation()))
            .unwrap(),
        generation,
        "an unchanged authenticated replay must reuse the actual committed generation"
    );
    assert_eq!(
        std::fs::read(&joined.path).unwrap(),
        before,
        "an unchanged authenticated replay must not rewrite durable owner state"
    );
}

#[test]
fn encrypted_committed_history_reads_through_held_keys_without_mutating_the_owner() {
    let joined = enrolled();
    let reader = member().verifying_key().to_bytes();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let before = std::fs::read(&joined.path).unwrap();
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
        .with_current(joined.room, |view| {
            assert!(
                view.read_document(reader, APP, "", None, 1024 * 1024, joined.now)
                    .is_err(),
                "the plaintext-only door must retain its encrypted refusal"
            );
            assert!(
                view.read_document_with_history(
                    [0x77; 32],
                    (APP, ""),
                    None,
                    1024 * 1024,
                    joined.now,
                    &mut opener
                )
                .is_err(),
                "a stranger cannot open history"
            );
            view.read_document_with_history(
                reader,
                (APP, ""),
                None,
                1024 * 1024,
                joined.now,
                &mut opener,
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
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
    let path = joined.path.clone();
    let room = joined.room;
    let now = joined.now;
    drop(joined.owner);
    let reopened = open_file(&path).unwrap();
    assert!(matches!(reopened.with_current(room, |view| {
        view.read_document_with_history(reader, (APP, ""), Some(captured.revision),
            1024 * 1024, now, &mut |op| {
                keys.open_content(op.artifact, &op.encryption.ok_or("not sealed")?,
                    &collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?)
                    .map(|bytes| bytes.to_vec()).map_err(|error| error.to_string())
            })
    }).unwrap(), DocumentRead::Unchanged(revision) if revision == captured.revision));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A governed change outside what media authorization covers: a sandbox
/// row named `name`.
fn governed_change(owner: &NativeWorkspaceOwner, room: RoomId, name: &str) -> NeighbourMsg {
    let mut doc = owner
        .with_current(room, |view| {
            Ok(view.peer().admitted_room().ok_or("no room")?.clone())
        })
        .unwrap();
    let before = doc.get_heads();
    let sandbox = match doc.get(ROOT, "sandbox").unwrap() {
        Some((automerge::Value::Object(_), sandbox)) => sandbox,
        _ => doc
            .put_object(ROOT, "sandbox", automerge::ObjType::Map)
            .unwrap(),
    };
    doc.put(
        &sandbox,
        name,
        automerge::ScalarValue::Bytes(b"{}".to_vec()),
    )
    .unwrap();
    doc.commit();
    NeighbourMsg::SyncResp {
        room: room.to_string(),
        changes: doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    }
}

#[test]
fn a_joined_room_follows_its_revoked_governors_successor_and_reopens_under_it() {
    use tilo_user_identity::{DeviceCert, GovernorSuccession, RevocationEntry, RevocationReason};
    let joined = enrolled();
    let root = SigningKey::from_bytes(&[62; 32]);
    let previous_key = SigningKey::from_bytes(&[61; 32]);
    let previous = crate::testing::certificate(&root, &previous_key, "Governor");
    let successor_key = SigningKey::from_bytes(&[70; 32]);
    let successor = crate::testing::certificate(&root, &successor_key, "Successor");
    let mut revocation = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: previous.device_public_key,
        revoked_at_ns: 2,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
    let succession =
        GovernorSuccession::sign(previous.clone(), revocation, successor.clone(), &root).unwrap();
    let governor = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(joined.room, |view| Ok(view.governor().clone()))
            .unwrap()
    };
    assert_eq!(governor(&joined.owner), previous);
    let from = |device: &DeviceCert| {
        joined.owner.receive(
            joined.room,
            TransportOrigin::RelayDelta(device.device_public_key),
            governed_change(&joined.owner, joined.room, &device.device_label),
            SystemTime::now(),
        )
    };
    from(&previous).unwrap();

    assert_eq!(
        joined
            .owner
            .follow_successions(std::slice::from_ref(&succession), joined.now)
            .unwrap(),
        vec![joined.room]
    );
    assert_eq!(governor(&joined.owner), successor);
    assert!(
        joined
            .owner
            .follow_successions(&[succession], joined.now)
            .unwrap()
            .is_empty(),
        "a room follows a succession once"
    );
    assert!(
        from(&previous).is_err(),
        "the revoked governor's governance is refused"
    );
    from(&successor).unwrap();

    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(
        governor(&reopened),
        successor,
        "restored under the successor"
    );
}
