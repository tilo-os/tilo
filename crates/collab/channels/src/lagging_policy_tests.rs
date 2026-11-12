// A newcomer's policy reads the workspace's governance as it reached that
// device, which is not when it reached the member that admits it. An honest
// Welcome then names a device the newcomer has not heard of yet, or one it
// already heard was removed. These measure how long such a newcomer waits for
// its keys: lag of a moment must cost a moment, never a fresh request's wait.

/// How far, in seconds, a lagging copy of the governance is behind: longer
/// than a newcomer takes to be admitted, so its Welcome arrives meanwhile.
const LAG: u64 = 5;

/// The channel's epoch as `device` holds it.
fn epoch_of(
    device: &mut Device,
    workspace_id: WorkspaceId,
    channel: ChannelId,
    now: Instant,
) -> u64 {
    ready_epoch(&device.service.access(workspace_id, channel, now)).expect("a member holds keys")
}

/// Wait, in simulated time, until `device` holds `channel`'s keys or
/// `limit` passed; how long it took.
fn time_to_keys(
    devices: &mut [&mut Device],
    who: usize,
    workspace_id: WorkspaceId,
    channel: ChannelId,
    now: &mut Instant,
    limit: Duration,
) -> Option<Duration> {
    let started = *now;
    while now.duration_since(started) < limit {
        if ready_epoch(&devices[who].service.access(workspace_id, channel, *now)).is_some() {
            return Some(now.duration_since(started));
        }
        run(devices, now, 1);
    }
    None
}

/// A channel of `members` (seeds `1..=members`), every one of them holding
/// its keys, under `policy`.
fn seeded_channel(
    workspace: &Arc<Workspace>,
    workspace_id: WorkspaceId,
    channel: ChannelId,
    members: u8,
    policy: &Policy,
    dirs: &[tempfile::TempDir],
    now: &mut Instant,
) -> Vec<Device> {
    let mut devices: Vec<Device> = (1..=members)
        .map(|seed| {
            device(
                workspace,
                workspace_id,
                usize::from(seed),
                seed,
                policy,
                dirs[usize::from(seed)].path(),
            )
        })
        .collect();
    create(&mut devices[0], workspace_id, channel, *now);
    for device in devices.iter_mut().skip(1) {
        device.service.access(workspace_id, channel, *now);
    }
    let mut all: Vec<&mut Device> = devices.iter_mut().collect();
    run(&mut all, now, 12);
    for device in &mut devices {
        assert!(
            ready_epoch(&device.service.access(workspace_id, channel, *now)).is_some(),
            "every seeded member holds the channel's keys"
        );
    }
    devices
}

/// A newcomer whose policy has not heard yet of a device the members
/// admitted a moment before it: its Welcome's tree names that device. It
/// holds its keys as soon as its policy catches up, not after asking again.
#[test]
fn a_newcomer_behind_on_an_earlier_admission_is_kept_waiting_only_for_its_policy() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let dirs: Vec<_> = (0..40).map(|_| tempfile::tempdir().unwrap()).collect();
    let policy = Policy::readers([1, 2]);
    let mut now = Instant::now();
    let mut members = seeded_channel(
        &workspace,
        workspace_id,
        channel,
        2,
        &policy,
        &dirs,
        &mut now,
    );

    // An earlier guest, admitted under the members' policy.
    policy.allowed.lock().unwrap().insert(public(10));
    let mut earlier = device(&workspace, workspace_id, 10, 10, &policy, dirs[10].path());
    earlier.service.access(workspace_id, channel, now);
    {
        let mut all: Vec<&mut Device> = members.iter_mut().collect();
        all.push(&mut earlier);
        assert!(
            time_to_keys(
                &mut all,
                2,
                workspace_id,
                channel,
                &mut now,
                Duration::from_secs(20)
            )
            .is_some()
        );
    }

    // The newcomer's own copy of the governance has heard of itself and the
    // members, and of the earlier guest only five seconds later: after the
    // Welcome admitting it arrived.
    policy.allowed.lock().unwrap().insert(public(11));
    let late = Policy::readers([1, 2, 11]);
    let mut newcomer = device(&workspace, workspace_id, 11, 11, &late, dirs[11].path());
    newcomer.service.access(workspace_id, channel, now);
    let before = epoch_of(&mut members[0], workspace_id, channel, now);
    let mut all: Vec<&mut Device> = members.iter_mut().collect();
    all.push(&mut earlier);
    all.push(&mut newcomer);
    let started = now;
    run(&mut all, &mut now, LAG);
    late.allowed.lock().unwrap().insert(public(10));
    let waited = time_to_keys(
        &mut all,
        3,
        workspace_id,
        channel,
        &mut now,
        Duration::from_secs(120),
    )
    .map(|_| now.duration_since(started));
    let commits = epoch_of(&mut members[0], workspace_id, channel, now) - before;
    eprintln!("newcomer behind on an earlier admission: keys after {waited:?}, {commits} commits");
    let waited = waited.expect("the newcomer is admitted");
    assert!(
        waited <= Duration::from_secs(LAG + 1),
        "a policy behind costs its lag, not a new request: {waited:?}"
    );
    assert_eq!(
        commits, 1,
        "the Welcome that admitted the newcomer is the one it took: it was never refused and asked for again"
    );
}

