/// An identity service answering with `access`: no keys while the route
/// waits, and `keys` once it is ready.
#[cfg(unix)]
struct AccessThenKeysIdentity {
    ready: std::sync::atomic::AtomicBool,
    keys: Vec<u8>,
}

#[cfg(unix)]
impl SdkCertifiedIdentity for AccessThenKeysIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        use runtime_identity::wire::ChannelAccess;
        let ready = self.ready.load(std::sync::atomic::Ordering::Acquire);
        Ok(runtime_identity::wire::ChannelKeysReply {
            access: if ready {
                ChannelAccess::Ready
            } else {
                ChannelAccess::Waiting
            },
            generation,
            keys: ready.then(|| self.keys.clone()),
        })
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([9; 64])
    }
}

/// An edit made while the route waits for its channel keys is held, and
/// sent once they arrive: a held frame counts as sent to the app, and is
/// re-offered under the keys the endpoint then holds.
#[test]
#[cfg(unix)]
fn an_edit_held_for_channel_keys_goes_out_when_the_keys_arrive() {
    let app_id = "so.tilo.test";
    let cluster = Uuid::new_v4();
    let channel = collab_types::document_channel_id(app_id, &cluster.to_string(), "");
    let group = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("device").unwrap(),
    )
    .unwrap();
    let changes = Arc::new(tokio::sync::watch::Sender::new(0_u64));
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            key_changes: Some(Arc::clone(&changes)),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let identity = Arc::new(AccessThenKeysIdentity {
        ready: std::sync::atomic::AtomicBool::new(false),
        keys: group.content_keys().unwrap().to_bytes().unwrap().to_vec(),
    });
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: Vec::new(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::clone(&identity) as _),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: app_id.into(),
        cluster_id: cluster,
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();

    // The edit is made while the service says the endpoint waits: the
    // frame is held, and the app is not told the commit failed.
    assert!(manager.send(CollabRequest::document_incremental(b"held edit".to_vec())));
    let barrier = manager.begin_document_publication_barrier().unwrap();
    assert_eq!(
        barrier.wait(Some(tilo_test_env::hang_deadline())),
        Ok(()),
        "a held edit is not a failed commit"
    );
    assert!(
        captures.lock().unwrap().signed_ops.is_empty(),
        "nothing is sent while the route holds no keys"
    );
    assert!(manager.is_connected());

    // The keys arrive: the push tells the endpoint, which re-seals the
    // held edit under them and sends it.
    identity
        .ready
        .store(true, std::sync::atomic::Ordering::Release);
    changes.send_replace(1);
    tilo_test_env::wait_until("the held edit to go out once its keys arrived", || {
        captures.lock().unwrap().signed_ops.len() == 1
    });
    drop(manager);
    thread.join().unwrap();
}
