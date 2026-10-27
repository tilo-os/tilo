// Uses the ordinary retained component, publication and public local action door.
struct CustodyCapturePeer {
    inner: CapturePeer,
    custody: Option<runtime_collab::committed_document::ConnectionSnapshot>,
}
impl crate::collaboration::PeerTransport for CustodyCapturePeer {
    fn connection_snapshot(
        &self,
        _: &crate::collaboration::RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        self.custody.clone()
    }
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        crate::collaboration::PeerTransport::send(&mut self.inner, request)
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
    fn send_to_route(
        &mut self,
        route: crate::collaboration::RouteSpec,
        request: CollabRequest,
    ) -> Result<(), String> {
        crate::collaboration::PeerTransport::send_to_route(&mut self.inner, route, request)
    }
}
struct CapturePeer(Rc<RefCell<Vec<(crate::collaboration::RouteSpec, CollabRequest)>>>);
fn capture_custody(
    route: &crate::collaboration::RouteSpec,
) -> runtime_collab::committed_document::ConnectionSnapshot {
    runtime_collab::committed_document::ConnectionSnapshot {
        root: [1; 32],
        device: [2; 32],
        agent: None,
        app_id: route.app_id.clone(),
        room: route.cluster_id.to_string(),
        document: route.record.clone().unwrap_or_default(),
    }
}
impl crate::collaboration::PeerTransport for CapturePeer {
    fn connection_snapshot(
        &self,
        route: &crate::collaboration::RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        Some(capture_custody(route))
    }
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
            result: Err(runtime_collab::LocalDocumentActionError::Uncertain(
                "reply lost after admission".into(),
            )),
        });
    assert_eq!(
        fixture.app.document_sync_status(),
        None,
        "a lost reply is reconciled quietly: still on its way"
    );
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

fn capture_registered_read_only_callback(action: &str, callback: &str) {
    COMPOUND_MODE.set(0);
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    let connected = Rc::new(Cell::new(true));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(RecoveringCapturePeer {
            sent: sent.clone(),
            connected: connected.clone(),
            reject_enqueue: Rc::new(Cell::new(false)),
        });
    let published = capture_address(&mut fixture);
    let offline_revision = if action == "read" {
        // Retain the authenticated input snapshot before losing transport.
        drop(fixture.app.begin_local_input_capture());
        let revision = fixture.app.action_directory_revision();
        connected.set(false);
        Some(revision)
    } else {
        None
    };
    let address = tilo_actions::ActionAddress::new(published.resource.clone(), action);
    let heads = fixture
        .records
        .read(&fixture.bindings[0].checkout.id, |doc| {
            doc.clone().get_heads()
        })
        .unwrap();
    CAPTURES.with_borrow_mut(Vec::clear);

    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();

    CAPTURES.with_borrow(|captures| {
        assert_eq!(
            captures
                .iter()
                .filter(|(label, _)| *label == callback)
                .count(),
            1,
            "the registered callback must actually run exactly once"
        );
    });
    assert_eq!(
        fixture
            .records
            .read(&fixture.bindings[0].checkout.id, |doc| doc
                .clone()
                .get_heads())
            .unwrap(),
        heads,
        "read-only callbacks do not mutate the canonical record"
    );
    if offline_revision.is_some() {
        assert!(capture_reports(&sent).is_empty());
        assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
        connected.set(true);
        fixture.app.drain_local_input_capture();
    }
    let reports = capture_reports(&sent);
    assert_eq!(reports.len(), 1);
    let report = &reports[0].1;
    if let Some(revision) = offline_revision {
        assert_eq!(report.revision, revision);
    }
    // Invocation refreshes publication before freezing its snapshot. Resolve
    // that retained snapshot, not an unrelated earlier publication revision.
    assert!(matches!(
        fixture.app.resolve_local_document_action_directory(
            &report.revision,
            &report.resource,
            &report.action,
        ),
        runtime_collab::ActionDirectoryResolution::Found {
            revision: Some(exact),
            ..
        } if exact == report.revision
    ));
    assert_eq!(report.resource, published.resource);
    assert_eq!(report.action, action);
    assert_eq!(report.bound_invocation().unwrap(), report.invocation);
    assert!(report.mutation_hashes.is_empty());
    assert_eq!(
        fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap(),
        report.resource,
        "read-only execution retains the same published component alias"
    );
    assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
}

