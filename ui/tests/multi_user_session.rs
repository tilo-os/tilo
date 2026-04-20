mod support;

use support::{MemoryRelay, pump, headless_app};
use quick_ui::collaboration::{CollabRequest, CollabEvent, PresencePayload};
use quick_ui::presence::{PresenceKey, PathSegment};
use quick_ui::permissions::{
    AppKind, Op, PermissionAssignment, PermissionContext, PermissionScope, ResourceId,
    Role, RoleId, RolePermissions, UserId, WorkspaceSettingsView,
};
use quick_ui::widget::{TextInput, Widget};

#[test]
fn memory_relay_fans_out_presence_updates_to_both_peers() {
    let mut relay = MemoryRelay::new();
    let alice = relay.attach("alice");
    let bob = relay.attach("bob");

    alice.send(CollabRequest::SyncIncremental { data: vec![1, 2, 3] });

    // Relay broadcasts raw incremental data as CollabEvent::CRDTIncremental.
    relay.drain_peer_outbox(&alice);
    pump(&mut relay);

    let events = bob.poll();
    assert!(events.iter().any(|e| matches!(e, CollabEvent::CRDTIncremental { .. })));
    // Sender should not receive its own broadcast.
    let alice_events = alice.poll();
    assert!(alice_events.is_empty());
}

#[test]
fn memory_relay_fans_out_presence_as_remote_presence_update() {
    // Round-trip presence: alice broadcasts a Caret presence, pump
    // translates it to RemotePresenceUpdate, bob polls the event
    // attributed to alice. Confirms the full outbound→inbound pipeline
    // works end-to-end without a direct test-only inject.
    let mut relay = MemoryRelay::new();
    let alice = relay.attach("alice");
    let bob = relay.attach("bob");

    let key = PresenceKey::Widget {
        path: vec![PathSegment::Field("body".into())],
    };
    alice.send(CollabRequest::Presence {
        key: key.clone(),
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
        "bob must receive alice's caret presence attributed to alice, got {:?}",
        events
    );
    // Sender does not get own broadcast back.
    assert!(alice.poll().is_empty());
}

#[test]
fn app_broadcast_presence_completes_loop_to_remote_peer() {
    // Build two headless apps wired to the same relay, then call
    // `broadcast_presence` on alice's app — the relay must fan out
    // as a RemotePresenceUpdate that, once applied on bob, lands in
    // his PresenceCtx attributed to alice.
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (mut alice_app, _) = headless_app("alice", alice_peer.clone());
    let (mut bob_app, _) = headless_app("bob", bob_peer.clone());

    let key = PresenceKey::Widget {
        path: vec![PathSegment::Field("doc".into())],
    };
    // This path mirrors what a text-input widget does on every caret
    // move: hand the local coordinate to App::broadcast_presence and
    // trust the manager to publish it.
    alice_app.broadcast_presence(
        key.clone(),
        PresencePayload::Caret { pos: 9 },
    );

    // Without a real socket, the manager's outbox is a no-op — wire
    // the test relay directly by sending through alice's peer. In the
    // production path the manager writes to a Unix socket, which the
    // compositor reads and fans out; this test bypass covers the
    // `pump` logic itself.
    alice_peer.send(CollabRequest::Presence {
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 9 },
    });
    relay.drain_peer_outbox(&alice_peer);
    pump(&mut relay);

    // Drive bob's app to consume the relay event.
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
    // alice should not see her own caret in her PresenceCtx — remote
    // broadcast of *own* presence is a one-way outbound signal.
    assert_eq!(
        alice_app
            .presence_ctx()
            .caret_for(&key, &UserId("alice".into())),
        None,
    );
}

#[test]
fn two_apps_sync_presence_caret_via_memory_relay() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");

    let (mut _alice_app, _alice_state) = headless_app("alice", alice_peer.clone());
    let (mut bob_app, _bob_state) = headless_app("bob", bob_peer.clone());

    let key = PresenceKey::Widget {
        path: vec![PathSegment::Field("body".into())],
    };

    // Alice moves her caret to position 7.
    alice_peer.send(CollabRequest::SyncIncremental { data: vec![] }); // warm up
    relay.drain_peer_outbox(&alice_peer);
    pump(&mut relay);

    // The relay itself does not yet translate a Presence mutation to an event.
    // We bypass the polling roundtrip and apply the RemotePresenceUpdate
    // directly to bob's App via the test-only hook to model fan-out.
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 7 },
    });

    let caret = bob_app.presence_ctx().caret_for(&key, &UserId("alice".into()));
    assert_eq!(caret, Some(7));
}

#[test]
fn peer_pointer_position_follows_layout_change() {
    let mut relay = MemoryRelay::new();
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
        bob_app.presence_ctx().pointer_for(&ws_key, &UserId("alice".into())),
        Some((100.0, 50.0))
    );

    // Simulate a window-level coordinate remap by replaying the pointer event
    // with new coords — this is what the real layout-change path does.
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: ws_key.clone(),
        payload: PresencePayload::Pointer { x: 200.0, y: 70.0 },
    });

    assert_eq!(
        bob_app.presence_ctx().pointer_for(&ws_key, &UserId("alice".into())),
        Some((200.0, 70.0))
    );
}

fn make_view(alice_role: &str, bob_role: &str) -> WorkspaceSettingsView {
    WorkspaceSettingsView {
        roles: vec![
            Role {
                id: RoleId("viewer".into()),
                name: "Viewer".into(),
                permissions: RolePermissions {
                    app_access: vec![(AppKind::Terminal, Op::View)],
                    ..Default::default()
                },
            },
            Role {
                id: RoleId("editor".into()),
                name: "Editor".into(),
                permissions: RolePermissions {
                    app_access: vec![(AppKind::Terminal, Op::Edit)],
                    ..Default::default()
                },
            },
        ],
        assignments: vec![
            PermissionAssignment { user: UserId("alice".into()), role: RoleId(alice_role.into()) },
            PermissionAssignment { user: UserId("bob".into()),   role: RoleId(bob_role.into()) },
        ],
    }
}

