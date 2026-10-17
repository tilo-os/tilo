use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::store::ContentStore;

struct InterruptAfterJournal(AtomicUsize);
impl LegacyImportAuthority for InterruptAfterJournal {
    fn authorize(&self, _: &LegacyImportRequest, _: &LegacyImportApproval) -> Result<()> {
        // Mint, initial check, lock acquisition and pre-journal validation pass.
        // The first copy is refused after the durable spent marker is written.
        ensure!(
            self.0.fetch_add(1, Ordering::SeqCst) < 4,
            "authority revoked"
        );
        Ok(())
    }
}
struct Owner;
impl LegacyImportAuthority for Owner {
    fn authorize(&self, _: &LegacyImportRequest, _: &LegacyImportApproval) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn interrupted_publication_is_durably_spent_and_does_not_release_legacy_roots() {
    let dir = tempfile::tempdir().unwrap();
    let source = DocumentMedia::new(Arc::new(ContentStore::memory()));
    let old = BlobScope::new("old", "photos").unwrap();
    let new = BlobScope::new("new", "photos").unwrap();
    let hash = source.store(&old, b"keep me".to_vec()).await.unwrap();
    let request = LegacyImportRequest {
        source_store: "host".into(),
        source_room: "old".into(),
        destination_room: "new".into(),
        hashes: vec![hash.to_hex()],
        max_bytes: 7,
    };
    let approval = LegacyImportApproval {
        account: "owner".into(),
        device: "device".into(),
        authority_revision: "current".into(),
        invocation: "interrupted".into(),
        nonce: "nonce".into(),
        argument_digest: [1; 32],
    };
    {
        let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
        let destination = DocumentMedia::new(store.clone());
        let importer = LegacyMediaImportHost::new(
            "host".into(),
            source.clone(),
            destination.clone(),
            Arc::new(InterruptAfterJournal(AtomicUsize::new(0))),
        );
        let permit = importer
            .permit(request.clone(), approval.clone(), Duration::from_secs(30))
            .unwrap();
        assert!(importer.import(permit).await.is_err());
        assert!(!destination.contains(&new, hash).await);
        assert!(source.contains(&old, hash).await);
        store.shutdown().await.unwrap();
    }
    let store = Arc::new(ContentStore::open(dir.path()).await.unwrap());
    let destination = DocumentMedia::new(store.clone());
    let importer = LegacyMediaImportHost::new(
        "host".into(),
        source.clone(),
        destination.clone(),
        Arc::new(Owner),
    );
    let replay = importer
        .permit(request, approval, Duration::from_secs(30))
        .unwrap();
    assert!(importer.import(replay).await.is_err());
    assert!(!destination.contains(&new, hash).await);
    assert_eq!(
        source.read_range(old, hash, 0, 100).await,
        runtime_fs::blobs::ReadOutcome::Ready {
            bytes: b"keep me".to_vec(),
            total_len: 7
        }
    );
    store.shutdown().await.unwrap();
}
