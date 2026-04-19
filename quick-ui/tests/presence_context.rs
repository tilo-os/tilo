use quick_ui::presence::{PresenceKey, PathSegment, SelectionRange};

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
