use ui::{UiGraphHandle, UiNode};

#[test]
fn serialized_graph_edits_have_shared_undo_history() {
    let graph = UiGraphHandle::from_json(include_str!("../ui/main.json"))
        .expect("the generated graph is valid");
    for next_label in ["Publ", "Publish"] {
        graph
            .update_node_grouped("graph-introduction", "intro-text", |node| {
                if let UiNode::Text {
                    text: ui::UiTextSource::Static { value },
                    ..
                } = node
                {
                    *value = next_label.into();
                }
            })
            .expect("a valid grouped runtime edit is published");
    }
    assert!(matches!(
        graph.node("graph-introduction"),
        Ok(UiNode::Text { text: ui::UiTextSource::Static { value }, .. }) if value == "Publish"
    ));

    assert!(graph.undo().expect("graph history remains available"));
    assert!(matches!(
        graph.node("graph-introduction"),
        Ok(UiNode::Text { text: ui::UiTextSource::Static { value }, .. })
            if value == "graph-introduction"
    ));
    assert!(!graph.can_undo(), "continuous typing created one undo step");
    assert!(graph.redo().expect("the undone edit can be reapplied"));
}
