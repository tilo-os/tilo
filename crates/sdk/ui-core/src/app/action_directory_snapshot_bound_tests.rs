fn capture_bound_directory(fixture: &mut ActionFixture) -> String {
    let resources = fixture.app.publication_resources();
    assert!(!resources.is_empty());
    let descriptors = tilo_actions::door::survey(&resources);
    fixture
        .app
        .capture_action_directory_snapshot(&resources, &descriptors);
    fixture.app.action_directory_revision()
}

#[test]
fn directory_snapshot_bound_projection_expires_or_revises_on_owner_changes() {
    // Case `directory_snapshot_bound_route_permission_and_closed_owner_expire_projection`.
    {
        let mut fixture = ActionFixture::broker();
        let revision = capture_bound_directory(&mut fixture);
        assert_eq!(capture_bound_directory(&mut fixture), revision);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found(_)
        ));
        fixture
            .app
            .actions
            .bound_broker_route_for_test
            .as_mut()
            .unwrap()
            .epoch = uuid::Uuid::new_v4();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert!(fixture.app.actions.action_directory_snapshot.is_none());
        let revision = capture_bound_directory(&mut fixture);
        *fixture.permissions.borrow_mut() =
            crate::permissions::PermissionContext::stranger("fixture-user").at_unix_time(100);
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert!(fixture.app.actions.action_directory_snapshot.is_none());
        *fixture.permissions.borrow_mut() = crate::permissions::PermissionContext::permissive();
        let revision = capture_bound_directory(&mut fixture);
        fixture.app.close_components();
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert!(fixture.app.actions.action_directory_snapshot.is_none());
    }

    // Case `directory_snapshot_bound_document_mutation_and_remounted_owner_change_revision`.
    {
        let mut fixture = ActionFixture::broker();
        let revision = capture_bound_directory(&mut fixture);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap();
        let target = fixture.target(0, "apply");
        fixture
            .app
            .invoke_bound_target(
                &target,
                &ResourceInvocation::new(&target.address.address, "fixture-user").confirmed(true),
            )
            .unwrap();
        assert_ne!(fixture.app.action_directory_revision(), revision);
        let revision = capture_bound_directory(&mut fixture);
        fixture.app.close_components();
        let mut replacement = ActionFixture::broker();
        let root = std::mem::replace(
            &mut replacement.app.root_widget,
            Box::new(crate::widget::Text::new("retired")),
        );
        fixture.app.root_widget = root;
        fixture.app.actions.bound_broker_route_for_test =
            replacement.app.actions.bound_broker_route_for_test.clone();
        assert_ne!(fixture.app.action_directory_revision(), revision);
        assert!(fixture.app.actions.action_directory_snapshot.is_none());
        let next = capture_bound_directory(&mut fixture);
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&next, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::NotFound
        ));
    }
}

/// Resolving with no revision reads the directory as it is now: an agent
/// that names an action it already knows reaches it after its own edit moved
/// the revision, without reading the revision first. A named revision that
/// moved is still refused.
#[test]
fn directory_resolution_without_a_revision_reads_the_directory_as_it_is_now() {
    let mut fixture = ActionFixture::broker();
    let before = capture_bound_directory(&mut fixture);
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    let target = fixture.target(0, "apply");
    fixture
        .app
        .invoke_bound_target(
            &target,
            &ResourceInvocation::new(&target.address.address, "fixture-user").confirmed(true),
        )
        .unwrap();
    let now = capture_bound_directory(&mut fixture);
    assert_ne!(now, before, "the edit moved the revision");
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&before, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::StaleRevision
    ));
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory("", &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found(_)
    ));
}
