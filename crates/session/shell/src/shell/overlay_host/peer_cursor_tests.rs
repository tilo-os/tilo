//! A peer caret on screen must mean a peer is connected.
//!
//! These tests pin both halves of the invariant on the
//! production paint path: nothing is drawn for an empty roster, and
//! a real peer is still drawn.
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, Collaborator, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};

fn one_group_seed() -> WorkspaceTree {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 100;
    let tab_id = 1;
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            width: StateWidth::Normal,
            maximized: false,
            tabs: vec![Tab {
                id: tab_id,
                title: "Terminal".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Terminal,
                    label: "Terminal".into(),
                },
            }],
            // A group's active tab has to name a tab it has; `0`
            // names nothing, and the resolvers read this.
            active_tab: tab_id,
        }],
    };
    // Through the constructor rather than a struct literal: a field
    // added to `Workspace` should not break this fixture.
    let mut ws = Workspace::new(ws_id, "Work");
    ws.strips = vec![strip];
    let mut tree = WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            ..Default::default()
        },
        ..Default::default()
    };
    // Both are per-parent maps, not single pointers: one active strip
    // per workspace, one active group per strip.
    tree.desktop.active_strip.insert(ws_id, strip_id);
    tree.desktop.active_group.insert(strip_id, group_id);
    tree
}

/// The key an app in this workspace actually publishes presence
/// under — the workspace's **room**, read off the tree rather than
/// spelled out here.
fn active_key(tree: &WorkspaceTree) -> PresenceKey {
    // The room is what the launcher stamps into an app as
    // `TILO_WORKSPACE_ID`, not the local `WorkspaceId` — naming the
    // local handle here would write a key nobody reads.
    let room = tree
        .desktop
        .workspaces
        .iter()
        .find(|w| w.id == tree.desktop.active)
        .map(|w| w.room.to_string())
        .expect("the seed has an active workspace");
    PresenceKey::Workspace { id: room }
}

/// Draws the peer layer for `presence`, returning how many draw
/// commands it emitted (0 = nothing painted). Takes the tree the
/// caller built `presence` keys against — `one_group_seed` mints a
/// fresh `WorkspaceId` per call.
fn peer_draw_ops(tree: &WorkspaceTree, presence: &PresenceCtx) -> usize {
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(tree);
    host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let theme = Theme::dark();
    let mut tr = ui::text_render::TextRenderer::new();
    let mut scene = Scene::new();
    host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence);
    scene.encoding().draw_tags.len()
}

