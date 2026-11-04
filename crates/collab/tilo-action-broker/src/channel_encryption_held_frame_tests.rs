// A device's policy reads its workspace as the governance reached it, which
// is not always before the channel governance does: a frame from a device the
// workspace has just admitted is judged again once the policy names it,
// rather than refused for good.

/// The founder's genesis reaches a device whose policy does not name the
/// founder yet, and its group is the founder's once the policy does: a
/// refused frame is kept for the policy that moves, so the two devices end in
/// one group rather than in two of their own.
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
    assert!(
        late.service.existing_keys(workspace_id, channel).is_none(),
        "a policy that does not name the founder backs no frame of its group"
    );

    // The policy catches up with the workspace it reads.
    late_policy.allowed.lock().unwrap().insert(public(1));
    run(&mut [&mut founder, &mut member, &mut late], &mut now, 8);

    let artifact = collab_types::document_artifact_id("so.tilo.notes", "workspace", "page");
    let founder_keys = founder
        .service
        .existing_keys(workspace_id, channel)
        .expect("the founder keeps its channel's keys");
    let (sealed, ciphertext) = founder_keys
        .seal_content(
            artifact,
            ContentRetention::Checkpoint,
            b"what the founder wrote",
        )
        .expect("the founder seals under its group");
    let keys = late
        .service
        .existing_keys(workspace_id, channel)
        .expect("the group the policy names admits the late device");
    let opened = keys
        .open_content(artifact, &sealed, &ciphertext)
        .expect("the late device reads the founder's group");
    assert_eq!(
        opened.as_slice(),
        b"what the founder wrote",
        "the late device holds the founder's group, not one it founded on its own"
    );
}
