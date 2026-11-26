//! A sealed document as the build before reader sets kept it, opened by this one.
//!
//! The bytes beside this file are what that build (f7ca66db910) wrote: `durable.hex`, the durable
//! state of a workspace's keyless server holding document `test.app`/`tab:1`, sealed under the
//! workspace's keys (`keys.hex`), with three writes: the owner's device `device(1, 2)` set
//! `title` and `owner-note` (its writes 3 and 4) and the editor's `device(110, 111)` set
//! `editor-note` (its write 1). `frame.hex` is the editor's write as that build's store kept it,
//! at the path [`FRAME_PATH`] under the store's root. Every write names its document under the
//! artifact of then ([`collab_types::artifact_before_reader_sets`]). A write that build makes
//! reaches this one live under the same artifact, sealed or in the clear.

use super::sealed_compaction_tests::{APP, RECORD as DOC, device, open, person};
use super::*;
use collab_types::CommitNamed as _;
use collab_types::ContentRetention;
use key_group::ContentKeys;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const FRAME_PATH: &str = "workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/1cfb749f6f313023861df11be00978118e738b9dd2711185ccdecb266818441e/c86d01ad8b2fb694b22ff60e3c9d9e7ed90db5604772b74450d40913b9a9cbaf/0000000000000001.d";
const WRITTEN: [(&str, &str); 3] = [
    ("title", "Before the update"),
    ("owner-note", "written by the owner"),
    ("editor-note", "written by the editor"),
];

fn kept(hex: &str) -> Vec<u8> {
    hex::decode(hex.split_whitespace().collect::<String>()).expect("hex")
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap()
}

/// The server's state, opened by this build.
fn opened() -> StatePeer {
    StatePeer::restore(
        &kept(include_str!("durable.hex")),
        WORKSPACE,
        device(1, 2).1,
        vec![],
        RevocationList::new(),
        now(),
    )
    .expect("a state written before reader sets opens")
}

fn keys() -> ContentKeys {
    ContentKeys::from_bytes(&kept(include_str!("keys.hex"))).unwrap()
}

/// What `peer`'s document reads as, opened with the workspace's keys.
fn read(peer: &StatePeer) -> AutoCommit {
    let keys = keys();
    let opened = peer
        .publication()
        .opened_document(APP, DOC, &mut |op| open(&keys, op))
        .unwrap()
        .expect("the document is held");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    AutoCommit::load(&opened.document).unwrap()
}

fn assert_reads_everything(peer: &StatePeer) {
    let document = read(peer);
    for (key, value) in WRITTEN {
        let held = document
            .get(ROOT, key)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok());
        assert_eq!(held.as_deref(), Some(value), "{key}");
    }
}

/// `peer`'s document's history sealed again under the document's own scope by `converter`, a device
/// holding the workspace's keys, as its compaction: signed by it alone.
fn reseal(peer: &mut StatePeer, converter: &SigningKey) {
    let keys = keys();
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, DOC);
    let mut ops = peer
        .publication()
        .draft_sealed_compaction(
            (APP, DOC),
            None,
            None,
            &mut |op| open(&keys, op),
            &mut |frame| {
                keys.seal_content(artifact, ContentRetention::of_frame(frame), frame)
                    .map_err(|error| error.to_string())
            },
            converter.verifying_key().to_bytes(),
            now(),
        )
        .unwrap();
    for op in &mut ops {
        collab_types::sign_op(op, converter).unwrap();
    }
    peer.publication_mut()
        .admit_sealed_compaction(APP, DOC, None, ops, now())
        .unwrap();
}

