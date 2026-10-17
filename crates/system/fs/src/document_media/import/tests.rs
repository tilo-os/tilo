use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::store::ContentStore;

fn scope(room: &str, app: &str) -> BlobScope {
    BlobScope::new(room, app).unwrap()
}

struct OwnerAuthority {
    request: LegacyImportRequest,
    approval: LegacyImportApproval,
    current: AtomicBool,
}

impl LegacyImportAuthority for OwnerAuthority {
    fn authorize(
        &self,
        request: &LegacyImportRequest,
        approval: &LegacyImportApproval,
    ) -> Result<()> {
        ensure!(
            self.current.load(Ordering::SeqCst)
                && *request == self.request
                && *approval == self.approval,
            "owner action refused"
        );
        Ok(())
    }
}

fn action(hashes: Vec<String>, max_bytes: u64) -> (LegacyImportRequest, LegacyImportApproval) {
    (
        LegacyImportRequest {
            source_store: "host-legacy".into(),
            source_room: "old-room".into(),
            destination_room: "new-room".into(),
            hashes,
            max_bytes,
        },
        LegacyImportApproval {
            account: "owner".into(),
            device: "device".into(),
            authority_revision: "revision".into(),
            invocation: "explicit-owner-import".into(),
            nonce: "one-use".into(),
            argument_digest: [42; 32],
        },
    )
}

fn host(
    source: Arc<DocumentMedia>,
    destination: Arc<DocumentMedia>,
    request: &LegacyImportRequest,
    approval: &LegacyImportApproval,
) -> (LegacyMediaImportHost, Arc<OwnerAuthority>) {
    let authority = Arc::new(OwnerAuthority {
        request: request.clone(),
        approval: approval.clone(),
        current: AtomicBool::new(true),
    });
    (
        LegacyMediaImportHost::new("host-legacy".into(), source, destination, authority.clone()),
        authority,
    )
}

