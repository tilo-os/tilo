/// A component's published tree as a shared forest that each edit changes in
/// a few nodes (TIL-766). After every edit of a random walk — a root or item
/// relabelled, items added, removed, moved, given actions or children, a
/// repeated id and a grant expiring and coming back — what is derived from it
/// incrementally (its permission filtering, its controls' allowed addresses,
/// its semantic nodes and the delta reported for them) is exactly what a
/// derivation from the same tree as ordinary resources gives.
#[test]
fn a_shared_forest_derives_exactly_what_a_fresh_tree_does_after_any_edit() {
    use crate::published::{SharedChildren, SharedResource};
    use tilo_actions::{ActionSpec, OperationKind, Resource};

    let fixture = ActionFixture::new();
    let binding = fixture.bindings[0].clone();
    let action = |id: &str| {
        ActionSpec::operation(id, id, OperationKind::Effect, "bound.follow-up", &()).unwrap()
    };
    let node = |id: &str, label: &str, actions: &[&str], children: Vec<SharedResource>| {
        let head = actions
            .iter()
            .fold(Resource::new(id, "probe.item", label), |head, id| {
                head.with_action(action(id))
            });
        SharedResource::new(head, SharedChildren::new(children))
    };
    let at = |time| {
        let moved = fixture.permissions.borrow().clone().at_unix_time(time);
        *fixture.permissions.borrow_mut() = moved;
    };
    let outline = |resources: &[Resource]| {
        fn walk(resource: &Resource, out: &mut Vec<String>) {
            out.push(format!(
                "{} {} [{}] ({}",
                resource.id.as_str(),
                resource.label,
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                resource.children.len()
            ));
            for child in &resource.children {
                walk(child, out);
            }
            out.push(")".into());
        }
        let mut out = Vec::new();
        for resource in resources {
            walk(resource, &mut out);
        }
        out
    };
    let content = |tree: &crate::a11y_tree::SemanticTree| {
        let mut bound = tree
            .bound_actions_for_test()
            .into_iter()
            .map(|(id, target)| (id, target.address.address))
            .collect::<Vec<_>>();
        bound.sort_by(|left, right| {
            (&left.0, &left.1.resource, &left.1.action).cmp(&(
                &right.0,
                &right.1.resource,
                &right.1.action,
            ))
        });
        (
            tree.iter().cloned().collect::<Vec<_>>(),
            bound,
            tree.len(),
            tree.duplicate_ids().to_vec(),
        )
    };

    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut random = move |below: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % u64::try_from(below.max(1)).unwrap_or(1)).unwrap_or(0)
    };
    let mut items = (0..16)
        .map(|item| {
            node(
                &format!("item:{item}"),
                &format!("Item {item}"),
                &["open"],
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    let mut children = SharedChildren::new(items.clone());
    let mut root_label = String::from("Probe");
    let root = Column::new();
    let admitted = crate::a11y_tree::AdmittedNodeNames::default();
    let node_map = std::collections::HashMap::new();
    let controls = Rc::default();
    let doors = std::collections::HashSet::from([crate::search::search_address()]);
    let mut kept = crate::a11y_tree::SemanticTree::new();
    let mut fresh = crate::a11y_tree::SemanticTree::new();
    let mut token = crate::published::fresh_token();
    let mut expired = false;
    for step in 0..240 {
        let label = format!("Step {step}");
        match random(11) {
            0 => root_label = label,
            1 if !items.is_empty() => {
                let index = random(items.len());
                let id = items[index].id().to_owned();
                items[index] = node(&id, &label, &["open"], Vec::new());
            }
            2 => {
                let index = random(items.len() + 1);
                items.insert(
                    index,
                    node(&format!("item:new{step}"), &label, &["open"], Vec::new()),
                );
            }
            3 if items.len() > 2 => {
                items.remove(random(items.len()));
            }
            4 if items.len() > 1 => {
                let (from, to) = (random(items.len()), random(items.len()));
                items.swap(from, to);
            }
            5 if !items.is_empty() => {
                let index = random(items.len());
                let id = items[index].id().to_owned();
                let label = items[index].head().label.clone();
                items[index] = node(&id, &label, &["open", "rename"], Vec::new());
            }
            6 if !items.is_empty() => {
                // A repeated id: only a rebuild reports it as the full walk does.
                let id = items[random(items.len())].id().to_owned();
                items.push(node(&id, &label, &["open"], Vec::new()));
            }
            7 if !items.is_empty() => {
                let index = random(items.len());
                let id = items[index].id().to_owned();
                let grandchild = node(&format!("{id}/part"), &label, &["open"], Vec::new());
                items[index] = node(&id, &label, &[], vec![grandchild]);
            }
            8 => {
                // A grant expires, or comes back: a new permission context.
                expired = !expired;
                at(if expired { 120 } else { 100 });
                token = crate::published::fresh_token();
            }
            9 => {
                // The repeated ids go again, and updates resume.
                let mut seen = std::collections::HashSet::new();
                items.retain(|item| seen.insert(item.id().to_owned()));
            }
            _ => {}
        }
        if !children
            .iter()
            .map(SharedResource::addr)
            .eq(items.iter().map(SharedResource::addr))
        {
            children = SharedChildren::new(items.clone());
        }
        let tree = node("bound:probe", &root_label, &["apply"], Vec::new());
        let tree = SharedResource::new(tree.head().clone(), children.clone());

        let filtered = crate::bound_dispatch::filter_shared(&binding, token, &tree);
        let expected = crate::bound_dispatch::filter_resource(&binding, tree.to_resource());
        assert_eq!(
            outline(
                &filtered
                    .iter()
                    .map(SharedResource::to_resource)
                    .collect::<Vec<_>>()
            ),
            outline(&expected.iter().cloned().collect::<Vec<_>>()),
            "step {step}: filtering"
        );

        let forest: Rc<[SharedResource]> = filtered.into_iter().collect();
        let shared = crate::bound_dispatch::BoundResourceSnapshot::shared(
            binding.clone(),
            Rc::clone(&forest),
            Rc::clone(&controls),
        );
        let plain = crate::bound_dispatch::BoundResourceSnapshot::new(
            binding.clone(),
            crate::published::to_resources(&forest),
        );
        assert_eq!(
            *crate::bound_dispatch::component_controls_allowed(
                &binding,
                std::slice::from_ref(&shared),
                doors.clone()
            ),
            *crate::bound_dispatch::component_controls_allowed(
                &binding,
                std::slice::from_ref(&plain),
                doors.clone()
            ),
            "step {step}: the controls' allowed addresses"
        );
        let delta = kept.refresh_components(
            &root,
            &node_map,
            &admitted,
            None,
            &[],
            std::slice::from_ref(&shared),
        );
        let expected = fresh.refresh_components(
            &root,
            &node_map,
            &admitted,
            None,
            &[],
            std::slice::from_ref(&plain),
        );
        assert_eq!(delta, expected, "step {step}: the reported delta");
        assert_eq!(
            content(&kept),
            content(&fresh),
            "step {step}: the semantic tree"
        );
    }
}

/// An edit to one item of a long list changes that item's semantic node and
/// reports it alone; every other node is kept as it was, not rebuilt
/// (TIL-766).
#[test]
fn an_item_edit_updates_that_items_semantic_node_only() {
    use crate::published::{SharedChildren, SharedResource};
    use tilo_actions::{ActionSpec, OperationKind, Resource};

    let fixture = ActionFixture::new();
    let binding = fixture.bindings[0].clone();
    let item = |index: usize, label: &str| {
        SharedResource::new(
            Resource::new(format!("item:{index}"), "probe.item", label).with_action(
                ActionSpec::operation(
                    "open",
                    "Open",
                    OperationKind::Effect,
                    "bound.follow-up",
                    &(),
                )
                .unwrap(),
            ),
            SharedChildren::default(),
        )
    };
    let mut items = (0..200)
        .map(|index| item(index, &format!("Item {index}")))
        .collect::<Vec<_>>();
    let forest = |items: &[SharedResource]| -> Rc<[SharedResource]> {
        Rc::from(vec![SharedResource::new(
            Resource::new("bound:probe", "probe", "Probe"),
            SharedChildren::new(items.to_vec()),
        )])
    };
    let root = Column::new();
    let admitted = crate::a11y_tree::AdmittedNodeNames::default();
    let node_map = std::collections::HashMap::new();
    let mut tree = crate::a11y_tree::SemanticTree::new();
    let mut refresh = |forest: Rc<[SharedResource]>| {
        let snapshot = crate::bound_dispatch::BoundResourceSnapshot::shared(
            binding.clone(),
            forest,
            Rc::default(),
        );
        let delta = tree.refresh_components(
            &root,
            &node_map,
            &admitted,
            None,
            &[],
            std::slice::from_ref(&snapshot),
        );
        let id = |index: usize| {
            crate::a11y_tree::SemanticId(format!(
                "ri:{}:{}:item:{index}",
                binding.instance_id, binding.lifetime_id
            ))
        };
        let kept = std::ptr::from_ref(tree.node(&id(150)).expect("item 150's node"));
        (delta, kept, id(3))
    };
    let (_, before, _) = refresh(forest(&items));
    items[3] = item(3, "Retitled");
    let (delta, after, edited) = refresh(forest(&items));
    assert_eq!(
        delta,
        crate::a11y_tree::TreeDelta {
            updated: vec![edited],
            ..crate::a11y_tree::TreeDelta::default()
        }
    );
    assert!(
        std::ptr::eq(before, after),
        "an item the edit did not touch keeps its node"
    );
}
