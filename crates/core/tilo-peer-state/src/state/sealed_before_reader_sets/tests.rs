//! A document written before reader sets: read, kept, and sealed again.
//!
//! A peer holds document `test.app`/`tab:1` with three writes, each named under the artifact a
//! write made before reader sets names ([`collab_types::artifact_before_reader_sets`]): the
//! owner's device set `title` and `owner-note`, the editor's device set `editor-note`. Each is
//! sealed under the workspace's keys for that artifact, so the peer keeps the writes without
//! opening them, and a device holding the keys reads the document and seals it again under its
//! own scope.

use super::sealed_compaction_tests::{
    APP, Author, RECORD as DOC, admit, device, fixture, open, person,
};
use super::*;
use collab_types::CommitNamed as _;
use collab_types::ContentRetention;
use key_group::ContentKeys;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const WRITTEN: [(&str, &str); 3] = [
    ("title", "Before the update"),
    ("owner-note", "written by the owner"),
    ("editor-note", "written by the editor"),
];

/// A peer holding [`DOC`] with three writes from before reader sets in its history, and the
/// workspace's keys that read them.
struct Sealed {
    peer: StatePeer,
    keys: ContentKeys,
    /// The writes as they arrived, in order.
    writes: Vec<NeighbourMsg>,
}

/// [`Sealed`]: the owner's device writes twice, the editor's device once, each sealed under the
/// workspace's keys for [`collab_types::artifact_before_reader_sets`] and admitted by the peer
/// without them.
fn sealed() -> Sealed {
    let fixture = fixture();
    let keys = fixture.keys();
    let (owner, editor) = (fixture.owner.clone(), fixture.editor.clone());
    let mut owner_device = Author::by(&fixture.replica, &owner);
    let title = owner_device.edit("title", "Before the update");
    let note = owner_device.edit("owner-note", "written by the owner");
    let mut editor_device = Author::by(&fixture.replica, &editor);
    let their_note = editor_device.edit("editor-note", "written by the editor");
    let now = fixture.now;
    let mut peer = fixture.replica;
    let mut writes = Vec::new();
    for (key, plaintext) in [(owner.clone(), title), (owner, note), (editor, their_note)] {
        let seq = next_seq(&peer, &key);
        let message = named_before_reader_sets(&key, seq, DOC, plaintext, Some(&keys));
        admit(&mut peer, &key, message.clone(), now);
        writes.push(message);
    }
    Sealed { peer, keys, writes }
}

/// `key`'s next write number of [`DOC`], after the seed's own writes.
fn next_seq(peer: &StatePeer, key: &SigningKey) -> u64 {
    let route = Route {
        app: APP.into(),
        record: DOC.into(),
    };
    peer.core.documents[&route]
        .channel
        .last_seq_per_author
        .get(&collab_types::Ed25519PublicKey::from_bytes(
            key.verifying_key().to_bytes(),
        ))
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap()
}

/// What `peer`'s document reads as, opened with `keys`.
fn read(peer: &StatePeer, keys: &ContentKeys) -> AutoCommit {
    let opened = peer
        .publication()
        .opened_document(APP, DOC, &mut |op| open(keys, op))
        .unwrap()
        .expect("the document is held");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    AutoCommit::load(&opened.document).unwrap()
}

fn assert_reads_everything(peer: &StatePeer, keys: &ContentKeys) {
    let document = read(peer, keys);
    for (key, value) in WRITTEN {
        let found = document
            .get(ROOT, key)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok());
        assert_eq!(found.as_deref(), Some(value), "{key}");
    }
}

/// `peer`'s document's history sealed again under the document's own scope by `converter`, a
/// device holding the workspace's keys, as its compaction: signed by it alone.
fn reseal(peer: &mut StatePeer, keys: &ContentKeys, converter: &SigningKey) {
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, DOC);
    let mut ops = peer
        .publication()
        .draft_sealed_compaction(
            (APP, DOC),
            None,
            None,
            &mut |op| open(keys, op),
            &mut |frame| {
                keys.seal_content(artifact, ContentRetention::of_frame(frame), frame)
                    .map_err(|error| error.to_string())
            },
            converter.verifying_key().to_bytes(),
            fixture_now(),
        )
        .unwrap();
    for op in &mut ops {
        collab_types::sign_op(op, converter).unwrap();
    }
    peer.publication_mut()
        .admit_sealed_compaction(APP, DOC, None, ops, fixture_now())
        .unwrap();
}

/// The time every fixture is made at.
fn fixture_now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap()
}

