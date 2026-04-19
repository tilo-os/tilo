use quick_ui::presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
use quick_ui::permissions::UserId;

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
