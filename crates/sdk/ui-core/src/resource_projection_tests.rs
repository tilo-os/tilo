struct SemanticCountingPublisher {
    counts: std::rc::Rc<[Cell<usize>; 2]>,
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
}

fn counted_control_app(
    duplicate: bool,
) -> (App, std::rc::Rc<[Cell<usize>; 2]>, std::rc::Rc<Cell<bool>>) {
    let counts = std::rc::Rc::new([Cell::new(0), Cell::new(0)]);
    let visible = std::rc::Rc::new(Cell::new(true));
    let resource = Publisher.resources(&AutoCommit::new()).remove(0);
    let root = SemanticCountingPublisher {
        counts: counts.clone(),
        visible: visible.clone(),
        duplicate,
        button: Button::new("flip", "Flip", None)
            .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
    };
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.action_registry_mut().register::<Flip>(FLIP_TYPE);
    (app, counts, visible)
}

#[test]
fn lazy_control_projection_defers_semantics_until_full_collection() {
    let (mut app, counts, _) = counted_control_app(false);
    app.refresh_control_action_availability();
    app.refresh_control_action_availability();
    assert_eq!(counts[0].get(), 1, "declared surface is reused");
    assert_eq!(
        counts[1].get(),
        0,
        "control refresh must not build semantic catalogue"
    );
    assert!(app.root_widget.children()[0].is_enabled());
    let full = app.collect_resources();
    assert!(
        full.iter()
            .any(|resource| resource.id.as_str() == "test.obj:1")
    );
    assert_eq!(counts[1].get(), 1);
    app.collect_resources();
    app.collect_local_resources();
    app.refresh_control_action_availability();
    assert_eq!(counts[0].get(), 1);
    assert_eq!(counts[1].get(), 1, "full cache remains materialized");
}

#[test]
fn lazy_control_projection_preserves_permissions_widget_changes_and_overflow() {
    let (mut app, counts, visible) = counted_control_app(false);
    app.refresh_control_action_availability();
    app.actions.permission_generation.set(u64::MAX);
    app.set_permission_ctx(PermissionContext::from_capabilities(
        collab_types::caps::Actor::person(None, "restricted"),
        Vec::new(),
    ));
    assert!(!app.root_widget.children()[0].is_enabled());
    app.set_permission_ctx(PermissionContext::permissive());
    assert!(app.root_widget.children()[0].is_enabled());
    assert_eq!(
        counts[0].get(),
        1,
        "permissions do not regenerate declarations"
    );
    assert_eq!(counts[1].get(), 0);
    visible.set(false);
    app.actions.resource_generation.set(u64::MAX);
    app.invalidate_widget_resource_cache();
    app.refresh_control_action_availability();
    assert!(!app.root_widget.children()[0].is_enabled());
    visible.set(true);
    app.invalidate_widget_resource_cache();
    app.refresh_control_action_availability();
    assert!(app.root_widget.children()[0].is_enabled());
    assert_eq!(counts[0].get(), 3);
    assert_eq!(counts[1].get(), 0);
    assert!(!app.collect_local_resources().is_empty());
    assert_eq!(counts[1].get(), 1);
}

#[test]
fn lazy_control_projection_preserves_rejected_surface_and_declared_availability() {
    let (mut app, counts, _) = counted_control_app(true);
    app.refresh_control_action_availability();
    assert!(
        app.root_widget.children()[0].is_enabled(),
        "baseline declared projection survives validation rejection"
    );
    assert!(
        app.collect_resources().is_empty(),
        "duplicate resource IDs reject complete surface"
    );
    assert_eq!(counts[1].get(), 1);
    assert!(app.collect_resources().is_empty());
    assert!(app.collect_local_resources().is_empty());
    app.refresh_control_action_availability();
    assert!(app.root_widget.children()[0].is_enabled());
    assert_eq!(counts[1].get(), 1, "rejected empty is cached, not unbuilt");
    let actor = app.collaboration.local_author.0.clone();
    let result = app.invoke_resource_action(
        &tilo_actions::ActionAddress::new("test.obj:1", "flip"),
        None,
        &actor,
        false,
        false,
        None,
        None,
        None,
    );
    assert!(
        result.is_err(),
        "declared availability cannot bypass full invocation validation"
    );
    assert!(app.state.doc.get(ROOT, "flipped").unwrap().is_none());
}

#[test]
fn lazy_control_projection_does_not_commit_pending_document_transaction() {
    let (mut app, counts, _) = counted_control_app(false);
    app.state_mut_for_test()
        .doc
        .put(ROOT, "typing", "pending")
        .unwrap();
    let pending = app.state.doc.pending_ops();
    let before = format!("{:?}", app.state.doc);
    app.refresh_control_action_availability();
    assert_eq!(counts[1].get(), 0);
    app.collect_resources();
    app.collect_local_resources();
    app.collect_declared_resources();
    assert_eq!(app.state.doc.pending_ops(), pending);
    assert_eq!(format!("{:?}", app.state.doc), before);
    assert_eq!(counts[1].get(), 1);
}

impl Widget for SemanticCountingPublisher {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &WindowEvent,
        _sf: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _ft: &mut Option<NodeId>,
        _ni: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.counts[0].set(self.counts[0].get() + 1);
        let mut resources = if self.visible.get() {
            Publisher.resources(_doc)
        } else {
            Vec::new()
        };
        if self.duplicate {
            resources.extend(resources.clone());
        }
        resources
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.counts[1].set(self.counts[1].get() + 1);
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        // Count full semantic construction, independently of inventory's
        // default accessibility-derived metadata implementation.
        self.button.control_action_bindings()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.button]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.button]
    }
}

