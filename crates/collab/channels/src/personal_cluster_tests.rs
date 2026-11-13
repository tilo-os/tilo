/// A workspace admits exactly the devices its directory names as its
/// channels' readers: the first founds the group and is answered its keys, a
/// second device of the same identity is admitted, and a device outside it
/// is never handed them.
#[test]
fn a_workspace_admits_the_readers_its_directory_names_and_no_one_else() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    // The identity's two devices, not the outsider's.
    let policy = Policy::default();
    policy.allowed.lock().unwrap().insert(public(1));
    policy.allowed.lock().unwrap().insert(public(2));
    let dirs = [
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    ];
    let mut first = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut second = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut outsider = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());

    let mut now = Instant::now();
    assert!(
        matches!(
            first.service.access(workspace_id, channel, now),
            ChannelAccess::Waiting
        ),
        "a channel is taken up once its readers are named"
    );
    assert!(matches!(
        outsider.service.access(workspace_id, channel, now),
        ChannelAccess::Unavailable
    ));

    // The first device founds the group after its while and is answered
    // its keys; the second is admitted once it asks.
    let access = create(&mut first, workspace_id, channel, now);
    assert!(
        ready_epoch(&access).is_some(),
        "the founding device is handed the channel's keys: {access:?}"
    );
    second.service.access(workspace_id, channel, now);
    run(&mut [&mut first, &mut second], &mut now, 5);
    assert!(
        second
            .service
            .existing_keys(workspace_id, channel)
            .is_some()
    );
    assert!(
        outsider
            .service
            .existing_keys(workspace_id, channel)
            .is_none(),
        "a device outside the identity never gets the keys"
    );
    assert!(outsider.service.peers.is_empty());
}
