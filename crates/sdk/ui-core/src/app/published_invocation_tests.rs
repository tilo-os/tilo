// Included in the existing action-door fixture module so these checks execute
// the same real document handler and retained asynchronous operation.
fn remote_request(
    wire_id: u64,
    action: &str,
) -> crate::invocation_scope::PublishedInvocationRequest {
    crate::invocation_scope::PublishedInvocationRequest {
        wire_id,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(wire_id) + 1000)),
        address: tilo_actions::ActionAddress::new("test.counter", action),
        argument: None,
        actor: "remote:original".into(),
        participant: None,
    }
}

fn deliver_remote(app: &mut App, request: &crate::invocation_scope::PublishedInvocationRequest) {
    app.invoke_published_action(
        request.wire_id,
        request.invocation,
        request.address.resource.clone(),
        request.address.action.clone(),
        request.argument.as_deref(),
        &request.actor,
        None,
    );
}

fn original_remote_grant(
    now: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> crate::permissions::PermissionContext {
    crate::permissions::PermissionContext::granted(
        "remote:original",
        &crate::permissions::TreePath::root().app("test.counter"),
        crate::permissions::policy::KindSet::ALL,
        Some(100),
    )
    .with_policy_time_sampler(std::sync::Arc::new(move || {
        Ok(now.load(std::sync::atomic::Ordering::Acquire))
    }))
}

fn original_counter_spec(action: &str) -> crate::invocation_scope::PublishedInvocationSpec {
    let resource =
        CounterCard::with_async_post_bump_reset().current_resource(&automerge::AutoCommit::new());
    let current = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .unwrap();
    crate::invocation_scope::PublishedInvocationSpec {
        kind: current.kind,
        contract: current.contract.clone(),
        params: current.params().to_vec(),
        input: tilo_actions::InputSpec::None,
        directory_revision: None,
    }
}

fn original_remote_authorizer(
    expected: crate::invocation_scope::PublishedInvocationRequest,
    grant: crate::permissions::PermissionContext,
) -> crate::invocation_scope::PublishedInvocationAuthorizer {
    let spec = original_counter_spec(&expected.address.action);
    original_remote_authorizer_with_spec(expected, grant, Some(spec))
}

fn original_remote_authorizer_with_spec(
    expected: crate::invocation_scope::PublishedInvocationRequest,
    grant: crate::permissions::PermissionContext,
    spec: Option<crate::invocation_scope::PublishedInvocationSpec>,
) -> crate::invocation_scope::PublishedInvocationAuthorizer {
    std::sync::Arc::new(move |delivered| {
        if delivered != &expected {
            return Err("unknown or altered original invocation".into());
        }
        let grant = grant.clone();
        let lease = crate::invocation_scope::PublishedInvocationLease::new(
            expected.clone(),
            crate::collaboration::RouteSpec::record(
                "test.counter",
                uuid::Uuid::from_u128(1),
                crate::RecordId::for_tab(900).as_str(),
            ),
            std::sync::Arc::new(move || {
                grant
                    .check(&crate::permissions::PermissionScope::app(
                        "test.counter",
                        crate::permissions::ActionKind::Undoable,
                    ))
                    .then_some(())
                    .ok_or_else(|| "original remote grant expired".into())
            }),
        );
        let lease = match &spec {
            Some(spec) => lease.with_dispatch_spec(spec.clone())?,
            None => lease,
        };
        lease.with_owner_token(std::sync::Arc::new(expected.clone()))
    })
}

#[test]
fn published_authorization_delayed_original_remote_grant_denies_before_document_handler() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(99));
    let grant = original_remote_grant(time.clone());
    assert!(grant.check(&crate::permissions::PermissionScope::app(
        "test.counter",
        crate::permissions::ActionKind::Undoable
    )));
    let request = remote_request(851, "bump");
    let mut app = app();
    app.collaboration.manager = admitted_delivery_manager();
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(request.clone(), grant))
        .unwrap();
    let before = app.state.doc.get_heads();
    time.store(100, Ordering::Release);
    deliver_remote(&mut app, &request);
    assert_eq!(
        counter(&app),
        0,
        "host permission must not replace expired remote caller"
    );
    assert_eq!(app.state.doc.get_heads(), before);
}