#[tokio::test]
async fn explicit_import_without_historical_provenance_is_new_durable_reachability() {
    let old_dir = tempfile::tempdir().unwrap();
    let new_dir = tempfile::tempdir().unwrap();
    let source_store = Arc::new(ContentStore::open(old_dir.path()).await.unwrap());
    let destination_store = Arc::new(ContentStore::open(new_dir.path()).await.unwrap());
    let source = DocumentMedia::new(source_store.clone());
    let destination = DocumentMedia::new(destination_store.clone());
    let hash = source
        .store(&scope("old-room", "photos"), b"legacy bytes".to_vec())
        .await
        .unwrap();
    let (request, approval) = action(vec![hash.to_hex()], 12);
    let (importer, _) = host(source.clone(), destination.clone(), &request, &approval);
    assert!(
        !destination
            .contains(&scope("new-room", "photos"), hash)
            .await
    );
    let permit = importer
        .permit(request.clone(), approval.clone(), Duration::from_secs(30))
        .unwrap();
    assert_eq!(importer.import(permit).await.unwrap(), 12);
    assert!(source.contains(&scope("old-room", "photos"), hash).await);
    assert!(!source.contains(&scope("new-room", "photos"), hash).await);
    assert!(
        !destination
            .contains(&scope("old-room", "photos"), hash)
            .await
    );
    assert_eq!(
        destination
            .read_range(scope("new-room", "photos"), hash, 0, 100)
            .await,
        runtime_fs::blobs::ReadOutcome::Ready {
            bytes: b"legacy bytes".to_vec(),
            total_len: 12
        }
    );
    let again = importer
        .permit(request.clone(), approval.clone(), Duration::from_secs(30))
        .unwrap();
    assert!(importer.import(again).await.is_err());
    drop(importer);
    drop(destination);
    destination_store.shutdown().await.unwrap();
    drop(destination_store);
    let reopened = Arc::new(ContentStore::open(new_dir.path()).await.unwrap());
    let destination = DocumentMedia::new(reopened.clone());
    assert!(
        destination
            .contains(&scope("new-room", "photos"), hash)
            .await
    );
    let (importer, _) = host(source.clone(), destination, &request, &approval);
    let replay = importer
        .permit(request, approval, Duration::from_secs(30))
        .unwrap();
    assert!(importer.import(replay).await.is_err());
    assert!(source.contains(&scope("old-room", "photos"), hash).await);
    reopened.shutdown().await.unwrap();
    source_store.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_custody_arguments_revocation_expiry_and_cross_host_permits_fail() {
    let source = DocumentMedia::new(Arc::new(ContentStore::memory()));
    let destination = DocumentMedia::new(Arc::new(ContentStore::memory()));
    let hash = source
        .store(&scope("old-room", "photos"), vec![1])
        .await
        .unwrap();
    let (request, approval) = action(vec![hash.to_hex()], 1);
    let (importer, authority) = host(source.clone(), destination.clone(), &request, &approval);
    for changed in [
        "account",
        "device",
        "revision",
        "invocation",
        "nonce",
        "digest",
    ] {
        let mut changed_approval = approval.clone();
        match changed {
            "account" => changed_approval.account = "foreign".into(),
            "device" => changed_approval.device = "foreign".into(),
            "revision" => changed_approval.authority_revision = "revoked".into(),
            "invocation" => changed_approval.invocation = "other".into(),
            "nonce" => changed_approval.nonce = "changed".into(),
            _ => changed_approval.argument_digest[0] ^= 1,
        }
        assert!(
            importer
                .permit(request.clone(), changed_approval, Duration::from_secs(30))
                .is_err()
        );
    }
    for field in ["source", "room", "destination", "hash", "limit"] {
        let mut changed = request.clone();
        match field {
            "source" => changed.source_store = "foreign".into(),
            "room" => changed.source_room = "other".into(),
            "destination" => changed.destination_room = "other".into(),
            "hash" => changed.hashes = vec![Hash::new(b"guessed").to_hex()],
            _ => changed.max_bytes += 1,
        }
        assert!(
            importer
                .permit(changed, approval.clone(), Duration::from_secs(30))
                .is_err()
        );
    }
    let mut expired = importer
        .permit(request.clone(), approval.clone(), Duration::from_secs(30))
        .unwrap();
    expired.expires = Instant::now();
    assert!(importer.import(expired).await.is_err());
    let revoked = importer
        .permit(request.clone(), approval.clone(), Duration::from_secs(30))
        .unwrap();
    authority.current.store(false, Ordering::SeqCst);
    assert!(importer.import(revoked).await.is_err());
    authority.current.store(true, Ordering::SeqCst);
    let (other, _) = host(source.clone(), destination.clone(), &request, &approval);
    let wrong_host = importer
        .permit(request.clone(), approval.clone(), Duration::from_secs(30))
        .unwrap();
    assert!(other.import(wrong_host).await.is_err());
    assert!(
        !destination
            .contains(&scope("new-room", "photos"), hash)
            .await
    );
    assert!(source.contains(&scope("old-room", "photos"), hash).await);
}

#[tokio::test]
async fn import_checks_roots_and_whole_selection_bounds_before_publication() {
    let source = DocumentMedia::new(Arc::new(ContentStore::memory()));
    let destination = DocumentMedia::new(Arc::new(ContentStore::memory()));
    let hash = source
        .store(&scope("old-room", "photos"), vec![1; 5])
        .await
        .unwrap();
    let unreferenced = source
        .store
        .ingest(bytes::Bytes::from_static(b"not legacy media"))
        .await
        .unwrap();
    for (hashes, limit) in [
        (vec![hash.to_hex()], 4),
        (vec![unreferenced.to_hex()], 100),
        (vec![hash.to_hex(), Hash::new(b"absent").to_hex()], 100),
    ] {
        let (request, approval) = action(hashes, limit);
        let (importer, _) = host(source.clone(), destination.clone(), &request, &approval);
        let permit = importer
            .permit(request, approval, Duration::from_secs(30))
            .unwrap();
        assert!(importer.import(permit).await.is_err());
        assert!(
            !destination
                .contains(&scope("new-room", "photos"), hash)
                .await
        );
    }
    for (hashes, limit) in [
        (vec![hash.to_hex(); MAX_LEGACY_IMPORT_ITEMS + 1], 100),
        (vec![hash.to_hex(), hash.to_hex()], 100),
        (vec![hash.to_hex()], MAX_LEGACY_IMPORT_BYTES + 1),
        (vec![], 1),
    ] {
        let (request, approval) = action(hashes, limit);
        let (importer, _) = host(source.clone(), destination.clone(), &request, &approval);
        assert!(
            importer
                .permit(request, approval, Duration::from_secs(30))
                .is_err()
        );
    }
    assert!(source.contains(&scope("old-room", "photos"), hash).await);
}