#[test]
fn registered_bound_view_callback_captures_empty_report_without_mutation() {
    capture_registered_read_only_callback("view", "bound-view");
}

#[test]
fn registered_bound_read_projection_captures_empty_report_without_mutation() {
    struct ReadCaptureGuard(bool);
    impl Drop for ReadCaptureGuard {
        fn drop(&mut self) {
            READ_CAPTURE_MODE.set(self.0);
        }
    }
    let _guard = ReadCaptureGuard(READ_CAPTURE_MODE.replace(true));
    capture_registered_read_only_callback("read", "bound-read");
}

#[test]
fn blocked_host_does_not_delay_local_edits_and_a_full_queue_still_takes_the_edit() {
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
    // CapturePeer never returns a reply. Every synchronous call commits its
    // edit, whether or not the reports still on their way leave room for
    // its own: an edit beyond the queue lands unreported, and says so.
    let mut reported = 0;
    for expected in 1..=129 {
        let before = heads(&fixture);
        fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .expect("an edit never waits for the history");
        assert_ne!(
            heads(&fixture),
            before,
            "edit {expected} finished without a host reply"
        );
        let pending = fixture.app.local_input_capture.pending_count();
        assert_eq!(capture_reports(&sent).len(), pending);
        if pending == expected {
            reported = expected;
            assert!(fixture.app.local_input_capture.failure.is_none());
        } else {
            assert_eq!(
                pending, reported,
                "an edit beyond the queue is not reported"
            );
            assert!(
                fixture.app.local_input_capture.failure.is_some(),
                "an edit the history could not take is shown"
            );
        }
    }
    // The conservative byte budget may saturate before the count limit.
    assert!((2..=128).contains(&reported));
    assert_eq!(fixture.app.local_input_capture.pending_count(), reported);
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
    assert_eq!(
        fixture.app.document_sync_status(),
        None,
        "a disconnected edit is the person's, saved here: nothing to say"
    );
    assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
    assert_eq!(fixture.app.local_document_action_admissions().count(), 0);
}

#[test]
fn disconnected_input_retains_the_last_published_revision_and_exact_route() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    let connected = Rc::new(std::cell::Cell::new(true));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(RecoveringCapturePeer {
            sent: sent.clone(),
            connected: connected.clone(),
            reject_enqueue: Rc::new(std::cell::Cell::new(false)),
        });
    let address = capture_address(&mut fixture);
    // Freeze a genuinely published input surface, then lose only transport.
    drop(fixture.app.begin_local_input_capture());
    let revision = fixture.app.action_directory_revision();
    connected.set(false);
    let before = capture_reports(&sent).len();
    for _ in 0..2 {
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
    }
    assert_eq!(capture_reports(&sent).len(), before);
    assert_eq!(fixture.app.local_input_capture.pending_count(), 2);
    assert_eq!(
        fixture.app.document_sync_status(),
        None,
        "disconnected edits awaiting admission are on their way: nothing to say"
    );
    connected.set(true);
    fixture.app.drain_local_input_capture();
    let reports = capture_reports(&sent);
    assert_eq!(reports.len(), 2);
    assert_ne!(reports[0].1.invocation, reports[1].1.invocation);
    for (_, report) in reports {
        assert_eq!(report.revision, revision);
        assert_eq!(report.resource, address.resource);
        assert_eq!(report.bound_invocation().unwrap(), report.invocation);
        assert!(!report.mutation_hashes.is_empty());
    }
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