tilo_test_env::scenario! {
    fn a_document_sealed_before_reader_sets_is_sealed_again_and_keeps_its_originals;

    phase it_opens_with_every_write_its_authors_signed;
    {
        let Sealed { peer, keys, .. } = sealed();
        let history = peer.publication().sealed_history(APP, DOC);
        assert_eq!(history.len(), 3);
        for write in &history {
            collab_types::verify_op(write).unwrap();
            assert!(collab_types::sealed_before_reader_sets(APP, WORKSPACE, DOC, write));
        }
        assert!(peer.publication().awaiting_reseal(APP, DOC));
        assert_eq!(
            peer.publication().sealed_to_compact(),
            vec![(APP.to_owned(), DOC.to_owned())],
            "a device with the keys seals it again at once"
        );
        // A device with the keys reads it as it is, before anything is sealed again.
        assert_reads_everything(&peer, &keys);
    }

    phase a_device_without_the_keys_keeps_every_write_until_one_with_them_seals_it_again;
    {
        let Sealed { peer, .. } = sealed();
        assert!(peer.publication().awaiting_reseal(APP, DOC), "the document waits");
        // Its writes are kept, not refused: nothing of the document is gone.
        assert_eq!(peer.publication().sealed_history(APP, DOC).len(), 3);
        // A device that opens nothing reads none of it, and loses none of it.
        let mut shut = |_: &SignedOp| Err::<Vec<u8>, String>("no keys here".into());
        match peer.publication().opened_document(APP, DOC, &mut shut) {
            Ok(Some(opened)) => assert!(!opened.unmerged.is_empty(), "every write waits"),
            Ok(None) => panic!("the document is kept"),
            Err(_) => {}
        }
    }

    phase the_first_device_with_the_keys_seals_it_again_signing_only_as_itself;
    {
        let Sealed { mut peer, keys, .. } = sealed();
        let originals = peer.publication().sealed_history(APP, DOC);
        let (converter, _) = device(1, 3);
        reseal(&mut peer, &keys, &converter);

        assert!(!peer.publication().awaiting_reseal(APP, DOC));
        let history = peer.publication().sealed_history(APP, DOC);
        let converter_key = collab_types::Ed25519PublicKey::from_bytes(
            converter.verifying_key().to_bytes(),
        );
        for write in &history {
            assert_eq!(write.author, converter_key, "the converter signs as itself alone");
            assert_eq!(
                write.artifact,
                collab_types::document_artifact_id(APP, WORKSPACE, DOC)
            );
            collab_types::verify_op(write).unwrap();
        }
        assert_reads_everything(&peer, &keys);

        // The originals stay beside the document, each as its author signed it.
        let kept = peer.publication().resealed_originals(APP, DOC);
        assert_eq!(kept, originals);
        for original in &kept {
            collab_types::verify_op(original).unwrap();
        }
        let editor = collab_types::Ed25519PublicKey::from_bytes(
            device(110, 111).0.verifying_key().to_bytes(),
        );
        assert!(kept.iter().any(|original| original.author == editor));

        // And so they stay when the state is kept and opened again.
        let again = StatePeer::restore(
            &peer.durable().unwrap(),
            WORKSPACE,
            device(1, 2).1,
            vec![],
            RevocationList::new(),
            fixture_now(),
        )
        .unwrap();
        assert_eq!(again.publication().resealed_originals(APP, DOC), originals);
        assert_eq!(again.publication().sealed_history(APP, DOC), history);
        assert!(!again.publication().awaiting_reseal(APP, DOC));
        assert_reads_everything(&again, &keys);
        // What it digests is what it keeps, the originals among it.
        assert_eq!(
            again.durable_hash().unwrap(),
            again.durable_hash_of_kept_parts().unwrap()
        );
    }

    phase a_frame_its_store_kept_is_served_and_kept_as_today;
    {
        let Sealed { writes, .. } = sealed();
        let editor_write = &writes[2];
        let root = tempfile::tempdir().unwrap();
        let store =
            crate::store::Store::open(root.path(), crate::store::Appetite::StoreAll).unwrap();
        let workspace = crate::join::WorkspaceId::parse(WORKSPACE).unwrap();
        let frame = crate::store::Frame::of(editor_write).unwrap();
        store.retain(workspace, &frame).unwrap();

        let replay = store
            .replay(workspace, &[], crate::store::Budget::default())
            .unwrap();
        assert_eq!(replay.frames.len(), 1, "the frame is served");
        let NeighbourMsg::AppDocument {
            envelope, readers, ..
        } = &replay.frames[0]
        else {
            panic!("a document write")
        };
        assert_eq!(*readers, None);
        collab_types::verify_op(envelope).unwrap();
        assert!(collab_types::sealed_before_reader_sets(APP, WORKSPACE, DOC, envelope));
        // Kept in a segment, its bytes the message's own.
        let today = postcard::to_allocvec(&replay.frames[0]).unwrap();
        assert_eq!(store.bytes(), u64::try_from(today.len()).unwrap());
        assert_eq!(store.segments(workspace).len(), 1, "the frame is in a segment");
        assert_eq!(
            std::fs::read_to_string(root.path().join("frames-version")).unwrap(),
            crate::store::FRAMES_VERSION.to_string()
        );
        // Opened again, it keeps the same bytes and writes nothing.
        drop(store);
        let again =
            crate::store::Store::open(root.path(), crate::store::Appetite::StoreAll).unwrap();
        assert_eq!(again.bytes(), u64::try_from(today.len()).unwrap());
    }
}

