// A document only some members of a workspace read seals under the group of its reader set:
// the devices of its people hold the keys, no one else's, and a person or a device that leaves
// gets none of what is sealed after.

/// The people a device is one of, by seed: devices 1 and 4 are Ada's, 2 Bob's, 3 Cleo's.
fn person_of(seed: u8) -> [u8; 32] {
    match seed {
        1 | 4 => [0xa0; 32],
        2 => [0xb0; 32],
        _ => [0xc0; 32],
    }
}

/// A policy whose workspace admits the devices `seeds` name, each one of its person's.
fn people_policy(seeds: impl IntoIterator<Item = u8> + Clone) -> Policy {
    let policy = Policy::readers(seeds.clone());
    for seed in seeds {
        policy
            .people
            .lock()
            .unwrap()
            .insert(public(seed), person_of(seed));
    }
    policy
}

/// Every device's protocol for `seconds` of simulated time.
fn pump_for(devices: &mut [&mut Device], now: &mut Instant, seconds: u64) {
    for _ in 0..seconds.saturating_mul(4) {
        *now = now.checked_add(Duration::from_millis(250)).unwrap();
        for device in devices.iter_mut() {
            device.service.pump(&device.endpoint, *now, 1_000);
        }
    }
}

/// What `device` may do with the group of `readers` in `workspace_id`, asking for it.
fn reader_access(
    device: &mut Device,
    workspace_id: WorkspaceId,
    readers: &str,
    now: Instant,
) -> KeyAccess {
    let group = device
        .service
        .reader_group(workspace_id, readers)
        .expect("a reader set");
    device.service.access(group, now)
}

/// The keys `device` holds of the group of `readers` in `workspace_id`.
fn reader_keys(
    device: &mut Device,
    workspace_id: WorkspaceId,
    readers: &str,
    now: Instant,
) -> ContentKeys {
    let KeyAccess::Ready(keys) = reader_access(device, workspace_id, readers, now) else {
        panic!("a reader's device holds the group's keys");
    };
    keys
}

/// The workspace no longer admits device `seed`, and its key holders follow.
fn leaves(policy: &Policy, seed: u8, devices: &mut [&mut Device], now: &mut Instant) {
    policy.allowed.lock().unwrap().remove(&public(seed));
    pump_for(devices, now, 5);
}

/// `founder` creates the workspace and `members` join its own group; answers the keys the last
/// of them holds.
fn members_join(
    founder: &mut Device,
    members: [&mut Device; 3],
    workspace_id: WorkspaceId,
    now: &mut Instant,
) -> ContentKeys {
    create(founder, workspace_id, *now);
    let [first, second, third] = members;
    for member in [&mut *first, &mut *second, &mut *third] {
        member.service.access(workspace_id, *now);
    }
    pump_for(&mut [founder, first, second, &mut *third], now, 20);
    let KeyAccess::Ready(keys) = third.service.access(workspace_id, *now) else {
        panic!("every member holds the workspace's keys");
    };
    keys
}

