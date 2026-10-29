/// An app counts its action directory as published once it is queued, and
/// never sends it again on that route. A directory queued while the primary
/// route's join is refused must reach the broker once a later join of that
/// route succeeds: dropping it left a window Ready that no one could find
/// or invoke.
#[test]
fn a_directory_published_before_the_primary_route_joins_is_sent_once_it_joins() {
    let broker = FakeBroker::default();
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let room = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.files", room);
    let hello = || CollabRequest::Hello {
        app_id: "so.tilo.files".into(),
        cluster_id: room,
        record: Some("tab:0000000000000259".into()),
        record_kind: None,
        handle: None,
    };
    // The window moves to its record's route; the broker refuses that join
    // for now (the record is not ready on this device yet).
    captures.lock().unwrap().join_refusal = Some("record not ready".into());
    assert!(manager.send(hello()));
    assert!(manager.is_connected());
    manager.publish_action_directory("so.tilo.files");
    assert!(
        manager.action_directory_observation().0,
        "the owner counts the directory as published and will not send it again"
    );
    tilo_test_env::wait_until("the refused record join", || {
        captures.lock().unwrap().joined.len() == 2
    });
    // The record is ready now: the route joins on the next Hello.
    captures.lock().unwrap().join_refusal = None;
    assert!(manager.send(hello()));
    let joined = manager.begin_document_publication_barrier().unwrap();
    assert_eq!(joined.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    assert_eq!(captures.lock().unwrap().joined.len(), 3);
    assert_eq!(
        captures.lock().unwrap().action_directories,
        ["so.tilo.files"],
        "the directory queued during the refused join reaches the broker once the route joins"
    );
    drop(manager);
    thread.join().unwrap();
}
