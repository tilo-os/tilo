use super::*;
use crate::bound_dispatch::legacy_document_tests::{Fixture, SetValue, address, registry};
use crate::widget::Column;
use automerge::{ROOT, ReadDoc, transaction::Transactable};

struct DocumentRouteTransport;
impl crate::collaboration::PeerTransport for DocumentRouteTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        vec![]
    }
    fn join_route(&mut self, _: crate::collaboration::RouteSpec) -> Result<(), String> {
        Ok(())
    }
    fn send_to_route(
        &mut self,
        _: crate::collaboration::RouteSpec,
        _: CollabRequest,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn native_legacy_route_exact_owner_publication_and_repeat_idle_do_not_touch_siblings() {
    let room = uuid::Uuid::new_v4();
    let mut a = Fixture::in_room(510, &room.to_string());
    let mut b = Fixture::in_room(511, &room.to_string());
    let root = Column::new()
        .push(Box::new(a.take_tile()))
        .push(Box::new(b.take_tile()));
    let mut state = State::new();
    state.doc.put(ROOT, "outer-only", true).unwrap();
    state.commit_action(None, None);
    let mut app = App::new_with_controlled_inputs(state, Box::new(root));
    app.actions.action_registry = registry();
    app.init_layout();
    app.focused_node = app.layout.node_map.keys().copied().find(|id| {
        app.bindings
            .legacy_for(*id)
            .is_some_and(|owner| owner.same_owner(&b.target.owner))
    });
    let original_a = a.snapshot();
    let original_b = b.snapshot();
    let outer = app.state.doc.save();
    app.collaboration.manager = CollaborationManager::with_peer_transport(DocumentRouteTransport);
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::app("controlled-shell", room),
        epoch: uuid::Uuid::new_v4(),
    });
    app.publish_owned_documents();
    assert_eq!(app.document_routes.sent.len(), 2);
    app.document_routes.sent.clear();
    assert!(
        app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user"))
            .is_err(),
        "bare duplicate refuses"
    );
    a.probe.offer();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(a.probe.acks.get(), 1);
    assert_eq!(b.probe.acks.get(), 0);
    assert_eq!(app.state.doc.save(), outer);
    assert_eq!(b.snapshot(), original_b);
    assert_eq!(a.snapshot().1, original_a.1 + 1);
    assert_eq!(a.snapshot().2, Some(original_a.2.unwrap() + 1));
    app.publish_owned_documents();
    assert_eq!(app.document_routes.sent.len(), 1);
    assert_eq!(
        app.document_routes.sent[0].0.record.as_deref(),
        Some(a.checkout.id.as_str())
    );
    assert!(matches!(
        app.document_routes.sent[0].1,
        CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Incremental(_),
            ..
        }
    ));
    assert!(!app.controlled_local_idle_for_test());
    app.publish_owned_documents();
    assert_eq!(app.document_routes.sent.len(), 1);
    assert_eq!(a.probe.acks.get(), 1);
    a.store.try_flush().unwrap();
    drop(app);
    let (temp, checkout) = a.into_closed_profile();
    let mut reopened_owners = crate::record::RecordOwners::new(temp.path().join("profile"));
    let reopened = reopened_owners
        .open("so.tilo.controlled", &room.to_string())
        .unwrap();
    reopened.try_open_record(&checkout).unwrap();
    assert_eq!(
        reopened
            .read_checked(&checkout.id, |doc| doc
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .as_i64())
            .unwrap(),
        Some(2)
    );
}
#[test]
fn native_legacy_route_old_changed_and_noop_actionhost_edges_have_one_boundary_each() {
    for noop in [false, true] {
        let mut f = Fixture::new(if noop { 522 } else { 521 });
        let probe = f.probe.clone();
        let before = f.snapshot();
        f.store
            .with_checked(&f.checkout.id, |doc| {
                probe
                    .actions
                    .borrow_mut()
                    .try_dispatch(
                        doc,
                        "controlled.legacy",
                        &SetValue {
                            value: if noop { 1 } else { 3 },
                            fail: false,
                            remote: false,
                        },
                    )
                    .unwrap()
            })
            .unwrap();
        let root = f.take_tile();
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.actions.action_registry = registry();
        app.init_layout();
        let committed = f
            .store
            .read_checked(&f.checkout.id, |doc| doc.base_heads())
            .unwrap();
        let status_node = app
            .layout
            .node_map
            .keys()
            .copied()
            .find(|id| app.bindings.legacy_for(*id).is_some())
            .unwrap();
        let before_status = f.snapshot();
        for _ in 0..3 {
            assert!(
                app.bindings
                    .legacy_status(status_node, &app.collaboration.local_author)
                    .is_some()
            );
        }
        assert_eq!(
            f.snapshot(),
            before_status,
            "status does not record the changed edge or increment generation"
        );
        probe.offer();
        assert!(app.controlled_local_idle_for_test());
        if noop {
            assert_eq!(probe.acks.get(), 1);
            assert_eq!(f.snapshot().1, before.1 + 1);
        } else {
            assert_eq!(
                probe.acks.get(),
                0,
                "unrecorded changed edge refuses resource action"
            );
            assert_eq!(
                f.store
                    .read_checked(&f.checkout.id, |doc| doc.base_heads())
                    .unwrap(),
                committed,
                "ordinary draining records without writing"
            );
            assert_eq!(f.snapshot().1, before.1 + 1);
            // The refused press is said over the window, never dropped
            // unseen; the person dismisses the notice and presses again.
            assert!(
                app.action_palette()
                    .is_some_and(|palette| palette.refusal().is_some())
            );
            app.close_action_palette();
            probe.offer();
            assert!(app.controlled_local_idle_for_test());
            assert_eq!(probe.acks.get(), 1);
            assert_eq!(f.snapshot().1, before.1 + 2);
        }
        let settled = f.snapshot();
        assert!(!app.controlled_local_idle_for_test());
        assert_eq!(f.snapshot(), settled);
        assert!(probe.actions.borrow_mut().take_committed().is_none());
    }
}
struct RejectTransport(std::rc::Rc<std::cell::Cell<usize>>);
impl tilo_actions::ActionTransport for RejectTransport {
    fn replicate(&mut self, _: &tilo_actions::ActionEnvelope) {
        self.0.set(self.0.get() + 1);
    }
    fn submit_to_host(&mut self, _: &tilo_actions::ActionEnvelope) -> tilo_actions::HostDecision {
        self.0.set(self.0.get() + 1);
        tilo_actions::HostDecision::AcceptedByHost
    }
}
#[test]
fn native_legacy_route_replacement_transport_refuses_before_calls_and_unique_bare_owner_works() {
    let mut f = Fixture::new(530);
    let before = f.snapshot();
    let root = f.take_tile();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.actions.action_registry = registry();
    app.init_layout();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    app.set_action_transport(Box::new(RejectTransport(calls.clone())));
    assert!(
        app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user"))
            .is_err()
    );
    assert_eq!(calls.get(), 0);
    assert_eq!(f.snapshot(), before);
    assert_eq!(f.probe.acks.get(), 0);
    // A new host with its actual default transport qualifies, not a type guess.
    let mut fresh = Fixture::new(531);
    let root = fresh.take_tile();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.actions.action_registry = registry();
    app.init_layout();
    app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user"))
        .unwrap();
    assert_eq!(fresh.probe.acks.get(), 1);
    assert!(
        app.document_routes.sent.is_empty(),
        "no real route witness, no publication claim"
    );
}

