/// An on-demand record the room's hosts have none of: the read says so
/// once, as a session event the app shows, and then waits on the broker
/// instead of opening again.
#[test]
#[cfg(unix)]
fn an_absent_on_demand_record_is_said_once_and_waited_on() {
    let room = Uuid::new_v4();
    let part = RouteSpec::record("so.tilo.chat", room, "derived:part.3@derived:messages@room");
    let awaits = Arc::new(tokio::sync::Notify::new());
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(automerge::AutoCommit::new().save()),
            committed_absent_channels: vec![part.channel_id()],
            committed_awaits: Some(Arc::clone(&awaits)),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", room);
    manager.join_route(part.clone());
    let unavailable = |events: &[CollabEvent]| {
        events
            .iter()
            .filter(
                |event| matches!(event, CollabEvent::RecordUnavailable { route } if *route == part),
            )
            .count()
    };
    let events = poll_until(&mut manager, |events| unavailable(events) > 0);
    assert_eq!(unavailable(&events), 1);
    let opens = || {
        captures
            .lock()
            .unwrap()
            .committed_reads
            .iter()
            .filter(|request| {
                matches!(
                    request,
                    runtime_collab::committed_document::Request::Open { .. }
                )
            })
            .count()
    };
    tilo_test_env::wait_until("the read to wait on the broker", || {
        captures
            .lock()
            .unwrap()
            .committed_reads
            .iter()
            .any(|request| {
                matches!(
                    request,
                    runtime_collab::committed_document::Request::Await {
                        known_revision: None
                    }
                )
            })
    });
    let asked = opens();
    thread::sleep(Duration::from_millis(500));
    let later = manager.poll();
    assert_eq!(unavailable(&later), 0, "said once");
    assert_eq!(opens(), asked, "nothing asks again while it waits");
}
