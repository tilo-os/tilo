struct ProjectionBranch {
    inner: Column,
    root: AccessNode,
}
impl crate::widget::Decorator for ProjectionBranch {
    type Inner = Column;
    fn inner(&self) -> &Column {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Column {
        &mut self.inner
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.root.clone())
    }
}
struct ProjectionLeaf {
    inner: crate::widget::Text,
    calls: std::rc::Rc<std::cell::Cell<usize>>,
}
impl crate::widget::Decorator for ProjectionLeaf {
    type Inner = crate::widget::Text;
    fn inner(&self) -> &Self::Inner {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.inner
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.calls.set(self.calls.get() + 1);
        Some(
            AccessNode::new(Role::Button)
                .with_semantic_key("leaf")
                .with_name("Leaf"),
        )
    }
}
fn projected_survey(
    root: &dyn Widget,
    map: &HashMap<TaffyNodeId, *mut dyn Widget>,
    projection: bool,
) -> Vec<tilo_actions::ActionDescriptor> {
    let mut tree = SemanticTree::new();
    let mut admitted = AdmittedNodeNames::default();
    fn admit_live(
        widget: &dyn Widget,
        map: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: &mut AdmittedNodeNames,
    ) {
        for (&id, &pointer) in map {
            if std::ptr::eq(std::ptr::from_ref(widget), pointer) {
                admitted.insert(id, pointer, widget.name().to_owned());
            }
        }
        widget.for_each_child(&mut |child| admit_live(child, map, admitted));
    }
    admit_live(root, map, &mut admitted);
    if projection {
        tree.refresh_action_projection(root, map, &admitted, None);
    } else {
        tree.refresh_bound(root, map, &admitted, None, &[]);
    }
    tilo_actions::door::survey(&tree.action_resources())
}
#[test]
fn action_projection_preserves_exported_surface_and_skips_non_durable_descendants() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let branch = ProjectionBranch {
        inner: Column::new().push(Box::new(ProjectionLeaf {
            inner: crate::widget::Text::new("Prose"),
            calls: std::rc::Rc::clone(&calls),
        })),
        root: AccessNode::new(Role::Group),
    };
    let mut root = Column::new()
        .push(Box::new(branch))
        .push(Box::new(Button::new("durable", "Durable")));
    let map = layout(&mut root);
    let full = projected_survey(&root, &map, false);
    assert!(!full.is_empty(), "a keyed sibling remains exported");
    assert!(
        calls.get() > 0,
        "full a11y still visits non-durable descendants"
    );
    calls.set(0);
    assert_eq!(projected_survey(&root, &map, true), full);
    assert_eq!(
        calls.get(),
        0,
        "publication must not build the discarded branch"
    );
}
#[test]
fn action_projection_keeps_duplicate_and_ambiguous_key_rejection() {
    for key in ["duplicate", "parent~0~k:7"] {
        let mut root = Column::new()
            .push(Box::new(Button::new(key, "First")))
            .push(Box::new(Button::new(key, "Second")));
        let map = layout(&mut root);
        assert_eq!(
            projected_survey(&root, &map, true),
            projected_survey(&root, &map, false)
        );
        assert!(
            projected_survey(&root, &map, true).is_empty(),
            "duplicate durable controls remain unexported"
        );
    }
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let pruned = ProjectionBranch {
        inner: Column::new().push(Box::new(ProjectionLeaf {
            inner: crate::widget::Text::new("Prose"),
            calls: std::rc::Rc::clone(&calls),
        })),
        root: AccessNode::new(Role::Group),
    };
    let mut root = Column::new()
        .push(Box::new(pruned))
        .push(Box::new(Button::new("parent~0~k:7", "Ambiguous")));
    let map = layout(&mut root);
    let full = projected_survey(&root, &map, false);
    calls.set(0);
    assert_eq!(projected_survey(&root, &map, true), full);
    assert!(
        calls.get() > 0,
        "ambiguous IDs retain the complete duplicate-detection walk"
    );
}

#[test]
fn action_projection_rejects_a_durable_key_colliding_with_a_pruned_position() {
    let branch = ProjectionBranch {
        inner: Column::new().push(Box::new(Button::new("leaf", "Positional descendant"))),
        root: AccessNode::new(Role::Group),
    };
    let root = Column::new()
        .push(Box::new(Button::new("prefix", "Prefix")))
        .push(Box::new(branch))
        .push(Box::new(Button::new("prefix~1~k:leaf", "Forged collision")));
    // A currently unlaid-out tree uses positional IDs under its last sibling.
    let map = HashMap::new();
    let full = projected_survey(&root, &map, false);
    assert!(
        full.iter()
            .all(|action| action.resource_label != "Forged collision")
    );
    assert_eq!(projected_survey(&root, &map, true), full);
}
