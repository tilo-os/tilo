//! A reference whose content has not arrived must say which of the two things
//! it is.
//!
//! The bytes behind an image live outside the document (see `blob_reference.rs`
//! for why), so a reference can legitimately reach a machine before its content
//! does. A renderer that only learns "the bytes are not here" draws the same
//! grey box for a transfer in flight and for one that will never complete, and a
//! reader cannot tell a slow link from a lost image. So the store reports three
//! states rather than an `Option`, and this pins them against the real store and
//! the real transport hook.

use std::sync::{Arc, Mutex};

use qdoc::Document;
use qdoc::blobstore::{self, BlobState, BlobStore, BlobTransport, MemoryBlobStore};

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

/// A transport that records what it was asked for. What matters here is that
/// *something* can fetch, not what it fetches over.
#[derive(Default)]
struct RecordingTransport {
    wanted: Mutex<Vec<String>>,
    offered: Mutex<Vec<String>>,
}

impl BlobTransport for RecordingTransport {
    fn offer(&self, hash: &str, _bytes: &[u8]) {
        self.offered.lock().unwrap().push(hash.to_string());
    }
    fn want(&self, hash: &str) {
        self.wanted.lock().unwrap().push(hash.to_string());
    }
}

#[test]
fn a_reference_whose_content_is_coming_reads_as_pending_and_then_resolved() {
    let _guard = serialised();
    let bytes = png();
    let hash = blobstore::hash_bytes(&bytes);

    // The receiving machine: its own empty store, and a door it can ask through.
    let store: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
    blobstore::set_store(Arc::clone(&store));
    let transport = Arc::new(RecordingTransport::default());
    blobstore::set_transport(Arc::clone(&transport) as Arc<dyn BlobTransport>);

    let doc = Document::new();

    assert_eq!(
        doc.blob(&hash),
        None,
        "the receiving machine cannot have content nobody has sent it yet",
    );
    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Pending,
        "a reference with a live transport behind it is arriving, not missing",
    );
    // A state that says "arriving" while nothing has been asked for is the same
    // lie in a nicer colour.
    assert!(
        transport.wanted.lock().unwrap().iter().any(|h| h == &hash),
        "reading a missing blob did not ask the transport for it",
    );

    assert!(store.put(&hash, &bytes), "the store took the content");

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
    // No door out of this process, so a miss is permanent by construction and
    // calling it "arriving" would be a placeholder that never resolves.
    blobstore::set_store(Arc::new(MemoryBlobStore::default()));
    blobstore::clear_transport();

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
fn content_this_machine_stored_itself_is_resolved_and_offered_onward() {
    let _guard = serialised();
    let store: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
    blobstore::set_store(Arc::clone(&store));
    let transport = Arc::new(RecordingTransport::default());
    blobstore::set_transport(Arc::clone(&transport) as Arc<dyn BlobTransport>);

    let mut doc = Document::new();
    let bytes = png();
    let hash = doc.put_blob(&bytes);

    assert_eq!(
        doc.blob_state(&hash),
        BlobState::Resolved,
        "the machine that stored the content has it",
    );
    // Storing is also what puts it on its way to everyone else. Without this the
    // bytes stay in the process that produced them and every other machine draws
    // a placeholder forever.
    assert!(
        transport.offered.lock().unwrap().iter().any(|h| h == &hash),
        "storing content did not offer it onward",
    );
}
