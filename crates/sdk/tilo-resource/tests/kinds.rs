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
    replica.apply(Board::RENAME.with("Launch")).unwrap();
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
    replica.apply(Board::RENAME.with("Roadmap")).unwrap();
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

#[test]
fn typed_and_erased_publications_declare_the_original_resource_identity() {
    let replica = Replica::<Board>::new();
    let id = "opened-board";
    let expected = tilo_actions::ResourceSemanticIdentity::new(Board::KIND, id);
    let typed = tilo_resource::publish::<Board>(id, "Board", &replica.state().unwrap());
    let erased = ResourceKind::of::<Board>()
        .publish(id, replica.doc())
        .unwrap();
    assert_eq!(typed.semantic_identity, expected);
    assert_eq!(erased.semantic_identity, expected);
    assert_eq!(typed.id.as_str(), id);
    assert_eq!(erased.id.as_str(), id);
}

#[test]
fn stateless_typed_publication_preserves_identity_validation_bounds() {
    let publish =
        |id: &str| tilo_resource::publish_child::<Board, Board>(id, "Board", None, |action| action);
    let maximum = "x".repeat(tilo_actions::ResourceSemanticIdentity::MAX_KEY_BYTES);
    for id in ["stateless-board", maximum.as_str()] {
        let published = publish(id);
        assert_eq!(
            published.semantic_identity,
            tilo_actions::ResourceSemanticIdentity::new(Board::KIND, id)
        );
        assert!(published.semantic_identity.is_some());
        assert_eq!(published.id.as_str(), id);
    }
    let oversized = "x".repeat(tilo_actions::ResourceSemanticIdentity::MAX_KEY_BYTES + 1);
    for id in ["", "board\n", oversized.as_str()] {
        let published = publish(id);
        assert!(published.semantic_identity.is_none());
        assert_eq!(published.id.as_str(), id);
    }
}