#[test]
fn published_authorization_missing_unknown_or_mismatched_request_never_executes() {
    use std::sync::{Arc, atomic::AtomicU64};
    for case in 0..5 {
        let expected = remote_request(852, "bump");
        let mut delivered = expected.clone();
        let mut app = app();
        app.collaboration.manager = admitted_delivery_manager();
        app.require_published_invocation_authorizer();
        if case != 0 {
            app.install_published_invocation_authorizer(original_remote_authorizer(
                expected,
                original_remote_grant(Arc::new(AtomicU64::new(99))),
            ))
            .unwrap();
        }
        match case {
            1 => delivered.wire_id += 1,
            2 => delivered.invocation = tilo_actions::InvocationId::new(),
            3 => delivered.actor = "remote:other".into(),
            4 => delivered.argument = Some("altered".into()),
            _ => {}
        }
        deliver_remote(&mut app, &delivered);
        assert_eq!(counter(&app), 0, "case {case} entered the handler");
    }
}

#[test]
fn published_authorization_wrong_lease_never_executes() {
    use std::sync::Arc;
    let request = remote_request(853, "bump");
    let mut app = app();
    app.collaboration.manager = admitted_delivery_manager();
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(Arc::new(move |delivered| {
        let mut wrong = delivered.clone();
        wrong.wire_id += 1;
        Ok(crate::invocation_scope::PublishedInvocationLease::new(
            wrong,
            crate::collaboration::RouteSpec::record(
                "test.counter",
                uuid::Uuid::from_u128(1),
                crate::RecordId::for_tab(900).as_str(),
            ),
            Arc::new(|| Ok(())),
        ))
    }))
    .unwrap();
    deliver_remote(&mut app, &request);
    assert_eq!(counter(&app), 0);
}

#[test]
fn published_authorization_valid_remote_still_uses_same_document_door() {
    use std::sync::{Arc, atomic::AtomicU64};
    let request = remote_request(854, "bump");
    let (mut app, store, checkout, _temp) = owned_counter(CounterCard::new());
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(
        request.clone(),
        original_remote_grant(Arc::new(AtomicU64::new(99))),
    ))
    .unwrap();
    deliver_remote(&mut app, &request);
    assert_eq!(owned_count(&store, &checkout), 1);
}

#[test]
fn published_authorization_accepted_continuation_retains_failed_custody() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(99));
    let request = remote_request(855, "async-bump");
    let (mut app, store, checkout, _temp) =
        owned_counter(CounterCard::with_async_post_bump_reset());
    app.collaboration.manager = disconnected_delivery_manager();
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(
        request.clone(),
        original_remote_grant(time.clone()),
    ))
    .unwrap();
    deliver_remote(&mut app, &request);
    assert!(
        app.actions
            .directed_invocations
            .contains_key(&request.invocation)
    );
    time.store(100, Ordering::Release);
    app.poll_directed_invocations();
    assert!(
        app.actions.published_invocations.failure.is_none(),
        "one retired job must not poison siblings"
    );
    // This disconnected fixture retains terminal publication uncertainty. Its
    // pending model job nevertheless shares the permanently retired scope.
    assert!(
        app.actions
            .directed_invocations
            .get(&request.invocation)
            .and_then(|pending| pending.authorization.as_ref())
            .unwrap()
            .check()
            .is_err()
    );
    // The actual queued model job still exists, but retains the retired scope.
    time.store(99, Ordering::Release);
    app.refresh_focus_order();
    assert_eq!(owned_count(&store, &checkout), 0);
}

#[test]
fn causal_scope_expired_accepted_job_must_not_mutate_from_model_sync() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(99));
    let request = remote_request(856, "async-bump");
    let (mut app, store, checkout, _temp) =
        owned_counter(CounterCard::with_async_post_bump_reset());
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(
        request.clone(),
        original_remote_grant(time.clone()),
    ))
    .unwrap();
    deliver_remote(&mut app, &request);
    assert!(
        app.actions
            .directed_invocations
            .contains_key(&request.invocation)
    );
    time.store(100, Ordering::Release);
    app.refresh_focus_order();
    assert_eq!(
        owned_count(&store, &checkout),
        0,
        "an Accepted job must recheck its own original caller before model mutation"
    );
}

