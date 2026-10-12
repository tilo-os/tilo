// Included in the actual App/record-owner action-door fixture.
fn deferred_counter_resource(
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    server: bool,
) -> Resource {
    use tilo_actions::{ContinuationEvidence, ContinuationKey, ContinuationQuery};
    Resource::new("test.counter", "test.counter", "Counter").with_action(
        ActionSpec::deferred(
            "create",
            "Create counter",
            if server {
                ExecutionLocus::ServerPreferred
            } else {
                ExecutionLocus::ClientLocal
            },
            COUNTER_TYPE,
            move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                EncodedAction::encode(
                    COUNTER_TYPE,
                    &if server {
                        CounterAction::HostBump
                    } else {
                        CounterAction::Bump
                    },
                )
                .unwrap()
            },
        )
        .with_continuation_identity(|query| {
            matches!(query, ContinuationQuery::Draft).then(|| ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("counter-create", [b"owner"]),
                handler: ContinuationKey::new("counter-create", [b"v1"]),
            })
        }),
    )
}
#[test]
fn deferred_server_preferred_client_routes_address_without_minting() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let resource = deferred_counter_resource(calls.clone(), true);
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(CounterCard::with_lazy_resource(resource)),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    let sent = std::rc::Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    let actor = app.collaboration.local_author.0.clone();
    let result = app
        .invoke_resource_action(
            ResourceInvocation::new(
                &tilo_actions::ActionAddress::new("test.counter", "create"),
                &actor,
            )
            .confirmed(true),
        )
        .unwrap();
    assert!(matches!(
        result,
        crate::widget::ResourceOperationStart::Accepted
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the client cannot mint the remote owner's operation"
    );
    // The broker is asked through the remote action protocol.
    assert!(sent.borrow().iter().any(|request| matches!(
        request,
        CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
        } if invoke.target.address.resource == "test.counter"
            && invoke.target.address.action == "create"
            && matches!(&invoke.argument, tilo_actions::remote::InvokeArgument::Json(arguments) if arguments.is_empty())
    )));
    assert_eq!(counter(&app), 0);
}
#[test]
fn deferred_actual_owned_dispatch_checks_original_grant_before_single_mint() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    for expired in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let time = Arc::new(AtomicU64::new(99));
        let resource = deferred_counter_resource(calls.clone(), false);
        let current = &resource.actions[0];
        let original = crate::invocation_scope::PublishedInvocationSpec {
            kind: current.kind,
            contract: current.contract.clone(),
            params: current.params().to_vec(),
            input: tilo_actions::InputSpec::None,
            directory_revision: None,
        };
        let (mut app, store, checkout, _temp) =
            owned_counter(CounterCard::with_lazy_resource(resource));
        let request = remote_request(9901, "create");
        app.require_published_invocation_authorizer();
        app.install_published_invocation_authorizer(original_remote_authorizer_with_spec(
            request.clone(),
            original_remote_grant(time.clone()),
            Some(original),
        ))
        .unwrap();
        if expired {
            time.store(100, Ordering::Release);
        }
        deliver_remote(&mut app, &request);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(!expired),
            "mint follows original caller admission"
        );
        assert_eq!(owned_count(&store, &checkout), i64::from(!expired));
    }
}
