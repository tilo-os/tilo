#[test]
fn private_checkpoints_follow_the_executed_child_owner_not_the_primary_document() {
    let mut fixture = ActionFixture::new();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.actions.bound_broker_route_for_test =
        Some(crate::collaboration::PrimaryRouteWitness {
            route: fixture.bindings[0].broker_route().unwrap(),
            epoch: uuid::Uuid::new_v4(),
        });
    for binding in &fixture.bindings {
        fixture
            .app
            .document_routes
            .owners
            .push(crate::history_mode::routes::OwnerRoute {
                route: binding.broker_route().unwrap(),
                owners: vec![crate::history_mode::HistoryOwner::component(
                    binding.clone(),
                )],
                sent_heads: Vec::new(),
                needs_snapshot: true,
                pending: Default::default(),
                outbound_failure: None,
                answers_snapshots: false,
            });
    }
    fixture.app.publish_action_surface();
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[0], "bound:probe")
        .unwrap();
    fixture
        .app
        .invoke_local_resource_action(
            &tilo_actions::ActionAddress::new(alias, "apply"),
            &serde_json::Map::new(),
        )
        .unwrap();
    let (route, report) = sent
        .borrow()
        .iter()
        .find_map(|(route, request)| match request {
            CollabRequest::ReportLocalDocumentAction { report, .. } => {
                Some((route.clone(), report.clone()))
            }
            _ => None,
        })
        .expect("the public local action door must report its execution");
    fixture.app.autosave_tick().unwrap();
    for binding in &fixture.bindings {
        fixture
            .records
            .try_flush_record(&binding.checkout.id)
            .unwrap();
    }
    let recovered = fixture
        .app
        .local_document_action_checkpoint(&route)
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].0.invocation, report.invocation);
    assert_eq!(recovered[0].0.capture_id, report.capture_id);
    assert_eq!(
        recovered[0].0.bound_invocation().unwrap(),
        report.invocation
    );
    assert!(
        recovered[0].1.is_none(),
        "an unconfirmed report is not an admission"
    );
    let other = fixture.bindings[1].broker_route().unwrap();
    assert_ne!(route, other);
    assert!(
        fixture
            .app
            .local_document_action_checkpoint(&other)
            .unwrap()
            .is_empty()
    );
    fixture
        .records
        .try_flush_record(&fixture.bindings[0].checkout.id)
        .unwrap();
}
