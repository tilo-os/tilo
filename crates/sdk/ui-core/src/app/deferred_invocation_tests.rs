// Included in the actual native/FrameHarness ordinary-confirmation fixtures.
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
    spec.contract.confirmation = ConfirmationPolicy::Confirm;
    let resource = Resource::new("ordinary:fixture", "fixture", "Fixture").with_action(spec);
    let binding = ActionBinding::unit(&resource, &resource.actions[0]).unwrap();
    let widget = Fixture {
        child: Box::new(
            crate::widget::Button::new("ordinary-emitter", "Emit request").on_action(binding),
        ),
        resources: Rc::new(RefCell::new(vec![resource])),
        ledger: Rc::new(RefCell::new(vec![])),
        output: Rc::new(RefCell::new(None)),
    };
    (widget, calls, domain)
}

#[test]
fn deferred_native_and_driver_confirmation_mint_once_only_at_handler() {
    use std::sync::atomic::Ordering;
    for native in [true, false] {
        let (widget, calls, _) = deferred_fixture();
        let ledger = widget.ledger.clone();
        let mut host = Host::new(native, managed(Box::new(widget)));
        for _ in 0..3 {
            host.frame();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "construction/model/a11y must be inert"
        );
        for expected in 0..2 {
            host.activate("Emit request");
            for _ in 0..3 {
                host.frame();
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                expected,
                "control and repeated confirmation preparation must be inert"
            );
            host.value("CONFIRM");
            host.activate("Confirm");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                expected + 1,
                "one admitted handler builds once"
            );
            assert_eq!(ledger.borrow()[expected], serde_json::json!(expected));
        }
    }
}
#[test]
fn deferred_changed_draft_and_current_permission_refuse_without_building() {
    use std::sync::atomic::Ordering;
    for native in [true, false] {
        for change in ["draft", "permission"] {
            let (widget, calls, domain) = deferred_fixture();
            let ledger = widget.ledger.clone();
            let mut host = Host::new(native, managed(Box::new(widget)));
            host.activate("Emit request");
            if change == "draft" {
                domain.store(1, Ordering::SeqCst);
            } else {
                host.revoke();
            }
            host.value("CONFIRM");
            host.activate("Confirm");
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{native}:{change}");
            assert!(ledger.borrow().is_empty());
        }
    }
}
#[test]
fn deferred_history_retains_draft_without_mint_and_checks_on_resume() {
    use std::sync::atomic::Ordering;
    for native in [true, false] {
        for changed in [false, true] {
            let (widget, calls, domain) = deferred_fixture();
            let ledger = widget.ledger.clone();
            let mut host = Host::new(native, managed(Box::new(widget)));
            host.activate("Emit request");
            host.value("CONFIRM");
            host.activate("History");
            if changed {
                domain.store(1, Ordering::SeqCst);
            }
            host.activate("History");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            host.value("CONFIRM");
            host.activate("Confirm");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(!changed),
                "{native}:{changed}"
            );
            assert_eq!(ledger.borrow().len(), usize::from(!changed));
        }
    }
}
#[test]
fn deferred_actual_component_close_refuses_before_mint() {
    use std::sync::atomic::Ordering;
    for native in [true, false] {
        let (widget, calls, _) = deferred_fixture();
        let ledger = widget.ledger.clone();
        let (root, guard) = owned(widget, true);
        let mut host = Host::new(native, guard.managed(root));
        host.activate("Emit request");
        guard.close();
        host.value("CONFIRM");
        host.activate("Confirm");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(ledger.borrow().is_empty());
    }
}

#[test]
fn deferred_semantic_tree_publishes_inert_address_and_uses_confirmation_door() {
    use std::sync::atomic::Ordering;
    let (widget, calls, _) = deferred_fixture();
    let ledger = widget.ledger.clone();
    let mut host = Host::new(true, managed(Box::new(widget)));
    let Host::Native(app) = &mut host else {
        unreachable!()
    };
    let id = crate::a11y_tree::SemanticId("r:ordinary:fixture".into());
    for _ in 0..3 {
        app.refresh_semantic_tree();
        assert!(
            matches!(&app.semantic_tree().action(&id, "run").unwrap().kind,
            crate::a11y_tree::SemanticActionKind::AppDeferred { address }
                if address == &tilo_actions::ActionAddress::new("ordinary:fixture", "run"))
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(app.invoke_semantic_action(&id, "run", None).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    layout(app);
    host.value("CONFIRM");
    host.activate("Confirm");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(&*ledger.borrow(), &[serde_json::json!(0)]);
}

#[test]
fn deferred_semantic_no_confirmation_reaches_final_owner_once() {
    use std::sync::atomic::Ordering;
    let (widget, calls, _) = deferred_fixture();
    widget.resources.borrow_mut()[0].actions[0]
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::None;
    let ledger = widget.ledger.clone();
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
        "no-confirmation semantic activation must reach the ordinary owner door"
    );
    assert_eq!(&*ledger.borrow(), &[serde_json::json!(0)]);
}
fn check_deferred_bound_view_actor(foreign: bool, typed_empty: bool) {
    use std::sync::atomic::Ordering;
    let (widget, calls, _) = deferred_fixture();
    {
        let mut resources = widget.resources.borrow_mut();
        let spec = &mut resources[0].actions[0];
        spec.kind = tilo_actions::OperationKind::View;
        spec.contract.confirmation = tilo_actions::ConfirmationPolicy::None;
    }
    let ledger = widget.ledger.clone();
    let (root, guard) = owned(widget, true);
    let OwnerGuard::Component { binding, .. } = &guard else {
        unreachable!()
    };
    let target = crate::bound_dispatch::BoundActionTarget {
        physical_text: false,
        binding: binding.clone(),
        address: crate::bound_dispatch::BoundActionAddress {
            instance_id: binding.instance_id,
            address: tilo_actions::ActionAddress::new("ordinary:fixture", "run"),
        },
    };
    let local_actor = binding.author.0.clone();
    let foreign_actor = format!("{local_actor}:foreign");
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
            matches!(result, Err(tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "private_view")
        );
        assert!(
            ledger.borrow().is_empty(),
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
            &*ledger.borrow(),
            &[serde_json::json!(0)],
            "one local View handler"
        );
    }
}

#[test]
fn deferred_bound_view_foreign_legacy_refuses_before_mint() {
    check_deferred_bound_view_actor(true, false);
}
#[test]
fn deferred_bound_view_foreign_typed_empty_refuses_before_mint() {
    check_deferred_bound_view_actor(true, true);
}
#[test]
fn deferred_bound_view_local_legacy_mints_once() {
    check_deferred_bound_view_actor(false, false);
}
#[test]
fn deferred_bound_view_local_typed_empty_mints_once() {
    check_deferred_bound_view_actor(false, true);
}