/// A newcomer whose view never comes to back its Welcome (its tree names a
/// device the newcomer's membership does not let in) holds it for
/// [`WELCOME_WAIT`] and no longer: it gives up, asks with a fresh key, and
/// holds the next Welcome alike. Once the members' commit leaves that device
/// out, the newcomer gives up on the Welcome the channel moved past at once,
/// and an honest commit admits it.
#[test]
fn a_welcome_the_view_never_backs_is_given_up_on_and_asked_for_again() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let dirs: Vec<_> = (0..40).map(|_| tempfile::tempdir().unwrap()).collect();
    let policy = Policy::readers([1, 2, 9]);
    let mut now = Instant::now();
    let mut members = seeded_channel(
        &workspace,
        workspace_id,
        channel,
        2,
        &policy,
        &dirs,
        &mut now,
    );
    let mut stranger = device(&workspace, workspace_id, 9, 9, &policy, dirs[9].path());
    stranger.service.access(workspace_id, channel, now);
    {
        let mut all: Vec<&mut Device> = members.iter_mut().collect();
        all.push(&mut stranger);
        assert!(
            time_to_keys(
                &mut all,
                2,
                workspace_id,
                channel,
                &mut now,
                Duration::from_secs(20)
            )
            .is_some()
        );
    }
    policy.allowed.lock().unwrap().insert(public(11));
    let view = Policy::readers([1, 2, 11]);
    let mut newcomer = device(&workspace, workspace_id, 11, 11, &view, dirs[11].path());
    newcomer.service.access(workspace_id, channel, now);
    let mut all: Vec<&mut Device> = members.iter_mut().collect();
    all.push(&mut stranger);
    all.push(&mut newcomer);
    let held = |device: &Device| {
        device.service.peers[&channel]
            .welcome
            .as_ref()
            .map(|held| held.frame.epoch)
    };
    // Every quarter second, which Welcome the newcomer holds.
    let mut seen = Vec::new();
    for _ in 0..(4 * (WELCOME_WAIT.as_secs() + 6)) {
        run(&mut all, &mut now, 0);
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in &mut all {
            device.service.pump(&device.endpoint, now, 1_000);
        }
        let epoch = held(all[3]);
        if seen.last().is_none_or(|(last, _)| *last != epoch) {
            seen.push((epoch, now));
        }
    }
    let held_at: Vec<(u64, Instant)> = seen
        .iter()
        .filter_map(|(epoch, at)| epoch.map(|epoch| (epoch, *at)))
        .collect();
    let [(first, first_at), (next, next_at), ..] = held_at[..] else {
        panic!("a Welcome held, given up on and the next one held: {seen:?}");
    };
    let given_up = seen
        .iter()
        .find(|(epoch, at)| epoch.is_none() && *at > first_at)
        .map(|(_, at)| at.duration_since(first_at));
    assert_eq!(
        given_up,
        Some(WELCOME_WAIT),
        "the first Welcome is held for the whole wait and no longer"
    );
    assert!(
        next > first,
        "the next Welcome is a new admission: {seen:?}"
    );
    assert!(
        next_at.duration_since(first_at) <= WELCOME_WAIT + Duration::from_secs(3),
        "asked for again at once: {seen:?}"
    );

    let started = now;
    policy.allowed.lock().unwrap().remove(&public(9));
    let waited = time_to_keys(
        &mut all,
        3,
        workspace_id,
        channel,
        &mut now,
        Duration::from_secs(60),
    )
    .map(|_| now.duration_since(started));
    eprintln!("newcomer once the members left the stranger out: keys after {waited:?}");
    let waited = waited.expect("the newcomer is admitted by an honest commit");
    assert!(
        waited <= Duration::from_secs(4),
        "the Welcome the channel moved past is given up on at once: {waited:?}"
    );
}