#[test]
fn peer_cursors_are_painted_only_for_real_peers() {
    /// The one that matters. A session with nobody else in it paints
    /// no caret, no name chip, no peer of any kind — so a screenshot
    /// of a solo desktop cannot show a collaborator.
    fn a_solo_session_paints_no_peer() {
        let tree = one_group_seed();
        assert_eq!(
            peer_draw_ops(&tree, &PresenceCtx::default()),
            0,
            "an empty roster must paint nothing"
        );
    }

    /// An empty pointer list for the active workspace is still an
    /// empty roster — the key existing must not conjure a peer.
    fn an_empty_pointer_list_paints_no_peer() {
        let tree = one_group_seed();
        let mut presence = PresenceCtx::default();
        presence.pointers.insert(active_key(&tree), vec![]);
        assert_eq!(
            peer_draw_ops(&tree, &presence),
            0,
            "an empty list is not a peer"
        );
    }

    /// The half that supports the no-peer assertions: if this fails,
    /// the helper reports zero for everything and proves nothing. A peer
    /// published under the active workspace's presence key still paints.
    fn a_real_peer_is_still_painted() {
        let tree = one_group_seed();
        let mut presence = PresenceCtx::default();
        presence.pointers.insert(
            active_key(&tree),
            vec![(UserId("alice".into()), (400.0, 300.0))],
        );
        assert!(
            peer_draw_ops(&tree, &presence) > 0,
            "a connected peer must still draw a cursor"
        );
    }

    a_solo_session_paints_no_peer();
    an_empty_pointer_list_paints_no_peer();
    a_real_peer_is_still_painted();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn following_a_peer_tracks_their_tab_binds_views_and_releases_on_local_intent() {
    fn clicking_a_peer_cursor_starts_follow_without_opening_the_roster() {
        let mut tree = one_group_seed();
        tree.desktop.workspaces[0]
            .collaborators
            .push(Collaborator::new("ada", "Ada"));
        let key = active_key(&tree);
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut presence = PresenceCtx::default();
        presence
            .pointers
            .insert(key, vec![(UserId("ada".into()), (400.0, 300.0))]);
        let mut scene = Scene::new();
        let mut text = ui::text_render::TextRenderer::new();
        host.draw_real_peers(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );

        assert!(host.handle_workspace_navigation_click(Point::new(400.0, 300.0)));
        assert_eq!(host.followed_peer.as_deref(), Some("ada"));
    }

    fn following_a_peer_tracks_their_live_tab() {
        let mut tree = one_group_seed();
        let strip = tree.desktop.workspaces[0].strips[0].id;
        let target_group = 200;
        let target_tab = 201;
        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(StateGroup {
                id: target_group,
                width: StateWidth::Normal,
                maximized: false,
                tabs: vec![Tab {
                    id: target_tab,
                    title: "Plan".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("Plan".into()),
                        label: "Plan".into(),
                    },
                }],
                active_tab: target_tab,
            });
        tree.desktop
            .collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab));

        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host.followed_peer = Some("ada".to_string());

        assert!(host.reconcile_followed_peer());
        assert_eq!(
            chrome.borrow().tree().desktop.active_group_for(strip),
            Some(target_group),
            "follow must move local focus to the participant's live group",
        );
    }

    fn follow_binds_app_views_to_the_live_session_and_stop_releases_them() {
        let mut tree = one_group_seed();
        let target_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab;
        tree.desktop.workspaces[0]
            .collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab).with_session(77));
        let expected_room = tree.desktop.workspaces[0].room;
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&calls);
        host.set_follow_bridge(std::rc::Rc::new(move |room, target| {
            captured.borrow_mut().push((room, target));
        }));

        host.set_followed_peer(Some("ada".into()));
        host.set_followed_peer(None);

        assert_eq!(
            calls.borrow().as_slice(),
            &[
                (expected_room, Some("peer@77".to_string())),
                (expected_room, None),
            ],
            "the shell and every local app enter and leave Follow together"
        );
    }

    fn a_local_workspace_click_releases_follow_before_routing_the_click() {
        let mut tree = one_group_seed();
        let target_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab;
        tree.desktop.workspaces[0]
            .collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab).with_session(77));
        let expected_room = tree.desktop.workspaces[0].room;
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&calls);
        host.set_follow_bridge(std::rc::Rc::new(move |room, target| {
            captured.borrow_mut().push((room, target));
        }));

        host.set_followed_peer(Some("ada".into()));
        let _ = host.handle_workspace_navigation_click(Point::new(640.0, 500.0));

        assert!(host.followed_peer.is_none(), "local input takes control");
        assert_eq!(
            calls.borrow().as_slice(),
            &[
                (expected_room, Some("peer@77".to_string())),
                (expected_room, None),
            ],
            "the same workspace's apps are released before the click reaches local content",
        );
    }

    fn escape_releases_follow_before_the_key_reaches_content() {
        let mut tree = one_group_seed();
        let target_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab;
        tree.desktop.workspaces[0]
            .collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab).with_session(77));
        let room = tree.desktop.workspaces[0].room;
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&calls);
        host.set_follow_bridge(std::rc::Rc::new(move |room, target| {
            captured.borrow_mut().push((room, target));
        }));
        host.set_followed_peer(Some("ada".into()));

        // Only the public fields read by the host are initialized; winit's
        // private KeyEvent payload is intentionally opaque to this unit test.
        let key = ui::synthetic_input::key_event(
            Key::Named(NamedKey::Escape),
            None,
            ElementState::Pressed,
        );
        let event = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: key,
            is_synthetic: false,
        };
        assert!(host.release_follow_for_local_key(&event));
        assert!(host.followed_peer.is_none());
        assert_eq!(
            calls.borrow().as_slice(),
            &[(room, Some("peer@77".to_string())), (room, None)],
            "Escape releases the original room before local key routing"
        );
    }

    fn leaving_a_workspace_releases_follow_in_the_original_room() {
        let mut tree = one_group_seed();
        let target_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab;
        tree.desktop.workspaces[0]
            .collaborators
            .push(Collaborator::with_active_tab("ada", "Ada", target_tab).with_session(77));
        let expected_room = tree.desktop.workspaces[0].room;
        let next_workspace = WorkspaceId::new();
        tree.desktop
            .workspaces
            .push(Workspace::new(next_workspace, "Elsewhere"));
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&calls);
        host.set_follow_bridge(std::rc::Rc::new(move |room, target| {
            captured.borrow_mut().push((room, target));
        }));

        host.set_followed_peer(Some("ada".into()));
        {
            let mut state = chrome.borrow_mut();
            state.tree_mut().desktop.active = next_workspace;
            state.bump();
        }
        assert!(
            host.reconcile_followed_peer(),
            "changing workspace exits the old room's follow mode"
        );

        assert!(host.followed_peer.is_none());
        assert_eq!(
            calls.borrow().as_slice(),
            &[
                (expected_room, Some("peer@77".to_string())),
                (expected_room, None),
            ],
            "release returns to the room that originally received the Follow command",
        );
    }

    clicking_a_peer_cursor_starts_follow_without_opening_the_roster();
    following_a_peer_tracks_their_live_tab();
    follow_binds_app_views_to_the_live_session_and_stop_releases_them();
    a_local_workspace_click_releases_follow_before_routing_the_click();
    escape_releases_follow_before_the_key_reaches_content();
    leaving_a_workspace_releases_follow_in_the_original_room();
}

