/// An edit made while the route waits for its channel keys is held, and
/// sent once they arrive: a held frame counts as sent to the app, and is
/// re-offered under the keys the endpoint then holds.
#[test]
#[cfg(unix)]
fn an_edit_held_for_channel_keys_goes_out_when_the_keys_arrive() {
    let app_id = "so.tilo.test";
    let cluster = Uuid::new_v4();
    let channel = collab_types::WorkspaceId::parse(&cluster.to_string()).unwrap();
    let group =
        key_group::KeyGroup::create(channel, key_group::MemberKey::generate("device").unwrap())
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
    // The identity service has no keys while the route waits, and the keys once they are ready.
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let keys = group.content_keys().unwrap().to_bytes().unwrap().to_vec();
    let answered = Arc::clone(&ready);
    let identity = FixtureIdentity::answering(move |_, generation| {
        let ready = answered.load(std::sync::atomic::Ordering::Acquire);
        Ok(tilo_identity_client::wire::ChannelKeysReply {
            access: if ready {
                tilo_identity_client::wire::KeyAccess::Ready
            } else {
                tilo_identity_client::wire::KeyAccess::Waiting
            },
            generation,
            keys: ready.then(|| keys.clone()),
        })
    });
    let mut manager = certified_manager(fd, identity, Vec::new(), (app_id, cluster));

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
    ready.store(true, std::sync::atomic::Ordering::Release);
    changes.send_replace(1);
    tilo_test_env::wait_until("the held edit to go out once its keys arrived", || {
        captures.lock().unwrap().signed_ops.len() == 1
    });
    drop(manager);
    thread.join().unwrap();
}
