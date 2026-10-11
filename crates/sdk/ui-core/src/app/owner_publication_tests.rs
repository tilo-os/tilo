// Checked component regressions for the pre-terminal document handoff.
thread_local! { static PUBLICATION_NORMAL_POLL: Cell<bool> = const { Cell::new(false) }; }
struct PublicationTransport(Rc<RefCell<Vec<CollabRequest>>>);
impl crate::collaboration::PeerTransport for PublicationTransport {
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
    fn take_primary_route_result(
        &mut self,
    ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
        None
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
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn owner_publication_blocked_checked_action_cannot_send_terminal() {
    for background in [false, true] {
        let mut fixture = ActionFixture::broker();
        let sent = Rc::new(RefCell::new(vec![]));
        fixture.app.collaboration.manager =
            CollaborationManager::with_peer_transport(PublicationTransport(sent.clone()));
        fixture.app.publication_resources();
        let resource = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[0], "bound:probe")
            .unwrap();
        fixture.app.publish_owned_documents();
        assert!(!fixture.app.document_routes.owners.is_empty());
        sent.borrow_mut().clear();
        fixture.app.document_routes.owners[0].pending.failure =
            Some("Remote merge needs recovery".into());
        let record = fixture.bindings[0].checkout.id.clone();
        let before = fixture
            .records
            .read(&record, |doc| doc.base_heads())
            .unwrap();
        let invocation = tilo_actions::InvocationId::new();
        fixture.app.invoke_published_action(
            72,
            invocation,
            resource.clone(),
            if background { "follow-up" } else { "apply" }.into(),
            None,
            "fixture-user",
            None,
        );
        let after = fixture
            .records
            .read(&record, |doc| doc.base_heads())
            .unwrap();
        assert_ne!(
            before, after,
            "the checked operation must actually commit its local owner"
        );
        if background {
            fixture
                .app
                .actions
                .directed_invocations
                .get_mut(&invocation)
                .unwrap()
                .cancellation_requested = true;
            PUBLICATION_NORMAL_POLL.set(true);
            fixture.app.poll_directed_invocations();
            PUBLICATION_NORMAL_POLL.set(false);
        }
        assert!(
            !sent.borrow().iter().any(|request| matches!(request,
            CollabRequest::UpdateInvocation { update, .. } if update.is_terminal())),
            "a skipped owned document must never be followed by a terminal outcome"
        );
        assert!(
            !sent
                .borrow()
                .iter()
                .any(|request| matches!(request, CollabRequest::Document { .. }))
        );
        assert!(
            !fixture.app.collaboration.manager.is_connected(),
            "publication omission must withdraw health"
        );
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("outcome unknown")
        );
        if background {
            assert!(
                fixture
                    .app
                    .actions
                    .directed_invocations
                    .contains_key(&invocation)
            );
            assert!(
                fixture
                    .app
                    .actions
                    .bound_invocations
                    .contains_key(&invocation)
            );
        }
        let captures = CAPTURES.with_borrow(|items| items.len());
        fixture.app.invoke_published_action(
            72,
            invocation,
            resource,
            if background { "follow-up" } else { "apply" }.into(),
            None,
            "fixture-user",
            None,
        );
        fixture.app.poll_directed_invocations();
        assert_eq!(
            CAPTURES.with_borrow(|items| items.len()),
            captures,
            "an uncertain completion must not re-enter the checked operation or poll its completed job"
        );
        assert_eq!(
            fixture
                .records
                .read(&record, |doc| doc.base_heads())
                .unwrap(),
            after
        );
        fixture.app.close_components();
    }
}

// A controlled transport exercises the actual local action door. It supplies
// no invocation authorization, so queued delivery alone cannot mutate a record.
struct NativeLocalDoorTransport {
    open: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sent: std::sync::Arc<std::sync::Mutex<Vec<CollabRequest>>>,
    acknowledgement: Option<crate::collaboration::RouteSpec>,
}
impl crate::collaboration::CollabTransport for NativeLocalDoorTransport {
    fn status(&self) -> crate::collaboration::TransportStatus {
        if self.open.load(std::sync::atomic::Ordering::Acquire) {
            crate::collaboration::TransportStatus::Ready
        } else {
            crate::collaboration::TransportStatus::Closed("native connection ended".into())
        }
    }
    fn submit(
        &mut self,
        command: crate::collaboration::TransportCommand,
    ) -> Result<(), crate::collaboration::TransportError> {
        use crate::collaboration::{TransportCommand, TransportError};
        if !self.open.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        if let TransportCommand::Primary(request) = command {
            if let CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } = &request
            {
                self.acknowledgement = Some(crate::collaboration::RouteSpec {
                    app_id: app_id.clone(),
                    cluster_id: *cluster_id,
                    record: record.clone(),
                });
            }
            self.sent.lock().unwrap().push(request);
        }
        Ok(())
    }
    fn drain(&mut self, _: &mut Vec<CollabEvent>) {}
    fn take_primary_route_result(
        &mut self,
    ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
        self.acknowledgement.take().map(Ok)
    }
    fn take_action_directory_requests(
        &mut self,
        _: &mut Vec<crate::collaboration::ActionDirectoryRequest>,
    ) {
    }
    fn committed_document_required(&self, _: &str) -> bool {
        true
    }
}

#[test]
fn owner_publication_native_local_document_waits_for_real_delivery_and_refuses_closed_route() {
    for connected in [true, false] {
        let mut fixture = ActionFixture::broker();
        let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sent = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let route = fixture.bindings[0].broker_route().unwrap();
        let mut manager =
            CollaborationManager::with_transport(Box::new(NativeLocalDoorTransport {
                open: open.clone(),
                sent: sent.clone(),
                acknowledgement: None,
            }));
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: route.record.clone(),
            record_kind: None,
            handle: None
        }));
        let _ = manager.poll();
        assert!(manager.has_native_committed_primary());
        open.store(connected, std::sync::atomic::Ordering::Release);
        fixture.app.collaboration.manager = manager;
        let target = fixture.target(0, "apply");
        let record = fixture.bindings[0].checkout.id.clone();
        let before = fixture
            .records
            .read(&record, |doc| doc.base_heads())
            .unwrap();
        let request = ResourceInvocation::new(&target.address.address, "fixture-user");
        let result = fixture.app.invoke_bound_target(&target, &request);
        assert_eq!(result.is_ok(), connected);
        assert_eq!(
            fixture
                .records
                .read(&record, |doc| doc.base_heads())
                .unwrap(),
            before,
            "neither queued capture nor closed native custody may run a scopeNone handler"
        );
        assert_eq!(
            sent.lock()
                .unwrap()
                .iter()
                .filter(|request| matches!(request, CollabRequest::RemoteAction { .. }))
                .count(),
            usize::from(connected)
        );
    }
}