crate::testing::test_app_widget!(
    CounterCard,
    "Scoped counter",
    icon = crate::widget::IconDescriptor::Glyph("C")
);
fn owned_counter(
    card: CounterCard,
) -> (
    App,
    crate::record::RecordStore,
    crate::RecordCheckout,
    tempfile::TempDir,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().join("records"));
    let store = owners
        .open("test.counter", &uuid::Uuid::from_u128(1).to_string())
        .unwrap();
    let checkout = crate::RecordCheckout::new(
        crate::RecordId::for_tab(900),
        crate::RecordKind::new("counter"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "n", 0).map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    store.require_invocation_provenance(&checkout).unwrap();
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(card)));
    tile.bind_record(store.clone(), checkout.clone());
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(tile));
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.collaboration.manager = admitted_delivery_manager();
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::app("test.counter", uuid::Uuid::from_u128(1)),
        epoch: uuid::Uuid::new_v4(),
    });
    app.init_layout();
    (app, store, checkout, temp)
}
fn owned_count(store: &crate::record::RecordStore, checkout: &crate::RecordCheckout) -> i64 {
    store.read(&checkout.id, counter_in_doc).unwrap()
}

#[test]
fn causal_scope_actual_concurrent_jobs_keep_distinct_callers_and_valid_jobs_progress() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    for expire_first in [false, true] {
        let time = Arc::new(AtomicU64::new(99));
        let (mut app, store, checkout, _temp) =
            owned_counter(CounterCard::with_async_post_bump_reset());
        let mut a = remote_request(861, "async-bump");
        a.actor = "remote:a".into();
        let mut b = remote_request(862, "async-bump");
        b.actor = "remote:b".into();
        let expected = [a.clone(), b.clone()];
        let sampled = time.clone();
        app.require_published_invocation_authorizer();
        app.install_published_invocation_authorizer(Arc::new(move |request| {
            if !expected.contains(request) {
                return Err("unknown original request".into());
            }
            let expiry = if request.actor == "remote:a" {
                100
            } else {
                200
            };
            let now = sampled.clone();
            let permission = crate::permissions::PermissionContext::granted(
                &request.actor,
                &crate::permissions::TreePath::root().app("test.counter"),
                crate::permissions::policy::KindSet::ALL,
                Some(expiry),
            )
            .with_policy_time_sampler(Arc::new(move || Ok(now.load(Ordering::Acquire))));
            crate::invocation_scope::PublishedInvocationLease::new(
                request.clone(),
                crate::collaboration::RouteSpec::record(
                    "test.counter",
                    uuid::Uuid::from_u128(1),
                    crate::RecordId::for_tab(900).as_str(),
                ),
                Arc::new(move || {
                    permission
                        .check(&crate::permissions::PermissionScope::app(
                            "test.counter",
                            crate::permissions::ActionKind::Undoable,
                        ))
                        .then_some(())
                        .ok_or_else(|| "original grant expired".into())
                }),
            )
            .with_dispatch_spec(original_counter_spec(&request.address.action))?
            .with_owner_token(Arc::new(request.clone()))
        }))
        .unwrap();
        deliver_remote(&mut app, &a);
        deliver_remote(&mut app, &b);
        assert_eq!(
            app.actions.directed_invocations.len(),
            2,
            "both real jobs admitted before either model tick"
        );
        let a_scope = app.actions.directed_invocations[&a.invocation]
            .authorization
            .clone()
            .unwrap();
        if expire_first {
            time.store(100, Ordering::Release);
        }
        app.refresh_focus_order();
        assert_eq!(
            owned_count(&store, &checkout),
            if expire_first { 1 } else { 2 }
        );
        if expire_first {
            assert!(a_scope.check().is_err());
            time.store(99, Ordering::Release);
            app.refresh_focus_order();
            assert_eq!(
                owned_count(&store, &checkout),
                1,
                "retired job cannot resurrect on a later model tick"
            );
        }
    }
}

