// A device keeps the state of the channels it takes part in on disk, and its
// host replays the governance it retained when the device comes back: the
// state takes that in before an app of the device reads the channel, so the
// first read decides on the group as it is rather than as the device left it.

/// A device that was away takes up the state it holds and applies what its
/// host kept, before any app of it reads the channel.
#[test]
fn a_device_takes_up_the_state_it_holds_before_any_app_reads() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([1, 2, 3]);
    let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    create(&mut founder, workspace_id, channel, now);
    member.service.access(workspace_id, channel, now);
    run(&mut [&mut founder, &mut member], &mut now, 10);
    let joined = ready_epoch(&member.service.access(workspace_id, channel, now));
    assert!(joined.is_some(), "the member joined the group");

    // It is away: no service keeps its channels, and its apps read none of
    // them, while the group admits another device.
    drop(member);
    let mut away = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut third = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    third.service.access(workspace_id, channel, now);
    run(&mut [&mut founder, &mut third], &mut now, 10);
    let advanced = ready_epoch(&founder.service.access(workspace_id, channel, now));
    assert!(
        advanced > joined,
        "the group advanced while the device was away"
    );

    // What the group committed while the device was away is what it kept for
    // it: it reaches the device before any app of it reads the channel, and no
    // access request was made for it here.
    run(&mut [&mut founder, &mut away], &mut now, 5);
    assert_eq!(
        ready_epoch(&away.service.access(workspace_id, channel, now)),
        advanced,
        "the state the device held took the replay in"
    );
}
