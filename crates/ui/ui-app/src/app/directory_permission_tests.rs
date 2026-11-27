// Actual bound owners and cached directory resolution, not a DTO equality test.
fn republish_directory(fixture: &mut ActionFixture) -> (String, Vec<tilo_actions::ResourceSpec>) {
    let resources = fixture.app.publication_resources();
    let descriptors = tilo_actions::api::survey(&resources);
    fixture
        .app
        .capture_action_directory_snapshot(&resources, &descriptors);
    (fixture.app.action_directory_revision(), resources)
}

tilo_test_env::scenario! {
    fn directory_permissions_invalidate_the_revision_resolved_under_them;

    phase directory_permission_verdict_change_invalidates_revision;
    {
        let mut fixture = ActionFixture::broker();
        let revision = capture_bound_directory(&mut fixture);
        // The device decides the person's actions anew: they are a viewer now.
        fixture
            .device
            .set_role(&ActionFixture::user(), tilo_ui_base::permissions::Role::Viewer);
        crate::application_test_support::answer_read(&fixture.permissions, &fixture.device);
        assert_ne!(fixture.app.action_directory_revision(), revision);
    }

    phase directory_revoked_access_cannot_republish_old_rows;
    {
        let mut fixture = ActionFixture::broker();
        let old = capture_bound_directory(&mut fixture);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        let address = tilo_actions::ActionAddress::new(&alias, "follow-up");
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&old, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
        // The device takes the person's access back, and the window hears it before any walk.
        fixture.device.remove(&ActionFixture::user());
        crate::application_test_support::answer_read(&fixture.permissions, &fixture.device);
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&old, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::StaleRevision
        ));
        assert!(fixture.app.actions.action_directory_snapshot.is_none());
        assert_ne!(fixture.app.action_directory_revision(), old);
        let (revision, resources) = republish_directory(&mut fixture);
        assert!(
            tilo_actions::api::resolve(&resources, &address).is_none(),
            "normal publication reused rows filtered under the revoked access"
        );
        assert!(!matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
    }

    phase directory_restored_access_republishes_the_action;
    {
        let mut fixture = ActionFixture::broker();
        fixture.set_role(None);
        let (old, resources) = republish_directory(&mut fixture);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        let address = tilo_actions::ActionAddress::new(&alias, "follow-up");
        assert!(tilo_actions::api::resolve(&resources, &address).is_none());
        fixture.set_role(Some(tilo_ui_base::permissions::Role::Owner));
        assert_ne!(fixture.app.action_directory_revision(), old);
        let (revision, resources) = republish_directory(&mut fixture);
        assert!(
            tilo_actions::api::resolve(&resources, &address).is_some(),
            "restored access must regenerate filtered rows, not permanently hide the action"
        );
        assert!(matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
    }

    phase hosted_directory_unknown_owner_domain_never_restores_revision;
    {
        let mut fixture = ActionFixture::broker();
        let old = capture_bound_directory(&mut fixture);
        let before = crate::bound_dispatch::directory_observation(
            fixture.app.root_widget.as_ref(),
            &fixture.app.state.doc,
        )
        .unwrap();
        assert!(
            before.iter().any(|owner| !owner.supports_model_refresh()),
            "the legacy fixture supplies no complete domain promise"
        );
        fixture.app.pump_hosted_model();
        assert_ne!(fixture.app.action_directory_revision(), old);
    }

    phase hosted_directory_revocation_and_owner_close_never_restore_old_revision;
    {
        for revoke in [true, false] {
            let mut fixture = ActionFixture::broker();
            let old = capture_bound_directory(&mut fixture);
            let alias = fixture
                .app
                .actions
                .bound_publication
                .alias(&fixture.bindings[1], "bound:probe")
                .unwrap();
            if revoke {
                fixture.device.remove(&ActionFixture::user());
                crate::application_test_support::answer_read(&fixture.permissions, &fixture.device);
            } else {
                fixture.app.close_components();
            }
            fixture.app.pump_hosted_model();
            assert_ne!(fixture.app.action_directory_revision(), old);
            assert!(!matches!(
                fixture
                    .app
                    .resolve_action_directory(&old, &alias, "follow-up"),
                runtime_collab::ActionDirectoryResolution::Found { .. }
            ));
        }
    }
}
