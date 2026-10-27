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
                opening: false,
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
    // Drop every in-memory capture and directory lease, as on process restart.
    // Recovery reads the existing private document checkpoint, not the live UI.
    let heads = fixture
        .records
        .read(&fixture.bindings[0].checkout.id, |doc| {
            doc.clone().get_heads()
        })
        .unwrap();
    fixture.app.local_input_capture = Default::default();
    fixture.app.actions.local_document_directory_pins.clear();
    // Recovered rows may share a revision but live in separate pins. An
    // unrelated row must not hide the report's row during the next checkpoint.
    let mut unrelated_row = fixture
        .app
        .local_action_checkpoint_entries(&route)
        .unwrap()
        .remove(0)
        .0
        .directory_row
        .unwrap();
    unrelated_row.action = Some("unrelated-checkpoint-action".into());
    let _unrelated_pin = fixture
        .app
        .restore_local_directory_row(
            report.revision.clone(),
            report.resource.clone(),
            "unrelated-checkpoint-action".into(),
            unrelated_row,
        )
        .unwrap();
    sent.borrow_mut().clear();
    fixture.app.restore_local_input_checkpoint(&route).unwrap();
    assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
    let checkpoint = fixture
        .app
        .local_action_checkpoint_entries(&route)
        .unwrap()
        .remove(0)
        .0;
    let original_custody = checkpoint
        .custody
        .expect("authenticated custody must survive restart");
    assert_eq!(original_custody, capture_custody(&route));
    // A new live connection cannot relabel old work, even when it owns the
    // same document and reports the same app-controlled action context.
    let mut mismatches = vec![None];
    for field in 0..6 {
        let mut changed = original_custody.clone();
        match field {
            0 => changed.root = [3; 32],
            1 => changed.device = [4; 32],
            2 => changed.agent = Some("different-agent".into()),
            3 => changed.app_id = "other-app".into(),
            4 => changed.room = uuid::Uuid::new_v4().to_string(),
            _ => changed.document = "other-document".into(),
        }
        mismatches.push(Some(changed));
    }
    for custody in mismatches {
        fixture.app.collaboration.manager =
            CollaborationManager::with_peer_transport(CustodyCapturePeer {
                inner: CapturePeer(sent.clone()),
                custody,
            });
        assert!(
            fixture
                .app
                .local_document_action_checkpoint(&route)
                .unwrap()
                .is_empty(),
            "mismatched sessions must not expose private intents or receipts"
        );
        assert!(fixture.app.restore_local_input_checkpoint(&route).is_err());
        fixture.app.local_input_capture.retry_now();
        fixture.app.drain_local_input_capture();
        assert!(
            capture_reports(&sent).is_empty(),
            "mismatched custody must never report"
        );
        assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
        assert!(fixture.app.local_input_capture.failure.is_some());
    }
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.local_input_capture.retry_now();
    fixture.app.drain_local_input_capture();
    let retried = capture_reports(&sent);
    assert_eq!(retried.len(), 1);
    assert!(
        fixture
            .app
            .checkpoint_local_directory_row(&report.revision, &report.resource, &report.action)
            .is_some(),
        "all recovered rows at a revision remain checkpointable"
    );
    assert_eq!(
        serde_json::to_vec(&retried[0].1).unwrap(),
        serde_json::to_vec(&report).unwrap()
    );
    assert!(matches!(
        fixture.app.resolve_local_document_action_directory(
            &report.revision,
            &report.resource,
            &report.action
        ),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
    assert_eq!(
        fixture
            .records
            .read(&fixture.bindings[0].checkout.id, |doc| doc
                .clone()
                .get_heads())
            .unwrap(),
        heads
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
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id: retried[0].0,
            result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                "refused".into(),
            )),
        });
    assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
    fixture
        .records
        .try_flush_record(&fixture.bindings[0].checkout.id)
        .unwrap();
    fixture.app.local_input_capture = Default::default();
    fixture.app.restore_local_input_checkpoint(&route).unwrap();
    assert_eq!(
        fixture.app.local_input_capture.pending_count(),
        0,
        "a durable terminal refusal must not become a fresh pending report"
    );
    assert_eq!(fixture.app.local_document_action_admissions().count(), 0);
}
