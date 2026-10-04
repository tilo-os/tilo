use super::*;
use crate::channels::semantic_tests::fixture;
use ed25519_dalek::SigningKey;
use std::sync::atomic::{AtomicBool, Ordering};

struct NoNetwork;
impl HttpClient for NoNetwork {
    fn post(&self, _: &str, _: Option<&str>, _: Vec<u8>) -> crate::HttpFuture<'_> {
        panic!("semantic observation must not use HTTP")
    }
}
#[derive(Default)]
struct Store {
    inner: crate::MemoryPeerStore,
    fail: AtomicBool,
}
impl PeerStore for Store {
    fn load(&self) -> Option<PeerRecord> {
        self.inner.load()
    }
    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        self.inner.save(record)
    }
    fn load_channels(&self) -> Option<Vec<u8>> {
        self.inner.load_channels()
    }
    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        if self.fail.load(Ordering::Relaxed) {
            Err("synthetic store failure".into())
        } else {
            self.inner.save_channels(bytes)
        }
    }
    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some([75; 32])
    }
}
fn task_fixture() -> (Task, DocumentRoute, [u8; 32], Vec<u8>, Arc<Store>) {
    let f = fixture();
    let bytes = f.seal(&f.payload());
    let browser = SigningKey::from_bytes(&[73; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: f.root.verifying_key().to_bytes(),
        device_public_key: browser.verifying_key().to_bytes(),
        device_label: "Synthetic browser".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &f.root).unwrap();
    let config = PeerConfig {
        host: Host::Https(Arc::new(NoNetwork)),
        cert: cert.clone(),
        host_cert: cert,
        revocations: RevocationList::new(),
    };
    let shared = Arc::new(Mutex::new(Shared {
        status: tests::status(&[]),
        queues: Vec::new(),
        wakes: Vec::new(),
    }));
    let store = Arc::new(Store::default());
    let mut task = Task::new(config, Arc::new(browser), store.clone(), shared);
    task.channels = f.channels;
    task.ciphers.insert(f.route.clone(), Box::new(f.cipher));
    (
        task,
        f.route,
        f.publisher.verifying_key().to_bytes(),
        bytes,
        store,
    )
}

#[tokio::test]
async fn semantic_command_drops_before_admission_offline_and_after_room_refusal() {
    let (mut task, route, publisher, bytes, _) = task_fixture();
    let workspace_id = route.workspace_id().unwrap();
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_none()
    );
    task.inbound(Envelope::Ready {
        workspace_id,
        pending_deltas: 0,
    });
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_some()
    );
    let (send, receive) = tokio::sync::oneshot::channel();
    task.apply(
        Command::OpenSemantic(
            route.clone(),
            publisher,
            bytes.clone(),
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        ),
        None,
    )
    .await;
    assert!(receive.await.unwrap().is_none());
    task.inbound(Envelope::Ready {
        workspace_id: workspace_id + 1,
        pending_deltas: 0,
    });
    task.inbound(Envelope::NotAuthorised {
        workspace_id,
        reason: "synthetic removal".into(),
    });
    assert_eq!(lock(&task.shared).status.phase, TransportPhase::Connected);
    assert_eq!(
        lock(&task.shared).status.rooms.get(&(workspace_id + 1)),
        Some(&true)
    );
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_none()
    );
    task.inbound(Envelope::Ready {
        workspace_id,
        pending_deltas: 0,
    });
    task.update(|status| status.phase = TransportPhase::Reconnecting);
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_none()
    );
}

#[test]
fn semantic_storage_failures_refuse_opening_and_retry_the_same_state() {
    let (mut task, route, publisher, bytes, store) = task_fixture();
    task.inbound(Envelope::Ready {
        workspace_id: route.workspace_id().unwrap(),
        pending_deltas: 0,
    });
    store.fail.store(true, Ordering::Relaxed);
    task.persist_channels(false);
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_none()
    );
    store.fail.store(false, Ordering::Relaxed);
    task.persist_channels(false);
    assert!(store.load_channels().is_some());
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_some()
    );
}

#[test]
fn semantic_channel_storage_migrates_v2_and_round_trips_v3() {
    let (mut task, route, _, _, store) = task_fixture();
    task.persist_channels(false);
    let encoded = store.load_channels().unwrap();
    assert!(encoded.starts_with(SAVED_CHANNELS_TAG));
    let loaded = SavedPeerChannels::decode(&encoded).unwrap();
    let StoredChannels::Current(current) = loaded.channels else {
        panic!("current format")
    };
    let expected_members = current.members.clone();
    let old = SavedPeerChannelsV2 {
        channels: crate::channels::SavedChannelsV2 {
            governors: current.governors,
            members: current.members,
        },
        held: vec![(route, vec![vec![1, 2, 3]])],
    };
    let mut bytes = SAVED_CHANNELS_V2_TAG.to_vec();
    postcard::to_io(&old, &mut bytes).unwrap();
    let migrated = SavedPeerChannels::decode(&bytes).unwrap();
    let StoredChannels::Current(migrated_channels) = migrated.channels else {
        panic!("migrated format")
    };
    assert_eq!(migrated_channels.members, expected_members);
    assert!(migrated_channels.revocations.is_empty());
    assert_eq!(migrated.held, old.held);
}

#[test]
fn semantic_corrupt_saved_authority_cannot_be_replaced_by_readmission_or_save() {
    let (fixture, route, publisher, bytes, store) = task_fixture();
    let mut corrupt = SAVED_CHANNELS_TAG.to_vec();
    corrupt.extend_from_slice(b"invalid saved authority");
    store.save_channels(&corrupt).unwrap();
    let mut task = Task::new(
        fixture.config.clone(),
        fixture.signer.clone(),
        store.clone(),
        fixture.shared.clone(),
    );
    task.channels = fixture.channels;
    task.ciphers = fixture.ciphers;
    task.inbound(Envelope::Ready {
        workspace_id: route.workspace_id().unwrap(),
        pending_deltas: 0,
    });
    assert!(!task.semantic_storage_trusted);
    task.persist_channels(true);
    assert_eq!(store.load_channels().unwrap(), corrupt);
    assert!(
        task.open_semantic_live(&route, publisher, &bytes, true)
            .is_none()
    );
}
