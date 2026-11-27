// A device's policy reads its workspace as the governance reached it, which
// is not always before the group governance does: a frame from a device the
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
    // The founder and a member read the group under the workspace's
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

    let created = create(&mut founder, workspace_id, now);
    assert!(
        ready_epoch(&created).is_some(),
        "the founding device is handed the group's keys: {created:?}"
    );
    assert!(matches!(
        late.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    drive(&mut [&mut founder, &mut member, &mut late], &mut now, 1);
    let held = |device: &Device| {
        device
            .service
            .peers
            .get(&workspace_id)
            .is_some_and(|entry| !entry.held.is_empty())
    };
    assert!(
        held(&late),
        "a policy that does not name the founder keeps its frame, and backs none of its group"
    );
    assert!(
        !late.service.peers[&workspace_id].peer.has_genesis(),
        "the frame the policy does not back is not applied"
    );
    assert!(
        late.service.existing_keys(workspace_id).is_none(),
        "a policy that does not name the founder backs no frame of its group"
    );

    // The policy catches up with the workspace it reads.
    late_policy.allowed.lock().unwrap().insert(public(1));
    drive(&mut [&mut founder, &mut member, &mut late], &mut now, 8);

    let entry = &late.service.peers[&workspace_id];
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
        late.service.existing_keys(workspace_id).is_none(),
        "no keys of its own: the group it holds is the founder's"
    );
}

/// A device that entered a workspace another device created never founds
/// its group, however long it waits: the creator founds it, and a second
/// genesis would split the workspace's keys.
#[test]
fn a_device_that_entered_a_workspace_never_founds_its_group() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1]);
    let dir = tempfile::tempdir().unwrap();
    let mut entered = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
    entered
        .founds
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut now = Instant::now();

    assert!(matches!(
        entered.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
    drive(&mut [&mut entered], &mut now, 30);
    assert!(!entered.service.peers[&workspace_id].peer.knows_group());
    assert!(matches!(
        entered.service.access(workspace_id, now),
        KeyAccess::Waiting
    ));
}

/// A record's group's frame reaches a device whose policy here cannot name the record's readers
/// yet: it is kept for the policy that moves, never refused. The device hears the group's scope
/// from these frames alone, so a frame dropped here is a record it could never open, however long
/// it waited.
#[test]
fn a_documents_group_frame_waits_for_the_policy_that_names_its_readers() {
    use std::sync::atomic::Ordering;

    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2]);
    // The record's readers are its workspace's, as the policy names them.
    *policy.document_readers.lock().unwrap() = [public(1), public(2)].into_iter().collect();
    let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut reader = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut now = Instant::now();
    assert!(
        ready_epoch(&create(&mut founder, workspace_id, now)).is_some(),
        "the founding device is handed the workspace's keys"
    );
    drive(&mut [&mut founder, &mut reader], &mut now, 1);
    let scope =
        neighbour_wire::KeyScope::document(workspace_id, "so.tilo.notes", "notes.index", None)
            .unwrap();
    let group = scope.group();

    // The founder makes the record's group, and its frame reaches the reader while the reader's
    // policy names no readers of the record yet.
    assert!(matches!(
        founder.service.access_scope(scope, now),
        KeyAccess::Waiting
    ));
    founder.service.found(group, now, 1_000);
    assert!(
        matches!(founder.service.access(group, now), KeyAccess::Ready(_)),
        "the founder is handed the record's group"
    );
    policy.unknown_documents.store(true, Ordering::SeqCst);
    drive(&mut [&mut founder, &mut reader], &mut now, 1);
    assert!(
        reader.service.waiting_scopes.iter().any(|waiting| {
            matches!(waiting, super::WaitingScope::Frame(scope, _) if scope.group() == group)
        }),
        "the frame waits for the policy that names the record's readers"
    );
    assert!(
        !reader.service.groups_seen.contains(&group),
        "the frame it could not judge founds no group of its own"
    );

    // The policy names the record's readers, but no pass judges what waits on its own: it is
    // the workspace settling that does.
    policy.unknown_documents.store(false, Ordering::SeqCst);
    drive(&mut [&mut reader], &mut now, 2);
    assert!(
        !reader.service.waiting_scopes.is_empty(),
        "what waits is judged by a settling workspace, never by a pass that comes on its own"
    );
    reader.service.workspace_settled(workspace_id);
    reader.service.pump(&reader.endpoint, now, 1_000);
    assert!(
        reader.service.waiting_scopes.is_empty(),
        "the frame the policy now backs leaves the wait"
    );
    assert!(
        reader.service.groups_seen.contains(&group),
        "the reader takes in the group the founder made"
    );
}

/// A record's group's frame whose readers the policy names, and the reading device is not one of
/// them, is refused: no message waits for a policy that would never back it.
#[test]
fn a_documents_group_frame_the_policy_reads_none_of_is_refused() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2]);
    *policy.document_readers.lock().unwrap() = [public(1), public(3)].into_iter().collect();
    let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let mut founder = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
    let mut stranger = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
    let mut now = Instant::now();
    assert!(
        ready_epoch(&create(&mut founder, workspace_id, now)).is_some(),
        "the founding device is handed the workspace's keys"
    );
    drive(&mut [&mut founder, &mut stranger], &mut now, 1);
    let scope =
        neighbour_wire::KeyScope::document(workspace_id, "so.tilo.notes", "notes.index", None)
            .unwrap();
    let group = scope.group();
    assert!(matches!(
        founder.service.access_scope(scope, now),
        KeyAccess::Waiting
    ));
    founder.service.found(group, now, 1_000);
    assert!(
        matches!(founder.service.access(group, now), KeyAccess::Ready(_)),
        "the founder is handed the record's group"
    );
    // The frame reaches the stranger before its policy can name the record's readers, and waits.
    policy.unknown_documents.store(true, Ordering::SeqCst);
    drive(&mut [&mut founder, &mut stranger], &mut now, 1);
    assert!(
        !stranger.service.waiting_scopes.is_empty(),
        "the frame waits while the record's readers are not named"
    );
    // Named, the readers do not include the stranger: the workspace settles and the frame is
    // refused, not kept for a policy that would never back it.
    policy.unknown_documents.store(false, Ordering::SeqCst);
    stranger.service.workspace_settled(workspace_id);
    stranger.service.pump(&stranger.endpoint, now, 1_000);
    assert!(
        stranger.service.waiting_scopes.is_empty(),
        "a frame the policy refuses does not wait"
    );
    assert!(
        !stranger.service.groups_seen.contains(&group),
        "the stranger takes in nothing of a record it does not read"
    );
}
