use super::*;
use whatsapp_rust::wacore::store::traits::{
    Backend, DeviceStore, MsgSecretStore, ProtocolStore, SignalStore,
};

fn owner() -> Owner {
    Owner {
        principal: "11".repeat(32),
        device: "fixture-device".into(),
        service: Service::Whatsapp,
        account: "fixture-account".into(),
        linking_generation: 1,
    }
}
fn fixture_key() -> VaultKey {
    VaultKey::from_slice(&[17; 32]).unwrap()
}
fn open(root: &Path) -> EncryptedBackend {
    EncryptedBackend::open_with_key(root, owner(), fixture_key()).unwrap()
}

#[tokio::test]
async fn encrypted_compaction_preserves_latest_ratchet_and_consumed_prekeys() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    backend.store_prekey(11, b"consumed", false).await.unwrap();
    backend.remove_prekey(11).await.unwrap();
    let value = "private-fixture-".repeat(70_000);
    for version in 0_u8..6 {
        backend
            .put(key("fixture", &["large"]), &(version, &value))
            .await
            .unwrap();
    }
    let log_bytes = std::fs::metadata(dir.path().join("state.enc"))
        .unwrap()
        .len();
    assert!(
        log_bytes < 4_000_000,
        "compaction must bound historical bytes"
    );
    drop(backend);
    let restored = open(dir.path());
    assert_eq!(
        restored
            .get::<(u8, String)>(key("fixture", &["large"]))
            .await
            .unwrap(),
        Some((5, value))
    );
    assert!(restored.load_prekey(11).await.unwrap().is_none());
}

#[tokio::test]
async fn actual_sdk_backend_persists_encrypted_device_ratchets_and_pending_batch() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    let sdk: Arc<dyn Backend> = Arc::new(backend.clone());
    assert!(!sdk.exists().await.unwrap());
    assert_eq!(sdk.create().await.unwrap(), 1);
    let registration = sdk.load().await.unwrap().unwrap().registration_id;
    sdk.put_identity("fixture:2@lid", [19; 32]).await.unwrap();
    sdk.put_session("fixture:2@lid", b"private-ratchet-fixture")
        .await
        .unwrap();
    sdk.store_pending_inbound_batch(&[
        whatsapp_rust::wacore::store::traits::PendingInboundRow {
            chat: "chat@lid",
            sender: "one@lid",
            id: "same-id",
            message: b"private-inbound-fixture",
        },
        whatsapp_rust::wacore::store::traits::PendingInboundRow {
            chat: "chat@lid",
            sender: "two@lid",
            id: "same-id",
            message: b"distinct-sender",
        },
    ])
    .await
    .unwrap();
    let encrypted = std::fs::read(dir.path().join("state.enc")).unwrap();
    for private in [
        b"private-ratchet-fixture".as_slice(),
        b"private-inbound-fixture",
        b"fixture:2@lid",
    ] {
        assert!(
            !encrypted
                .windows(private.len())
                .any(|value| value == private)
        );
    }
    drop(sdk);
    drop(backend);
    let restored = open(dir.path());
    assert_eq!(
        restored.load().await.unwrap().unwrap().registration_id,
        registration
    );
    assert_eq!(
        restored.load_identity("fixture:2@lid").await.unwrap(),
        Some([19; 32])
    );
    assert_eq!(
        restored
            .get_session("fixture:2@lid")
            .await
            .unwrap()
            .unwrap(),
        b"private-ratchet-fixture".as_slice()
    );
    assert_eq!(
        restored
            .get_pending_inbound("chat@lid", "two@lid", "same-id")
            .await
            .unwrap(),
        Some(b"distinct-sender".to_vec())
    );
    assert!(
        restored
            .get_pending_inbound("different@lid", "two@lid", "same-id")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn consumed_prekeys_and_atomic_retry_take_stay_consumed_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    backend.store_prekey(7, b"prekey", false).await.unwrap();
    backend.remove_prekey(7).await.unwrap();
    backend.mark_prekeys_uploaded(&[7]).await.unwrap();
    backend
        .store_sent_message("chat@lid", "id", b"payload")
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        backend.take_sent_message("chat@lid", "id"),
        backend.take_sent_message("chat@lid", "id")
    );
    assert_eq!(
        usize::from(first.unwrap().is_some())
            .saturating_add(usize::from(second.unwrap().is_some())),
        1
    );
    drop(backend);
    let restored = open(dir.path());
    assert!(restored.load_prekey(7).await.unwrap().is_none());
    assert!(
        restored
            .take_sent_message("chat@lid", "id")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn concurrent_token_writers_preserve_fields_and_expiry_requires_both_stale() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    let (sent, received) = tokio::join!(
        backend.touch_tc_token_sender_timestamp("peer@lid", 200),
        backend.store_received_tc_token("peer@lid", b"new-token", 100)
    );
    sent.unwrap();
    received.unwrap();
    backend
        .store_received_tc_token("peer@lid", b"stale-token", 90)
        .await
        .unwrap();
    backend
        .touch_tc_token_sender_timestamp("peer@lid", 180)
        .await
        .unwrap();
    let value = backend.get_tc_token("peer@lid").await.unwrap().unwrap();
    assert_eq!(value.token, b"new-token");
    assert_eq!(value.sender_timestamp, Some(200));
    assert_eq!(backend.delete_expired_tc_tokens(150, 200).await.unwrap(), 0);
    assert_eq!(backend.delete_expired_tc_tokens(150, 201).await.unwrap(), 1);
}

