//! A peer's change and the words this window is typing.
//!
//! Delivery is one moment between two people: a merge is an action
//! boundary of this window's document, so what the person here is typing
//! becomes its own step and the peer's change lands at once — never
//! republished as this window's echo, never a step of this window's undo.
//! What no action owns is held back, because a step no action names cannot
//! be committed, and a queue that overflows says so and recovers from a
//! fresh snapshot.

use super::*;

#[test]
fn remote_delivery_lands_at_the_burst_boundary_and_recovers_overflow() {
    // A peer's change and this person's words arrive at the same moment. A
    // merge is an action boundary, so a burst an action owns is closed and the
    // change lands; a burst no action owns is not a step anyone can commit, so
    // the change waits for the person's own boundary rather than being
    // committed unnamed. Either way what was typed here keeps its place.
    {
        // No action owns these words, so there is no step to end: the change
        // waits, and lands at the person's next boundary without taking the
        // words with it.
        for incremental in [false, true] {
            let mut fixture = RoutedOwners::new();
            let incoming = fixture.remote(0, "remote-key", incremental);
            let checkout = &fixture.bindings[0].checkout;
            let before = fixture
                .records
                .read_checked(&checkout.id, |doc| doc.base_heads())
                .unwrap();
            fixture
                .records
                .with_action_state(&checkout.id, |doc, _, _| {
                    doc.put(ROOT, "local-draft", "unfinished").unwrap();
                })
                .unwrap();
            fixture.app.apply_signed_document_envelope(&incoming, None);
            fixture
                .records
                .read_checked(&checkout.id, |doc| {
                    assert_eq!(doc.base_heads(), before);
                    assert!(doc.pending_ops() > 0);
                })
                .unwrap();
            assert_eq!(fixture.value(0, "remote-key"), None);
            assert!(
                fixture
                    .app
                    .document_sync_status()
                    .unwrap()
                    .contains("edit finishes")
            );
            fixture
                .records
                .with_history(&checkout.id, |doc, history| {
                    history.commit_action_authored(
                        doc,
                        None,
                        None,
                        Some(fixture.bindings[0].author.clone()),
                    );
                })
                .unwrap();
            let undo_revision = fixture
                .records
                .read_history(&checkout.id, |_, history| history.revision())
                .unwrap();
            fixture.app.drain_document_routes();
            assert_eq!(fixture.value(0, "remote-key").as_deref(), Some("remote"));
            assert_eq!(
                fixture.value(0, "local-draft").as_deref(),
                Some("unfinished")
            );
            assert_eq!(
                fixture
                    .records
                    .read_history(&checkout.id, |_, history| history.revision())
                    .unwrap(),
                undo_revision,
                "remote merge creates no local undo entry"
            );
        }
    }

    {
        use automerge::ReadDoc;
        let mut fixture = RoutedOwners::new();
        fixture.app.collaboration.manager = CollaborationManager::disabled();
        fixture.app.actions.bound_broker_route_for_test = None;
        fixture.app.refresh_document_routes();
        fixture
            .records
            .with_history(&fixture.bindings[0].checkout.id, |doc, history| {
                doc.put(ROOT, "offline", "restored locally").unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
        fixture.app.publish_owned_documents();
        assert!(fixture.app.document_routes.sent.is_empty());
        fixture.app.actions.bound_broker_route_for_test =
            Some(crate::collaboration::PrimaryRouteWitness {
                route: crate::collaboration::RouteSpec::app(
                    "outer-shell",
                    fixture.bindings[0].broker_route().unwrap().cluster_id,
                ),
                epoch: uuid::Uuid::new_v4(),
            });
        fixture.app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
        // Resolve the broker's document mode on the new secondary route before
        // any document is eligible for publication. Production retries each frame.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            fixture.app.publish_owned_documents();
            if fixture
                .app
                .document_routes
                .sent
                .iter()
                .any(|(route, _)| Some(route) == fixture.bindings[0].broker_route().as_ref())
            {
                break;
            }
            assert!(
                crate::time::now() < deadline,
                "reconnected route did not become publishable"
            );
            std::thread::sleep(crate::time::Duration::from_millis(10));
        }
        let (_, request) = fixture
            .app
            .document_routes
            .sent
            .iter()
            .find(|(route, _)| Some(route) == fixture.bindings[0].broker_route().as_ref())
            .unwrap();
        let CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(data),
            ..
        } = request
        else {
            panic!("reconnect needs fresh snapshot")
        };
        let doc = automerge::AutoCommit::load(data).unwrap();
        assert_eq!(
            doc.get(ROOT, "offline").unwrap().unwrap().0.as_str(),
            Some("restored locally")
        );
    }

    {
        // An action owns these words and its burst is still open: the merge
        // ends that step as an action boundary, so the change lands now
        // instead of after the person's next click, and the words are the step
        // the merge ended.
        let mut fixture = RoutedOwners::new();
        let incoming = fixture.remote(0, "remote-logical", true);
        let binding = fixture.bindings[0].clone();
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "logical-draft", "unfinished").unwrap();
            })
            .unwrap();
        let before = fixture
            .records
            .read_checked(&binding.checkout.id, |doc| {
                assert!(doc.pending_ops() > 0, "the burst is still open");
                doc.base_heads()
            })
            .unwrap();
        let authored = fixture
            .records
            .read_history(&binding.checkout.id, |_, history| history.revision())
            .unwrap();
        fixture.app.apply_signed_document_envelope(&incoming, None);
        assert_eq!(
            fixture.value(0, "remote-logical").as_deref(),
            Some("remote"),
            "the change lands while the burst is open"
        );
        assert_eq!(
            fixture.value(0, "logical-draft").as_deref(),
            Some("unfinished"),
            "and the words typed here are not lost with it"
        );
        assert_ne!(
            fixture
                .records
                .read_checked(&binding.checkout.id, |doc| doc.base_heads())
                .unwrap(),
            before,
            "the record took the peer's change in"
        );
        // The burst is the one step the merge authored: the words the person
        // typed, which they can still take back. The peer's change is its
        // author's step and joins none of this window's.
        let merged = fixture
            .records
            .read_history(&binding.checkout.id, |_, history| history.revision())
            .unwrap();
        assert_eq!(
            merged,
            authored + 1,
            "closing the burst authors the words, and nothing else"
        );
        assert!(
            binding
                .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
                .unwrap()
                .is_some_and(|step| step.author == Some(fixture.bindings[0].author.clone())),
            "taking the words back takes back this person's step, not the peer's change"
        );
        let undo_revision = fixture
            .records
            .read_history(&binding.checkout.id, |_, history| history.revision())
            .unwrap();
        fixture.app.drain_document_routes();
        assert_eq!(
            fixture.value(0, "remote-logical").as_deref(),
            Some("remote"),
            "the change landed once, and is still there"
        );
        assert_eq!(
            fixture
                .records
                .read_history(&binding.checkout.id, |_, history| history.revision())
                .unwrap(),
            undo_revision,
            "network applies no new authored undo boundary"
        );
    }

    {
        let mut fixture = RoutedOwners::new();
        fixture.install_history();
        let incoming = fixture.remote(0, "remote-key", false);
        let checkout = &fixture.bindings[0].checkout;
        fixture
            .records
            .with_action_state(&checkout.id, |doc, _, _| {
                doc.put(ROOT, "draft", "pending").unwrap();
            })
            .unwrap();
        for _ in 0..65 {
            fixture.app.apply_signed_document_envelope(&incoming, None);
        }
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("Too many changes from others")
        );
        layout(&mut fixture.app);
        assert!(
            fixture
                .app
                .build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node
                    .label()
                    .is_some_and(|label| label.contains("Too many changes from others"))),
            "recovery is visible in shared History chrome"
        );
        fixture
            .records
            .with_history(&checkout.id, |doc, history| {
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
        fixture.app.drain_document_routes();
        assert_eq!(
            fixture.value(0, "remote-key").as_deref(),
            Some("remote"),
            "previously admitted entries were retained"
        );
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("Too many changes from others")
        );
        fixture.app.apply_signed_document_envelope(&incoming, None);
        assert_eq!(fixture.app.document_sync_status(), None);
    }
}

