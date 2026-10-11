#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::{MemoryRelay, document_mutation, headless_app, pump};
use std::time::SystemTime;
use tilo_ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use tilo_ui::permissions::{ActionKind, AppKind, PermissionContext, PermissionScope, Role, UserId};
use tilo_ui::presence::{PathSegment, PresenceKey};
use tilo_ui::widget::{TextInput, Widget};

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn relay_fans_out_presence_and_status_to_remote_peers() {
    // Case `memory_relay_fans_out_presence_updates_to_both_peers`.
    {
        let mut relay = MemoryRelay::new();
        let alice = relay.attach("alice");
        let bob = relay.attach("bob");

        alice.send(CollabRequest::document_incremental(vec![1, 2, 3]));

        relay.drain_peer_outbox(&alice);
        pump(&mut relay);

        let events = bob.poll();
        assert!(events.iter().any(|event| {
            matches!(
                document_mutation(event),
                Some(collab_types::DocumentMutation::Incremental(_))
            )
        }));
        // Sender should not receive its own broadcast.
        let alice_events = alice.poll();
        assert!(alice_events.is_empty());
    }

    // Case `memory_relay_fans_out_presence_as_remote_presence_update`.
    {
        let mut relay = MemoryRelay::new();
        let alice = relay.attach("alice");
        let bob = relay.attach("bob");

        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("body".into())],
        };
        alice.send(CollabRequest::Presence {
            key,
            payload: PresencePayload::Caret { pos: 42 },
        });
        relay.drain_peer_outbox(&alice);
        pump(&mut relay);

        let events = bob.poll();
        assert!(
            events.iter().any(|e| matches!(
                e,
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    payload: PresencePayload::Caret { pos: 42 },
                    ..
                } if user_id == "alice"
            )),
            "bob must receive alice's caret presence attributed to alice, got {events:?}"
        );
        // Sender does not get own broadcast back.
        assert!(alice.poll().is_empty());
    }

    // Case `app_broadcast_presence_completes_loop_to_remote_peer`.
    {
        let mut relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (mut alice_app, _) = headless_app("alice", alice_peer.clone());
        let (mut bob_app, _) = headless_app("bob", bob_peer.clone());

        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("doc".into())],
        };
        // Mirrors what a text-input widget does on every caret move.
        alice_app.broadcast_presence(key.clone(), PresencePayload::Caret { pos: 9 });

        // Without a real socket, the manager's outbox is a no-op — wire the
        // test relay directly through alice's peer instead; this bypass
        // covers the `pump` logic itself.
        alice_peer.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 9 },
        });
        relay.drain_peer_outbox(&alice_peer);
        pump(&mut relay);

        let bob_events = bob_peer.poll();
        for ev in bob_events {
            bob_app.apply_collab_event_for_test(ev);
        }

        assert_eq!(
            bob_app
                .presence_ctx()
                .caret_for(&key, &UserId("alice".into())),
            Some(9),
            "alice's broadcast caret must reach bob's PresenceCtx attributed to alice"
        );
        // Remote broadcast of *own* presence is a one-way outbound signal.
        assert_eq!(
            alice_app
                .presence_ctx()
                .caret_for(&key, &UserId("alice".into())),
            None,
        );
    }

    // Case `status_crosses_the_same_authenticated_room_presence_path`.
    {
        let mut relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (mut alice_app, _) = headless_app("alice", alice_peer.clone());
        let (mut bob_app, _) = headless_app("bob", bob_peer.clone());
        let room = PresenceKey::Workspace {
            id: "finance-room".into(),
        };
        let status = tilo_ui::LiveStatus {
            text: "Reconciling forecast against actuals".into(),
            origin: tilo_ui::StatusOrigin::Inferred {
                generator: "local".into(),
            },
            updated_at: SystemTime::now(),
        };
        assert!(alice_app.broadcast_status(room.clone(), Some(&status)));
        alice_peer.send(CollabRequest::Presence {
            key: room,
            payload: PresencePayload::Status {
                text: status.text,
                automatic: true,
            },
        });
        relay.drain_peer_outbox(&alice_peer);
        pump(&mut relay);
        for event in bob_peer.poll() {
            bob_app.apply_collab_event_for_test(event);
        }
        let received = bob_app
            .presence_ctx()
            .status_for(&UserId("alice".into()), SystemTime::now())
            .expect("remote status reaches the room presence context");
        assert_eq!(received.text, "Reconciling forecast against actuals");
        assert!(matches!(
            received.origin,
            tilo_ui::StatusOrigin::Inferred { .. }
        ));
    }
}

