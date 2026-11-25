//! What a record names, as the device that wrote it reads it.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};

use super::*;

/// The saved document holding `fields`, each a string or bytes value.
fn record_of(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    for (key, value) in fields {
        doc.put(ROOT, *key, *value).unwrap();
    }
    doc.save()
}

const PHOTOS: &str = "so.tilo.photos";

tilo_test_env::scenario! {
    fn a_record_names_the_content_its_references_name_and_nothing_else;

    phase a_bytes_value_names_the_address_its_reference_keeps;
    {
        let plain = "a1".repeat(32);
        let stored = format!("blake3:{plain}:12");
        let doc = record_of(&[("photo", stored.as_str())]);
        assert_eq!(content_of(PHOTOS, &doc).unwrap(), [plain]);
    }

    phase a_sealed_reference_names_its_ciphertext_address;
    {
        let plain = "d4".repeat(32);
        let address = "e5".repeat(32);
        let key = tilo_seal::SymmetricKey::generate().unwrap();
        let reference = tilo_content::ContentRef::sealed(&address, key).to_string();
        let stored = format!("blake3:{plain}:4096:{reference}");
        let doc = record_of(&[("photo", stored.as_str())]);
        assert_eq!(
            content_of(PHOTOS, &doc).unwrap(),
            [address],
            "the host keeps and serves the ciphertext, which is keyed by its address"
        );
    }

    phase an_actions_content_address_names_its_content;
    {
        let hash = "b2".repeat(32);
        let mut doc = AutoCommit::new();
        let images = doc.put_object(ROOT, "images", ObjType::List).unwrap();
        doc.insert(&images, 0, format!("tilo.content:{hash}")).unwrap();
        assert_eq!(content_of(PHOTOS, &doc.save()).unwrap(), [hash]);
    }

    phase a_files_blob_address_and_a_pictures_content_are_named;
    // A file a message carries is named by the `tilo-blob://` address a
    // `File` reads at, and a picture in rich text by the hash under its
    // paragraph's `blob` attribute: what a member posts in Chat, and what a
    // feedback report's window picture is.
    {
        let sent = "6a".repeat(32);
        let pictured = "7b".repeat(32);
        let mut doc = AutoCommit::new();
        let carried = doc.put_object(ROOT, "attachments", ObjType::List).unwrap();
        let attached = doc.insert_object(&carried, 0, ObjType::Map).unwrap();
        doc.put(&attached, "blob", format!("tilo-blob://{sent}"))
            .unwrap();
        let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
        let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
        let picture = doc.put_object(&blocks, "shot", ObjType::Map).unwrap();
        doc.put(&picture, "kind", "image").unwrap();
        let attrs = doc.put_object(&picture, "attrs", ObjType::Map).unwrap();
        doc.put(&attrs, tilo_automerge::rich_text::ATTR_BLOB, pictured.as_str())
            .unwrap();
        assert_eq!(
            content_of(tilo_app_id::apps::CHAT, &doc.save()).unwrap(),
            [sent, pictured]
        );
    }

    phase a_value_that_only_looks_like_an_address_names_nothing;
    // A device key, an id, a hash a record keeps for its own reasons:
    // nothing stands behind them for the host to keep, and asking the
    // workspace for them would leave it lacking content forever.
    {
        let key = "f6".repeat(32);
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "device", key.as_str()).unwrap();
        doc.put(ROOT, "host", [7_u8; 32].to_vec()).unwrap();
        let entries = doc.put_object(ROOT, "servers", ObjType::Map).unwrap();
        doc.put_object(&entries, key.as_str(), ObjType::Map).unwrap();
        let note = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&note, 0, 0, &format!("see {key} and blake3:{key}:9"))
            .unwrap();
        doc.put(ROOT, "label", format!("key {key}")).unwrap();
        assert!(content_of(PHOTOS, &doc.save()).unwrap().is_empty());
    }

    phase a_record_that_does_not_load_names_nothing;
    {
        assert!(content_of(PHOTOS, b"not a document").is_err());
    }
}

/// A Files tree record names each file's content not in the trash, never the
/// device that wrote it, and each version a branch kept.
#[cfg(feature = "files")]
#[test]
fn a_files_tree_names_its_files_and_the_versions_kept() {
    const FILES: &str = tilo_app_id::apps::FILE_EXPLORER;
    /// A Files tree record holding the node `id` under `parent` with content
    /// `hash`, written by the device `author`.
    fn files_node(doc: &mut AutoCommit, id: &str, parent: &str, hash: &str, author: &str) {
        use fs_protocol::tree::{AUTHOR_KEY, HASH_KEY, NODES_KEY, PARENT_KEY};
        let nodes = match doc.get(ROOT, NODES_KEY).unwrap() {
            Some((_, nodes)) => nodes,
            None => doc.put_object(ROOT, NODES_KEY, ObjType::Map).unwrap(),
        };
        let node = doc.put_object(&nodes, id, ObjType::Map).unwrap();
        doc.put(&node, PARENT_KEY, parent).unwrap();
        doc.put(&node, HASH_KEY, hash).unwrap();
        doc.put(&node, AUTHOR_KEY, author).unwrap();
    }
    {
        let hash = "39".repeat(32);
        let trashed = "28".repeat(32);
        let device = "4a".repeat(32);
        let mut doc = AutoCommit::new();
        files_node(
            &mut doc,
            "device/1",
            fs_protocol::tree::ROOT_ID,
            &hash,
            &device,
        );
        files_node(
            &mut doc,
            "device/2",
            fs_protocol::tree::TRASH_ID,
            &trashed,
            &device,
        );
        assert_eq!(
            content_of(FILES, &doc.save()).unwrap(),
            std::slice::from_ref(&hash),
            "a file in the trash is not the workspace's any more, and its author names no bytes"
        );
        assert!(
            content_of(PHOTOS, &doc.save()).unwrap().is_empty(),
            "a node's hash names content in the Files tree alone"
        );
    }

    {
        let kept = "5b".repeat(32);
        let mut doc = AutoCommit::new();
        let aliases = doc
            .put_object(
                ROOT,
                fs_protocol::tree::FILE_VERSION_ALIASES_KEY,
                ObjType::Map,
            )
            .unwrap();
        doc.put(&aliases, "6c".repeat(32), kept.as_str()).unwrap();
        assert_eq!(content_of(FILES, &doc.save()).unwrap(), [kept]);
    }
}