static EFFECT_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "so.tilo.controlled",
        scopes: &[crate::history_mode::HistoryScopeDefinition {
            key: "document",
            title: "Controlled activity",
            presentation: crate::history_mode::HistoryPresentation::Domain {
                create: |_| Err("No historical external results".into()),
            },
            content_schema: None,
        }],
    };
#[test]
fn native_legacy_effect_capture_keeps_exact_accepted_owner_hidden_poll_cancel_and_view() {
    for cancel in [false, true] {
        let room = uuid::Uuid::new_v4();
        let mut a = Fixture::in_room(if cancel { 542 } else { 540 }, &room.to_string());
        let mut b = Fixture::in_room(if cancel { 543 } else { 541 }, &room.to_string());
        *a.probe.spec.borrow_mut() = tilo_actions::ActionSpec::operation(
            "set",
            "Controlled effect",
            tilo_actions::OperationKind::Effect,
            "controlled.effect",
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: true });
        *b.probe.spec.borrow_mut() = a.probe.spec.borrow().clone();
        if cancel {
            b.probe.spec.borrow_mut().type_mut().id = "other".into();
        }
        a.tile.register_document_history(&EFFECT_HISTORY).unwrap();
        let before_a = a.snapshot();
        let before_b = b.snapshot();
        let root = Column::new()
            .push(Box::new(a.take_tile()))
            .push(Box::new(b.take_tile()));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let outer = app.state.doc.save();
        if cancel {
            // Directed delivery requires an admitted document route before its
            // terminal can cross the publication gate. This peer proves queue
            // admission only; the local-control arm remains disconnected.
            app.collaboration.manager =
                CollaborationManager::with_peer_transport(DocumentRouteTransport);
            app.actions.bound_broker_route_for_test =
                Some(crate::collaboration::PrimaryRouteWitness {
                    route: crate::collaboration::RouteSpec::app("controlled-shell", room),
                    epoch: uuid::Uuid::new_v4(),
                });
            app.publish_owned_documents();
            assert_eq!(app.document_routes.sent.len(), 2);
            for record in [&a.checkout.id, &b.checkout.id] {
                let expected = crate::collaboration::RouteSpec::record(
                    "so.tilo.controlled",
                    room,
                    record.as_str(),
                );
                assert!(app.document_routes.sent.iter().any(|(route, request)| {
                    route == &expected && matches!(request, CollabRequest::Document { .. })
                }));
            }
            assert!(app.action_publication_failure.is_none());
            app.invoke_published_action(
                777,
                tilo_actions::InvocationId(uuid::Uuid::from_u128(777)),
                address().resource,
                address().action,
                None,
                "fixture-user",
                None,
            );
            assert_eq!(app.actions.directed_invocations.len(), 1);
        } else {
            a.probe.offer();
            assert!(app.controlled_local_idle_for_test());
            assert_eq!(app.actions.local_invocations.len(), 1);
        }
        assert_eq!(a.probe.starts.get(), 1);
        assert_eq!(b.probe.starts.get(), 0);
        assert!(a.probe.invocation.get().is_some());
        let open = app
            .build_a11y_tree_update(1.0)
            .nodes
            .into_iter()
            .find(|(_, n)| n.label() == Some("History"))
            .unwrap()
            .0;
        app.dispatch_a11y_action(open, &crate::a11y::Action::Activate);
        app.rebuild_structure();
        assert!(
            crate::bound_dispatch::legacy_document::resolve_legacy_invocation(
                app.root_widget.as_ref(),
                &a.target
            )
            .is_err(),
            "new work remains hidden"
        );
        assert!(!app.poll_directed_invocations());
        assert_eq!(a.probe.starts.get(), 1);
        assert!(a.probe.polls.get() > 0);
        assert_eq!(b.probe.polls.get(), 0);
        if cancel {
            app.request_invocation_cancellation(777);
            assert!(app.poll_directed_invocations());
            assert!(app.action_publication_failure.is_none());
            assert!(app.actions.directed_invocations.is_empty());
            assert_eq!(a.probe.invocation.get(), None);
        } else {
            a.probe.ready.set(true);
            assert!(app.poll_directed_invocations());
            assert!(app.actions.local_invocations.is_empty());
        }
        assert!(!app.poll_directed_invocations());
        assert_eq!(a.probe.starts.get(), 1);
        let after_a = a.snapshot();
        let after_b = b.snapshot();
        assert_eq!((&after_a.0, after_a.1), (&before_a.0, before_a.1));
        assert_eq!((&after_b.0, after_b.1), (&before_b.0, before_b.1));
        assert_eq!(app.state.doc.save(), outer);
    }
    let mut f = Fixture::new(544);
    *f.probe.spec.borrow_mut() = tilo_actions::ActionSpec::operation(
        "set",
        "Controlled view",
        tilo_actions::OperationKind::View,
        "controlled.effect",
        &(),
    )
    .unwrap();
    let root = f.take_tile();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    let before = f.snapshot();
    app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user"))
        .unwrap();
    assert_eq!(f.probe.starts.get(), 1);
    let after = f.snapshot();
    assert_eq!((&after.0, after.1), (&before.0, before.1));
    assert_eq!(
        after.2,
        before.2.map(|g| g + 1),
        "existing TileHost mutable effect delivery increments generation once"
    );
    assert!(app.actions.local_invocations.is_empty());
    assert!(!app.controlled_local_idle_for_test());
    assert_eq!(f.snapshot(), after);
}