#[test]
fn following_cross_app_context_releases_all_apps_when_the_peer_or_window_leaves() {
    for interruption in [
        "close",
        "disconnect",
        "focus-left",
        "session-replaced",
        "local-focus",
    ] {
        let tree = cross_app_follow_tree();
        let strip = tree.desktop.workspaces[0].strips[0].id;
        let room = tree.desktop.workspaces[0].room;
        let fanout = std::sync::Arc::new(crate::shell::collab_broker::FanoutState::new());
        let dispatcher = crate::shell::collab_broker::SessionFollowDispatcher::new(
            std::sync::Arc::clone(&fanout),
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // Separate room subscriptions exercise the real dispatcher used by
        // every local app; a third room must never receive this relationship.
        let subscriptions: Vec<_> = [room.to_string(), room.to_string(), "other-room".into()]
            .into_iter()
            .enumerate()
            .map(|(app, subscribed_room)| {
                let events = std::sync::Arc::clone(&events);
                fanout.subscribe_rooms(
                    vec![subscribed_room],
                    Box::new(move |event, _| {
                        if let ui::collaboration::CollabEvent::FollowChanged { peer_id } = event {
                            events.lock().unwrap().push((app, peer_id.clone()));
                        }
                    }),
                )
            })
            .collect();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        host.set_follow_bridge(std::rc::Rc::new(move |room, target| {
            dispatcher.set(&room.to_string(), target);
        }));
        host.set_followed_peer(Some("ada".into()));
        assert_eq!(
            chrome.borrow().tree().desktop.active_group_for(strip),
            Some(200)
        );
        {
            let mut state = chrome.borrow_mut();
            // The same remote session moves from Chat into Terminal.
            state.tree_mut().desktop.workspaces[0].collaborators[0].active_tab = Some(1);
            state.bump();
        }
        assert!(host.reconcile_followed_peer());
        assert_eq!(
            chrome.borrow().tree().desktop.active_group_for(strip),
            Some(100)
        );
        {
            let mut state = chrome.borrow_mut();
            match interruption {
                "disconnect" => assert!(state.tree_mut().retire_session(77)),
                "focus-left" => {
                    state.tree_mut().desktop.workspaces[0].collaborators[0].active_tab = None;
                }
                "session-replaced" => {
                    state.tree_mut().desktop.workspaces[0].collaborators[0].session = Some(88);
                }
                "local-focus" => state.focus_tab(tree.desktop.active, strip, 200, 201),
                _ => state.tree_mut().desktop.workspaces[0].strips[0].window_groups[0]
                    .tabs
                    .clear(),
            }
            state.bump();
        }
        assert!(host.reconcile_followed_peer());
        assert!(host.followed_peer.is_none());
        assert!(host.followed_room.is_none());
        assert_eq!(host.follow_stop_rect, Rect::ZERO);
        let received = events.lock().unwrap();
        for app in [0, 1] {
            assert_eq!(
                received
                    .iter()
                    .filter(|(id, _)| *id == app)
                    .map(|(_, peer)| peer.clone())
                    .collect::<Vec<_>>(),
                vec![Some("peer@77".into()), None],
                "each app releases the old authenticated session"
            );
        }
        assert!(!received.iter().any(|(id, _)| *id == 2));
        // A reconnect or replacement tab cannot resume the released relationship.
        drop(received);
        {
            let mut state = chrome.borrow_mut();
            state.tree_mut().desktop.workspaces[0].collaborators =
                vec![Collaborator::with_active_tab("ada", "Ada", 201).with_session(88)];
            state.bump();
        }
        assert!(!host.reconcile_followed_peer());
        assert_eq!(events.lock().unwrap().len(), 4);
        drop(subscriptions);
    }
}

fn cross_app_follow_tree() -> WorkspaceTree {
    let mut tree = one_group_seed();
    tree.desktop.workspaces[0].strips[0]
        .window_groups
        .push(StateGroup {
            id: 200,
            width: StateWidth::Normal,
            maximized: false,
            tabs: vec![Tab {
                id: 201,
                title: "Chat".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("Chat".into()),
                    label: "Chat".into(),
                },
            }],
            active_tab: 201,
        });
    tree.desktop.workspaces[0]
        .collaborators
        .push(Collaborator::with_active_tab("ada", "Ada", 201).with_session(77));
    tree
}