fn capture_address(fixture: &mut ActionFixture) -> tilo_actions::ActionAddress {
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.publish_action_surface();
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[0], "bound:probe")
        .unwrap();
    tilo_actions::ActionAddress::new(alias, "apply")
}

fn capture_reports(
    sent: &Rc<RefCell<Vec<(crate::collaboration::RouteSpec, CollabRequest)>>>,
) -> Vec<(u64, runtime_collab::LocalDocumentActionReport)> {
    sent.borrow()
        .iter()
        .filter_map(|(_, request)| match request {
            CollabRequest::ReportLocalDocumentAction { request_id, report } => {
                Some((*request_id, report.clone()))
            }
            _ => None,
        })
        .collect()
}

fn capture_reply(
    request_id: u64,
    invocation: uuid::Uuid,
    update: tilo_actions::InvocationUpdate,
    admission: bool,
) -> CollabEvent {
    CollabEvent::LocalDocumentActionReported {
        request_id,
        result: Ok(runtime_collab::PublishedInvocationObservation {
            host: [1; 16],
            invocation: *invocation.as_bytes(),
            result: Some(runtime_collab::PublishedInvocationResult {
                id: request_id,
                update_json: serde_json::to_string(&update).unwrap(),
                admission_json: admission.then(|| "{\"private_admission\":true}".into()),
                terminal_json: None,
                uncertain: false,
                cancellation_requested: false,
            }),
        }),
    }
}

#[test]
fn definitive_local_capture_failures_release_pins_and_editing_recovers() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    let address = capture_address(&mut fixture);
    for index in 0..195 {
        fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .unwrap();
        let (request_id, report) = capture_reports(&sent).pop().unwrap();
        let invocation = tilo_actions::InvocationId(report.invocation);
        let reply = match index % 3 {
            0 => CollabEvent::LocalDocumentActionReported {
                request_id,
                result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                    "policy refused".into(),
                )),
            },
            1 => capture_reply(
                request_id,
                report.invocation,
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                },
                false,
            ),
            _ => capture_reply(
                request_id,
                report.invocation,
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "refused".into(),
                        message: "policy refused".into(),
                        details: None,
                        retryable: false,
                    },
                },
                false,
            ),
        };
        fixture.app.apply_collab_event_for_test(reply);
        assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
        assert_eq!(fixture.app.local_input_capture.custody_size(), 0);
        assert!(
            fixture
                .app
                .actions
                .local_document_directory_pins
                .iter()
                .all(|pin| pin.strong_count() == 0)
        );
        assert!(fixture.app.document_sync_status().is_some());
    }
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    let (request_id, report) = capture_reports(&sent).pop().unwrap();
    fixture.app.apply_collab_event_for_test(capture_reply(
        request_id,
        report.invocation,
        tilo_actions::InvocationUpdate::Accepted {
            invocation: tilo_actions::InvocationId(report.invocation),
        },
        true,
    ));
    assert_eq!(capture_reports(&sent).len(), 196);
    assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
    assert_eq!(fixture.app.local_document_action_admissions().count(), 1);
}

