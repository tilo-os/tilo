/// A route whose first committed read never answers holds up nothing else
/// on the app's connection: another route joins and reads meanwhile, and the
/// stuck one ends unjoined once its join's bound passed, with a reason the
/// app reads and the route's access shown as unavailable. A Chat window
/// whose new message part never answered its first read never answered its
/// send either: the whole connection waited on that one route.
#[test]
fn a_route_whose_first_read_never_answers_holds_up_nothing_else() {
    let workspace = Uuid::new_v4();
    let stuck = RouteSpec::record(
        "so.tilo.chat",
        workspace,
        "derived:part.0@derived:messages@workspace",
    );
    let other = RouteSpec::record(
        "so.tilo.chat",
        workspace,
        "derived:part.1@derived:messages@workspace",
    );
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(automerge::AutoCommit::new().save()),
            committed_open_unanswered_channels: vec![stuck.channel_id()],
            committed_absent_channels: vec![other.channel_id()],
            committed_awaits: Some(Arc::new(tokio::sync::Notify::new())),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, _captures, _thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", workspace);
    manager.join_route(stuck.clone(), crate::record::RecordKind::new(""));
    manager.join_route(other.clone(), crate::record::RecordKind::new(""));
    let started = Instant::now();
    let events = poll_until(&mut manager, |events| {
        events.iter().any(
            |event| matches!(event, CollabEvent::RecordUnavailable { route } if *route == other),
        )
    });
    assert!(
        started.elapsed() < ROUTE_JOIN_WAIT,
        "the other route's read came while the stuck one still waited"
    );
    assert_eq!(manager.route_join_failure(&stuck), None, "still joining");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            CollabEvent::ChannelAccess { route, .. } if *route == stuck
        )),
        "nothing said of the stuck route yet"
    );
    let events = poll_until(&mut manager, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                CollabEvent::ChannelAccess {
                    route,
                    state: crate::ChannelAccessState::Unavailable,
                } if *route == stuck
            )
        })
    });
    drop(events);
    let reason = manager
        .route_join_failure(&stuck)
        .expect("the stuck route says why it did not join");
    assert!(reason.contains("did not load"), "{reason}");
    assert_eq!(manager.route_join_failure(&other), None);
}
