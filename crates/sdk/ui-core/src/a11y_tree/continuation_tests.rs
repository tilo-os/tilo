use super::*;
use tilo_actions::{ActionInput, ContinuationArguments, ContinuationEvidence, ContinuationQuery};

fn projected() -> (SemanticTree, SemanticId) {
    let mut field =
        crate::widget::EditableLabel::new("Original").with_accessible_name("Page title");
    field.set_semantic_target(Some("notes:page-title:page-a".into()));
    let mut taffy = taffy::TaffyTree::<()>::new();
    let mut map = HashMap::new();
    let root = field.build_layout(&mut taffy, &crate::theme::Theme::dark(), &mut map);
    taffy
        .compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(800.),
                height: taffy::AvailableSpace::Definite(600.),
            },
        )
        .unwrap();
    field.apply_layout(&taffy, root, vello::kurbo::Point::ZERO);
    let mut tree = SemanticTree::new();
    tree.refresh(&field, &map, None, &[]);
    let id = tree.find_by_name("Page title").unwrap().id.clone();
    (tree, id)
}

fn spec(tree: &SemanticTree) -> ActionSpec {
    tree.action_resources()
        .into_iter()
        .flat_map(|resource| resource.actions)
        .find(|action| matches!(action.input, ActionInput::Prompted { .. }))
        .expect("actual generated SetValue declaration")
}

fn draft(tree: &SemanticTree) -> Option<ContinuationEvidence> {
    spec(tree).continuation_identity(ContinuationQuery::Draft)
}

#[test]
fn semantic_set_value_continuation_preserves_both_dynamic_encoder_lanes() {
    let (tree, id) = projected();
    let spec = spec(&tree);
    assert!(spec.fixed_invocation_encoding().is_none());
    assert!(matches!(spec.input, ActionInput::Prompted { .. }));
    assert!(matches!(
        draft(&tree),
        Some(ContinuationEvidence::Draft { .. })
    ));
    let resources = tree.action_resources();
    let address = tilo_actions::ActionAddress::new(resources[0].id.as_str(), "set-value");
    let raw = "  New title\nß  ";
    let args = serde_json::json!({"value":raw})
        .as_object()
        .unwrap()
        .clone();
    let legacy = tilo_actions::door::encode(&resources, &address, Some(raw)).unwrap();
    let typed = tilo_actions::door::encode_json(&resources, &address, &args).unwrap();
    assert_eq!(legacy, typed);
    let operation: SemanticOperation = serde_json::from_slice(&legacy.payload).unwrap();
    assert_eq!(operation.node, id.0);
    assert_eq!(operation.action, "set-value");
    assert_eq!(operation.value.as_deref(), Some(raw));
    let exact =
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Text(raw)));
    assert!(matches!(exact, Some(ContinuationEvidence::Exact { .. })));
    assert_eq!(
        exact,
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(&args)))
    );
    assert_ne!(
        exact,
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Text(
            raw.trim()
        )))
    );
    assert!(
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
            .is_none()
    );
    for args in [
        serde_json::json!({}),
        serde_json::json!({"value":1}),
        serde_json::json!({"value":"x","extra":true}),
    ] {
        assert!(
            spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                args.as_object().unwrap()
            )))
            .is_none()
        );
    }
}

#[test]
fn semantic_set_value_continuation_binds_current_dispatch_route() {
    let (mut tree, id) = projected();
    let original = tree.nodes[&id].clone();
    let baseline = draft(&tree).expect("generated current route has Draft");
    assert_eq!(draft(&tree), Some(baseline.clone()));
    let changes: Vec<Box<dyn Fn(&mut SemanticNode)>> = vec![
        Box::new(|node| node.id.0.push_str("-replacement")),
        Box::new(|node| node.semantic_target = Some("notes:page-title:page-b".into())),
        Box::new(|node| node.widget_taffy_id = node.widget_taffy_id.map(|id| id + 1)),
        Box::new(|node| node.role = Role::MultilineTextInput),
        Box::new(|node| {
            node.actions
                .iter_mut()
                .find(|action| action.name == "set-value")
                .unwrap()
                .name = "replace-value".into()
        }),
        Box::new(|node| node.bounds = Some(Rect::new(4., 5., 60., 70.))),
        Box::new(|node| node.semantic_target = Some("tilo.key-action:owner:2/3".into())),
    ];
    for (index, change) in changes.into_iter().enumerate() {
        tree.nodes.insert(id.clone(), original.clone());
        change(tree.nodes.get_mut(&id).unwrap());
        assert_ne!(draft(&tree), Some(baseline.clone()), "route change {index}");
    }
    tree.nodes.insert(id.clone(), original);
    assert_eq!(draft(&tree), Some(baseline));
}

#[test]
fn semantic_set_value_continuation_refuses_unknown_or_special_admission() {
    let (mut tree, id) = projected();
    let original = tree.nodes[&id].clone();
    tree.nodes.get_mut(&id).unwrap().widget_taffy_id = None;
    assert!(draft(&tree).is_none());
    tree.nodes.insert(id.clone(), original.clone());
    tree.nodes.get_mut(&id).unwrap().semantic_target = Some("ordinary-confirmation:approve".into());
    assert!(draft(&tree).is_none());
    tree.nodes.insert(id.clone(), original.clone());
    tree.nodes.get_mut(&id).unwrap().bounds = Some(Rect::new(f64::NAN, 0., 0., 0.));
    assert!(draft(&tree).is_none());
    tree.nodes.insert(id.clone(), original.clone());
    tree.nodes.get_mut(&id).unwrap().bounds = Some(Rect::new(f64::MAX, 0., f64::MAX, 0.));
    assert!(
        draft(&tree).is_none(),
        "finite corners can still overflow the dispatched center"
    );
    tree.nodes.insert(id.clone(), original.clone());
    tree.nodes.get_mut(&id).unwrap().semantic_target = None;
    assert!(
        tree.action_resources()
            .iter()
            .flat_map(|resource| &resource.actions)
            .all(|action| action.id != "set-value")
    );
    tree.nodes.insert(id.clone(), original.clone());
    tree.nodes.get_mut(&id).unwrap().durable_identity = false;
    assert!(tree.action_resources().is_empty());
    tree.nodes.insert(id.clone(), original);
    tree.duplicate_ids.push(id);
    assert!(tree.action_resources().is_empty());
}
