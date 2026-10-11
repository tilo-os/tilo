use super::*;

#[test]
fn pointer_broadcast_throttle_gates_by_gap_and_resets_on_scope() {
    // Case `pointer_broadcast_allowed_first_call_always_passes`.
    {
        let now = crate::time::now();
        assert!(pointer_broadcast_allowed(
            None,
            now,
            Duration::from_millis(33)
        ));
    }

    // Case `pointer_broadcast_allowed_blocks_inside_gap`.
    {
        let t0 = crate::time::now();
        let t1 = t0 + Duration::from_millis(10);
        assert!(!pointer_broadcast_allowed(
            Some(t0),
            t1,
            Duration::from_millis(33),
        ));
    }

    // Case `pointer_broadcast_allowed_passes_at_and_past_gap`.
    {
        // Exactly at the gap boundary is allowed — strict `>` would
        // accumulate single-ms drift under a sawtooth wakeup schedule.
        let t0 = crate::time::now();
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(33),
            Duration::from_millis(33),
        ));
        assert!(pointer_broadcast_allowed(
            Some(t0),
            t0 + Duration::from_millis(50),
            Duration::from_millis(33),
        ));
    }

    // Case `set_presence_broadcast_scope_resets_throttle`.
    {
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        app.presence.last_pointer_broadcast = Some(crate::time::now());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: "home".into() }));
        assert!(
            app.presence.last_pointer_broadcast.is_none(),
            "scope change must clear the throttle so the next move broadcasts immediately"
        );
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace { id: "home".into() })
        );
    }
}

#[test]
fn document_broadcast_merges_snapshots_and_leaves_commits_incremental() {
    // Case `a_commit_leaves_the_edit_for_the_incremental_broadcast`:
    // Guards against `commit_action` serialising the whole document (which
    // advances `AutoCommit::save`'s cursor and leaves `broadcast_doc_delta`
    // nothing to send).
    {
        use automerge::transaction::Transactable;

        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        // Drain the baseline so what the assertion sees is this edit alone.
        let _ = app.state.save_incremental();

        app.state_mut_for_test()
            .doc
            .put(&automerge::ROOT, "body", "the quarterly report")
            .expect("put on the root object");
        app.commit_action();

        assert!(
            !app.state.save_incremental().is_empty(),
            "the committed edit must survive as an incremental delta; an \
         intervening full save() would have advanced the save cursor and \
         left broadcast_doc_delta with nothing to send"
        );
    }

    // Case `document_snapshot_merges_not_replaces`.
    {
        use automerge::transaction::Transactable;
        use automerge::{ObjType, ROOT, ReadDoc};
        let mut base = automerge::AutoCommit::new();
        let m = base.put_object(&ROOT, "m", ObjType::Map).unwrap();
        base.put(&m, "x", 1).unwrap();
        let base_bytes = base.save();

        let app_state = State::load(&base_bytes).unwrap();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(app_state, root);
        let route = crate::collaboration::RouteSpec::app("snapshot-fixture", uuid::Uuid::new_v4());
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
            route: route.clone(),
            epoch: uuid::Uuid::new_v4(),
        });
        {
            let doc = &mut app.state_mut_for_test().doc;
            let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
                panic!("m exists")
            };
            doc.put(&m, "a", 10).unwrap();
        }

        let mut peer = automerge::AutoCommit::load(&base_bytes).unwrap();
        let Ok(Some((automerge::Value::Object(ObjType::Map), pm))) = peer.get(&ROOT, "m") else {
            panic!("m exists")
        };
        peer.put(&pm, "b", 20).unwrap();
        let incoming = peer.save();

        let envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(incoming)
                .encode()
                .unwrap(),
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id.to_string(), ""),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::document_attestation_id(&route.app_id, &route.cluster_id.to_string(), ""),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            0,
        );
        app.state.commit_action(None, None);
        app.apply_signed_document_envelope(&envelope, None);

        // A replace would drop "a".
        let doc = &mut app.state_mut_for_test().doc;
        let Ok(Some((automerge::Value::Object(ObjType::Map), m))) = doc.get(&ROOT, "m") else {
            panic!("m exists")
        };
        assert!(
            doc.get(&m, "a").unwrap().is_some(),
            "local edit survives merge"
        );
        assert!(doc.get(&m, "b").unwrap().is_some(), "remote edit merged in");
        assert!(doc.get(&m, "x").unwrap().is_some(), "base value present");
    }
}

#[test]
fn presence_publishes_on_workspace_scope_through_the_sink() {
    // Case `an_app_in_a_workspace_publishes_presence_without_being_asked`:
    // The scope is what sends `Arrived`, so opt-in would mean an app that
    // forgets to set it never receives state written before it opened.
    {
        let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", room)
            .remove("QUICK_CLUSTER_ID");
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert_eq!(
            app.presence_broadcast_scope(),
            Some(&PresenceKey::Workspace {
                id: room.to_string()
            }),
            "a workspace in the environment is a scope, with no app-side call"
        );
    }

    // Case `an_app_with_no_workspace_publishes_nothing`:
    // The nil cluster addresses nobody, so no room means nothing published.
    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_WORKSPACE_ID").remove("QUICK_CLUSTER_ID");
        let app = App::new(State::new(), Box::new(crate::widget::Text::new("t")));
        assert!(app.presence_broadcast_scope().is_none());
    }

    // Case `setting_scope_announces_arrival`.
    {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.set_presence_broadcast_scope(Some(key.clone()));
        // Clone out of the guard so the lock is released before the assertion,
        // rather than held across it.
        let observed = captured.lock().unwrap().clone();
        assert!(
            observed
                .iter()
                .any(|(k, p)| *k == key && matches!(p, PresencePayload::Arrived)),
            "setting a scope must announce Arrival, got {observed:?}"
        );
    }

    // Case `presence_broadcast_sink_receives_both_key_and_payload`.
    {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_captured = Arc::clone(&captured);
        app.set_presence_broadcast_sink(move |k, p| {
            sink_captured.lock().unwrap().push((k.clone(), p.clone()));
        });
        let key = PresenceKey::Workspace { id: "home".into() };
        app.broadcast_presence(key.clone(), PresencePayload::Pointer { x: 5.0, y: 7.0 });
        let observed = captured.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, key);
        match observed[0].1 {
            PresencePayload::Pointer { x, y } => {
                assert_eq!(x, 5.0);
                assert_eq!(y, 7.0);
            }
            ref other => panic!("expected Pointer payload, got {other:?}"),
        }
    }

    // Case `clear_presence_broadcast_sink_drops_callback`.
    {
        use std::sync::{Arc, Mutex};
        let state = State::new();
        let root = Box::new(crate::widget::Text::new("t"));
        let mut app = App::new(state, root);
        let count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let sink_count = Arc::clone(&count);
        app.set_presence_broadcast_sink(move |_, _| {
            *sink_count.lock().unwrap() += 1;
        });
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        app.clear_presence_broadcast_sink();
        app.broadcast_presence(
            PresenceKey::Workspace { id: "x".into() },
            PresencePayload::Arrived,
        );
        assert_eq!(
            *count.lock().unwrap(),
            1,
            "sink fires before clear, never after"
        );
    }
}
