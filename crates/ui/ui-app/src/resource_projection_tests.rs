struct SemanticCountingPublisher {
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
}

fn counted_control_app(duplicate: bool) -> (App, std::rc::Rc<Cell<bool>>) {
    let visible = std::rc::Rc::new(Cell::new(true));
    let resource = crate::widget::resources::resources(&Publisher, &AutoCommit::new()).remove(0);
    let root = SemanticCountingPublisher {
        visible: visible.clone(),
        duplicate,
        button: Button::new("flip", "Flip")
            .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
    };
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.action_registry_mut().register::<Flip>(FLIP_TYPE);
    (app, visible)
}

#[test]
fn control_availability_follows_visibility_and_reads_without_committing() {
    {
        let (mut app, visible) = counted_control_app(false);
        app.refresh_control_action_availability();
        assert!(app.root_widget.children()[0].is_enabled());
        visible.set(false);
        app.actions.resource_generation.set(u64::MAX);
        app.invalidate_widget_resource_cache();
        app.refresh_control_action_availability();
        assert!(!app.root_widget.children()[0].is_enabled());
        visible.set(true);
        app.invalidate_widget_resource_cache();
        app.refresh_control_action_availability();
        assert!(app.root_widget.children()[0].is_enabled());
        assert!(!app.collect_local_resources().is_empty());
    }

    {
        let (mut app, _) = counted_control_app(true);
        app.refresh_control_action_availability();
        assert!(
            app.root_widget.children()[0].is_enabled(),
            "baseline declared projection survives validation rejection"
        );
        // The repeat of a resource id is refused alone: the id it repeats is withdrawn, and the
        // rest of the app's surface — the control's own semantic action — still stands.
        let offered = app.collect_resources();
        assert!(
            !offered
                .iter()
                .any(|resource| resource.id.as_str() == "test.obj:1"),
            "a resource id published twice is refused whole"
        );
        assert!(
            !offered.is_empty(),
            "the app's other controls are still offered"
        );
        assert!(
            !app.collect_local_resources()
                .iter()
                .any(|resource| resource.id.as_str() == "test.obj:1")
        );
        app.refresh_control_action_availability();
        assert!(app.root_widget.children()[0].is_enabled());
        let actor = tilo_actions::Actor::Member(app.local_author.clone());
        let address = tilo_actions::ActionAddress::new("test.obj:1", "flip");
        let result = app.invoke_resource_action(ResourceInvocation::new(&address, &actor));
        assert!(
            result.is_err(),
            "declared availability cannot bypass full invocation validation"
        );
        assert!(app.state.doc.get(ROOT, "flipped").unwrap().is_none());
    }

    {
        let (mut app, _) = counted_control_app(false);
        app.state_mut_for_test()
            .doc
            .put(ROOT, "typing", "pending")
            .unwrap();
        // Read without closing the open transaction: `get_heads` would commit
        // it, which is the very thing under test.
        let committed = |doc: &AutoCommit| doc.stats().num_changes;
        let pending = app.state.doc.pending_ops();
        let changes = committed(&app.state.doc);
        let value = app.state.doc.hydrate(ROOT, None).unwrap();
        app.refresh_control_action_availability();
        app.collect_resources();
        app.collect_local_resources();
        app.collect_declared_resources();
        assert_eq!(
            app.state.doc.pending_ops(),
            pending,
            "transaction left open"
        );
        assert_eq!(committed(&app.state.doc), changes, "nothing committed");
        assert_eq!(app.state.doc.hydrate(ROOT, None).unwrap(), value);
    }
}

impl Widget for SemanticCountingPublisher {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources
    );

    fn sync(&mut self, _s: &AppModel, _p: &ModelPath) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.button]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.button]
    }
}

fn counted_resources(visible: &Cell<bool>, duplicate: bool, doc: &AutoCommit) -> Vec<ResourceSpec> {
    let mut resources = if visible.get() {
        crate::widget::resources::PublishesResources::resources(&Publisher, doc)
    } else {
        Vec::new()
    };
    if duplicate {
        resources.extend(resources.clone());
    }
    resources
}

impl crate::widget::resources::PublishesResources for SemanticCountingPublisher {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        counted_resources(&self.visible, self.duplicate, doc)
    }
}

impl tilo_ui_base::widget::controls::ActionControls for SemanticCountingPublisher {
    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        self.button.control_action_bindings()
    }
}

struct InventoryOnlyPublisher {
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
    enabled: std::rc::Rc<Cell<bool>>,
}

fn inventory_only_publisher() -> (Box<dyn Widget>, std::rc::Rc<Cell<bool>>) {
    let resource = crate::widget::resources::resources(&Publisher, &AutoCommit::new()).remove(0);
    let enabled = std::rc::Rc::new(Cell::new(false));
    (
        Box::new(InventoryOnlyPublisher {
            visible: std::rc::Rc::new(Cell::new(true)),
            duplicate: false,
            button: Button::new("custom-flip", "Flip")
                .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
            enabled: enabled.clone(),
        }),
        enabled,
    )
}

