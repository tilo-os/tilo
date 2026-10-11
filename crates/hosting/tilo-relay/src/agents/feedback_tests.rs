use super::*;
use tilo_actions::remote::Caller;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

fn governor() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[10; 32]);
    let key = SigningKey::from_bytes(&[11; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "synthetic feedback owner".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

fn serve_owner(
    listener: std::os::unix::net::UnixListener,
    media: Arc<fs::document_media::DocumentMedia>,
    runtime: tokio::runtime::Handle,
) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(true).unwrap();
        let local_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&local_runtime, async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                fs::document_media::serve_connection(
                    tokio::net::UnixStream::from_std(stream).unwrap(),
                    media,
                    runtime,
                ),
            )
            .await
            .is_ok()
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay; Clippy's suggested subsequent drop uses a moved value, as in rooms' view-policy fixtures"
)]
async fn governed_relay_binds_real_scoped_custody_and_refuses_lost_owner_bytes() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = Relay::start_with_relay_mode(
        crate::RelayConfig {
            state_root: state.path().join("relay"),
            ..Default::default()
        },
        RelayIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Synthetic media custody").await.unwrap();
    let store = Arc::new(
        fs::ContentStore::open(state.path().join("content"))
            .await
            .unwrap(),
    );
    let media = fs::document_media::DocumentMedia::new(store.clone());
    // Deterministic opaque black one-pixel PNG; no desktop/app capture.
    let png = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63606060f80f00010401005fe5c34b0000000049454e44ae426082").unwrap();
    let scope = fs::document_media::BlobScope::new(&created.room, "so.tilo.chat").unwrap();
    let hash = media.store(&scope, png).await.unwrap();
    let socket = state.path().join("media.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = serve_owner(listener, media, tokio::runtime::Handle::current());
    let link = RelayRecordsLink {
        user_public_key: cert.user_public_key,
        rooms: Some(rooms),
        browser_origin: Arc::new(std::sync::RwLock::new(None)),
        browser_page: None,
        member: RoomMember {
            endpoint: HostEndpoint::iroh(relay.node_addr(), cert.clone()),
            key,
            runtime: tokio::runtime::Handle::current(),
        },
    };
    let images = tokio::task::spawn_blocking(move || {
        let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key);
        let stranger = Caller::authenticated(cert.user_public_key, [7; 32]);
        assert!(
            link.feedback_attachments_at(created.workspace_id, &stranger, &created.room, &socket)
                .is_err()
        );
        assert!(
            link.feedback_attachments_at(created.workspace_id, &caller, "wrong-room", &socket)
                .is_err()
        );
        link.feedback_attachments_at(created.workspace_id, &caller, &created.room, &socket)
            .unwrap()
            .unwrap()
    })
    .await
    .unwrap();
    let reference = serde_json::from_value(serde_json::json!({
        "hash":hash.to_hex(), "width":1, "height":1,
    }))
    .unwrap();
    images.verify(&reference).unwrap();
    let key = blake3::derive_key("tilo shared content document media v1", hash.as_bytes());
    std::fs::remove_file(store.files().path_of(&key)).unwrap();
    assert!(
        images.verify(&reference).is_err(),
        "warm owner bytes cannot satisfy fresh custody"
    );
    drop(images);
    assert!(server.join().unwrap(), "one-connection fixture must stop");
    store.store().shutdown().await.unwrap();
    relay.shutdown().await;
}