struct RecoveringCapturePeer {
    sent: Rc<RefCell<Vec<(crate::collaboration::RouteSpec, CollabRequest)>>>,
    connected: Rc<std::cell::Cell<bool>>,
    reject_enqueue: Rc<std::cell::Cell<bool>>,
}
impl crate::collaboration::PeerTransport for RecoveringCapturePeer {
    fn connection_snapshot(
        &self,
        route: &crate::collaboration::RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        Some(capture_custody(route))
    }
    fn is_connected(&self) -> bool {
        self.connected.get()
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
        self.sent.borrow_mut().push((route, request));
        if self.reject_enqueue.get() {
            Err("transport queue temporarily unavailable".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn uncertain_and_enqueue_failed_captures_recover_exact_uuid_without_redispatch() {
    for enqueue_failure in [false, true] {
        let mut fixture = ActionFixture::broker();
        let sent = Rc::new(RefCell::new(Vec::new()));
        let connected = Rc::new(std::cell::Cell::new(true));
        let reject_enqueue = Rc::new(std::cell::Cell::new(enqueue_failure));
        fixture.app.collaboration.manager =
            CollaborationManager::with_peer_transport(RecoveringCapturePeer {
                sent: sent.clone(),
                connected: connected.clone(),
                reject_enqueue: reject_enqueue.clone(),
            });
        let address = capture_address(&mut fixture);
        fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .unwrap();
        let (request_id, original) = capture_reports(&sent).pop().unwrap();
        let heads = fixture
            .records
            .read(&fixture.bindings[0].checkout.id, |doc| {
                doc.clone().get_heads()
            })
            .unwrap();
        if !enqueue_failure {
            fixture
                .app
                .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
                    request_id,
                    result: Err(runtime_collab::LocalDocumentActionError::Uncertain(
                        "reply lost".into(),
                    )),
                });
        }
        assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
        assert_eq!(
            fixture.app.document_sync_status(),
            None,
            "a change still on its way to the history says nothing"
        );
        for _ in 0..20 {
            fixture.app.poll_and_apply_collab_events();
        }
        assert_eq!(capture_reports(&sent).len(), 1, "backoff must not spin");
        connected.set(false);
        fixture.app.poll_and_apply_collab_events();
        reject_enqueue.set(false);
        connected.set(true);
        if enqueue_failure {
            // A refused enqueue closes the transport. A reconnect installs a
            // new transport rather than reviving the closed adapter.
            fixture.app.collaboration.manager =
                CollaborationManager::with_peer_transport(RecoveringCapturePeer {
                    sent: sent.clone(),
                    connected: connected.clone(),
                    reject_enqueue: reject_enqueue.clone(),
                });
        }
        fixture.app.poll_and_apply_collab_events();
        let reports = capture_reports(&sent);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[1].0, request_id);
        assert_eq!(reports[1].1.encode().unwrap(), original.encode().unwrap());
        assert_eq!(
            fixture
                .records
                .read(&fixture.bindings[0].checkout.id, |doc| doc
                    .clone()
                    .get_heads())
                .unwrap(),
            heads
        );
        fixture.app.apply_collab_event_for_test(capture_reply(
            request_id,
            original.invocation,
            tilo_actions::InvocationUpdate::Accepted {
                invocation: tilo_actions::InvocationId(original.invocation),
            },
            true,
        ));
        assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
        assert_eq!(fixture.app.local_input_capture.custody_size(), 0);
        assert!(
            fixture
                .app
                .actions
                .local_document_directory_pins
                .iter()
                .all(|pin| pin.strong_count() == 0)
        );
        fixture.app.local_input_capture.retry_now();
        fixture.app.poll_and_apply_collab_events();
        assert_eq!(capture_reports(&sent).len(), 2);
    }
}

#[test]
fn uncertain_local_capture_backpressure_is_bounded_and_releases_on_reconciliation() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    let address = capture_address(&mut fixture);
    let heads = |fixture: &ActionFixture| {
        fixture
            .records
            .read(&fixture.bindings[0].checkout.id, |doc| {
                doc.clone().get_heads()
            })
            .unwrap()
    };
    let mut admitted = 0;
    for _ in 0..65 {
        let before = heads(&fixture);
        fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .expect("uncertain earlier reports never refuse an edit");
        assert_ne!(
            heads(&fixture),
            before,
            "uncertain earlier reports do not block edits"
        );
        let mut reports = capture_reports(&sent);
        if reports.len() == admitted {
            // Every surface that may be held is held by a change on its way:
            // this edit landed without a report.
            break;
        }
        admitted += 1;
        let (request_id, _) = reports.pop().unwrap();
        fixture
            .app
            .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
                request_id,
                result: Err(runtime_collab::LocalDocumentActionError::Uncertain(
                    "reply lost".into(),
                )),
            });
    }
    // Distinct snapshots also have a byte budget, which may fill before
    // the 64-revision limit. Neither bound may evict uncertain custody.
    assert!((2..=64).contains(&admitted));
    assert_eq!(fixture.app.local_input_capture.pending_count(), admitted);
    assert!(fixture.app.local_input_capture.custody_size() < 4 * 1024 * 1024);
    // One more edit still lands; the history cannot carry it, and the window
    // says so rather than dropping the keystroke.
    let before = heads(&fixture);
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .expect("a full history never refuses an edit");
    assert_ne!(heads(&fixture), before, "the edit lands unreported");
    assert_eq!(capture_reports(&sent).len(), admitted);
    assert!(fixture.app.local_input_capture.failure.is_some());
    assert_eq!(
        fixture
            .app
            .actions
            .local_document_directory_pins
            .iter()
            .filter(|pin| pin.strong_count() > 0)
            .count(),
        admitted
    );
    for (request_id, _) in capture_reports(&sent) {
        fixture
            .app
            .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
                request_id,
                result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                    "policy refused".into(),
                )),
            });
    }
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    assert_eq!(capture_reports(&sent).len(), admitted + 1);
    assert_eq!(fixture.app.local_input_capture.pending_count(), 1);
}

