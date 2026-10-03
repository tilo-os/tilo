#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end sync test keeps one transport and undo timeline visible"
)]
#[test]
fn native_bound_sync_routes_verified_bytes_and_preserves_undo_retries_and_dependencies() {
    // Case `native_bound_sync_routes_notebook_bytes_and_preserves_local_undo_and_retries`:
    // Exercise the same record selection, serialization and merge used by App's
    // broker pump. The fixture provides an admitted route without a live socket.
    {
        let mut fixture = ActionFixture::new();
        let binding = fixture.bindings[0].clone();
        fixture.app.actions.bound_broker_route_for_test =
            Some(crate::collaboration::PrimaryRouteWitness {
                route: binding.broker_route().unwrap(),
                epoch: uuid::Uuid::new_v4(),
            });
        fixture
            .app
            .state
            .doc
            .put(ROOT, "host-only", "never replicated")
            .unwrap();
        let outer = fixture.app.state.doc.get_heads();
        let other = fixture
            .records
            .read(
                &fixture.bindings[1].checkout.id,
                automerge::transaction::Transactable::base_heads,
            )
            .unwrap();
        let (initial, heads) = fixture.app.pending_primary_document_delta().unwrap();
        let mut remote = automerge::AutoCommit::new();
        remote.load_incremental(&initial).unwrap();
        assert_eq!(crate::record::identity(&remote).unwrap(), binding.checkout);
        assert!(remote.get(ROOT, "host-only").unwrap().is_none());
        fixture.app.advance_primary_document(heads);
        let shared = remote.get_heads();
        remote.set_actor(automerge::ActorId::from(b"remote-notebook".as_slice()));
        remote
            .put(ROOT, "remote-only", "kept after local undo")
            .unwrap();

        // An unfinished local typing action, not a test-only history entry.
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "local-only", "retry me").unwrap();
            })
            .unwrap();
        let (rejected, _) = fixture.app.pending_primary_document_delta().unwrap();
        assert!(!rejected.is_empty());
        // Saving or a late-join snapshot must not consume the transport cursor.
        let snapshot = fixture.app.primary_document_snapshot().unwrap();
        assert_eq!(
            crate::record::identity(&automerge::AutoCommit::load(&snapshot).unwrap()).unwrap(),
            binding.checkout
        );
        fixture.records.try_flush().unwrap();
        assert_eq!(
            fixture.app.pending_primary_document_delta().unwrap().0,
            rejected
        );
        assert!(
            fixture
                .app
                .apply_primary_document(collab_types::DocumentMutation::Incremental(
                    remote.save_after(&shared)
                ))
                .unwrap()
        );
        let (retry, accepted) = fixture.app.pending_primary_document_delta().unwrap();
        assert!(
            !retry.is_empty(),
            "inbound changes cannot acknowledge a rejected local send"
        );
        remote.load_incremental(&retry).unwrap();
        assert_eq!(
            remote.get(ROOT, "local-only").unwrap().unwrap().0.as_str(),
            Some("retry me")
        );
        assert_eq!(
            remote.get(ROOT, "remote-only").unwrap().unwrap().0.as_str(),
            Some("kept after local undo")
        );
        fixture.app.advance_primary_document(accepted);
        assert!(
            fixture
                .app
                .pending_primary_document_delta()
                .unwrap()
                .0
                .is_empty(),
            "no inbound echo"
        );
        assert!(
            binding
                .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .is_some()
        );
        fixture
            .records
            .read(&binding.checkout.id, |doc| {
                assert!(doc.get(ROOT, "local-only").unwrap().is_none());
                assert_eq!(
                    doc.get(ROOT, "remote-only").unwrap().unwrap().0.as_str(),
                    Some("kept after local undo")
                );
            })
            .unwrap();
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
        assert_eq!(
            fixture
                .records
                .read(
                    &fixture.bindings[1].checkout.id,
                    automerge::transaction::Transactable::base_heads,
                )
                .unwrap(),
            other
        );
        fixture.app.close_components();
        assert!(fixture.app.actions.bound_record_sync.is_none());
    }

    // Case `native_bound_sync_refuses_foreign_or_corrupt_bytes_without_closing_typing`.
    {
        let mut fixture = ActionFixture::broker();
        let binding = fixture.bindings[0].clone();
        let mut peer = fixture
            .records
            .read(&binding.checkout.id, Clone::clone)
            .unwrap();
        let base = peer.get_heads();
        peer.set_actor(automerge::ActorId::from(b"foreign-record".as_slice()));
        let meta = peer.get(ROOT, "tilo:record").unwrap().unwrap().1;
        let mut mutations = vec![collab_types::DocumentMutation::Incremental(vec![1, 2, 3])];
        for (field, value) in [
            ("id", "uuid:00000000-0000-0000-0000-000000000099"),
            ("kind", "foreign-kind"),
            ("room", "00000000-0000-0000-0000-000000000099"),
            ("home", "foreign-origin"),
        ] {
            let mut foreign = peer.clone();
            foreign.put(&meta, field, value).unwrap();
            mutations.push(collab_types::DocumentMutation::Snapshot(foreign.save()));
            mutations.push(collab_types::DocumentMutation::Incremental(
                foreign.save_after(&base),
            ));
        }
        peer.put(ROOT, "must-not-leak", "valid prefix of corrupt payload")
            .unwrap();
        let mut corrupt = peer.save_after(&base);
        corrupt.extend_from_slice(&[1, 2, 3]);
        mutations.push(collab_types::DocumentMutation::Incremental(corrupt));
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "local-pending", "still typing").unwrap();
            })
            .unwrap();
        let before = fixture
            .records
            .read_history(&binding.checkout.id, |doc, history| {
                (doc.base_heads(), doc.pending_ops(), history.revision())
            })
            .unwrap();
        for (case, mutation) in mutations.into_iter().enumerate() {
            assert!(
                fixture.app.apply_primary_document(mutation).is_err(),
                "invalid mutation {case}"
            );
            assert_eq!(
                fixture
                    .records
                    .read_history(&binding.checkout.id, |doc, history| (
                        doc.base_heads(),
                        doc.pending_ops(),
                        history.revision()
                    ))
                    .unwrap(),
                before
            );
        }
        // Continue the same logical typing action after all refusals.
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "local-pending", "typing continued").unwrap();
            })
            .unwrap();
        assert!(
            binding
                .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .records
                .read(&binding.checkout.id, |doc| doc
                    .get(ROOT, "local-pending")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        fixture.app.close_components();
    }

    // Case `native_bound_sync_retains_out_of_order_dependencies_without_echoing_them`.
    {
        let mut fixture = ActionFixture::broker();
        let (initial, heads) = fixture.app.pending_primary_document_delta().unwrap();
        fixture.app.advance_primary_document(heads);
        let binding = fixture.bindings[0].clone();
        let mut remote = automerge::AutoCommit::load(&initial).unwrap();
        remote.set_actor(automerge::ActorId::from(b"out-of-order-peer".as_slice()));
        let shared = remote.get_heads();
        remote.put(ROOT, "first-remote", "one").unwrap();
        let first = remote.save_after(&shared);
        let first_heads = remote.get_heads();
        remote.put(ROOT, "second-remote", "two").unwrap();
        let second = remote.save_after(&first_heads);
        assert!(
            !fixture
                .app
                .apply_primary_document(collab_types::DocumentMutation::Incremental(second))
                .unwrap()
        );
        assert!(
            fixture
                .app
                .apply_primary_document(collab_types::DocumentMutation::Incremental(first))
                .unwrap()
        );
        fixture
            .records
            .read(&binding.checkout.id, |doc| {
                assert_eq!(
                    doc.get(ROOT, "first-remote").unwrap().unwrap().0.as_str(),
                    Some("one")
                );
                assert_eq!(
                    doc.get(ROOT, "second-remote").unwrap().unwrap().0.as_str(),
                    Some("two")
                );
            })
            .unwrap();
        assert!(
            fixture
                .app
                .pending_primary_document_delta()
                .unwrap()
                .0
                .is_empty()
        );
        assert!(
            !fixture
                .app
                .apply_primary_document(collab_types::DocumentMutation::Snapshot(remote.save()))
                .unwrap()
        );
        assert!(
            fixture
                .records
                .read_history(&binding.checkout.id, |_, history| history
                    .next_undo_in(&binding.author, binding.instance_id)
                    .is_none())
                .unwrap()
        );
        fixture.app.close_components();
    }

    // Case `native_bound_sync_requires_live_matching_route_and_resends_after_reconnection`.
    {
        let mut fixture = ActionFixture::broker();
        let (initial, heads) = fixture.app.pending_primary_document_delta().unwrap();
        assert!(!initial.is_empty());
        fixture.app.advance_primary_document(heads);
        assert!(
            fixture
                .app
                .pending_primary_document_delta()
                .unwrap()
                .0
                .is_empty()
        );
        // Two views share one record; changing transport epoch must resend that
        // record, even though its heads and both view lifetimes are unchanged.
        fixture
            .app
            .actions
            .bound_broker_route_for_test
            .as_mut()
            .unwrap()
            .epoch = uuid::Uuid::new_v4();
        assert!(
            !fixture
                .app
                .pending_primary_document_delta()
                .unwrap()
                .0
                .is_empty()
        );
        let route = fixture
            .app
            .actions
            .bound_broker_route_for_test
            .take()
            .unwrap();
        let outer = fixture.app.state.doc.get_heads();
        assert!(fixture.app.primary_document_snapshot().is_err());
        assert!(fixture.app.pending_primary_document_delta().is_err());
        let mut wrong = route.clone();
        wrong.route.app_id = "com.example.foreign".into();
        fixture.app.actions.bound_broker_route_for_test = Some(wrong);
        assert!(fixture.app.primary_document_snapshot().is_err());
        fixture.app.actions.bound_broker_route_for_test = Some(route);
        assert!(!fixture.app.primary_document_snapshot().unwrap().is_empty());
        fixture.app.close_components();
        assert!(
            fixture.app.primary_document_snapshot().is_err(),
            "closed components cannot fall back to the outer document"
        );
        assert_eq!(fixture.app.state.doc.get_heads(), outer);
    }
}
