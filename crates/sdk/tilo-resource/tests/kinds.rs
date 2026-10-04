#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A host that knows a resource only by the kind its record names reads,
//! publishes and replays it exactly as the typed resource does.

use tilo_resource::{Doc, Kinds, Replica, Resource, ResourceKind};

use crate::golden_path::Board;

fn kinds() -> Kinds {
    Kinds::new().with(ResourceKind::of::<Board>().hosted_by("example.boards", "Boards"))
}

#[test]
fn a_kind_reads_and_publishes_what_the_typed_resource_does() {
    let mut replica = Replica::<Board>::new();
    replica.apply(Board::RENAME.with("Launch".into())).unwrap();
    let kinds = kinds();
    let board = kinds.get("test.board").expect("registered by kind");
    assert_eq!(
        kinds.hosted_by("example.boards").unwrap().kind(),
        "test.board"
    );
    assert_eq!(board.app_title(), Some("Boards"));

    let state: Board = board.read_as(replica.doc()).unwrap();
    assert_eq!(state.title, "Launch");
    assert!(board.read_as::<String>(replica.doc()).is_err());

    let erased = board.publish("test.board", replica.doc()).unwrap();
    let typed = tilo_resource::publish::<Board>("test.board", "Board", &replica.state().unwrap());
    let ids = |resource: &tilo_actions::Resource| {
        resource
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&erased), ids(&typed));
    assert_eq!(erased.kind, "test.board");
    assert_eq!(erased.label, "Board");
}

#[test]
fn a_kind_names_an_open_resource_from_its_state() {
    fn titled(state: &dyn std::any::Any) -> Option<String> {
        state
            .downcast_ref::<Board>()
            .map(|board| board.title.clone())
    }
    let mut replica = Replica::<Board>::new();
    replica.apply(Board::RENAME.with("Roadmap".into())).unwrap();
    let board = ResourceKind::of::<Board>().named(titled);
    assert_eq!(
        board.publish("test.board", replica.doc()).unwrap().label,
        "Roadmap"
    );
}

#[test]
fn a_typed_action_built_by_kind_replays_through_the_registry() {
    let kinds = kinds();
    let board = kinds.get("test.board").unwrap();
    let registry = kinds.registry();
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".into(), serde_json::json!("Renamed"));
    let action = tilo_resource::build_action::<Board>(Board::RENAME.id, &arguments, 0).unwrap();
    let encoded = board
        .encode(serde_json::to_value(&action).unwrap())
        .unwrap();
    assert_eq!(encoded.type_id, Board::ACTION_TYPE);

    let mut doc = Doc::new();
    board.initialize(&mut doc).unwrap();
    registry.decode(&encoded).unwrap().apply(&mut doc).unwrap();
    doc.commit();
    assert_eq!(board.read_as::<Board>(&doc).unwrap().title, "Renamed");

    assert!(
        board
            .encode(serde_json::json!({"no": "such action"}))
            .is_err()
    );
}