#[test]
fn native_legacy_and_outer_colliding_controls_keep_their_captured_document() {
    let mut owner = Fixture::new(560);
    let outer_probe = crate::bound_dispatch::legacy_document_tests::Probe::new();
    let root = Column::new()
        .push(Box::new(owner.take_tile()))
        .push(Box::new(outer_probe.clone()));
    let mut state = State::new();
    state.doc.put(ROOT, "value", 10).unwrap();
    state.commit_action(None, None);
    let mut app = App::new_with_controlled_inputs(state, Box::new(root));
    app.actions.action_registry = registry();
    app.init_layout();
    let before = owner.snapshot();
    assert!(
        app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user"))
            .is_err()
    );
    outer_probe.offer();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(
        app.state
            .doc
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .as_i64(),
        Some(2)
    );
    assert_eq!(owner.snapshot(), before);
    assert_eq!(owner.probe.acks.get(), 0);
    let outer = app.state.doc.save();
    owner.probe.offer();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(owner.probe.acks.get(), 1);
    assert_eq!(app.state.doc.save(), outer);
    assert_eq!(owner.snapshot().1, before.1 + 1);
}

#[test]
fn native_legacy_effect_control_keeps_existing_confirmation_gate() {
    let mut f = Fixture::new(570);
    *f.probe.spec.borrow_mut() = tilo_actions::ActionSpec::operation(
        "set",
        "Confirm controlled effect",
        tilo_actions::OperationKind::Effect,
        "controlled.effect",
        &(),
    )
    .unwrap()
    .execution(tilo_actions::ExecutionMode::Background { cancellable: true });
    f.probe.spec.borrow_mut().type_mut().contract.confirmation =
        tilo_actions::ConfirmationPolicy::Confirm;
    let root = f.take_tile();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    let before = f.snapshot();
    f.probe.offer();
    assert!(app.controlled_local_idle_for_test());
    assert!(app.action_palette().is_some());
    assert_eq!(f.probe.starts.get(), 0);
    let gated = f.snapshot();
    assert_eq!(
        gated.0, before.0,
        "opening confirmation must not mutate durable document bytes"
    );
    assert_eq!(
        gated.1, before.1,
        "opening confirmation must not add an undo boundary"
    );
    assert!(
        gated
            .2
            .zip(before.2)
            .is_some_and(|(after, before)| after > before),
        "palette admission must revoke authority resolved before resource rediscovery"
    );
    app.invoke_resource_action(ResourceInvocation::new(&address(), "fixture-user").confirmed(true))
        .unwrap();
    assert_eq!(f.probe.starts.get(), 1);
    assert_eq!(app.actions.local_invocations.len(), 1);
    f.probe.ready.set(true);
    assert!(app.poll_directed_invocations());
    assert!(app.actions.local_invocations.is_empty());
}

