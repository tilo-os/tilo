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

crate::testing::test_app_widget!(
    CounterCard,
    "Scoped counter",
    icon = crate::widget::IconDescriptor::Glyph("C")
);
