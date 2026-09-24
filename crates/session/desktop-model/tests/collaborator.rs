//! The collaborator row and its persisted shape.

use tilo_desktop_model::Collaborator;

#[test]
fn new_initializes_all_concept_fields_to_none() {
    let c = Collaborator::new("alice", "Alice");
    assert_eq!(c.id, "alice");
    assert_eq!(c.name, "Alice");
    assert_eq!(c.active_tab, None);
    assert_eq!(c.cursor_color, None);
    assert_eq!(c.avatar, None);
    assert_eq!(c.role_id, None);
}

#[test]
fn with_active_tab_delegates_to_new_and_sets_tab() {
    let c = Collaborator::with_active_tab("bob", "Bob", 42);
    assert_eq!(c.active_tab, Some(42));
    // Other concept fields stay at their defaults.
    assert_eq!(c.cursor_color, None);
    assert_eq!(c.avatar, None);
    assert_eq!(c.role_id, None);
}

#[test]
fn with_cursor_color_sets_hex_override() {
    let c = Collaborator::new("alice", "Alice").with_cursor_color("#ff00aa");
    assert_eq!(c.cursor_color.as_deref(), Some("#ff00aa"));
}

#[test]
fn with_avatar_sets_reference() {
    let c = Collaborator::new("charlie", "Charlie").with_avatar("CC");
    assert_eq!(c.avatar.as_deref(), Some("CC"));
}

#[test]
fn with_role_sets_role_id() {
    let c = Collaborator::new("bob", "Bob").with_role("editor");
    assert_eq!(c.role_id.as_deref(), Some("editor"));
}

#[test]
fn builder_methods_can_be_chained() {
    let c = Collaborator::with_active_tab("me", "Me", 7)
        .with_cursor_color("#112233")
        .with_avatar("ME")
        .with_role("owner");
    assert_eq!(c.active_tab, Some(7));
    assert_eq!(c.cursor_color.as_deref(), Some("#112233"));
    assert_eq!(c.avatar.as_deref(), Some("ME"));
    assert_eq!(c.role_id.as_deref(), Some("owner"));
}

#[test]
fn legacy_json_without_concept_fields_still_deserializes() {
    // Workspaces persisted before the struct grew concept fields
    // omit them; `#[serde(default)]` keeps those payloads loadable.
    let legacy = r#"{"id":"alice","name":"Alice","active_tab":null}"#;
    let c: Collaborator = serde_json::from_str(legacy).unwrap();
    assert_eq!(c.id, "alice");
    assert_eq!(c.name, "Alice");
    assert_eq!(c.active_tab, None);
    assert_eq!(c.cursor_color, None);
    assert_eq!(c.avatar, None);
    assert_eq!(c.role_id, None);
    // Present, not invited — defaulting the other way would redraw
    // every already-saved participant as someone who never showed.
    assert!(!c.invited);
}

#[test]
fn fully_populated_round_trip() {
    let c = Collaborator::with_active_tab("alice", "Alice", 3)
        .with_cursor_color("#aabbcc")
        .with_avatar("A")
        .with_role("editor");
    let json = serde_json::to_string(&c).unwrap();
    let back: Collaborator = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
}