#[tokio::test]
async fn competing_owner_wrong_key_and_corrupt_frames_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    backend.put_session("peer@lid", b"secret").await.unwrap();
    assert!(EncryptedBackend::open_with_key(dir.path(), owner(), fixture_key()).is_err());
    drop(backend);
    let path = dir.path().join("state.enc");
    let original = std::fs::read(&path).unwrap();
    let mut persona = owner();
    persona.principal = "22".repeat(32);
    assert!(EncryptedBackend::open_with_key(dir.path(), persona, fixture_key()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut other = owner();
    other.linking_generation += 1;
    assert!(EncryptedBackend::open_with_key(dir.path(), other, fixture_key()).is_err());
    assert!(
        EncryptedBackend::open_with_key(
            dir.path(),
            owner(),
            VaultKey::from_slice(&[18; 32]).unwrap()
        )
        .is_err()
    );
    let path = dir.path().join("state.enc");
    let before = std::fs::read(&path).unwrap();
    let mut corrupt = before.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &corrupt).unwrap();
    assert!(EncryptedBackend::open_with_key(dir.path(), owner(), fixture_key()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    std::fs::write(&path, &before[..before.len() - 1]).unwrap();
    assert!(EncryptedBackend::open_with_key(dir.path(), owner(), fixture_key()).is_err());
}

#[tokio::test]
async fn failed_write_poisoning_prevents_ack_and_live_store_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    let path = dir.path().join("state.enc");
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    (&file).write_all(b"torn").unwrap();
    assert!(backend.put_session("peer@lid", b"secret").await.is_err());
    assert!(backend.get_session("peer@lid").await.is_err());
    drop(backend);
    assert!(EncryptedBackend::open_with_key(dir.path(), owner(), fixture_key()).is_err());
}

#[tokio::test]
async fn quota_failure_is_atomic_and_secret_retention_merges_within_batch() {
    let dir = tempfile::tempdir().unwrap();
    let backend = open(dir.path());
    backend.put_session("peer@lid", b"old").await.unwrap();
    let mut entries = Vec::new();
    for (expiry, time) in [(100, 50), (0, 0), (80, 70)] {
        entries.push(whatsapp_rust::wacore::store::traits::MsgSecretEntry {
            chat: Arc::from("chat@lid"),
            sender: Arc::from("peer@lid"),
            msg_id: Arc::from("id"),
            secret: [9; 32],
            expires_at: expiry,
            message_ts: time,
        });
    }
    backend.put_msg_secrets(entries).await.unwrap();
    assert_eq!(
        backend
            .get_msg_secret_with_ts("chat@lid", "peer@lid", "id")
            .await
            .unwrap(),
        Some((vec![9; 32], 70))
    );
    assert_eq!(backend.delete_expired_msg_secrets(1000).await.unwrap(), 0);
    assert!(
        backend
            .put_session("peer@lid", &vec![0; MAX_LIVE_BYTES])
            .await
            .is_err()
    );
    assert_eq!(
        backend.get_session("peer@lid").await.unwrap().unwrap(),
        b"old".as_slice()
    );
}

struct Grant(std::sync::atomic::AtomicBool);
impl StateAuthority for Grant {
    fn allowed(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn approved_profile_key_is_bound_before_sdk_state_and_survives_only_original_grant() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("profile");
    let grant = Arc::new(Grant(std::sync::atomic::AtomicBool::new(false)));
    assert!(EncryptedBackend::open_profile(&root, owner(), fixture_key(), grant.clone()).is_err());
    assert!(!root.exists(), "unapproved profile custody performs no I/O");
    grant.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let backend =
        EncryptedBackend::open_profile(&root, owner(), fixture_key(), grant.clone()).unwrap();
    let sdk: Arc<dyn Backend> = Arc::new(backend.clone());
    sdk.create().await.unwrap();
    sdk.put_session("profile-peer@lid", b"approved-private-ratchet")
        .await
        .unwrap();
    let before = std::fs::read(root.join("state.enc")).unwrap();
    assert!(!before.windows(24).any(|b| b == b"approved-private-ratchet"));
    grant.0.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(sdk.load().await.is_err());
    assert!(
        backend
            .clone()
            .put_session("profile-peer@lid", b"refused")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(root.join("state.enc")).unwrap(), before);
    drop(sdk);
    drop(backend);
    assert!(EncryptedBackend::open_profile(&root, owner(), fixture_key(), grant.clone()).is_err());
    grant.0.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        EncryptedBackend::open_profile(
            &root,
            owner(),
            VaultKey::from_bytes([31; 32]),
            grant.clone(),
        )
        .is_err()
    );
    assert_eq!(std::fs::read(root.join("state.enc")).unwrap(), before);
    let restored = EncryptedBackend::open_profile(&root, owner(), fixture_key(), grant).unwrap();
    assert_eq!(
        restored
            .get_session("profile-peer@lid")
            .await
            .unwrap()
            .unwrap(),
        b"approved-private-ratchet".as_slice()
    );
}

#[tokio::test]
async fn revoked_original_grant_stops_actual_sdk_ratchet_and_pending_writes() {
    let dir = tempfile::tempdir().unwrap();
    let grant = Arc::new(Grant(std::sync::atomic::AtomicBool::new(true)));
    let initial = open(dir.path());
    let prior_clone = initial.clone();
    let backend = initial.guarded(grant.clone()).unwrap();
    let sdk: Arc<dyn Backend> = Arc::new(backend.clone());
    sdk.create().await.unwrap();
    sdk.put_session("peer@lid", b"original-ratchet")
        .await
        .unwrap();
    let before = std::fs::read(dir.path().join("state.enc")).unwrap();
    grant.0.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        sdk.put_session("peer@lid", b"revoked-ratchet")
            .await
            .is_err()
    );
    assert!(
        sdk.store_prekey(27, b"revoked-prekey", false)
            .await
            .is_err()
    );
    assert!(
        sdk.store_pending_inbound_batch(&[
            whatsapp_rust::wacore::store::traits::PendingInboundRow {
                chat: "chat@lid",
                sender: "peer@lid",
                id: "revoked-message",
                message: b"revoked-text",
            },
        ])
        .await
        .is_err()
    );
    assert!(sdk.load().await.is_err());
    let forbidden = dir.path().join("revoked-custody");
    assert!(EncryptedBackend::open_system(&forbidden, owner(), grant.clone()).is_err());
    assert!(
        !forbidden.exists(),
        "revoked grants must perform no custody I/O"
    );
    assert!(
        prior_clone
            .clone()
            .guarded(Arc::new(Grant(std::sync::atomic::AtomicBool::new(true))))
            .is_err()
    );
    assert!(
        prior_clone
            .put_session("peer@lid", b"clone-write")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(dir.path().join("state.enc")).unwrap(), before);
    drop(sdk);
    drop(backend);
    drop(prior_clone);
    let restored = open(dir.path());
    assert_eq!(
        restored.get_session("peer@lid").await.unwrap().unwrap(),
        b"original-ratchet".as_slice()
    );
    assert!(restored.load_prekey(27).await.unwrap().is_none());
    assert!(
        restored
            .get_pending_inbound("chat@lid", "peer@lid", "revoked-message")
            .await
            .unwrap()
            .is_none()
    );
}

#[test]
fn revocation_during_custody_lookup_prevents_store_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("never-initialized");
    let grant = Arc::new(Grant(std::sync::atomic::AtomicBool::new(true)));
    let result = EncryptedBackend::open_authorized(&root, owner(), grant.clone(), || {
        grant.0.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(VaultKey::from_slice(&[7; 32]).unwrap())
    });
    assert!(result.is_err());
    assert!(
        !root.exists(),
        "a grant revoked while custody responds must not create a private store"
    );
}
