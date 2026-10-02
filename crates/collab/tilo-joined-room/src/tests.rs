//! The owner over its own file, as the standalone runtime keeps it.
use crate::testing::{APP, ROOM, raw_fixture};
use crate::{DocumentRead, NativeWorkspaceOwner, RoomId, open_file};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};
use tilo_peer_state::{
    NeighbourMsg, TransportOrigin,
    owner_commit::{CommitOperation, OwnerCommitRequest},
};

struct Enrolled {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
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
    let message = owner
        .with_current(room, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            chat.put(ROOT, "topic", topic).unwrap();
            chat.commit();
            view.peer().clone().publish_incremental(
                APP,
                "",
                chat.save_after(&before),
                &member(),
                now,
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a signed document write")
    };
    OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope,
        },
    }
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
    let OwnerCommitRequest {
        operation: CommitOperation::Document { mut envelope, .. },
        ..
    } = edit_with(&joined.owner, joined.room, joined.now, SECRET)
    else {
        panic!("a document write")
    };
    let (metadata, sealed) = keys
        .seal_content(
            envelope.artifact,
            collab_types::ContentRetention::Delta,
            &envelope.payload,
        )
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