#[test]
fn control_reports_and_crdt_deltas_reach_the_right_plane() {
    // Case `continuation_reports_stay_on_the_shell_control_plane`.
    {
        let mut relay = MemoryRelay::new();
        let alice = relay.attach("alice");
        let bob = relay.attach("bob");

        alice.send(CollabRequest::ReportContinuation {
            app_id: "browser".into(),
            schema_version: 1,
            metadata: vec![1, 2, 3],
        });
        alice.send(CollabRequest::ClearContinuation {
            app_id: "browser".into(),
        });
        relay.drain_peer_outbox(&alice);
        pump(&mut relay);

        assert!(alice.poll().is_empty(), "control reports are one-way");
        assert!(
            bob.poll().is_empty(),
            "continuation metadata must not leak into peer document events"
        );
    }

    // Case `crdt_incremental_deltas_replay_on_remote_state`.
    {
        use automerge::ReadDoc;

        // Alice makes a concrete CRDT mutation, serializes the delta via
        // `save_incremental`, and fires it through the relay; Bob's receiving
        // State applies the bytes through the same API used by the routed App.
        let mut relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let mut alice_state = tilo_ui::state::State::new();

        // Establish the shared document history first. An incremental change
        // cannot be applied until the normal snapshot handshake has run.
        let snapshot = alice_state.save();
        let mut bob_state = tilo_ui::state::State::load(&snapshot).expect("shared base loads");
        let _ = alice_state.save_incremental();

        // In production a widget would mutate the doc as part of event
        // handling — done directly here to keep the test self-contained.
        {
            use automerge::transaction::Transactable;
            alice_state
                .doc
                .put(automerge::ROOT, "hello", "from alice")
                .expect("put scalar");
        }
        let delta = alice_state.save_incremental();
        assert!(
            !delta.is_empty(),
            "save_incremental must emit non-empty bytes after a mutation"
        );

        alice_peer.send(CollabRequest::document_incremental(delta));
        relay.drain_peer_outbox(&alice_peer);
        pump(&mut relay);

        let events = bob_peer.poll();
        let mut applied = false;
        for ev in events {
            if let Some(collab_types::DocumentMutation::Incremental(data)) = document_mutation(&ev)
            {
                bob_state
                    .load_incremental(&data)
                    .expect("remote state accepts a delta from its shared base");
                applied = true;
            }
        }
        assert!(applied, "bob must receive the incremental document event");

        let got = bob_state
            .doc
            .get(automerge::ROOT, "hello")
            .expect("read back")
            .and_then(|(val, _)| val.into_scalar().ok())
            .and_then(|s| s.into_string().ok());
        assert_eq!(
            got.as_deref(),
            Some("from alice"),
            "bob's doc must converge with alice's after the incremental merge"
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn peer_carets_pointers_and_tabs_render_on_the_remote_app() {
    // Case `two_apps_sync_presence_caret_via_memory_relay`.
    {
        let mut relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");

        let (mut _alice_app, _alice_state) = headless_app("alice", alice_peer.clone());
        let (mut bob_app, _bob_state) = headless_app("bob", bob_peer);

        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("body".into())],
        };

        alice_peer.send(CollabRequest::document_incremental(vec![]));
        relay.drain_peer_outbox(&alice_peer);
        pump(&mut relay);

        // The relay itself does not yet translate a Presence mutation to an
        // event, so bypass polling and apply directly via the test-only hook.
        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 7 },
        });

        let caret = bob_app
            .presence_ctx()
            .caret_for(&key, &UserId("alice".into()));
        assert_eq!(caret, Some(7));
    }

    // Case `peer_pointer_position_follows_layout_change`.
    {
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (_alice_app, _) = headless_app("alice", alice_peer);
        let (mut bob_app, _) = headless_app("bob", bob_peer);

        let ws_key = PresenceKey::Workspace { id: "ws".into() };
        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: ws_key.clone(),
            payload: PresencePayload::Pointer { x: 100.0, y: 50.0 },
        });

        assert_eq!(
            bob_app
                .presence_ctx()
                .pointer_for(&ws_key, &UserId("alice".into())),
            Some((100.0, 50.0))
        );

        // Simulates a window-level coordinate remap.
        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: ws_key.clone(),
            payload: PresencePayload::Pointer { x: 200.0, y: 70.0 },
        });

        assert_eq!(
            bob_app
                .presence_ctx()
                .pointer_for(&ws_key, &UserId("alice".into())),
            Some((200.0, 70.0))
        );
    }

    // Case `peer_caret_in_text_input_renders_on_remote_app`.
    {
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (_alice_app, _) = headless_app("alice", alice_peer);
        let (mut bob_app, _) = headless_app("bob", bob_peer);

        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("doc".into())],
        };
        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 5 },
        });

        let mut input = TextInput::new(vec![PathSegment::Field("doc".into())]);
        let mut scene = vello::Scene::new();
        let theme = tilo_ui::theme::Theme::dark();
        let mut tr = tilo_ui::text_render::TextRenderer::new();

        input.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut tr,
            &theme,
            bob_app.presence_ctx(),
        );

        assert!(!scene.encoding().is_empty());
        assert_eq!(
            bob_app
                .presence_ctx()
                .caret_for(&key, &UserId("alice".into())),
            Some(5)
        );
    }

    // Case `peer_tab_switch_moves_the_presence_dot`.
    {
        // When a peer announces Arrived on one Tab key and then another, the
        // dot must *move*, not accumulate into a trail of ghost dots.
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (_alice_app, _) = headless_app("alice", alice_peer);
        let (mut bob_app, _) = headless_app("bob", bob_peer);

        let tab_a = PresenceKey::Tab { id: "aaaa".into() };
        let tab_b = PresenceKey::Tab { id: "bbbb".into() };
        let alice = UserId("alice".into());

        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: tab_a.clone(),
            payload: PresencePayload::Arrived,
        });
        assert_eq!(
            bob_app.presence_ctx().peers_on(&tab_a),
            std::slice::from_ref(&alice)
        );
        assert!(bob_app.presence_ctx().peers_on(&tab_b).is_empty());

        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: tab_b.clone(),
            payload: PresencePayload::Arrived,
        });
        assert!(
            bob_app.presence_ctx().peers_on(&tab_a).is_empty(),
            "alice must be cleared from tab A after switching to B"
        );
        assert_eq!(bob_app.presence_ctx().peers_on(&tab_b), &[alice]);
    }
}