#[test]
fn control_presence_keeps_inventory_permissions_and_availability_stamps() {
    {
        for wrapper in 0..4 {
            let (root, enabled) = inventory_only_publisher();
            assert!(root.children().is_empty());
            let root: Box<dyn Widget> = match wrapper {
                0 => root,
                1 => Box::new(crate::widget::Column::new().push(root)),
                2 => Box::new(crate::widget::Scrollable::new_vertical().with_child(root)),
                _ => Box::new(
                    tilo_ui_nav::NavShell::new()
                        .sidebar(Box::new(crate::widget::Text::new("sidebar")))
                        .content(root),
                ),
            };
            assert!(tilo_ui_base::widget::controls::has_action_control_bindings(
                root.as_ref()
            ));
            let mut app = App::new_with_controlled_inputs(State::new(), root);
            app.action_registry_mut().register::<Flip>(FLIP_TYPE);
            app.refresh_control_action_availability();
            assert!(enabled.get(), "wrapper {wrapper}");
        }
    }

    {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Column::new()));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        let swap = |app: &mut App, root: Box<dyn Widget>| {
            app.root_widget = root;
            app.layout.walks = super::runtime::TreeWalks::default();
        };
        app.refresh_control_action_availability();
        assert!(app.actions.last_control_availability.is_none());
        let (root, enabled) = inventory_only_publisher();
        swap(&mut app, root);
        app.refresh_control_action_availability();
        assert!(enabled.get());
        assert!(app.actions.last_control_availability.is_some());
        swap(&mut app, Box::<crate::widget::Column>::default());
        app.refresh_control_action_availability();
        assert!(app.actions.last_control_availability.is_none());
        let (root, enabled) = inventory_only_publisher();
        swap(&mut app, root);
        app.refresh_control_action_availability();
        assert!(
            enabled.get(),
            "new emitter must not inherit removed emitter refresh stamp"
        );
    }

    {
        for wrapper in 0..4 {
            let resource =
                crate::widget::resources::resources(&Publisher, &AutoCommit::new()).remove(0);
            let root = FastInventoryPublisher {
                visible: std::rc::Rc::new(Cell::new(true)),
                duplicate: false,
                enabled: std::rc::Rc::new(Cell::new(false)),
                button: Button::new("fast-flip", "Flip")
                    .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
            };
            let root: Box<dyn Widget> = match wrapper {
                0 => Box::new(root),
                1 => Box::new(crate::widget::Column::new().push(Box::new(root))),
                2 => Box::new(crate::widget::Scrollable::new_vertical().with_child(Box::new(root))),
                _ => Box::new(
                    tilo_ui_nav::NavShell::new()
                        .sidebar(Box::new(crate::widget::Text::new("sidebar")))
                        .content(Box::new(root)),
                ),
            };
            let mut app = App::new_with_controlled_inputs(State::new(), root);
            app.action_registry_mut().register::<Flip>(FLIP_TYPE);
            app.refresh_control_action_availability();
            app.refresh_control_action_availability();
            // One action-bound control, in every wrapper: the shell's own sidebar toggle is a
            // host request run from a closure, not an action a view binds, so it is no action
            // control of the surface.
            assert_eq!(
                app.control_action_coverage().len(),
                1,
                "wrapper {wrapper} keeps the one action-bound control"
            );
        }
    }
}

impl Widget for InventoryOnlyPublisher {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
}

impl crate::widget::resources::PublishesResources for InventoryOnlyPublisher {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        counted_resources(&self.visible, self.duplicate, doc)
    }
}

impl tilo_ui_base::widget::controls::ActionControls for InventoryOnlyPublisher {
    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        self.button.control_action_bindings()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.button.refresh_control_action_availability(allowed);
        self.enabled.set(self.button.is_enabled());
    }
}

struct FastInventoryPublisher {
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
    enabled: std::rc::Rc<Cell<bool>>,
}

impl Widget for FastInventoryPublisher {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
}

impl crate::widget::resources::PublishesResources for FastInventoryPublisher {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        counted_resources(&self.visible, self.duplicate, doc)
    }
}

impl tilo_ui_base::widget::controls::ActionControls for FastInventoryPublisher {
    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        self.button.control_action_bindings()
    }
    fn has_action_control_bindings(&self) -> bool {
        true
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.button.refresh_control_action_availability(allowed);
        self.enabled.set(self.button.is_enabled());
    }
}

/// A window showing a live child in its place answers for that child and walks it too: the walk
/// takes each resource root once, so the child's control is one control.
#[test]
fn a_widget_answering_for_a_child_it_walks_publishes_that_childs_resources_once() {
    struct Displaying(Publisher);

    impl tilo_ui_base::widget::Widget for Displaying {
        fn provide<'a>(&'a self, request: &mut tilo_ui_base::widget::capability::Request<'a, '_>) {
            self.0.provide(request);
        }

        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn tilo_ui_base::widget::Widget)) {
            visitor(&self.0);
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn tilo_ui_base::widget::Widget>,
        ) -> NodeId {
            taffy.widget_leaf(taffy::Style::default(), node_map, self)
        }

        fn name(&self) -> &'static str {
            "displaying"
        }
    }

    let mut resources = Vec::new();
    crate::bound_dispatch::collect_widget_resources(
        &Displaying(Publisher),
        &AutoCommit::new(),
        &mut resources,
    );
    let ids: Vec<&str> = resources
        .iter()
        .map(|resource| resource.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["test.obj:1"],
        "the displayed child's resource stands once"
    );
}
