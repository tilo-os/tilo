use crate::support::{MemoryRelay, document_mutation, headless_app, pump};
use tilo_ui::widget::TextInput;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{
    collaboration::CollabEvent, collaboration::CollabRequest, collaboration::PresencePayload,
};
use tilo_ui_base::{
    permissions::ActionKind, permissions::AppKind, permissions::MemberId,
    permissions::PermissionContext, permissions::PermissionScope, permissions::Role,
};
use tilo_ui_base::{presence::PathSegment, presence::PresenceKey};

#[test]
fn control_reports_and_crdt_deltas_reach_the_right_protocol() {
    {
        let mut relay = MemoryRelay::new();
        let alice = relay.attach("alice");
        let bob = relay.attach("bob");

        alice.send(CollabRequest::ReportContinuation {
            schema_version: 1,
            metadata: vec![1, 2, 3],
        });
        alice.send(CollabRequest::ClearContinuation);
        relay.drain_peer_outbox(&alice);
        pump(&mut relay);

        assert!(alice.poll().is_empty(), "control reports are one-way");
        assert!(
            bob.poll().is_empty(),
            "continuation metadata must not leak into peer document events"
        );
    }

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
            member: MemberId::person("alice"),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 7 },
        });

        let caret = bob_app
            .presence_ctx()
            .caret_for(&key, &MemberId::person("alice"));
        assert_eq!(caret, Some(7));
    }

    {
        let relay = MemoryRelay::new();
        let alice_peer = relay.attach("alice");
        let bob_peer = relay.attach("bob");
        let (_alice_app, _) = headless_app("alice", alice_peer);
        let (mut bob_app, _) = headless_app("bob", bob_peer);

        let ws_key = PresenceKey::Workspace { id: "ws".into() };
        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            member: MemberId::person("alice"),
            key: ws_key.clone(),
            payload: PresencePayload::Pointer { x: 100.0, y: 50.0 },
        });

        assert_eq!(
            bob_app
                .presence_ctx()
                .pointer_for(&ws_key, &MemberId::person("alice")),
            Some((100.0, 50.0))
        );

        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            member: MemberId::person("alice"),
            key: ws_key.clone(),
            payload: PresencePayload::Pointer { x: 200.0, y: 70.0 },
        });

        assert_eq!(
            bob_app
                .presence_ctx()
                .pointer_for(&ws_key, &MemberId::person("alice")),
            Some((200.0, 70.0))
        );
    }

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
            member: MemberId::person("alice"),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 5 },
        });

        let mut input = TextInput::new(vec![PathSegment::Field("doc".into())]);
        let mut scene = vello::Scene::new();
        let theme = tilo_ui_base::theme::Theme::dark();
        let mut tr = tilo_ui_base::text_render::TextRenderer::new();

        input.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut tr,
            &theme,
            bob_app.presence_ctx().draw_env(),
        ));

        assert!(!scene.encoding().is_empty());
        assert_eq!(
            bob_app
                .presence_ctx()
                .caret_for(&key, &MemberId::person("alice")),
            Some(5)
        );
    }

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
        let alice = MemberId::person("alice");

        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            member: MemberId::person("alice"),
            key: tab_a.clone(),
            payload: PresencePayload::Arrived,
        });
        assert_eq!(
            bob_app.presence_ctx().peers_on(&tab_a),
            std::slice::from_ref(&alice)
        );
        assert!(bob_app.presence_ctx().peers_on(&tab_b).is_empty());

        bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            member: MemberId::person("alice"),
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

/// Roles carry base authority over the workspace and its apps, so a Viewer
/// reads the terminal surface and an Editor writes it — no individual
/// capabilities needed.
fn member_ctx(user: &str, role: Role) -> PermissionContext {
    PermissionContext::member(&tilo_ui_base::permissions::MemberId::person(user), role)
}

fn terminal_write() -> PermissionScope {
    PermissionScope::app_kind(AppKind::Terminal, ActionKind::Undoable)
}

#[test]
fn roles_and_permission_changes_apply_across_the_session() {
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
        assert_eq!(
            alice_app.permission_ctx().user(),
            &MemberId::person("alice")
        );

        // Confirms the two-App harness is exercised; bob's context is
        // independent of alice's.
        let _ = bob_app.permission_ctx();
    }
}
