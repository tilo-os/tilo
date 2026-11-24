// Included in `control_fixture_tests` (the Fixture widget and its managed root live there).
fn deferred_fixture() -> (
    Fixture,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tilo_actions::*;
    let calls = Arc::new(AtomicUsize::new(0));
    let domain = Arc::new(AtomicUsize::new(0));
    let build_calls = calls.clone();
    let draft_domain = domain.clone();
    let mut spec = ActionSpec::deferred_operation(
        "run",
        "Run fixture",
        OperationKind::Effect,
        "fixture.control",
        move || {
            EncodedAction::encode_operation(
                "fixture.control",
                &build_calls.fetch_add(1, Ordering::SeqCst),
            )
            .unwrap()
        },
    )
    .with_continuation_identity(move |query| {
        matches!(query, ContinuationQuery::Draft).then(|| ContinuationEvidence::Draft {
            resolution_domain: ContinuationKey::new(
                "creation-domain",
                [draft_domain.load(Ordering::SeqCst).to_le_bytes()],
            ),
            handler: ContinuationKey::new("creation-handler", [b"v1"]),
        })
    });
    spec.type_mut().contract.confirmation = ConfirmationPolicy::Confirm;
    let resource = ResourceSpec::new("ordinary:fixture", "fixture", "Fixture").with_action(spec);
    let binding = ActionBinding::unit(&resource, &resource.actions[0]).unwrap();
    let widget = Fixture {
        child: Box::new(
            crate::widget::Button::new("ordinary-emitter", "Emit request").on_action(binding),
        ),
        resources: Rc::new(RefCell::new(vec![resource])),
        log: Rc::new(RefCell::new(vec![])),
        output: Rc::new(RefCell::new(None)),
    };
    (widget, calls, domain)
}

tilo_test_env::scenario! {
    fn deferred_actions_mint_only_at_their_handler;

    phase a_press_that_asks_builds_nothing_until_the_person_allows_it;
    // The press is refused until the person answers: its handler builds nothing while it waits,
    // and once only when the call comes back with their yes.
    {
        use std::sync::atomic::Ordering;
        let (widget, calls, _) = deferred_fixture();
        let log = widget.log.clone();
        let mut driver = crate::headless::Driver::from_managed_root_with_controlled_inputs(
            State::new(),
            managed(Box::new(widget)),
            (900, 650),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::MemberId::person("fixture"),
        )
        .unwrap();
        for _ in 0..3 {
            driver.relayout();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0, "construction is inert");
        driver
            .perform_semantic_named("ordinary-emitter", None, &crate::a11y::Action::Activate)
            .unwrap();
        assert!(driver.asking().is_some(), "the press waits for the person");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing is built while it waits");
        assert!(driver.confirm_asked().is_some(), "the person allows it");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "it builds once");
        assert_eq!(&*log.borrow(), &[serde_json::json!(0)]);
    }

    phase a_declined_press_builds_nothing;
    {
        use std::sync::atomic::Ordering;
        let (widget, calls, _) = deferred_fixture();
        let log = widget.log.clone();
        let mut driver = crate::headless::Driver::from_managed_root_with_controlled_inputs(
            State::new(),
            managed(Box::new(widget)),
            (900, 650),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::MemberId::person("fixture"),
        )
        .unwrap();
        driver
            .perform_semantic_named("ordinary-emitter", None, &crate::a11y::Action::Activate)
            .unwrap();
        assert!(driver.decline_asked().is_some(), "the person declines");
        assert!(driver.asking().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(log.borrow().is_empty());
    }

    phase deferred_semantic_no_confirmation_reaches_final_owner_once;
    {
        use std::sync::atomic::Ordering;
        let (widget, calls, _) = deferred_fixture();
        widget.resources.borrow_mut()[0].actions[0]
            .type_mut()
            .contract
            .confirmation = tilo_actions::ConfirmationPolicy::None;
        let log = widget.log.clone();
        let mut app =
            App::from_managed_root_with_controlled_inputs(State::new(), managed(Box::new(widget)))
                .unwrap();
        layout(&mut app);
        app.refresh_semantic_tree();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            app.invoke_semantic_action(
                &crate::a11y_tree::SemanticId("r:ordinary:fixture".into()),
                "run",
                None
            )
            .unwrap()
        );
        app.controlled_local_idle_for_test();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no-confirmation semantic activation must reach the ordinary owner api"
        );
        assert_eq!(&*log.borrow(), &[serde_json::json!(0)]);
    }
}
fn check_deferred_bound_view_actor(foreign: bool, typed_empty: bool) {
    use std::sync::atomic::Ordering;
    let (widget, calls, _) = deferred_fixture();
    {
        let mut resources = widget.resources.borrow_mut();
        let spec = &mut resources[0].actions[0];
        spec.type_mut().kind = tilo_actions::OperationKind::View;
        spec.type_mut().contract.confirmation = tilo_actions::ConfirmationPolicy::None;
    }
    let log = widget.log.clone();
    let (root, guard) = owned(widget);
    let binding = &guard.binding;
    let target = crate::bound_dispatch::BoundActionTarget {
        input: crate::bound_dispatch::Input::Discrete,
        binding: binding.clone(),
        address: crate::bound_dispatch::BoundActionAddress {
            instance_id: binding.instance_id,
            address: tilo_actions::ActionAddress::new("ordinary:fixture", "run"),
        },
    };
    let local_actor = tilo_actions::Actor::Member(binding.author.clone());
    let foreign_actor = tilo_actions::Actor::Member(tilo_actions::MemberId::person("foreign"));
    let actor = if foreign {
        &foreign_actor
    } else {
        &local_actor
    };
    let mut app =
        App::from_managed_root_with_controlled_inputs(State::new(), guard.managed(root)).unwrap();
    layout(&mut app);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "mounted discovery is inert"
    );
    let json_empty = format!("{}{{}}", tilo_actions::ARGUMENTS_JSON_PREFIX);
    let result = app.invoke_bound_target_with_broker(
        &target,
        &ResourceInvocation::new(&target.address.address, actor)
            .argument(typed_empty.then_some(json_empty.as_str())),
        None,
    );
    if foreign {
        assert!(
            matches!(result, Err(tilo_actions::ApiError::Unavailable { ref code, .. }) if code == "private_view")
        );
        assert!(
            log.borrow().is_empty(),
            "the private View handler must not run"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the refused foreign View must not invoke its Deferred builder"
        );
    } else {
        assert!(matches!(
            result,
            Ok(crate::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one local owner mint");
        assert_eq!(
            &*log.borrow(),
            &[serde_json::json!(0)],
            "one local View handler"
        );
    }
}

tilo_test_env::scenario! {
    fn deferred_bound_view_refuses_foreign_actors_and_mints_once_for_its_owner;

    phase deferred_bound_view_foreign_legacy_refuses_before_mint;
    {
        check_deferred_bound_view_actor(true, false);
    }

    phase deferred_bound_view_foreign_typed_empty_refuses_before_mint;
    {
        check_deferred_bound_view_actor(true, true);
    }

    phase deferred_bound_view_local_legacy_mints_once;
    {
        check_deferred_bound_view_actor(false, false);
    }

    phase deferred_bound_view_local_typed_empty_mints_once;
    {
        check_deferred_bound_view_actor(false, true);
    }
}
