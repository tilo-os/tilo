#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::permissions::UserId;
use tilo_ui::presence::{PathSegment, PresenceCtx, PresenceKey, SelectionRange};

#[test]
fn presence_keys_and_locations_are_single_per_peer() {
    // Case `presence_key_widget_path_equality_and_hash`.
    {
        let a = PresenceKey::Widget {
            path: vec![PathSegment::Field("root".into()), PathSegment::Index(2)],
        };
        let b = PresenceKey::Widget {
            path: vec![PathSegment::Field("root".into()), PathSegment::Index(2)],
        };
        let c = PresenceKey::Widget {
            path: vec![PathSegment::Field("root".into()), PathSegment::Index(3)],
        };
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut map = std::collections::HashMap::new();
        map.insert(a, 1u32);
        assert_eq!(map.get(&b), Some(&1));
        assert_eq!(map.get(&c), None);
    }

    // Case `selection_range_construction`.
    {
        let r = SelectionRange { start: 3, end: 10 };
        assert_eq!(r.len(), 7);
        assert!(!r.is_caret());
        let caret = SelectionRange { start: 5, end: 5 };
        assert!(caret.is_caret());
    }

    // Case `caret_is_single_location_per_peer`.
    {
        // A caret moving from block A to B must not leave a stale one on A.
        let mut ctx = PresenceCtx::empty();
        let a = PresenceKey::Widget {
            path: vec![PathSegment::Index(0)],
        };
        let b = PresenceKey::Widget {
            path: vec![PathSegment::Index(1)],
        };
        let alice = UserId("alice".into());

        ctx.set_caret(&a, alice.clone(), 2);
        assert_eq!(ctx.caret_for(&a, &alice), Some(2));

        ctx.set_caret(&b, alice.clone(), 5);
        assert_eq!(ctx.caret_for(&b, &alice), Some(5));
        assert_eq!(
            ctx.caret_for(&a, &alice),
            None,
            "the caret must clear from the previous block when it moves"
        );
        // A second peer is unaffected.
        let bob = UserId("bob".into());
        ctx.set_caret(&a, bob.clone(), 1);
        ctx.set_caret(&b, alice, 6);
        assert_eq!(
            ctx.caret_for(&a, &bob),
            Some(1),
            "other peers keep their caret"
        );
    }

    // Case `selection_is_single_location_per_peer`.
    {
        let mut ctx = PresenceCtx::empty();
        let a = PresenceKey::Widget {
            path: vec![PathSegment::Index(0)],
        };
        let b = PresenceKey::Widget {
            path: vec![PathSegment::Index(1)],
        };
        let alice = UserId("alice".into());

        ctx.set_selection(&a, alice.clone(), SelectionRange { start: 0, end: 3 });
        assert!(ctx.selection_for(&a, &alice).is_some());
        ctx.set_selection(&b, alice.clone(), SelectionRange { start: 1, end: 2 });
        assert!(
            ctx.selection_for(&a, &alice).is_none(),
            "selection must clear from the previous block when it moves"
        );
    }
}

#[test]
fn presence_queries_return_peers_pointers_and_selections() {
    // Case `peers_on_returns_users_registered_against_key`.
    {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Tab { id: "tab-1".into() };
        ctx.register_presence(&key, UserId("alice".into()));
        ctx.register_presence(&key, UserId("bob".into()));

        let peers = ctx.peers_on(&key);
        assert_eq!(peers.len(), 2);
        assert!(peers.iter().any(|u| u.0 == "alice"));
        assert!(peers.iter().any(|u| u.0 == "bob"));
    }

    // Case `pointer_for_returns_latest_coord`.
    {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Workspace { id: "ws-1".into() };
        ctx.set_pointer(&key, UserId("alice".into()), (10.0, 20.0));
        ctx.set_pointer(&key, UserId("alice".into()), (30.0, 40.0));
        let pos = ctx.pointer_for(&key, &UserId("alice".into()));
        assert_eq!(pos, Some((30.0, 40.0)));
    }

    // Case `selection_for_returns_range`.
    {
        let mut ctx = PresenceCtx::empty();
        let key = PresenceKey::Widget {
            path: vec![PathSegment::Field("body".into())],
        };
        ctx.set_selection(
            &key,
            UserId("alice".into()),
            SelectionRange { start: 4, end: 9 },
        );
        let sel = ctx.selection_for(&key, &UserId("alice".into())).unwrap();
        assert_eq!(sel.start, 4);
        assert_eq!(sel.end, 9);
    }
}

use tilo_ui::theme::Theme;
use vello::Scene;

#[test]
fn peer_carets_and_selections_paint_into_scenes() {
    // Case `draw_peer_caret_emits_scene_content`.
    {
        let ctx = PresenceCtx::empty();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        assert!(
            scene.encoding().is_empty(),
            "precondition: scene must start empty"
        );
        ctx.draw_peer_caret(
            &mut scene,
            &theme,
            vello::kurbo::Affine::IDENTITY,
            &UserId("alice".into()),
            vello::kurbo::Rect::new(100.0, 50.0, 102.0, 70.0),
        );
        assert!(
            !scene.encoding().is_empty(),
            "draw_peer_caret must add geometry to the scene"
        );
    }

    // Case `draw_peer_selection_emits_scene_content`.
    {
        let ctx = PresenceCtx::empty();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        ctx.draw_peer_selection(
            &mut scene,
            &theme,
            vello::kurbo::Affine::IDENTITY,
            &UserId("alice".into()),
            vello::kurbo::Rect::new(10.0, 20.0, 60.0, 40.0),
        );
        assert!(!scene.encoding().is_empty());
    }

    // Case `text_input_renders_peer_caret_when_presence_ctx_has_caret`.
    {
        use tilo_ui::permissions::PermissionContext;
        use tilo_ui::text_render::TextRenderer;
        use tilo_ui::widget::TextInput;
        use tilo_ui::widget::Widget;
        use vello::kurbo::Affine;

        let mut input = TextInput::new(vec![PathSegment::Field("body".into())]);
        let mut presence = PresenceCtx::empty();
        let key = tilo_ui::presence::capability::presence_key(&input)
            .expect("text_input exposes presence_key");
        presence.set_caret(&key, UserId("alice".into()), 3);

        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let perms = PermissionContext::permissive();

        input.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &perms,
        );
        assert!(
            !scene.encoding().is_empty(),
            "peer caret should produce scene geometry"
        );
    }
}
