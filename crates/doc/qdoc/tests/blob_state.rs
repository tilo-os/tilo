//! A reference whose content has not arrived must say which of the two things
//! it is.
//!
//! The bytes behind an image live outside the document (see `blob_reference.rs`
//! for why), so a reference can legitimately reach a machine before its content
//! does. A renderer that only learns "the bytes are not here" draws the same
//! grey box for a transfer in flight and for one that will never complete, and a
//! reader cannot tell a slow link from a lost image. So the store reports three
//! states rather than an `Option`, and this pins them against the real store
//! front and a content store that fetches from collaborators.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use qdoc::Document;
use qdoc::blobstore::{
    self, BlobState, BlobStore, BlobStoreError, BlobStoreLimits, CachedContentStore,
    ContentStoreClient, Fetch, MemoryBlobStore,
};

use crate::blobstore_fixture::serialised;

fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&7u32.to_be_bytes());
    v.extend_from_slice(&11u32.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend_from_slice(&[0u8; 4]);
    v.extend(std::iter::repeat_n(0xA5, 4096));
    v
}

/// A content store standing in for the filesystem daemon: it records what it
/// was asked for, and answers `Pending` for content a collaborator has not yet
/// delivered.
#[derive(Default)]
struct CollaboratorStore {
    delivered: Mutex<Vec<(String, Vec<u8>)>>,
    asked: Mutex<Vec<String>>,
    put: Mutex<Vec<String>>,
}

#[allow(
    clippy::unwrap_used,
    reason = "test helper: a poisoned lock should propagate its panic, not be swallowed"
)]
impl ContentStoreClient for CollaboratorStore {
    fn put(&self, bytes: &[u8], _mime: &str) -> Result<String, BlobStoreError> {
        let hash = blobstore::hash_bytes(bytes);
        self.put.lock().unwrap().push(hash.clone());
        self.delivered
            .lock()
            .unwrap()
            .push((hash.clone(), bytes.to_vec()));
        Ok(hash)
    }
    fn read(&self, hash: &str) -> Fetch {
        self.asked.lock().unwrap().push(hash.to_string());
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .find(|(h, _)| h == hash)
            .map_or(Fetch::Pending, |(_, bytes)| Fetch::Ready(bytes.clone()))
    }
    fn has(&self, hash: &str) -> bool {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .any(|(h, _)| h == hash)
    }
}

fn install(content: &Arc<CollaboratorStore>) -> blobstore::BlobScope {
    blobstore::scope(Arc::new(CachedContentStore::new(
        Arc::clone(content) as Arc<dyn ContentStoreClient>,
        BlobStoreLimits::default(),
    )))
}

#[test]
fn a_reference_whose_content_is_coming_reads_as_pending_and_then_resolved() {
    let _guard = serialised();
    let bytes = png();
    let hash = blobstore::hash_bytes(&bytes);

    // The receiving machine: a content store whose collaborator has not yet
    // delivered the bytes.
    let content = Arc::new(CollaboratorStore::default());
    let _scope = install(&content);

    let doc = Document::new();

    assert_eq!(
        doc.blob(&hash),
        None,
        "the receiving machine cannot have content nobody has sent it yet",
    );
    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Pending,
        "a reference with a content store behind it is arriving, not missing",
    );
    // A state that says "arriving" while nothing has been asked for is the same
    // lie in a nicer colour.
    assert!(
        content.asked.lock().unwrap().iter().any(|h| h == &hash),
        "reading a missing blob did not ask the content store for it",
    );

    content
        .delivered
        .lock()
        .unwrap()
        .push((hash.clone(), bytes.clone()));
    // A renderer asks again on a later frame, at most once a second.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    assert_eq!(
        doc.blob(&hash).as_deref(),
        Some(bytes.as_slice()),
        "the content is readable once it has arrived",
    );
    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Resolved,
        "content that is present must not still read as arriving",
    );
}

#[test]
fn a_reference_nothing_can_fetch_is_reported_as_unavailable_rather_than_arriving() {
    let _guard = serialised();
    // No store beyond this process, so a miss is permanent by construction and
    // calling it "arriving" would be a placeholder that never resolves.
    let _scope = blobstore::scope(Arc::new(MemoryBlobStore::default()));

    let hash = blobstore::hash_bytes(&png());
    let doc = Document::new();
    assert_eq!(doc.blob(&hash), None);
    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Unavailable,
        "with nothing able to fetch it, content that is absent is not coming",
    );
}

#[test]
fn content_this_machine_stored_itself_is_resolved_and_in_the_content_store() {
    let _guard = serialised();
    let content = Arc::new(CollaboratorStore::default());
    let _scope = install(&content);

    let mut doc = Document::new();
    let bytes = png();
    let hash = doc.put_blob(&bytes).expect("test image fits blob quota");

    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Resolved,
        "the machine that stored the content has it",
    );
    // Storing puts the bytes in the device content store, which is what every
    // collaborator's device fetches from. Bytes kept only in the producing
    // process would leave every other machine drawing a placeholder forever.
    assert!(
        content.put.lock().unwrap().iter().any(|h| h == &hash),
        "storing content did not reach the content store",
    );
}

#[test]
fn a_quota_refusal_does_not_commit_a_descriptor_that_would_dangle() {
    let _guard = serialised();
    let tight = BlobStoreLimits {
        max_blob_bytes: 2,
        max_total_bytes: 2,
        max_entries: 1,
    };
    let stores: [Arc<dyn BlobStore>; 2] = [
        Arc::new(MemoryBlobStore::new(tight)),
        Arc::new(CachedContentStore::new(
            Arc::new(CollaboratorStore::default()),
            tight,
        )),
    ];
    for store in stores {
        let _scope = blobstore::scope(store);

        let mut doc = Document::new();
        let bytes = b"too large";
        assert_eq!(doc.put_blob(bytes), None);

        let raw = automerge::AutoCommit::load(&doc.save()).expect("document saved");
        let containers = qdoc::model::open(&raw, &automerge::ROOT).expect("qdoc schema");
        assert!(
            qdoc::model::blob_hashes(&raw, &containers).is_empty(),
            "a rejected local blob must not leave a CRDT descriptor behind"
        );
    }
}

#[test]
fn a_scoped_store_is_gone_when_its_scope_ends() {
    let _guard = serialised();
    let bytes = png();
    let hash = blobstore::hash_bytes(&bytes);
    let tight = BlobStoreLimits {
        max_blob_bytes: 2,
        max_total_bytes: 2,
        max_entries: 1,
    };
    {
        let _scope = blobstore::scope(Arc::new(MemoryBlobStore::new(tight)));
        assert_eq!(
            Document::new().put_blob(&bytes),
            None,
            "the scope's quota applies"
        );
        let inner = Arc::new(MemoryBlobStore::default());
        {
            let _nested = blobstore::scope(Arc::clone(&inner) as Arc<dyn BlobStore>);
            assert!(Document::new().put_blob(&bytes).is_some());
        }
        assert!(inner.has(&hash), "the nested scope wrote to its own store");
        assert_eq!(
            Document::new().put_blob(&bytes),
            None,
            "leaving the nested scope restores the outer one"
        );
    }
    // Whatever runs after a scope sees the process's store again, not the scope's quota.
    let mut doc = Document::new();
    assert_eq!(doc.put_blob(&bytes).as_deref(), Some(hash.as_str()));
    assert_eq!(doc.blob(&hash).as_deref(), Some(bytes.as_slice()));
}