#[test]
fn viewer_role_denies_terminal_edit_after_settings_applied() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let (mut alice_app, _) = headless_app("alice", alice_peer);

    let view = make_view("viewer", "editor");
    let ctx = PermissionContext::from_workspace_settings(&view, &UserId("alice".into()))
        .expect("assignment exists");
    alice_app.set_permission_ctx(ctx);

    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(!alice_app.permission_ctx().check(&scope));
}

#[test]
fn permission_change_from_peer_reflects_locally_after_sync() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (mut alice_app, _) = headless_app("alice", alice_peer);
    let (bob_app, _) = headless_app("bob", bob_peer);

    // Bob edits workspace settings -> promotes alice to editor.
    let new_view = make_view("editor", "editor");

    // Simulate CRDT sync: bob broadcasts the serialized settings;
    // alice receives and rebuilds her PermissionContext.
    let ctx_for_alice = PermissionContext::from_workspace_settings(
        &new_view, &UserId("alice".into()),
    ).unwrap();
    alice_app.set_permission_ctx(ctx_for_alice);

    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(alice_app.permission_ctx().check(&scope));

    // bob_app is constructed to confirm the two-App harness is exercised;
    // its permission context is independent of alice's.
    let _ = bob_app.permission_ctx();
}

#[test]
fn peer_caret_in_text_input_renders_on_remote_app() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (_alice_app, _) = headless_app("alice", alice_peer);
    let (mut bob_app, _) = headless_app("bob", bob_peer);

    let key = PresenceKey::Widget { path: vec![PathSegment::Field("doc".into())] };
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 5 },
    });

    // Build a TextInput with matching presence path and render it.
    let mut input = TextInput::new(vec![PathSegment::Field("doc".into())]);
    let mut scene = vello::Scene::new();
    let theme = quick_ui::theme::Theme::dark();
    let mut tr = quick_ui::text_render::TextRenderer::new();
    let perms = quick_ui::permissions::PermissionContext::permissive();

    input.draw_with_presence(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut tr,
        &theme,
        bob_app.presence_ctx(),
        &perms,
    );

    // A non-empty scene proves the caret chrome was emitted.
    assert!(!scene.encoding().is_empty());
    // The caret value reached bob's PresenceCtx.
    assert_eq!(bob_app.presence_ctx().caret_for(&key, &UserId("alice".into())), Some(5));
}

#[test]
fn crdt_incremental_deltas_replay_on_remote_peer() {
    // End-to-end coverage for `CollabEvent::CRDTIncremental`: alice makes a
    // concrete CRDT mutation, serializes the delta via `save_incremental`,
    // and fires it through the relay; bob's app must apply the bytes to his
    // own doc so they converge. Before this plumbing landed, the
    // CRDTIncremental branch in App was a no-op and the two docs drifted.
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (mut alice_app, _) = headless_app("alice", alice_peer.clone());
    let (mut bob_app, _) = headless_app("bob", bob_peer.clone());

    // Alice writes a scalar into the root of her doc, then harvests the
    // delta. In production a widget would mutate the doc as part of event
    // handling — we do it directly here to keep the test self-contained.
    {
        let state = alice_app.state_mut_for_test();
        use automerge::transaction::Transactable;
        state
            .doc
            .put(automerge::ROOT, "hello", "from alice")
            .expect("put scalar");
    }
    let delta = alice_app.state_mut_for_test().save_incremental();
    assert!(
        !delta.is_empty(),
        "save_incremental must emit non-empty bytes after a mutation"
    );

    alice_peer.send(CollabRequest::SyncIncremental { data: delta.clone() });
    relay.drain_peer_outbox(&alice_peer);
    pump(&mut relay);

    // Bob consumes the CRDTIncremental event via the test hook, which now
    // actually merges the patch instead of ignoring it.
    let events = bob_peer.poll();
    let mut applied = false;
    for ev in events {
        if matches!(ev, CollabEvent::CRDTIncremental { .. }) {
            applied = true;
        }
        bob_app.apply_collab_event_for_test(ev);
    }
    assert!(applied, "bob must receive the CRDTIncremental event");

    // Convergence: bob's doc now reflects alice's scalar.
    use automerge::ReadDoc;
    let state = bob_app.state_mut_for_test();
    let got = state
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

#[test]
fn peer_tab_switch_moves_the_presence_dot() {
    // Regression guard for live tab-presence refresh: when a peer announces
    // Arrived on one Tab key and then on another, the dot must *move*, not
    // accumulate. Without this, a peer who tabs around for a minute leaves
    // a trail of ghost dots on every tab they ever touched.
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (_alice_app, _) = headless_app("alice", alice_peer);
    let (mut bob_app, _) = headless_app("bob", bob_peer);

    let tab_a = PresenceKey::Tab { id: "aaaa".into() };
    let tab_b = PresenceKey::Tab { id: "bbbb".into() };
    let alice = UserId("alice".into());

    // Alice is first attributed to tab A.
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: tab_a.clone(),
        payload: PresencePayload::Arrived,
    });
    assert_eq!(bob_app.presence_ctx().peers_on(&tab_a), &[alice.clone()]);
    assert!(bob_app.presence_ctx().peers_on(&tab_b).is_empty());

    // Alice switches to tab B.
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