#[test]
fn every_member_reads_under_the_workspaces_own_group() {
    let workspace_id = WorkspaceId::new();
    let policy = people_policy([1]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = Arc::new(Workspace::default());
    let mut ada = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
    assert_eq!(
        ada.service
            .reader_group(workspace_id, collab_types::EVERY_MEMBER),
        Some(workspace_id)
    );
    assert_eq!(ada.service.reader_group(workspace_id, "not a set"), None);
}

#[test]
fn only_the_readers_devices_hold_their_groups_keys_and_one_that_leaves_reads_nothing_after() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = people_policy([1, 2, 3, 4]);
    let dirs = [(); 4].map(|()| tempfile::tempdir().unwrap());
    let mut now = Instant::now();
    let mut ada = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut bob = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut cleo = device(&workspace, workspace_id, 2, 3, &policy, dirs[2].path());
    let mut phone = device(&workspace, workspace_id, 3, 4, &policy, dirs[3].path());
    // Every member reads the workspace's own group.
    let everyone = members_join(
        &mut ada,
        [&mut bob, &mut cleo, &mut phone],
        workspace_id,
        &mut now,
    );

    let readers = collab_types::Readers::people([person_of(1), person_of(2)])
        .unwrap()
        .text();
    let group = collab_types::reader_group(workspace_id, &readers).unwrap();
    // Cleo is no reader: her device takes no part in the group.
    assert!(matches!(
        reader_access(&mut cleo, workspace_id, &readers, now),
        KeyAccess::Unavailable
    ));
    reader_access(&mut ada, workspace_id, &readers, now);
    reader_access(&mut bob, workspace_id, &readers, now);
    reader_access(&mut phone, workspace_id, &readers, now);
    pump_for(
        &mut [&mut ada, &mut bob, &mut cleo, &mut phone],
        &mut now,
        20,
    );
    // Every device of a reader holds the group's keys.
    let held = [&mut ada, &mut bob, &mut phone]
        .map(|reader| reader_keys(reader, workspace_id, &readers, now));
    assert!(!matches!(cleo.service.record_keys(group), Some(Ok(_))));
    assert!(matches!(
        reader_access(&mut cleo, workspace_id, &readers, now),
        KeyAccess::Unavailable
    ));

    // The readers' devices derive one key for a document; Cleo's keys open none of it.
    let id = workspace_id.to_string();
    let artifact = collab_types::content_artifact_id(&id, &readers, "plans/budget#0");
    let (metadata, payload) = held[0]
        .seal_content(artifact, ContentRetention::Delta, b"for the two of us")
        .unwrap();
    for keys in &held[1..] {
        let opened = keys.open_content(artifact, &metadata, &payload).unwrap();
        assert_eq!(opened.as_slice(), b"for the two of us");
    }
    assert!(
        everyone
            .open_content(artifact, &metadata, &payload)
            .is_err()
    );
    // The key is the document's: the same content under another document does not open.
    let other = collab_types::content_artifact_id(&id, &readers, "plans/budget#1");
    assert!(held[1].open_content(other, &metadata, &payload).is_err());

    // A revoked device leaves the group: what is sealed after, it cannot open.
    leaves(
        &policy,
        4,
        &mut [&mut ada, &mut bob, &mut cleo, &mut phone],
        &mut now,
    );
    assert!(matches!(
        reader_access(&mut phone, workspace_id, &readers, now),
        KeyAccess::Removed
    ));
    let after_revocation = reader_keys(&mut ada, workspace_id, &readers, now);
    assert!(after_revocation.epoch() > held[2].epoch());
    let (metadata, payload) = after_revocation
        .seal_content(artifact, ContentRetention::Delta, b"after the phone")
        .unwrap();
    assert!(held[2].open_content(artifact, &metadata, &payload).is_err());
    let bobs = reader_keys(&mut bob, workspace_id, &readers, now);
    assert_eq!(
        bobs.open_content(artifact, &metadata, &payload)
            .unwrap()
            .as_slice(),
        b"after the phone"
    );

    // Bob leaves the workspace: he leaves the group, and reads nothing sealed after.
    leaves(
        &policy,
        2,
        &mut [&mut ada, &mut bob, &mut cleo, &mut phone],
        &mut now,
    );
    assert!(matches!(
        reader_access(&mut bob, workspace_id, &readers, now),
        KeyAccess::Removed
    ));
    let after_removal = reader_keys(&mut ada, workspace_id, &readers, now);
    assert!(after_removal.epoch() > bobs.epoch());
    let (metadata, payload) = after_removal
        .seal_content(artifact, ContentRetention::Delta, b"Ada alone")
        .unwrap();
    assert!(bobs.open_content(artifact, &metadata, &payload).is_err());
    // The workspace's own group went on as it was, for the members it still has.
    assert!(matches!(
        cleo.service.access(workspace_id, now),
        KeyAccess::Ready(_)
    ));
}