#[test]
fn native_drain_keeps_independent_document_and_disposable_view_controls() {
    for case in ["closed", "unavailable", "busy"] {
        for failed_first in [false, true] {
            for view in [false, true] {
                let (a, b, root) =
                    crate::bound_dispatch::legacy_document_tests::independent_batch_fixture(
                        case,
                        failed_first,
                        view,
                    );
                let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
                app.actions.action_registry = registry();
                app.init_layout();
                let outer = app.state.doc.save();
                let before_b = b.snapshot();
                a.probe.offer();
                b.probe.offer();
                if case == "busy" {
                    b.store
                        .with_action_state(&b.checkout.id, |_, _, _| {
                            app.controlled_local_idle_for_test();
                        })
                        .unwrap();
                } else {
                    app.controlled_local_idle_for_test();
                }
                assert_eq!(
                    a.probe.acks.get(),
                    usize::from(!view),
                    "healthy A must dispatch: {case}, failed_first={failed_first}, view={view}"
                );
                assert_eq!(a.probe.starts.get(), usize::from(view));
                assert_eq!(b.probe.acks.get(), 0);
                assert_eq!(b.probe.starts.get(), 0);
                assert_eq!(b.probe.queue.borrow().len(), 1);
                assert_eq!(b.snapshot(), before_b);
                assert_eq!(app.state.doc.save(), outer);
                app.controlled_local_idle_for_test();
                assert_eq!(a.probe.acks.get(), usize::from(!view));
                assert_eq!(a.probe.starts.get(), usize::from(view));
                assert_eq!(b.probe.acks.get(), usize::from(case == "busy" && !view));
                assert_eq!(b.probe.starts.get(), usize::from(case == "busy" && view));
            }
        }
    }
}

