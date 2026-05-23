use ui::presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
use ui::permissions::UserId;

#[test]
fn presence_key_widget_path_equality_and_hash() {
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
    map.insert(a.clone(), 1u32);
    assert_eq!(map.get(&b), Some(&1));
    assert_eq!(map.get(&c), None);
}

#[test]
fn selection_range_construction() {
    let r = SelectionRange { start: 3, end: 10 };
    assert_eq!(r.len(), 7);
    assert!(!r.is_caret());
    let caret = SelectionRange { start: 5, end: 5 };
    assert!(caret.is_caret());
}

#[test]
fn peers_on_returns_users_registered_against_key() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Tab { id: "tab-1".into() };
    ctx.register_presence(&key, UserId("alice".into()));
    ctx.register_presence(&key, UserId("bob".into()));

    let peers = ctx.peers_on(&key);
    assert_eq!(peers.len(), 2);
    assert!(peers.iter().any(|u| u.0 == "alice"));
    assert!(peers.iter().any(|u| u.0 == "bob"));
}

#[test]
fn pointer_for_returns_latest_coord() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Workspace { id: "ws-1".into() };
    ctx.set_pointer(&key, UserId("alice".into()), (10.0, 20.0));
    ctx.set_pointer(&key, UserId("alice".into()), (30.0, 40.0));
    let pos = ctx.pointer_for(&key, &UserId("alice".into()));
    assert_eq!(pos, Some((30.0, 40.0)));
}

#[test]
fn selection_for_returns_range() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Widget { path: vec![PathSegment::Field("body".into())] };
    ctx.set_selection(
        &key,
        UserId("alice".into()),
        SelectionRange { start: 4, end: 9 },
    );
    let sel = ctx.selection_for(&key, &UserId("alice".into())).unwrap();
    assert_eq!(sel.start, 4);
    assert_eq!(sel.end, 9);
}

use vello::Scene;
use ui::theme::Theme;

#[test]
fn draw_peer_caret_emits_scene_content() {
    let ctx = PresenceCtx::empty();
    let mut scene = Scene::new();
    let theme = Theme::dark();
    // A fresh scene is structurally empty; after drawing a caret it must have content.
    assert!(scene.encoding().is_empty(),
        "precondition: scene must start empty");
    ctx.draw_peer_caret(&mut scene, &theme, &UserId("alice".into()), (100.0, 50.0));
    assert!(!scene.encoding().is_empty(),
        "draw_peer_caret must add geometry to the scene");
}

#[test]
fn draw_peer_selection_emits_scene_content() {
    let ctx = PresenceCtx::empty();
    let mut scene = Scene::new();
    let theme = Theme::dark();
    ctx.draw_peer_selection(
        &mut scene,
        &theme,
        &UserId("alice".into()),
        &SelectionRange { start: 0, end: 5 },
        /* line_rects = */ &[vello::kurbo::Rect::new(10.0, 20.0, 60.0, 40.0)],
    );
    assert!(!scene.encoding().is_empty());
}

#[test]
fn text_input_renders_peer_caret_when_presence_ctx_has_caret() {
    use ui::widget::TextInput;
    use ui::text_render::TextRenderer;
    use ui::permissions::PermissionContext;
    use ui::widget::Widget;
    use vello::kurbo::Affine;

    let mut input = TextInput::new(vec![PathSegment::Field("body".into())]);
    let mut presence = PresenceCtx::empty();
    let key = input.presence_key().expect("text_input exposes presence_key");
    presence.set_caret(&key, UserId("alice".into()), 3);

    let mut scene = Scene::new();
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let perms = PermissionContext::permissive();

    input.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
    assert!(!scene.encoding().is_empty(),
        "peer caret should produce scene geometry");
}

use ui::collaboration::{CollabEvent, PresencePayload};

#[test]
fn remote_presence_update_round_trips_through_serde() {
    let event = CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: PresenceKey::Tab { id: "tab-1".into() },
        payload: PresencePayload::Caret { pos: 12 },
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: CollabEvent = serde_json::from_str(&json).unwrap();
    match back {
        CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
            assert_eq!(user_id, "alice");
            assert_eq!(key, PresenceKey::Tab { id: "tab-1".into() });
            assert!(matches!(payload, PresencePayload::Caret { pos: 12 }));
        }
        _ => panic!("wrong variant"),
    }
}