/// A click that edits nothing never says anything, whatever the state of
/// the published surface.
#[test]
fn a_click_that_edits_nothing_never_says_anything() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.publish_action_surface();
    for stale in [false, true] {
        if stale && let Some((_, revision)) = fixture.app.actions.action_directory_stamp.as_mut() {
            revision.clear();
        }
        let scope = fixture.app.begin_local_input_capture();
        drop(scope);
        fixture.app.drain_local_input_capture();
        assert_eq!(
            fixture.app.document_sync_status(),
            None,
            "stale surface: {stale}"
        );
    }
    assert!(capture_reports(&sent).is_empty());
}

/// An edit made while the published surface had moved under its input is
/// applied at once, said nothing about while it waits, and recorded with
/// the broker once a surface it can be reported against is published.
#[test]
fn an_edit_during_a_stale_surface_is_recorded_after_the_refresh_quietly() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    fixture.app.collaboration.local_author = fixture.bindings[0].author.clone();
    fixture.app.publish_action_surface();
    // The surface moves under the next input: it cannot be pinned.
    if let Some((_, revision)) = fixture.app.actions.action_directory_stamp.as_mut() {
        revision.clear();
    }
    let record = fixture.bindings[0].checkout.id.clone();
    let before = fixture
        .records
        .read(&record, |doc| doc.clone().get_heads())
        .unwrap();
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
    assert_ne!(
        fixture
            .records
            .read(&record, |doc| doc.clone().get_heads())
            .unwrap(),
        before,
        "the edit is applied at once"
    );
    assert_eq!(
        fixture.app.document_sync_status(),
        None,
        "on its way: nothing to say"
    );

    // The surface refreshes.
    fixture.app.actions.action_directory_stamp = None;
    fixture.app.drain_local_input_capture();
    let reports = capture_reports(&sent);
    assert_eq!(reports.len(), 1, "recorded after the refresh");
    assert!(!reports[0].1.revision.is_empty());
    assert_eq!(
        reports[0].1.bound_invocation().unwrap(),
        reports[0].1.invocation,
        "its identity is bound to the surface it is reported against"
    );
    assert_eq!(fixture.app.document_sync_status(), None);
}

