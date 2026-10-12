// Real layout, retained view replacement and socket-backed publication ordering.
// This fixture grants no native caller authority.
use super::*;

struct ResponsiveNav(crate::widget::Element);
crate::testing::test_app_widget!(ResponsiveNav, "Responsive navigation");
impl Widget for ResponsiveNav {
    crate::testing::delegate_widget_methods!(
        0: sync, build_layout, apply_layout, draw, handle_event, get_rect
    );

    fn name(&self) -> &str {
        "ResponsiveNav"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.0.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.0.as_mut()]
    }
}

fn responsive_nav_factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    let root = crate::widget::ModelView::from_document(
        |doc, _| doc.get(ROOT, "setup").unwrap().is_some(),
        |setup| {
            let mut nav = crate::widget::NavShell::new()
                .overview(Box::new(crate::widget::Text::new("Conversations")))
                .overview_header(
                    "Conversations",
                    vec![Box::new(
                        crate::widget::Button::new("account-setup", "Accounts").on_view(|| {}),
                    )],
                )
                .content(Box::new(crate::widget::Text::new(if *setup {
                    "Account setup"
                } else {
                    "Conversation"
                })));
            nav.show(crate::widget::NavPane::Content);
            Box::new(nav)
        },
    );
    Ok(AppComponent::new(
        context,
        Box::new(ResponsiveNav(Box::new(root))),
        tilo_actions::ActionRegistry::new(),
    ))
}

#[test]
fn responsive_nav_publication_tracks_layout_model_and_revocation() {
    let mut definition = crate::application_test_support::definition(responsive_nav_factory);
    definition.room_record = true;
    let temp = tempfile::tempdir().unwrap();
    let room = uuid::Uuid::new_v4().to_string();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners.open(definition.app_id, &room).unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "fixture-device",
            &room,
        )
        .unwrap(),
    );
    let context = HostContext::prepare(
        &definition,
        HostContextInputs {
            records,
            views,
            device_id: "fixture-device".into(),
            instance_id: 92,
            permissions: crate::application_test_support::runner_permissions(),
            author: crate::permissions::UserId("probe-author".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut app =
        App::from_component(component, crate::application_test_support::runner_inputs()).unwrap();
    let owner = app.root_widget.callback_binding().unwrap();
    let original = owner
        .records
        .read(&owner.checkout.id, |doc| doc.clone().save())
        .unwrap();
    app.collaboration.manager = CollaborationManager::native_initial_named_fixture_for_test(
        owner.broker_route().unwrap(),
        original,
    );
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "actual bound history never applied"
        );
        std::thread::yield_now();
    }
    // Startup refresh discovers resources before the first layout exists.
    let _ = app.collect_resources();
    app.init_layout();
    for (width, scale, narrow) in [
        (1400, 1.0, false),
        (480, 1.0, true),
        (960, 2.0, true),
        (1400, 1.0, false),
        (480, 1.0, true),
    ] {
        let root = app
            .layout
            .root_node
            .expect("hosted navigation layout exists");
        run_layout_pass(&mut app, root, width, 700, scale, false, None).unwrap();
        let coverage = app.control_action_coverage();
        assert!(
            coverage
                .iter()
                .all(|entry| entry.status != ControlBindingStatus::StaleAddress),
            "current navigation controls need current resource doors at width {width}: {coverage:?}"
        );
        if narrow {
            assert!(
                coverage
                    .iter()
                    .any(|entry| entry.binding.control_id == "nav-shell-back"),
                "the narrow layout must actually exercise its back control"
            );
        }
        app.publish_action_surface();
        assert!(
            app.actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty()),
            "the real native publication must remain populated at width {width}"
        );
        assert!(app.collaboration.manager.is_connected());
        assert!(app.published_action_history_ready());
        if narrow {
            let back = tilo_actions::ActionAddress::new(
                app.actions
                    .bound_publication
                    .alias(&owner, "tilo.control:nav-shell-back")
                    .unwrap(),
                "invoke",
            );
            assert!(
                app.actions
                    .last_published_actions
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|descriptor| {
                        descriptor.address == back
                            && descriptor.contract.availability.is_available()
                    }),
                "the exact native Back door must be offered, not merely another action"
            );
        }
    }
    owner
        .with_document(&mut app.state.doc, |doc, _| {
            doc.put(ROOT, "setup", true).unwrap();
        })
        .unwrap();
    app.sync_model(true);
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    run_layout_pass(&mut app, root, 480, 700, 1.0, false, None).unwrap();
    let coverage = app.control_action_coverage();
    assert!(
        coverage
            .iter()
            .all(|entry| entry.status != ControlBindingStatus::StaleAddress),
        "a replaced navigation tree needs current resource doors: {coverage:?}"
    );
    app.publish_action_surface();
    assert!(
        app.actions
            .last_published_actions
            .as_ref()
            .is_some_and(|actions| !actions.is_empty()),
        "replacing the navigation tree must not withdraw the native publication"
    );
    *owner.permissions.borrow_mut() =
        crate::permissions::PermissionContext::stranger("probe-author").at_unix_time(100);
    app.publish_action_surface();
    assert!(
        app.actions
            .last_published_actions
            .as_ref()
            .is_some_and(Vec::is_empty),
        "revocation must withdraw the previously offered native control doors"
    );
    app.close_components();
}
