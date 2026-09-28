use super::*;
use crate::theme::Theme;
use taffy::TaffyTree;

struct Leaf {
    name: &'static str,
}

#[repr(C)]
struct Parent {
    child: Leaf,
    name: &'static str,
}

impl Widget for Leaf {
    crate::testing::inert_widget_methods!(apply_layout, draw, get_rect, leaf_layout, name_field);
    fn accessibility(&self) -> Option<AccessNode> {
        Some(AccessNode::new(Role::Button).with_semantic_key("child"))
    }
}

impl Widget for Parent {
    crate::testing::inert_widget_methods!(apply_layout, draw, get_rect, name_field);
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<TaffyNodeId, *mut dyn Widget>,
    ) -> TaffyNodeId {
        let child = self.child.build_layout(tree, theme, nodes);
        let node = tree
            .new_with_children(taffy::Style::default(), &[child])
            .unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.child]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.child]
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(AccessNode::new(Role::Generic).with_semantic_key("parent"))
    }
}

fn fixture() -> Parent {
    Parent {
        child: Leaf { name: "Leaf" },
        name: "Parent",
    }
}

#[test]
fn semantic_identity_parent_and_first_field_keep_their_exact_layout_ids() {
    let mut root = fixture();
    assert_eq!(
        std::ptr::from_ref(&root).cast::<()>(),
        std::ptr::from_ref(&root.child).cast::<()>()
    );
    let mut layout = TaffyTree::new();
    let mut nodes = HashMap::new();
    let parent = root.build_layout(&mut layout, &Theme::dark(), &mut nodes);
    let child = layout.children(parent).unwrap()[0];
    let mut semantics = SemanticTree::new();
    semantics.refresh(&root, &nodes, Some(child), &[]);
    let parent_node = semantics.node(&SemanticId("root~k:parent".into())).unwrap();
    let child_node = semantics
        .node(&SemanticId("root~k:parent~k:child".into()))
        .unwrap();
    assert_eq!(parent_node.widget_taffy_id, Some(u64::from(parent)));
    assert_eq!(child_node.widget_taffy_id, Some(u64::from(child)));
    assert!(!parent_node.state.focused);
    assert!(child_node.state.focused);
    for _ in 0..16 {
        let rehashed = nodes.iter().map(|(id, pointer)| (*id, *pointer)).collect();
        assert!(
            semantics
                .refresh(&root, &rehashed, Some(child), &[])
                .is_empty()
        );
    }
}

#[test]
fn semantic_identity_ambiguous_duplicate_registration_has_no_layout_dispatch() {
    let mut leaf = Leaf { name: "Leaf" };
    let mut layout = TaffyTree::new();
    let mut nodes = HashMap::new();
    leaf.build_layout(&mut layout, &Theme::dark(), &mut nodes);
    let duplicate = layout.new_leaf(taffy::Style::default()).unwrap();
    nodes.insert(duplicate, std::ptr::from_mut::<dyn Widget>(&mut leaf));
    let mut semantics = SemanticTree::new();
    semantics.refresh(&leaf, &nodes, Some(duplicate), &[]);
    let node = semantics.node(&SemanticId("root~k:child".into())).unwrap();
    assert_eq!(
        node.widget_taffy_id, None,
        "an arbitrary duplicate must not become a dispatch id"
    );
    assert!(!node.state.focused);
}

#[test]
fn semantic_identity_unknown_raw_pointer_is_never_dereferenced() {
    let leaf = Leaf { name: "Leaf" };
    let mut nodes = HashMap::new();
    let stale: *mut dyn Widget = std::ptr::NonNull::<Leaf>::dangling().as_ptr();
    nodes.insert(TaffyNodeId::from(99_u64), stale);
    let mut semantics = SemanticTree::new();
    semantics.refresh(&leaf, &nodes, None, &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        None
    );
}