fn component_bare_address(lazy: bool, duplicate: bool) {
    use crate::application::{AppComponent, LaunchTarget};
    use crate::application_test_support::{ProbeHost, probe_definition};
    use crate::bound_dispatch::legacy_document_tests::Probe;
    let host = ProbeHost::new();
    let mut context = host
        .prepare_with_permissions(
            probe_definition(),
            721,
            &LaunchTarget::New,
            std::rc::Rc::new(std::cell::RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
        )
        .unwrap();
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "value", 1).unwrap();
            Ok(())
        })
        .unwrap();
    let checkout = context.record().clone();
    let probe = Probe::new();
    probe.eager.set(!lazy);
    probe
        .declarations
        .set(if duplicate && !lazy { 2 } else { 1 });
    if duplicate && lazy {
        let child = Probe::new();
        child.eager.set(false);
        probe.children.borrow_mut().push(Box::new(child));
    }
    let component = AppComponent::new(context, Box::new(probe.clone()), registry());
    let mut sibling = Fixture::new(722);
    sibling.probe.declarations.set(0);
    let root = Column::new()
        .push(Box::new(crate::widget::TileHost::from_component(component)))
        .push(Box::new(sibling.take_tile()));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    let outer = app.state.doc.save();
    let before = sibling.snapshot();
    let actor = app.collaboration.local_author.0.clone();
    let result = app.invoke_resource_action(ResourceInvocation::new(&address(), &actor));
    if duplicate {
        assert!(
            matches!(result, Err(tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "ambiguous_action_target")
        );
    } else {
        assert!(
            result.is_ok(),
            "unique actual component must execute through bare address, lazy={lazy}: {result:?}"
        );
    }
    let expected = usize::from(!duplicate);
    assert_eq!(probe.acks.get(), expected);
    assert_eq!(sibling.probe.acks.get(), 0);
    assert_eq!(sibling.snapshot(), before);
    assert_eq!(app.state.doc.save(), outer);
    assert_eq!(
        host.records
            .read(&checkout.id, |doc| doc
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .as_i64())
            .unwrap(),
        Some(if duplicate { 1 } else { 2 })
    );
    assert!(!app.controlled_local_idle_for_test());
    assert_eq!(probe.acks.get(), expected);
}
#[test]
fn native_unique_component_eager_bare_address_executes_once() {
    component_bare_address(false, false);
}
#[test]
fn native_unique_component_lazy_bare_address_executes_once() {
    component_bare_address(true, false);
}

#[test]
fn native_actual_component_duplicate_eager_bare_address_refuses() {
    component_bare_address(false, true);
}
#[test]
fn native_actual_component_duplicate_lazy_bare_address_refuses() {
    component_bare_address(true, true);
}