/// A change the broker definitively refuses is said in plain words, with
/// the person's Retry, which reports it again and says nothing while that
/// is on its way.
#[test]
fn a_change_that_cannot_be_saved_says_so_and_can_be_retried() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    let address = capture_address(&mut fixture);
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    let (request_id, _) = capture_reports(&sent).pop().unwrap();
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                "policy refused".into(),
            )),
        });
    assert_eq!(
        fixture.app.document_sync_status().as_deref(),
        Some(crate::i18n::I18nManager::lookup_plain("history-local-action-not-saved").as_str())
    );
    assert!(fixture.app.has_unsaved_changes_to_retry());
    // The header shows Retry beside the message: the window's own action.
    let status = fixture.app.window_status().expect("the header says so");
    let retry = status.action.expect("with Retry beside it");
    assert_eq!(
        retry.label,
        crate::i18n::I18nManager::lookup_plain("history-local-action-retry")
    );
    fixture
        .app
        .invoke_local_resource_action(retry.binding.address(), &serde_json::Map::new())
        .unwrap();
    assert_eq!(fixture.app.document_sync_status(), None);
    assert_eq!(fixture.app.window_status(), None);
    fixture.app.drain_local_input_capture();
    let reports = capture_reports(&sent);
    assert_eq!(reports.len(), 2, "reported again");
    let (_, again) = &reports[1];
    assert!(
        !again.revision.is_empty(),
        "against the surface published now"
    );
    assert_eq!(
        again.bound_invocation().unwrap(),
        again.invocation,
        "its identity is bound to the surface it is reported against"
    );
    assert!(!fixture.app.has_unsaved_changes_to_retry());
}

/// The Retry button in the window's header delivers its control to the
/// window, which runs its own Retry rather than looking for an app action.
#[test]
fn the_header_retry_button_runs_the_windows_retry() {
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    let address = capture_address(&mut fixture);
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    let (request_id, _) = capture_reports(&sent).pop().unwrap();
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                "policy refused".into(),
            )),
        });
    let retry = fixture
        .app
        .window_status()
        .and_then(|status| status.action)
        .expect("Retry beside the message");
    let click = tilo_actions::ControlInvocation {
        address: retry.binding.address().clone(),
        arguments: serde_json::Map::new(),
    };
    assert!(fixture.app.take_history_invocation(&click, None));
    assert_eq!(fixture.app.document_sync_status(), None);
    fixture.app.drain_local_input_capture();
    assert_eq!(capture_reports(&sent).len(), 2, "reported again");
}

/// An app's own connection is no host worker: the broker admits no report on
/// it. The edit stays, saved and unreported; the window stops reporting on
/// that connection, says nothing and offers nothing to retry, and every later
/// edit lands at once.
#[test]
fn edits_on_a_connection_that_admits_no_reports_land_and_say_history_misses_them() {
    let unrecorded = crate::i18n::I18nManager::lookup_plain("history-local-action-unrecorded");
    let mut fixture = ActionFixture::broker();
    let sent = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(CapturePeer(sent.clone()));
    let address = capture_address(&mut fixture);
    let heads = |fixture: &ActionFixture| {
        fixture
            .records
            .read(&fixture.bindings[0].checkout.id, |doc| {
                doc.clone().get_heads()
            })
            .unwrap()
    };
    fixture
        .app
        .invoke_local_resource_action(&address, &serde_json::Map::new())
        .unwrap();
    let (request_id, _) = capture_reports(&sent).pop().unwrap();
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                runtime_collab::LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING.into(),
            )),
        });
    assert_eq!(fixture.app.local_input_capture.pending_count(), 0);
    assert_eq!(
        fixture.app.document_sync_status().as_deref(),
        Some(unrecorded.as_str()),
        "the person is told their history does not record these edits"
    );
    assert!(
        !fixture.app.has_unsaved_changes_to_retry(),
        "a retry would be refused the same way"
    );
    assert_eq!(
        fixture
            .app
            .actions
            .local_document_directory_pins
            .iter()
            .filter(|pin| pin.strong_count() > 0)
            .count(),
        0,
        "no surface is held for a report that cannot be made"
    );
    for _ in 0..200 {
        let before = heads(&fixture);
        fixture
            .app
            .invoke_local_resource_action(&address, &serde_json::Map::new())
            .expect("the edit lands");
        assert_ne!(heads(&fixture), before);
    }
    assert_eq!(capture_reports(&sent).len(), 1, "nothing more is reported");
    assert_eq!(
        fixture.app.document_sync_status().as_deref(),
        Some(unrecorded.as_str())
    );
}