tilo_test_env::scenario! {
    fn a_document_sealed_before_reader_sets_is_sealed_again_and_keeps_its_originals;

    phase it_opens_with_every_write_its_authors_signed;
    {
        let peer = opened();
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
            "a key holder seals it again at once"
        );
        // A key holder reads it as it is, before anything is sealed again.
        assert_reads_everything(&peer);
    }

    phase a_device_without_the_keys_keeps_every_write_until_one_with_them_seals_it_again;
    {
        let peer = opened();
        assert!(peer.publication().awaiting_reseal(APP, DOC), "the document waits");
        // Its writes are held, not refused: nothing of the document is gone.
        assert_eq!(peer.publication().sealed_history(APP, DOC).len(), 3);
        // A replica that opens nothing reads none of it, and loses none of it.
        let mut shut = |_: &SignedOp| Err::<Vec<u8>, String>("no keys here".into());
        assert!(
            peer.publication()
                .opened_document(APP, DOC, &mut shut)
                .map_or(true, |opened| opened.is_none_or(|opened| !opened.unmerged.is_empty()))
        );
    }

    phase the_first_device_with_the_keys_seals_it_again_signing_only_as_itself;
    {
        let mut peer = opened();
        let originals = peer.publication().sealed_history(APP, DOC);
        let (converter, _) = device(1, 3);
        reseal(&mut peer, &converter);

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
        assert_reads_everything(&peer);

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
            now(),
        )
        .unwrap();
        assert_eq!(again.publication().resealed_originals(APP, DOC), originals);
        assert_eq!(again.publication().sealed_history(APP, DOC), history);
        assert!(!again.publication().awaiting_reseal(APP, DOC));
        assert_reads_everything(&again);
        // What it digests is what it keeps, the originals among it.
        assert_eq!(
            again.durable_hash().unwrap(),
            again.durable_hash_of_kept_parts().unwrap()
        );
    }

    phase a_frame_its_store_kept_is_served_and_kept_as_today;
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FRAME_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = kept(include_str!("frame.hex"));
        std::fs::write(&path, &bytes).unwrap();
        let store = crate::store::Store::open(root.path(), crate::store::Appetite::StoreAll)
            .unwrap();
        let workspace = crate::join::WorkspaceId::parse(WORKSPACE).unwrap();
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
        // Kept again in today's layout, in a segment: the envelope's bytes are the same.
        let today = postcard::to_allocvec(&replay.frames[0]).unwrap();
        assert_eq!(store.bytes(), u64::try_from(today.len()).unwrap());
        assert_eq!(today.get(..bytes.len()), Some(bytes.as_slice()));
        assert!(!path.exists(), "the frame's own file went once it was in a segment");
        // And the store says which layout it keeps now: opened again, nothing is written.
        assert_eq!(
            std::fs::read_to_string(root.path().join("frames-version")).unwrap(),
            crate::store::FRAMES_VERSION.to_string()
        );
        drop(store);
        let again = crate::store::Store::open(root.path(), crate::store::Appetite::StoreAll)
            .unwrap();
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
        now(),
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
        let mut author = super::sealed_compaction_tests::Author::new(&f.replica);
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
        let mut author = super::sealed_compaction_tests::Author::new(&f.replica);
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

/// A store that kept its frames as version 2, which today's frame reads as: it brings the
/// version it keeps up to today's before serving, and the frame reads whole.
#[test]
fn a_store_kept_as_version_two_is_brought_up_to_today() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FRAME_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = kept(include_str!("frame.hex"));
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(root.path().join("frames-version"), b"2").unwrap();
    let store = crate::store::Store::open(root.path(), crate::store::Appetite::StoreAll).unwrap();
    let workspace = crate::join::WorkspaceId::parse(WORKSPACE).unwrap();
    let replay = store
        .replay(workspace, &[], crate::store::Budget::default())
        .unwrap();
    assert_eq!(replay.frames.len(), 1, "the frame is served");
    assert!(matches!(
        &replay.frames[0],
        NeighbourMsg::AppDocument { readers: None, .. }
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("frames-version")).unwrap(),
        crate::store::FRAMES_VERSION.to_string(),
        "the store records the version it keeps now"
    );
}
