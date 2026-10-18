// Uses the ordinary retained component, publication and public local action door.
struct CapturePeer(Rc<RefCell<Vec<(crate::collaboration::RouteSpec, CollabRequest)>>>);
impl crate::collaboration::PeerTransport for CapturePeer {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
    fn join_route(&mut self, _: crate::collaboration::RouteSpec) -> Result<(), String> {
        Ok(())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
    fn send_to_route(
        &mut self,
        route: crate::collaboration::RouteSpec,
        request: CollabRequest,
    ) -> Result<(), String> {
        self.0.borrow_mut().push((route, request));
        Ok(())
    }
}

#[test]
fn local_component_action_drains_exact_pre_execution_snapshot_and_alias() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.publish_action_surface();
    let revision = fixture.app.action_directory_revision();
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[0], "bound:probe")
        .unwrap();
    let address = tilo_actions::ActionAddress::new(alias.clone(), "apply");
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    let requests = sent.borrow();
    let (route, request_id, report) = requests
        .iter()
        .find_map(|(route, request)| {
            if let CollabRequest::ReportLocalDocumentAction { request_id, report } = request {
                Some((route, *request_id, report))
            } else {
                None
            }
        })
        .expect("the public local semantic door reports its actual typed execution");
    assert_eq!(route, &fixture.bindings[0].broker_route().unwrap());
    assert_eq!(report.revision, revision);
    assert_eq!(report.resource, alias);
    assert_eq!(report.action, "apply");
    assert!(!report.mutation_hashes.is_empty());
    assert_ne!(fixture.app.action_directory_revision(), report.revision);
    drop(requests);
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Err("the captured directory was refused".into()),
        });
    assert!(fixture.app.document_sync_status().is_some());
    assert!(
        fixture
            .app
            .actions
            .local_document_directory_pins
            .iter()
            .any(|pin| pin.strong_count() > 0)
    );
    assert_eq!(fixture.app.local_document_action_admissions().count(), 0);
    // A later authenticated status for the same UUID can confirm admission;
    // the unknown reply did not drop its snapshot or reserve another identity.
    let invocation = sent
        .borrow()
        .iter()
        .find_map(|(_, request)| match request {
            CollabRequest::ReportLocalDocumentAction {
                request_id: id,
                report,
            } if *id == request_id => Some(report.invocation),
            _ => None,
        })
        .unwrap();
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Ok(runtime_collab::PublishedInvocationObservation {
                host: [1; 16],
                invocation: *invocation.as_bytes(),
                result: Some(runtime_collab::PublishedInvocationResult {
                    id: request_id,
                    update_json: serde_json::to_string(&tilo_actions::InvocationUpdate::Accepted {
                        invocation: tilo_actions::InvocationId(invocation),
                    })
                    .unwrap(),
                    admission_json: Some("{\"private_admission\":true}".into()),
                    terminal_json: None,
                    uncertain: false,
                    cancellation_requested: false,
                }),
            }),
        });
    let (capture, admission) = fixture
        .app
        .local_document_action_admissions()
        .next()
        .unwrap();
    assert_eq!(capture.invocation, invocation);
    assert_eq!(capture.revision, revision);
    assert_eq!(admission, "{\"private_admission\":true}");
    assert_eq!(fixture.app.local_document_action_admission_evictions(), 0);
    // Refusal is visible, but is neither a blocked UI thread nor a consumed
    // next keystroke: the next ordinary action still enters the same door.
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
}

#[test]
fn blocked_host_does_not_delay_local_edits_and_capacity_refusal_is_atomic() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.publish_action_surface();
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[0], "bound:probe")
        .unwrap();
    let address = tilo_actions::ActionAddress::new(alias, "apply");
    let record = fixture.bindings[0].checkout.id.clone();
    let heads = |fixture: &ActionFixture| {
        fixture
            .records
            .read(&record, |doc| doc.clone().get_heads())
            .unwrap()
    };
    // CapturePeer never returns a reply. Each synchronous call must commit a
    // distinct edit while earlier reports still hold their capacity leases.
    let mut completed = 0;
    for expected in 1..=129 {
        let before = heads(&fixture);
        if fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .is_err()
        {
            assert_eq!(
                heads(&fixture),
                before,
                "capacity refusal precedes mutation"
            );
            break;
        }
        completed = expected;
        assert_ne!(
            heads(&fixture),
            before,
            "edit {expected} finished without a host reply"
        );
        assert_eq!(fixture.app.local_input_capture.pending_count(), expected);
        assert_eq!(
            sent.borrow()
                .iter()
                .filter(|(_, request)| matches!(
                    request,
                    CollabRequest::ReportLocalDocumentAction { .. }
                ))
                .count(),
            expected
        );
        assert!(fixture.app.local_input_capture.failure.is_none());
    }
    // The conservative byte budget may saturate before the count limit.
    assert!((2..=128).contains(&completed));
    assert_eq!(fixture.app.local_input_capture.pending_count(), completed);
    assert_eq!(fixture.app.local_document_action_admissions().count(), 0);
}

#[test]
fn disconnected_input_keeps_local_framework_edits_available() {
    let mut fixture = ActionFixture::broker();
    fixture.app.collaboration.manager = CollaborationManager::new();
    let scope = fixture.app.begin_local_input_capture();
    let target = fixture.target(0, "apply");
    let actor = fixture.bindings[0].author.0.clone();
    fixture
        .app
        .invoke_bound_target(
            &target,
            &ResourceInvocation::new(&target.address.address, &actor),
        )
        .unwrap();
    drop(scope);
    fixture.app.drain_local_input_capture();
    assert!(fixture.app.local_input_capture.failure.is_none());
    assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
}

#[test]
fn model_action_outside_local_input_does_not_create_a_local_report() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    capture_bound_directory(&mut fixture);
    let target = fixture.target(0, "apply");
    // This is the same internal door a delivered/model action uses, not the
    // public local-user semantic entry point.
    let actor = fixture.bindings[0].author.0.clone();
    fixture
        .app
        .invoke_bound_target(
            &target,
            &ResourceInvocation::new(&target.address.address, &actor),
        )
        .unwrap();
    fixture.app.drain_local_input_capture();
    assert!(
        sent.borrow().iter().all(|(_, request)| !matches!(
            request,
            CollabRequest::ReportLocalDocumentAction { .. }
        ))
    );
}