#[test]
fn completed_owned_job_retires_retained_handler_and_worker_custody() {
    use std::sync::{Arc, atomic::AtomicU64};
    struct TerminalTransport(std::rc::Rc<RefCell<Vec<CollabRequest>>>);
    impl crate::collaboration::PeerTransport for TerminalTransport {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, request: CollabRequest) -> Result<(), String> {
            self.0.borrow_mut().push(request);
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
            request: CollabRequest,
        ) -> Result<(), String> {
            self.send(request)
        }
    }
    let time = Arc::new(AtomicU64::new(99));
    let (mut app, store, checkout, _temp) =
        owned_counter(CounterCard::with_async_post_bump_reset());
    let request = remote_request(8991, "async-bump");
    // Exercise the production terminal gate with an admitted publication route.
    // This transport proves queue admission, not a remote durability receipt.
    let sent = std::rc::Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(TerminalTransport(sent.clone()));
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::app("test.counter", uuid::Uuid::from_u128(1)),
        epoch: uuid::Uuid::new_v4(),
    });
    app.publish_owned_documents();
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(
        request.clone(),
        original_remote_grant(time),
    ))
    .unwrap();
    deliver_remote(&mut app, &request);
    let scope = app.actions.directed_invocations[&request.invocation]
        .authorization
        .clone()
        .unwrap();
    let worker = scope.effect_scope().unwrap();
    assert!(scope.check().is_ok());
    app.refresh_focus_order();
    assert_eq!(owned_count(&store, &checkout), 1);
    assert!(
        app.poll_directed_invocations(),
        "the real terminal update arrived"
    );
    assert_eq!(app.action_publication_failure, None);
    assert!(
        !app.actions
            .directed_invocations
            .contains_key(&request.invocation)
    );
    assert!(
        scope.check().is_err(),
        "completed handlers cannot submit late work"
    );
    let mut ran = false;
    assert!(
        worker
            .with_effect(|| {
                ran = true;
                Ok(())
            })
            .is_err()
    );
    assert!(
        !ran,
        "a retained worker cannot act after the original job ended"
    );
    assert!(
        !app.poll_directed_invocations(),
        "late polling cannot answer twice"
    );
    assert_eq!(
        sent.borrow()
            .iter()
            .filter(|request| matches!(request,
                CollabRequest::UpdateInvocation { update, .. } if update.is_terminal()
            ))
            .count(),
        1,
        "the original job has exactly one terminal receipt"
    );
}

#[test]
fn published_authorization_original_spec_missing_or_changed_never_mutates_actual_record() {
    use std::sync::{Arc, atomic::AtomicU64};
    for case in 0..5 {
        let request = remote_request(875 + case, "bump");
        let (mut app, store, checkout, _temp) = owned_counter(CounterCard::new());
        let mut spec = original_counter_spec("bump");
        match case {
            1 => spec.kind = tilo_actions::OperationKind::Effect,
            2 => spec.contract.kind = tilo_actions::ActionKind::Read,
            3 => spec.params.push(tilo_actions::ActionParam::optional(
                "extra",
                tilo_actions::ParamType::Text,
                "original input",
            )),
            4 => {
                spec.input = tilo_actions::InputSpec::Text {
                    prompt: "original prompt".into(),
                    placeholder: "value".into(),
                    collects: tilo_actions::Collects::Content,
                }
            }
            _ => {}
        }
        app.require_published_invocation_authorizer();
        app.install_published_invocation_authorizer(original_remote_authorizer_with_spec(
            request.clone(),
            original_remote_grant(Arc::new(AtomicU64::new(99))),
            (case != 0).then_some(spec),
        ))
        .unwrap();
        deliver_remote(&mut app, &request);
        assert_eq!(
            owned_count(&store, &checkout),
            0,
            "declaration mismatch case {case}"
        );
        assert!(
            !app.actions
                .directed_invocations
                .contains_key(&request.invocation)
        );
    }
}

#[test]
fn directory_permission_original_revision_checked_before_actual_handler() {
    use std::sync::{Arc, atomic::AtomicU64};
    for stale in [false, true] {
        let request = remote_request(888 + u64::from(stale), "bump");
        let (mut app, store, checkout, _temp) = owned_counter(CounterCard::new());
        let mut spec = original_counter_spec("bump");
        spec.directory_revision = Some(if stale {
            "stale-admitted-revision".into()
        } else {
            app.action_directory_revision()
        });
        app.require_published_invocation_authorizer();
        app.install_published_invocation_authorizer(original_remote_authorizer_with_spec(
            request.clone(),
            original_remote_grant(Arc::new(AtomicU64::new(99))),
            Some(spec),
        ))
        .unwrap();
        deliver_remote(&mut app, &request);
        assert_eq!(
            owned_count(&store, &checkout),
            i64::from(!stale),
            "stale original revision must refuse before mutation; identical control must still run"
        );
    }
}