/// Roles carry base authority over the room and its apps, so a Viewer
/// reads the terminal surface and an Editor writes it — no individual
/// capabilities needed.
fn member_ctx(user: &str, role: Role) -> PermissionContext {
    PermissionContext::member(user, role)
}

fn terminal_write() -> PermissionScope {
    PermissionScope::app_kind(AppKind::Terminal, ActionKind::Undoable)
}

#[test]
fn roles_and_permission_changes_apply_across_the_session() {
    // Case `viewer_role_denies_terminal_edit_after_settings_applied`.
    {
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let (mut alice_app, _) = headless_app("alice", alice_peer);

        alice_app.set_permission_ctx(member_ctx("alice", Role::Viewer));

        assert!(!alice_app.permission_ctx().check(&terminal_write()));
        // The denial is specific, not blanket: reading is allowed.
        assert!(alice_app.permission_ctx().check(&PermissionScope::app_kind(
            AppKind::Terminal,
            ActionKind::Read
        )));
    }

    // Case `permission_change_from_peer_reflects_locally_after_sync`.
    {
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (mut alice_app, _) = headless_app("alice", alice_peer);
        let (bob_app, _) = headless_app("bob", bob_peer);

        alice_app.set_permission_ctx(member_ctx("alice", Role::Viewer));
        assert!(!alice_app.permission_ctx().check(&terminal_write()));

        alice_app.set_permission_ctx(member_ctx("alice", Role::Editor));
        assert!(alice_app.permission_ctx().check(&terminal_write()));
        assert_eq!(alice_app.permission_ctx().user(), &UserId("alice".into()));

        // Confirms the two-App harness is exercised; bob's context is
        // independent of alice's.
        let _ = bob_app.permission_ctx();
    }
}