/// Twenty guests arrive in two crowds a second apart, each one's copy of the
/// governance hearing of the others in its own order, up to five seconds
/// late: the Welcome admitting the second crowd names guests of the first
/// that some of the second have not heard of yet. Every guest holds its keys
/// once its view caught up, and no Welcome is refused and asked for again.
#[test]
fn twenty_guests_in_two_crowds_with_lagging_views_hold_their_keys_without_asking_again() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let dirs: Vec<_> = (0..64).map(|_| tempfile::tempdir().unwrap()).collect();
    let policy = Policy::readers([1, 2, 3]);
    let mut now = Instant::now();
    let mut members = seeded_channel(
        &workspace,
        workspace_id,
        channel,
        3,
        &policy,
        &dirs,
        &mut now,
    );

    let guests: Vec<u8> = (20..40).collect();
    let views: Vec<Policy> = guests
        .iter()
        .map(|guest| Policy::readers([1, 2, 3, *guest]))
        .collect();
    let mut arrived: Vec<Device> = guests
        .iter()
        .zip(&views)
        .map(|(guest, view)| {
            policy.allowed.lock().unwrap().insert(public(*guest));
            device(
                &workspace,
                workspace_id,
                usize::from(*guest),
                *guest,
                view,
                dirs[usize::from(*guest)].path(),
            )
        })
        .collect();
    let before = epoch_of(&mut members[0], workspace_id, channel, now);
    let started = now;
    let mut held = vec![None; guests.len()];
    // Guest `i` hears of guest `j` after `(i * 7 + j * 3) % 21` quarter
    // seconds: up to five seconds, in an order of its own.
    for quarter in 0..(4 * 90) {
        // The first crowd asks at once, the second a second later.
        for (i, guest) in arrived.iter_mut().enumerate() {
            if quarter == if i < 10 { 0 } else { 4 } {
                guest.service.access(workspace_id, channel, now);
            }
        }
        for (i, view) in views.iter().enumerate() {
            for (j, other) in guests.iter().enumerate() {
                if (i * 7 + j * 3) % 21 == quarter {
                    view.allowed.lock().unwrap().insert(public(*other));
                }
            }
        }
        let mut all: Vec<&mut Device> = members.iter_mut().collect();
        all.extend(arrived.iter_mut());
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in &mut all {
            device.service.pump(&device.endpoint, now, 1_000);
        }
        for (i, guest) in arrived.iter_mut().enumerate() {
            if held[i].is_none()
                && ready_epoch(&guest.service.access(workspace_id, channel, now)).is_some()
            {
                held[i] = Some(now.duration_since(started));
            }
        }
        if held.iter().all(Option::is_some) {
            break;
        }
    }
    let commits = epoch_of(&mut members[0], workspace_id, channel, now) - before;
    eprintln!("twenty guests in two crowds: keys after {held:?}, {commits} commits");
    let slowest = held
        .iter()
        .map(|waited| waited.expect("every guest is admitted"))
        .max()
        .unwrap();
    assert!(
        slowest <= Duration::from_secs(LAG + 2),
        "the slowest guest waits for its view and the commits, not a new request: {slowest:?}"
    );
    assert!(
        commits <= 3,
        "a commit or two admits the crowds, none for a Welcome asked for again: {commits}"
    );
}

/// A device leaves the channel's readers (its member removed, or the device
/// revoked while its person's other device stays: both reach a channel as a
/// device leaving `allowed`) and a guest arrives a moment later. The guest's
/// copy of the governance already left the device out; the members' copies
/// still name it for a second, so the Welcome they write keeps its leaf. The
/// guest holds its keys within seconds, and the device that left holds no key
/// of the guest's epoch.
#[test]
fn a_guest_arriving_just_after_a_device_left_holds_its_keys_within_seconds() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let channel = ChannelId::new();
    let dirs: Vec<_> = (0..40).map(|_| tempfile::tempdir().unwrap()).collect();
    // Seed 3 is the device that leaves; seed 4 stays (its person's other
    // device, when seed 3 is revoked).
    let policy = Policy::readers([1, 2, 3, 4]);
    let mut now = Instant::now();
    let mut members = seeded_channel(
        &workspace,
        workspace_id,
        channel,
        4,
        &policy,
        &dirs,
        &mut now,
    );

    // The guest's copy heard the device left; the members' copies hear it
    // five seconds later, after they admitted the guest.
    let guest_view = Policy::readers([1, 2, 4, 30]);
    policy.allowed.lock().unwrap().insert(public(30));
    let mut guest = device(
        &workspace,
        workspace_id,
        30,
        30,
        &guest_view,
        dirs[30].path(),
    );
    guest.service.access(workspace_id, channel, now);
    let before = epoch_of(&mut members[0], workspace_id, channel, now);
    let started = now;
    let waited = {
        let mut all: Vec<&mut Device> = members.iter_mut().collect();
        all.push(&mut guest);
        run(&mut all, &mut now, LAG);
        policy.allowed.lock().unwrap().remove(&public(3));
        time_to_keys(
            &mut all,
            4,
            workspace_id,
            channel,
            &mut now,
            Duration::from_secs(120),
        )
        .map(|_| now.duration_since(started))
    };
    let commits = epoch_of(&mut members[0], workspace_id, channel, now) - before;
    eprintln!("guest just after a device left: keys after {waited:?}, {commits} commits");
    let waited = waited.expect("the guest is admitted");
    assert!(
        waited <= Duration::from_secs(LAG + 3),
        "the members' lag costs the guest about that lag, not a new request's wait: {waited:?}"
    );
    assert!(
        commits <= 3,
        "one admission the guest held, the removal, and one admission it took: {commits}"
    );
    let mut all: Vec<&mut Device> = members.iter_mut().collect();
    all.push(&mut guest);
    run(&mut all, &mut now, 6);
    let guest_epoch = ready_epoch(&guest.service.access(workspace_id, channel, now)).unwrap();
    let removed_epoch = ready_epoch(&members[2].service.access(workspace_id, channel, now));
    assert!(
        removed_epoch.is_none_or(|epoch| epoch < guest_epoch),
        "the removed device holds no key of the epoch the guest is in: {removed_epoch:?} vs {guest_epoch}"
    );
}
