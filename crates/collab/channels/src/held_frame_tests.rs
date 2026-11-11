// A device's policy reads its workspace as the governance reached it, which
// is not always before the channel governance does: a frame from a device the
// workspace has just admitted is judged again once the policy names it,
// rather than refused for good.

/// The founder's genesis reaches a device whose policy does not name the
/// founder yet, and is applied once the policy does: a frame the policy does
/// not back is kept for the policy that moves, so the device is handed the
/// group that exists rather than founding one of its own.
#[test]
fn a_genesis_is_kept_for_the_policy_that_names_its_signer_later() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    // The founder and a member read the channel under the workspace's
    // governance; the late device's own copy has heard of the member but not
    // of the founder yet.
    let policy = Policy::readers([1, 2]);
    let late_policy = Policy::readers([2, 3]);
    let dirs = [
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    ];
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut member = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut late = device(&workspace, workspace_id, 2, 3, &late_policy, dirs[2].path());
    let mut now = Instant::now();

    let created = create(&mut founder, workspace_id, channel, now);
    assert!(
        ready_epoch(&created).is_some(),
        "the founding device is handed the channel's keys: {created:?}"
    );
    assert!(matches!(
        late.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut founder, &mut member, &mut late], &mut now, 1);
    let held = |device: &Device| {
        device
            .service
            .peers
            .get(&channel)
            .is_some_and(|entry| !entry.held.is_empty())
    };
    assert!(
        held(&late),
        "a policy that does not name the founder keeps its frame, and backs none of its group"
    );
    assert!(
        !late.service.peers[&channel].peer.has_genesis(),
        "the frame the policy does not back is not applied"
    );
    assert!(
        late.service.existing_keys(workspace_id, channel).is_none(),
        "a policy that does not name the founder backs no frame of its group"
    );

    // The policy catches up with the workspace it reads.
    late_policy.allowed.lock().unwrap().insert(public(1));
    run(&mut [&mut founder, &mut member, &mut late], &mut now, 8);

    let entry = &late.service.peers[&channel];
    assert!(
        entry.held.is_empty(),
        "the frame the policy now backs leaves the hold"
    );
    assert!(
        entry.peer.has_genesis(),
        "the late device takes the founder's genesis"
    );
    assert!(
        matches!(entry.peer.access(), Access::Waiting),
        "it is handed the group that exists, and asks to join it: it founds no group of its own"
    );
    assert!(
        late.service.existing_keys(workspace_id, channel).is_none(),
        "no keys of its own: the group it holds is the founder's"
    );
}

/// A device of a workspace hosted elsewhere founds none of its channels
/// before the host answered its request, however long that takes: the host
/// may carry the channel's genesis, and a second one would split the channel
/// under the members already in it. Once the host says it carries none, the
/// device founds the group at once.
#[test]
fn a_device_founds_a_channel_once_the_host_says_it_has_no_group() {
    let workspace = Arc::new(Workspace::default());
    workspace
        .silent
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let policy = Policy::readers([1]);
    let dir = tempfile::tempdir().unwrap();
    let mut entered = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
    let mut now = Instant::now();

    assert!(matches!(
        entered.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ));
    run(&mut [&mut entered], &mut now, 30);
    assert!(
        !entered.service.peers[&channel].peer.knows_group(),
        "no group is founded while the host has not answered"
    );

    workspace
        .silent
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let asked = now;
    while matches!(
        entered.service.access(workspace_id, channel, now),
        ChannelAccess::Waiting
    ) {
        run(&mut [&mut entered], &mut now, 1);
    }
    assert!(
        ready_epoch(&entered.service.access(workspace_id, channel, now)).is_some(),
        "a host that carries no genesis leaves the channel to this device to found"
    );
    assert!(
        now.duration_since(asked) <= Duration::from_secs(1),
        "founded as soon as the host's answer arrived, not after a wait: {:?}",
        now.duration_since(asked)
    );
}
