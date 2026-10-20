use serde_json::json;
use tilo_ui_graph::{UiGraphEdit, UiNode};
use tilolang_tilo::ui::{
    ActionRef, Bind, Control, Request, ResourceRef, Translator, UiElem, UiEvent, UiPatch,
};

fn counter_ref() -> ResourceRef {
    ResourceRef {
        app: "so.tilo.counter".into(),
        type_id: "tcnt0001".into(),
        id: "c1".into(),
    }
}

fn plus() -> ActionRef {
    ActionRef {
        app: "so.tilo.counter".into(),
        action_id: "aincr001".into(),
        resource: Some(counter_ref()),
        args: serde_json::Map::new(),
    }
}

fn counter_view(value: i64) -> UiElem {
    UiElem::new("root", "column")
        .child(UiElem::new("n1", "text").prop("text", value))
        .child(
            UiElem::new("plus", "button")
                .prop("label", "+1")
                .on_press(plus()),
        )
}

#[test]
fn a_tree_becomes_a_graph_with_node_ids_from_keys() {
    let mut translator = Translator::new();
    let rendered = translator.load(counter_view(0)).unwrap();
    let document = &rendered.document;
    assert!(document.node("n1").is_some());
    match document.node("plus").unwrap() {
        UiNode::Button {
            label,
            on_click_path,
            enabled,
            ..
        } => {
            assert_eq!(label, "+1");
            assert_eq!(on_click_path.as_deref(), Some("plus"));
            assert!(enabled);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rendered.controls.len(), 1);
}

#[test]
fn a_changed_property_is_one_property_edit() {
    let mut translator = Translator::new();
    translator.load(counter_view(0)).unwrap();
    let update = translator
        .apply(&[UiPatch::SetProp {
            key: "n1".into(),
            name: "text".into(),
            value: json!(1),
        }])
        .unwrap();
    assert_eq!(
        update.edits,
        [UiGraphEdit::SetProperty {
            id: "n1".into(),
            property: "text.value".into(),
            value: json!("1")
        }]
    );
}

#[test]
fn inserts_removes_and_moves_become_graph_edits_and_keep_the_graph_valid() {
    let mut translator = Translator::new();
    let rendered = translator.load(counter_view(0)).unwrap();
    let mut document = rendered.document;
    let patches = [
        UiPatch::Insert {
            parent: "root".into(),
            index: 0,
            node: UiElem::new("title", "heading").prop("text", "Counter"),
        },
        UiPatch::Move {
            key: "plus".into(),
            parent: "root".into(),
            index: 0,
        },
        UiPatch::Remove { key: "n1".into() },
    ];
    let update = translator.apply(&patches).unwrap();
    document.apply_edits(update.edits).unwrap();
    let ids: Vec<&str> = match &document.root {
        UiNode::Column { children, .. } => children.iter().map(UiNode::id).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(ids, ["plus", "title"]);
    assert_eq!(update.unregistered, ["n1"]);
    // The kept tree is what a fresh translation sees.
    let kept = translator.tree().unwrap().clone();
    let fresh = Translator::new().load(kept).unwrap();
    assert_eq!(fresh.document, document);
}

#[test]
fn a_property_without_a_graph_counterpart_replaces_the_node() {
    let mut translator = Translator::new();
    translator.load(counter_view(0)).unwrap();
    let update = translator
        .apply(&[UiPatch::SetProp {
            key: "plus".into(),
            name: "primary".into(),
            value: json!(true),
        }])
        .unwrap();
    assert!(matches!(update.edits.as_slice(), [UiGraphEdit::Replace { id, .. }] if id == "plus"));
}

#[test]
fn clicks_edits_and_toggles_become_runtime_requests() {
    let bind = |field: &str| Bind {
        resource: counter_ref(),
        field: field.into(),
    };
    let tree = UiElem::new("root", "column")
        .child(
            UiElem::new("plus", "button")
                .prop("label", "+1")
                .on_press(plus()),
        )
        .child(
            UiElem::new("name", "input")
                .prop("label", "Name")
                .prop("value", "ada")
                .bound(bind("fname")),
        )
        .child(
            UiElem::new("done", "checkbox")
                .prop("label", "Done")
                .prop("checked", false)
                .bound(bind("fdone")),
        );
    let mut translator = Translator::new();
    translator.load(tree).unwrap();
    assert_eq!(
        translator.request(&UiEvent::Press { key: "plus".into() }),
        Some(Request::Invoke(plus()))
    );
    assert_eq!(
        translator.request(&UiEvent::Edit {
            key: "name".into(),
            text: "adam".into()
        }),
        Some(Request::SetText {
            bind: bind("fname"),
            text: "adam".into()
        })
    );
    assert_eq!(
        translator.request(&UiEvent::Toggle {
            key: "done".into(),
            checked: true
        }),
        Some(Request::SetFlag {
            bind: bind("fdone"),
            checked: true
        })
    );
    assert_eq!(
        translator.request(&UiEvent::Press { key: "name".into() }),
        None
    );
    assert!(
        matches!(translator.control("name"), Some(Control::Text { shown, .. }) if shown == "ada")
    );
    // An edit coming from a collaborator updates what the input shows, not the graph.
    let update = translator
        .apply(&[UiPatch::SetProp {
            key: "name".into(),
            name: "value".into(),
            value: json!("ada l"),
        }])
        .unwrap();
    assert!(update.edits.is_empty());
    assert_eq!(update.text, [("name".to_owned(), "ada l".to_owned())]);
}

#[test]
fn an_unlabelled_input_is_refused_rather_than_given_an_invented_name() {
    let bind = Bind {
        resource: counter_ref(),
        field: "f".into(),
    };
    let tree = UiElem::new("root", "column").child(UiElem::new("name", "input").bound(bind));
    assert!(Translator::new().load(tree).is_err());
}
