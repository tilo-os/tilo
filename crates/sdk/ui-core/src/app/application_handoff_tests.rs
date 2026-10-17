// Exercise the real host action door, not an app-local launcher.
#[test]
fn application_action_handoff_requires_workspace_admission_and_routes_exact_destination() {
    use crate::collaboration::{PeerTransport, RouteSpec};
    use tilo_actions::remote::{ApplicationLaunch, InvokeArgument, RemoteActionMessage};
    struct Transport {
        sent: Rc<RefCell<Vec<CollabRequest>>>,
        joined: Option<RouteSpec>,
    }
    impl PeerTransport for Transport {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, request: CollabRequest) -> Result<(), String> {
            if let CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = &request
            {
                self.joined = Some(RouteSpec {
                    app_id: app_id.clone(),
                    cluster_id: *cluster_id,
                    record: record.clone(),
                });
            }
            self.sent.borrow_mut().push(request);
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            vec![]
        }
        fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
            self.joined.take().map(Ok)
        }
    }
    let launch = ApplicationLaunch::with_action(
        "so.tilo.file-explorer",
        "files.remote-account:616461",
        "open-remote-account",
    );
    let resource = Resource::new("settings.account:ada", "settings.account", "Ada")
        .with_action(launch.spec("open-files", "Browse files", "Browse this account in Files"));
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(CounterCard::with_lazy_resource(resource)),
    );
    app.init_layout();
    let actor = app.collaboration.local_author.0.clone();
    let address = tilo_actions::ActionAddress::new("settings.account:ada", "open-files");
    let invoke = |app: &mut App| {
        app.invoke_resource_action(
            ResourceInvocation::new(&address, &actor)
                .broker_admitted(true)
                .invocation(Some(tilo_actions::InvocationId::new())),
        )
    };
    assert!(
        invoke(&mut app).is_err(),
        "no broker must not fall back to an empty app"
    );
    let sent = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager = CollaborationManager::with_peer_transport(Transport {
        sent: sent.clone(),
        joined: None,
    });
    assert!(
        invoke(&mut app).is_err(),
        "a connection without an admitted workspace is insufficient"
    );
    let workspace = uuid::Uuid::new_v4();
    assert!(app.collaboration.manager.send(CollabRequest::Hello {
        app_id: "so.tilo.settings".into(),
        cluster_id: workspace,
        record: None,
        record_kind: None,
        handle: None,
    }));
    app.collaboration.manager.poll();
    assert!(app.collaboration.manager.primary_route().is_some());
    assert!(app.invoke_resource_action(
        ResourceInvocation::new(&address, &actor).broker_admitted(true),
    ).is_err(), "a routing effect requires an admitted invocation receipt");
    assert!(matches!(
        invoke(&mut app).unwrap(),
        crate::widget::ResourceOperationStart::Accepted
    ));
    let sent = sent.borrow();
    let routed: Vec<_> = sent
        .iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].target, launch.target(&workspace.to_string()));
    assert!(matches!(routed[0].argument, InvokeArgument::None));
    assert_eq!(app.actions.pending_application_launches.len(), 1);
}
