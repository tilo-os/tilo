// Members of a channel's group take turns at committing a change of it: one
// commits, the others follow its commit. Members that all commit the same
// change at once fork the group, and every one whose commit lost commits again
// to hand its branch's keys over, forking it again.

/// Run every device's protocol for `seconds` of simulated time, each pass
/// handing every device what the others said in the pass before: a device
/// hears another's commit only after it had its own chance to commit.
fn run_together(devices: &mut [&mut Device], now: &mut Instant, seconds: u64) {
    for _ in 0..seconds.saturating_mul(4) {
        *now = now.checked_add(Duration::from_millis(250)).unwrap();
        let heard: Vec<Vec<InboundChannelGovernance>> = devices
            .iter()
            .map(|device| device.endpoint.take_inbound())
            .collect();
        for (device, heard) in devices.iter_mut().zip(heard) {
            device
                .service
                .pump_inbound(heard, &device.endpoint, *now, 1_000);
        }
    }
}

/// A device every member hears ask at once is added by one commit, which the
/// others follow, rather than by one commit of each that fork the group.
#[test]
fn a_device_every_member_hears_ask_is_added_by_one_commit() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = collab_types::workspace_scope(&workspace_id.to_string());
    let policy = Policy::readers([1, 2, 3]);
    let dirs = [(); 4].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut site = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut ada = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut bob = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    create(&mut site, workspace_id, channel, now);
    ada.service.access(workspace_id, now);
    bob.service.access(workspace_id, now);
    run(&mut [&mut site, &mut ada, &mut bob], &mut now, 20);
    let before = ready_epoch(&site.service.access(workspace_id, now)).unwrap();
    for member in [&mut ada, &mut bob] {
        assert_eq!(
            ready_epoch(&member.service.access(workspace_id, now)),
            Some(before),
            "every member is in the group"
        );
    }

    policy.allowed.lock().unwrap().insert(public(4));
    let mut cyd = device(&workspace, workspace_id, 3, 4, &policy, dirs[3].path());
    cyd.service.access(workspace_id, now);
    run_together(&mut [&mut site, &mut ada, &mut bob, &mut cyd], &mut now, 20);

    for (name, device) in [
        ("site", &mut site),
        ("Ada", &mut ada),
        ("Bob", &mut bob),
        ("Cyd", &mut cyd),
    ] {
        assert_eq!(
            ready_epoch(&device.service.access(workspace_id, now)),
            Some(before + 1),
            "{name} is at the one commit that added Cyd"
        );
    }
}

/// Devices that ask within [`COMMIT_SPACING`] of a commit are admitted
/// together by the next one, rather than by a commit each that every member
/// follows; the first after a quiet spell is admitted at once.
#[test]
fn devices_asking_close_together_are_admitted_in_one_commit() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = collab_types::workspace_scope(&workspace_id.to_string());
    let policy = Policy::readers([1, 2, 3, 4, 5]);
    let dirs = [(); 5].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut site = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut ada = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    create(&mut site, workspace_id, channel, now);
    ada.service.access(workspace_id, now);
    run(&mut [&mut site, &mut ada], &mut now, 20);
    let before = ready_epoch(&site.service.access(workspace_id, now)).unwrap();

    let step = |devices: &mut [&mut Device], now: &mut Instant| {
        *now = now.checked_add(Duration::from_millis(100)).unwrap();
        for device in devices.iter_mut() {
            device.service.pump(&device.endpoint, *now, 1_000);
        }
    };
    let mut bob = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    let mut cyd = device(&workspace, workspace_id, 3, 4, &policy, dirs[3].path());
    let mut dan = device(&workspace, workspace_id, 4, 5, &policy, dirs[4].path());
    // Bob asks after a quiet spell: the site admits him at once.
    bob.service.access(workspace_id, now);
    step(&mut [&mut site, &mut bob], &mut now);
    step(&mut [&mut site, &mut bob], &mut now);
    step(&mut [&mut site, &mut bob], &mut now);
    assert_eq!(
        ready_epoch(&bob.service.access(workspace_id, now)),
        Some(before + 1),
        "the first to ask after a quiet spell is admitted at once"
    );
    // Cyd and Dan ask a moment apart, right after.
    cyd.service.access(workspace_id, now);
    step(
        &mut [&mut site, &mut ada, &mut bob, &mut cyd, &mut dan],
        &mut now,
    );
    dan.service.access(workspace_id, now);
    run(
        &mut [&mut site, &mut ada, &mut bob, &mut cyd, &mut dan],
        &mut now,
        5,
    );
    for (name, device) in [
        ("site", &mut site),
        ("Ada", &mut ada),
        ("Bob", &mut bob),
        ("Cyd", &mut cyd),
        ("Dan", &mut dan),
    ] {
        assert_eq!(
            ready_epoch(&device.service.access(workspace_id, now)),
            Some(before + 2),
            "{name} is at the one commit that added Cyd and Dan"
        );
    }
}
