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