struct InventoryOnlyPublisher {
    counts: std::rc::Rc<[Cell<usize>; 2]>,
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
    enabled: std::rc::Rc<Cell<bool>>,
}

fn inventory_only_publisher() -> (Box<dyn Widget>, std::rc::Rc<Cell<bool>>) {
    let resource = Publisher.resources(&AutoCommit::new()).remove(0);
    let enabled = std::rc::Rc::new(Cell::new(false));
    (
        Box::new(InventoryOnlyPublisher {
            counts: std::rc::Rc::new([Cell::new(0), Cell::new(0)]),
            visible: std::rc::Rc::new(Cell::new(true)),
            duplicate: false,
            button: Button::new("custom-flip", "Flip", None)
                .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
            enabled: enabled.clone(),
        }),
        enabled,
    )
}

#[test]
fn control_presence_keeps_inventory_only_custom_emitters_and_permissions() {
    for wrapper in 0..4 {
        let (root, enabled) = inventory_only_publisher();
        assert!(root.children().is_empty());
        let root: Box<dyn Widget> = match wrapper {
            0 => root,
            1 => Box::new(crate::widget::Column::new().push(root)),
            2 => Box::new(crate::widget::Scrollable::new_vertical().with_child(root)),
            _ => Box::new(crate::widget::NavShell::sidebar_over_content(
                Box::new(crate::widget::Text::new("sidebar")),
                root,
            )),
        };
        assert!(root.has_action_control_bindings());
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        app.refresh_control_action_availability();
        assert!(enabled.get(), "wrapper {wrapper}");
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        assert!(
            !enabled.get(),
            "wrapper {wrapper} must refresh hidden custom emitter"
        );
    }
}

#[test]
fn control_presence_replacement_resets_availability_stamp() {
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Column::new()));
    app.action_registry_mut().register::<Flip>(FLIP_TYPE);
    app.refresh_control_action_availability();
    assert!(app.actions.last_control_availability.is_none());
    let (root, enabled) = inventory_only_publisher();
    app.root_widget = root;
    app.refresh_control_action_availability();
    assert!(enabled.get());
    assert!(app.actions.last_control_availability.is_some());
    app.root_widget = Box::new(crate::widget::Column::new());
    app.refresh_control_action_availability();
    assert!(app.actions.last_control_availability.is_none());
    let (root, enabled) = inventory_only_publisher();
    app.root_widget = root;
    app.refresh_control_action_availability();
    assert!(
        enabled.get(),
        "new emitter must not inherit removed emitter refresh stamp"
    );
}
impl Widget for InventoryOnlyPublisher {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &WindowEvent,
        _sf: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _ft: &mut Option<NodeId>,
        _ni: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.counts[0].set(self.counts[0].get() + 1);
        let mut resources = if self.visible.get() {
            Publisher.resources(_doc)
        } else {
            Vec::new()
        };
        if self.duplicate {
            resources.extend(resources.clone());
        }
        resources
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.counts[1].set(self.counts[1].get() + 1);
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        // Count full semantic construction, independently of inventory's
        // default accessibility-derived metadata implementation.
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
    counts: std::rc::Rc<[Cell<usize>; 2]>,
    visible: std::rc::Rc<Cell<bool>>,
    duplicate: bool,
    button: Button,
    enabled: std::rc::Rc<Cell<bool>>,
}

#[test]
fn control_presence_host_uses_predicate_but_coverage_keeps_inventory() {
    for wrapper in 0..4 {
        let counts = std::rc::Rc::new([Cell::new(0), Cell::new(0)]);
        let resource = Publisher.resources(&AutoCommit::new()).remove(0);
        let root = FastInventoryPublisher {
            counts: counts.clone(),
            visible: std::rc::Rc::new(Cell::new(true)),
            duplicate: false,
            enabled: std::rc::Rc::new(Cell::new(false)),
            button: Button::new("fast-flip", "Flip", None)
                .on_action(tilo_actions::ActionBinding::unit_for(&resource, "flip").unwrap()),
        };
        let root: Box<dyn Widget> = match wrapper {
            0 => Box::new(root),
            1 => Box::new(crate::widget::Column::new().push(Box::new(root))),
            2 => Box::new(crate::widget::Scrollable::new_vertical().with_child(Box::new(root))),
            _ => Box::new(crate::widget::NavShell::sidebar_over_content(
                Box::new(crate::widget::Text::new("sidebar")),
                Box::new(root),
            )),
        };
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        app.refresh_control_action_availability();
        app.refresh_control_action_availability();
        assert_eq!(
            counts[1].get(),
            0,
            "presence refresh must not inventory controls"
        );
        assert_eq!(app.control_action_coverage().len(), 1);
        assert!(
            counts[1].get() > 0,
            "explicit coverage still inventories controls"
        );
    }
}

impl Widget for FastInventoryPublisher {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &WindowEvent,
        _sf: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _ft: &mut Option<NodeId>,
        _ni: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.counts[0].set(self.counts[0].get() + 1);
        let mut resources = if self.visible.get() {
            Publisher.resources(_doc)
        } else {
            Vec::new()
        };
        if self.duplicate {
            resources.extend(resources.clone());
        }
        resources
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.counts[1].set(self.counts[1].get() + 1);
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_semantic_key("counted-catalogue"),
        )
    }
    fn has_action_control_bindings(&self) -> bool {
        true
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        // Count full semantic construction, independently of inventory's
        // default accessibility-derived metadata implementation.
        self.counts[1].set(self.counts[1].get() + 1);
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