#[test]
fn semantic_identity_private_refresh_requires_matching_admitted_entry() {
    let mut leaf = Leaf { name: "Leaf" };
    let mut layout = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = leaf.build_layout(&mut layout, &Theme::dark(), &mut nodes);
    let mut semantics = SemanticTree::new();
    let absent = AdmittedNodeNames::default();
    semantics.refresh_bound(&leaf, &nodes, &absent, Some(node), &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        None
    );
    let mut replaced = AdmittedNodeNames::default();
    let stale: *mut dyn Widget = std::ptr::NonNull::<Leaf>::dangling().as_ptr();
    replaced.insert(node, stale, leaf.name().to_owned());
    semantics.refresh_bound(&leaf, &nodes, &replaced, Some(node), &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        None
    );
}

#[test]
fn semantic_identity_actual_admission_preserves_names_and_rejects_replaced_pointer() {
    let mut root = fixture();
    let mut layout = TaffyTree::new();
    let mut nodes = HashMap::new();
    let parent = root.build_layout(&mut layout, &Theme::dark(), &mut nodes);
    let child = layout.children(parent).unwrap()[0];
    let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the fixture owns this complete live tree and its exact layout.
    unsafe { bindings.rebuild(&root, &layout, parent, &nodes) };
    let exported = bindings.exported_nodes(&nodes);
    let mut semantics = SemanticTree::new();
    semantics.refresh_bound(&root, &exported.nodes, &exported.names, Some(child), &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:parent".into()))
            .unwrap()
            .widget_taffy_id,
        Some(u64::from(parent))
    );
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:parent~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        Some(u64::from(child))
    );
    let stale: *mut dyn Widget = std::ptr::NonNull::<Leaf>::dangling().as_ptr();
    nodes.insert(child, stale);
    let replaced = bindings.exported_nodes(&nodes);
    assert!(!replaced.nodes.contains_key(&child));
    semantics.refresh_bound(&root, &replaced.nodes, &replaced.names, None, &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:parent~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        None
    );
}

#[test]
fn semantic_identity_admitted_unique_names_handle_different_coercion_metadata() {
    let root = fixture();
    assert_different_metadata_mapping(&root, true);
}

#[test]
fn semantic_identity_same_name_alias_without_exact_metadata_is_structural() {
    let root = Parent {
        child: Leaf { name: "Same" },
        name: "Same",
    };
    assert_different_metadata_mapping(&root, false);
}

fn assert_different_metadata_mapping(root: &Parent, uniquely_named: bool) {
    let parent = TaffyNodeId::from(1_u64);
    let child = TaffyNodeId::from(2_u64);
    // Force distinct comparison metadata without relying on compiler vtable
    // duplication choices. This inert pointer is NEVER dereferenced. The
    // production sidecar is separately tested through actual live admission.
    let marker = crate::widget::Button::new("marker", "Marker", None);
    let different = std::ptr::from_ref::<dyn Widget>(&marker)
        .with_addr(std::ptr::from_ref(root).addr()) as *mut dyn Widget;
    assert!(!std::ptr::eq(
        different,
        std::ptr::from_ref::<dyn Widget>(root)
    ));
    assert!(!std::ptr::eq(
        different,
        std::ptr::from_ref::<dyn Widget>(&root.child)
    ));
    let nodes = HashMap::from([(parent, different), (child, different)]);
    let mut names = AdmittedNodeNames::default();
    names.insert(parent, different, root.name().to_owned());
    names.insert(child, different, root.child.name().to_owned());
    let mut semantics = SemanticTree::new();
    // Public raw-only callers cannot disambiguate this alias and never receive
    // an arbitrary layout id even though the address is present.
    semantics.refresh(root, &nodes, Some(child), &[]);
    assert!(semantics.iter().all(|node| node.widget_taffy_id.is_none()));
    semantics.refresh_bound(root, &nodes, &names, Some(child), &[]);
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:parent".into()))
            .unwrap()
            .widget_taffy_id,
        uniquely_named.then_some(u64::from(parent))
    );
    assert_eq!(
        semantics
            .node(&SemanticId("root~k:parent~k:child".into()))
            .unwrap()
            .widget_taffy_id,
        uniquely_named.then_some(u64::from(child))
    );
    for _ in 0..16 {
        let rehashed = nodes.iter().map(|(id, pointer)| (*id, *pointer)).collect();
        assert!(
            semantics
                .refresh_bound(root, &rehashed, &names, Some(child), &[])
                .is_empty()
        );
    }
}