/// `key`'s write `seq` of `document`, named as the build before reader sets named every write of
/// a document ([`collab_types::artifact_before_reader_sets`]): `plaintext` sealed under the
/// workspace's `keys` for that artifact, or in the clear without them.
fn named_before_reader_sets(
    key: &SigningKey,
    seq: u64,
    document: &str,
    plaintext: Vec<u8>,
    keys: Option<&ContentKeys>,
) -> NeighbourMsg {
    let artifact = collab_types::artifact_before_reader_sets(APP, WORKSPACE, document);
    let (encryption, payload) = match keys {
        Some(keys) => {
            let (metadata, ciphertext) = keys
                .seal_content(artifact, ContentRetention::Delta, &plaintext)
                .unwrap();
            (Some(metadata), ciphertext)
        }
        None => (None, plaintext),
    };
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact,
        collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(APP, WORKSPACE, document),
        OpKind::Change,
        fixture_now(),
        seq,
    );
    op.encryption = encryption;
    collab_types::sign_op(&mut op, key).unwrap();
    let mut message = super::sealed_compaction_tests::message(op);
    if let NeighbourMsg::AppDocument { document_id, .. } = &mut message {
        *document_id = document.to_owned();
    }
    message
}

tilo_test_env::scenario! {
    fn a_write_arriving_live_as_the_build_before_reader_sets_named_it_is_its_documents;

    phase a_write_in_the_clear_is_taken_in;
    {
        let mut f = super::sealed_compaction_tests::fixture();
        let mut author = super::sealed_compaction_tests::Author::new(&f.replica);
        let heads = author.0.get_heads();
        author.0.put(ROOT, "before", "in the clear").unwrap();
        author.0.commit_edit("fixture", "before", &person(&f.replica, &f.editor));
        let edit = DocumentMutation::Incremental(author.0.save_after(&heads))
            .encode()
            .unwrap();
        let editor = f.editor.clone();
        super::sealed_compaction_tests::admit(
            &mut f.replica,
            &editor,
            named_before_reader_sets(&editor, 1, DOC, edit, None),
            f.now,
        );
        let document = AutoCommit::load(&f.replica.document(APP, DOC).unwrap()).unwrap();
        let held = document
            .get(ROOT, "before")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok());
        assert_eq!(held.as_deref(), Some("in the clear"));
    }

    phase a_sealed_write_is_kept_as_history_a_device_with_the_keys_seals_again;
    {
        let mut f = super::sealed_compaction_tests::fixture();
        let keys = f.keys();
        let mut author = super::sealed_compaction_tests::Author::by(&f.replica, &f.editor);
        let edit = author.edit("before", "sealed then");
        let editor = f.editor.clone();
        super::sealed_compaction_tests::admit(
            &mut f.replica,
            &editor,
            named_before_reader_sets(&editor, 1, DOC, edit, Some(&keys)),
            f.now,
        );
        let history = f.replica.publication().sealed_history(APP, DOC);
        assert_eq!(history.len(), 1);
        assert!(collab_types::sealed_before_reader_sets(APP, WORKSPACE, DOC, &history[0]));
        assert!(f.replica.publication().awaiting_reseal(APP, DOC));
        let opened = f
            .replica
            .publication()
            .opened_document(APP, DOC, &mut |op| open(&keys, op))
            .unwrap()
            .expect("the document is held");
        assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
        let held = AutoCommit::load(&opened.document)
            .unwrap()
            .get(ROOT, "before")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok());
        assert_eq!(held.as_deref(), Some("sealed then"));
    }

    phase a_write_named_for_another_document_is_refused;
    {
        let mut f = super::sealed_compaction_tests::fixture();
        let mut author = super::sealed_compaction_tests::Author::by(&f.replica, &f.editor);
        let edit = author.edit("before", "elsewhere");
        let keys = f.keys();
        let mut message = named_before_reader_sets(&f.editor, 1, "tab:2", edit, Some(&keys));
        if let NeighbourMsg::AppDocument { document_id, .. } = &mut message {
            *document_id = DOC.into();
        }
        let error = f
            .replica
            .receive_from(
                super::sealed_compaction_tests::origin(&f.editor),
                message,
                f.now,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("does not match its collaboration route"),
            "{error}"
        );
        assert!(f.replica.publication().sealed_history(APP, DOC).is_empty());
    }
}
