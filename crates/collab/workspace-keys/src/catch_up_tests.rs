// A device keeps the state of the groups it takes part in on disk, and its
// host replays the governance it retained when the device comes back: the
// state takes that in before an app of the device reads the group, so the
// first read decides on the group as it is rather than as the device left it.

/// A device that was away takes up the state it holds and applies what its
/// host kept, before any app of it reads the group.
#[test]
fn a_device_takes_up_the_state_it_holds_before_any_app_reads() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2, 3]);
    let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    create(&mut founder, workspace_id, now);
    member.service.access(workspace_id, now);
    run(&mut [&mut founder, &mut member], &mut now, 10);
    let joined = ready_epoch(&member.service.access(workspace_id, now));
    assert!(joined.is_some(), "the member joined the group");

    // It is away: no service keeps its groups, and its apps read none of
    // them, while the group admits another device.
    drop(member);
    let mut away = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut third = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    third.service.access(workspace_id, now);
    run(&mut [&mut founder, &mut third], &mut now, 10);
    let advanced = ready_epoch(&founder.service.access(workspace_id, now));
    assert!(
        advanced > joined,
        "the group advanced while the device was away"
    );

    // What the group committed while the device was away is what it kept for
    // it: it reaches the device before any app of it reads the group, and no
    // access request was made for it here.
    run(&mut [&mut founder, &mut away], &mut now, 5);
    assert_eq!(
        ready_epoch(&away.service.access(workspace_id, now)),
        advanced,
        "the state the device held took the replay in"
    );
}

/// A member that was away while the group moved on follows it from the frames another member hands
/// it, with no host to replay them; a device the policy does not admit is handed none.
#[test]
fn a_member_hands_another_the_frames_it_was_away_for() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2, 3]);
    let dirs = [(); 3].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    create(&mut founder, workspace_id, now);
    member.service.access(workspace_id, now);
    run(&mut [&mut founder, &mut member], &mut now, 10);
    let joined = ready_epoch(&member.service.access(workspace_id, now)).unwrap();

    // The member is away while the group admits another device.
    drop(member);
    let mut third = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    third.service.access(workspace_id, now);
    run(&mut [&mut founder, &mut third], &mut now, 10);
    let advanced = ready_epoch(&founder.service.access(workspace_id, now));
    assert!(advanced > Some(joined), "the group moved on");

    // It comes back where no host replays what it missed; the founder hands it the frames from
    // the epoch it holds, and to no device outside the policy.
    let mut away = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    assert_eq!(
        ready_epoch(&away.service.access(workspace_id, now)),
        Some(joined)
    );
    assert!(
        founder
            .service
            .frames_for(workspace_id, &public(9), joined, 1_000)
            .is_empty(),
        "a device the policy does not admit is handed nothing"
    );
    let handed = founder
        .service
        .frames_for(workspace_id, &public(2), joined, 1_000)
        .into_iter()
        .map(|frame| InboundGovernance::Frame {
            workspace: workspace_id,
            frame: Box::new(frame),
        })
        .collect();
    away.service
        .pump_inbound(handed, &away.endpoint, now, 1_000);
    assert_eq!(
        ready_epoch(&away.service.access(workspace_id, now)),
        advanced,
        "the member follows the frames it was handed"
    );
}