#[test]
fn native_owned_remote_merge_is_not_republished_as_an_echo() {
    let mut fixture = RoutedOwners::new();
    let id = fixture.bindings[0].checkout.id.clone();
    let route = fixture.bindings[0].broker_route().unwrap();
    // Only the first owner is under test; the second may publish its own
    // initial snapshot whenever its asynchronous route join completes.
    let sent = |fixture: &RoutedOwners| {
        fixture
            .app
            .document_routes
            .sent
            .iter()
            .filter(|(sent, _)| *sent == route)
            .map(|(_, request)| request.clone())
            .collect::<Vec<_>>()
    };
    let heads = |fixture: &RoutedOwners| {
        fixture
            .records
            .read_checked(&id, |doc| doc.base_heads())
            .unwrap()
    };
    let published = |fixture: &RoutedOwners| match &sent(fixture)[..] {
        [request] => match request {
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Incremental(bytes),
                ..
            } => bytes.clone(),
            other => panic!("expected an incremental publication, got {other:?}"),
        },
        other => panic!("expected exactly one publication, got {}", other.len()),
    };
    let commit_local = |fixture: &RoutedOwners, value: &str| {
        fixture
            .records
            .with_history(&id, |doc, history| {
                doc.put(ROOT, "local", value).unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
    };

    // The initial snapshot carries an authored change (the bare factory
    // record has no publishable provenance).
    commit_local(&fixture, "initial");
    // The fake broker admits the route join asynchronously; publication is
    // refused (and retried) until the route is publishable.
    let deadline = tilo_test_env::hang_deadline();
    while sent(&fixture).is_empty() {
        fixture.app.publish_owned_documents();
        assert!(
            crate::time::now() < deadline,
            "route never became publishable"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    assert_eq!(sent(&fixture).len(), 1, "initial snapshot");
    fixture.app.document_routes.sent.clear();

    // Fully published before the merge: the merged remote history is held by
    // its sender and owner, so it is not echoed back as our Incremental.
    for incremental in [true, false] {
        let incoming = fixture.remote(0, &format!("remote-{incremental}"), incremental);
        fixture.app.apply_signed_document_envelope(&incoming, None);
        assert_eq!(
            fixture
                .value(0, &format!("remote-{incremental}"))
                .as_deref(),
            Some("remote")
        );
        fixture.app.publish_owned_documents();
        assert!(
            sent(&fixture).is_empty(),
            "a merged remote change must not be republished"
        );
    }

    // A later local edit publishes exactly its own change.
    let before = heads(&fixture);
    commit_local(&fixture, "after merge");
    fixture.app.publish_owned_documents();
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    assert_eq!(published(&fixture), expected);
    fixture.app.document_routes.sent.clear();

    // Unsent local history interleaved with a merge is still published on its
    // own: the merged remote change is held by the route already and carrying
    // it back would be an echo no action names.
    let before = heads(&fixture);
    commit_local(&fixture, "unsent before merge");
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    let incoming = fixture.remote(0, "interleaved-remote", true);
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(
        fixture.value(0, "interleaved-remote").as_deref(),
        Some("remote")
    );
    fixture.app.publish_owned_documents();
    assert_eq!(
        published(&fixture),
        expected,
        "the next publication carries this replica's unsent change only"
    );
}
/// A committed read hands the owner history the native owner already holds:
/// merging it must not make the replica publish it back, which cost the
/// owner one more commit and record write per remote edit.
#[cfg(feature = "broker-transport")]
#[test]
fn native_owned_committed_read_is_not_republished_as_an_echo() {
    let mut fixture = RoutedOwners::new();
    let id = fixture.bindings[0].checkout.id.clone();
    let route = fixture.bindings[0].broker_route().unwrap();
    let sent = |fixture: &RoutedOwners| {
        fixture
            .app
            .document_routes
            .sent
            .iter()
            .filter(|(sent, _)| *sent == route)
            .map(|(_, request)| request.clone())
            .collect::<Vec<_>>()
    };
    let heads = |fixture: &RoutedOwners| {
        fixture
            .records
            .read_checked(&id, |doc| doc.base_heads())
            .unwrap()
    };
    let commit_local = |fixture: &RoutedOwners, value: &str| {
        fixture
            .records
            .with_history(&id, |doc, history| {
                doc.put(ROOT, "local", value).unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
    };
    // A remote change as a committed read: only the changes since this
    // replica's heads.
    let committed = |fixture: &RoutedOwners, name: &str| {
        let mut doc = fixture.records.read_checked(&id, Clone::clone).unwrap();
        let base = doc.base_heads();
        doc.set_actor(automerge::ActorId::from(b"committed-peer".as_slice()));
        doc.put(ROOT, name, "remote").unwrap();
        doc.commit();
        crate::collaboration::CommittedDocument::for_test(doc.save_after(&base), Some(base))
    };

    commit_local(&fixture, "initial");
    let deadline = tilo_test_env::hang_deadline();
    while sent(&fixture).is_empty() {
        fixture.app.publish_owned_documents();
        assert!(
            crate::time::now() < deadline,
            "route never became publishable"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    fixture.app.document_routes.sent.clear();
    // The same record is also the app's primary document, published on its
    // own frontier.
    fixture.app.actions.bound_broker_route_for_test =
        Some(crate::collaboration::PrimaryRouteWitness {
            route: route.clone(),
            epoch: uuid::Uuid::new_v4(),
        });
    let primary = |fixture: &mut RoutedOwners| fixture.app.pending_primary_document().unwrap();
    let (_, published, _) = primary(&mut fixture).expect("the record whole at first");
    fixture.app.advance_primary_document(published);
    assert!(primary(&mut fixture).is_none());
    // Binding the primary document has the route republish its record
    // whole once; after that nothing is pending on either.
    fixture.app.publish_owned_documents();
    fixture.app.document_routes.sent.clear();
    fixture.app.publish_owned_documents();
    assert!(sent(&fixture).is_empty(), "fully published before any read");

    for edit in 0..3 {
        let name = format!("committed-{edit}");
        let read = committed(&fixture, &name);
        fixture
            .app
            .queue_committed_document(route.clone(), read.clone());
        assert!(read.was_applied(), "the owner merged the committed read");
        assert_eq!(fixture.value(0, &name).as_deref(), Some("remote"));
        fixture.app.publish_owned_documents();
        assert!(
            sent(&fixture).is_empty(),
            "a committed remote change must not be republished, got {:?}",
            sent(&fixture)
        );
        assert!(
            primary(&mut fixture).is_none(),
            "nor as the primary document"
        );
    }

    // A later local edit publishes exactly its own change.
    let before = heads(&fixture);
    commit_local(&fixture, "after committed reads");
    fixture.app.publish_owned_documents();
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    match &sent(&fixture)[..] {
        [
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Incremental(bytes),
                ..
            },
        ] => assert_eq!(bytes, &expected),
        other => panic!("expected exactly the local incremental, got {other:?}"),
    }
    fixture.app.document_routes.sent.clear();
    let (mutation, published, _) = primary(&mut fixture).expect("the local change travels");
    assert_eq!(
        mutation.data(),
        expected,
        "the primary document publishes it too"
    );
    fixture.app.advance_primary_document(published);

    // Unsent local history interleaved with a committed read is still
    // published on its own: the read's history is held by the owner that
    // served it and is not echoed back.
    let before = heads(&fixture);
    commit_local(&fixture, "unsent before committed read");
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    let read = committed(&fixture, "interleaved-committed");
    fixture
        .app
        .queue_committed_document(route.clone(), read.clone());
    assert!(read.was_applied());
    fixture.app.publish_owned_documents();
    match &sent(&fixture)[..] {
        [
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Incremental(bytes),
                ..
            },
        ] => assert_eq!(
            bytes, &expected,
            "the next publication carries this replica's unsent change only"
        ),
        other => panic!("expected one incremental with the unsent local change, got {other:?}"),
    }
    assert_eq!(
        primary(&mut fixture)
            .map(|(mutation, _, _)| mutation.data().to_vec())
            .expect("the primary document keeps them too"),
        expected,
        "and the primary document keeps them too"
    );
}

/// A committed read that arrives while the person is mid-word must not wait
/// for their next input: its delivery turn holds every action queued behind
/// it, so an idle window with an unfinished word stalled the workspace. The merge
/// closes the word as its own step, named by the action typing it, keeps it
/// and applies at once. A step no action names stays open instead: committed
/// unnamed, the identity service would refuse to sign it.
#[cfg(feature = "broker-transport")]
#[test]
fn native_owned_committed_read_applies_over_an_unfinished_word() {
    fn committed_read(
        fixture: &RoutedOwners,
    ) -> (
        crate::collaboration::CommittedDocument,
        Vec<automerge::ChangeHash>,
    ) {
        let id = &fixture.bindings[0].checkout.id;
        let mut remote = fixture.records.read_checked(id, Clone::clone).unwrap();
        let base = remote.base_heads();
        remote.set_actor(automerge::ActorId::from(b"committed-peer".as_slice()));
        remote.put(ROOT, "remote-key", "remote").unwrap();
        remote.commit();
        let read = crate::collaboration::CommittedDocument::for_test(
            remote.save_after(&base),
            Some(base.clone()),
        );
        (read, base)
    }
    fn unnamed_local_changes(fixture: &RoutedOwners, since: &[automerge::ChangeHash]) -> usize {
        fixture
            .records
            .read_checked(&fixture.bindings[0].checkout.id, |doc| {
                // Only what is committed: reading changes of a copy would
                // commit its still-open edit first.
                let committed = doc.base_heads();
                doc.clone()
                    .fork_at(&committed)
                    .unwrap()
                    .get_changes(since)
                    .into_iter()
                    .filter(|change| change.actor_id().to_bytes() != b"committed-peer")
                    .filter(|change| {
                        collab_types::ChangeClaim::of_change(
                            change.hash().0,
                            change.message(),
                            Vec::new(),
                        )
                        .is_err()
                    })
                    .count()
            })
            .unwrap()
    }

    // The word is typed through its action: the merge names and closes it.
    {
        let mut fixture = RoutedOwners::new();
        let route = fixture.bindings[0].broker_route().unwrap();
        let (read, base) = committed_read(&fixture);
        let binding = fixture.bindings[0].clone();
        binding
            .with_document(&mut fixture.app.state.doc, |doc, _| {
                doc.put(ROOT, "local-draft", "unfinished").unwrap();
            })
            .unwrap();
        fixture
            .app
            .queue_committed_document(route.clone(), read.clone());
        assert!(
            read.was_applied(),
            "the committed read merged without waiting for further input"
        );
        assert!(fixture.app.committed_documents.is_empty());
        assert_eq!(fixture.value(0, "remote-key").as_deref(), Some("remote"));
        assert_eq!(
            fixture.value(0, "local-draft").as_deref(),
            Some("unfinished"),
            "the unfinished word is kept"
        );
        fixture
            .records
            .read_checked(&binding.checkout.id, |doc| {
                assert_eq!(doc.pending_ops(), 0, "the word is a finished step");
            })
            .unwrap();
        assert_eq!(
            unnamed_local_changes(&fixture, &base),
            0,
            "the closed word names its action"
        );
    }

    // Nothing names the open edit: it is neither committed unnamed nor lost.
    {
        let mut fixture = RoutedOwners::new();
        let id = fixture.bindings[0].checkout.id.clone();
        let route = fixture.bindings[0].broker_route().unwrap();
        let (read, base) = committed_read(&fixture);
        fixture
            .records
            .with_action_state(&id, |doc, _, _| {
                doc.put(ROOT, "stray-draft", "unfinished").unwrap();
            })
            .unwrap();
        fixture
            .app
            .queue_committed_document(route.clone(), read.clone());
        assert!(!read.was_applied());
        assert_eq!(fixture.app.committed_documents.len(), 1);
        fixture
            .records
            .read_checked(&id, |doc| assert!(doc.pending_ops() > 0))
            .unwrap();
        assert_eq!(unnamed_local_changes(&fixture, &base), 0);
        assert_eq!(
            fixture.value(0, "stray-draft").as_deref(),
            Some("unfinished")
        );
    }
}

/// A peer's change lands mid-word: the word closes as its own step instead of the change waiting.
#[test]
fn a_peers_change_lands_while_the_person_here_is_typing() {
    let mut fixture = RoutedOwners::new();
    let incoming = fixture.remote(0, "peer-line", true);
    let binding = fixture.bindings[0].clone();
    let typed = binding.with_document(&mut fixture.app.state.doc, |doc, _| {
        doc.put(ROOT, "local-draft", "unfinished").unwrap();
    });
    typed.unwrap();
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(fixture.value(0, "peer-line").as_deref(), Some("remote"));
    assert_eq!(
        fixture.value(0, "local-draft").as_deref(),
        Some("unfinished")
    );
    let records = &fixture.records;
    let pending = records.read_checked(&binding.checkout.id, |doc| doc.pending_ops());
    assert_eq!(pending.unwrap(), 0, "the word is a finished step");
}
