//! Controlled callback routing fixtures: in-memory widgets, explicit persistence only.
use automerge::ROOT;
use std::collections::HashMap;
use ui::bound_dispatch::{BoundDispatchError, BoundNodeMap, invoke_bound_node};
use ui::widget::{Button, Widget};

#[test]
fn cleared_or_unadmitted_nodes_never_dispatch_as_outer_widgets() {
    let mut root = Button::new("probe", "Probe", None);
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = root.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    let mut outer = automerge::AutoCommit::new();
    // SAFETY: the exclusive test owns this unchanged live layout and no widget borrow escapes.
    unsafe {
        assert!(matches!(
            invoke_bound_node::<()>(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |_, _, _| panic!("unadmitted node executed")
            ),
            Err(BoundDispatchError::MissingNode)
        ));
        bindings.rebuild(&root, &tree, node, &nodes);
        assert!(
            invoke_bound_node(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |widget, _, _| widget.is_focusable()
            )
            .unwrap()
        );
        let original = nodes[&node];
        let mut other = Button::new("other", "Other", None);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(&mut other));
        assert!(matches!(
            invoke_bound_node::<()>(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |_, _, _| panic!("mismatched pointer ran")
            ),
            Err(BoundDispatchError::MissingNode)
        ));
        nodes.insert(node, original);
        bindings.clear();
        assert!(matches!(
            invoke_bound_node::<()>(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |_, _, _| panic!("cleared node executed")
            ),
            Err(BoundDispatchError::MissingNode)
        ));
    }
}

use crate::support::application::{ProbeHost, definition};
use automerge::{AutoCommit, ObjId, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use ui::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use ui::local_state::LocalState;
use ui::widget::{AppKind, AppWidget, Column, IconDescriptor, TileHost};
type RefusedNode = (
    BoundNodeMap,
    HashMap<taffy::NodeId, *mut dyn Widget>,
    taffy::NodeId,
);
thread_local! {
    static CAPTURES: RefCell<Vec<(String, &'static str, LocalState<u32>)>> = const { RefCell::new(vec![]) };
    static TASKS: RefCell<Vec<ui::LatestTask<u32>>> = const { RefCell::new(vec![]) };
    static LAYOUT_ONLY: Cell<bool> = const { Cell::new(false) };
    static PANIC_CANCEL: Cell<bool> = const { Cell::new(false) };
    static FINISH_OPERATION: Cell<bool> = const { Cell::new(false) };
    static REENTRY: RefCell<Option<RefusedNode>> = const { RefCell::new(None) };
    static QUEUED_CONTROL: RefCell<Option<qos_actions::ControlInvocation>> = const { RefCell::new(None) };
    static LEGACY_INVOKED: Cell<bool> = const { Cell::new(false) };
    static COMMAND_CALLS: Cell<usize> = const { Cell::new(0) };
}
thread_local! { static CANCELLED: Cell<usize> = const { Cell::new(0) }; static CLOSED_DROPS: Cell<usize> = const { Cell::new(0) }; }
struct Leaf {
    record: String,
    rect: vello::kurbo::Rect,
    focused: bool,
    operation_state: Option<LocalState<u32>>,
    owned_state: LocalState<u32>,
    layout_node: Option<taffy::NodeId>,
}
impl Leaf {
    fn capture(&self, label: &'static str) {
        CAPTURES.with_borrow_mut(|captures| {
            captures.push((self.record.clone(), label, LocalState::new(0)));
        });
        TASKS.with_borrow_mut(|tasks| tasks.push(ui::LatestTask::new(0)));
    }
    fn edit(&self, doc: &mut AutoCommit, label: &'static str) {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.record.as_str())
        );
        doc.put(ROOT, "last_callback", label).unwrap();
        self.capture(label);
    }
}
impl Widget for Leaf {
    fn sync(&mut self, doc: &mut AutoCommit, _: &ObjId) {
        self.edit(doc, "sync");
    }
    fn update(&mut self, doc: &mut AutoCommit, _: &ObjId) -> bool {
        self.edit(doc, "update");
        false
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        _: &ui::Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.capture("build");
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.layout_node = Some(node);
        node
    }
    fn measure(
        &mut self,
        _: taffy::Size<Option<f32>>,
        _: taffy::Size<taffy::AvailableSpace>,
        _: &mut ui::TextRenderer,
    ) -> taffy::Size<f32> {
        self.capture("measure");
        taffy::Size {
            width: 30.0,
            height: 20.0,
        }
    }
    fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: taffy::NodeId, _: vello::kurbo::Point) {
        self.capture("apply");
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::PresenceCtx,
    ) {
        self.capture("draw");
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        _: &taffy::TaffyTree<()>,
    ) {
        assert_eq!(
            self.layout_node,
            Some(node),
            "adapter shifted event topology"
        );
        self.edit(doc, "event");
    }
    fn name(&self) -> &'static str {
        "Leaf"
    }
    fn test_id(&self) -> Option<&str> {
        Some("dispatch-leaf")
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        self.capture("focus-order");
        true
    }
    fn set_focus(&mut self, value: bool) {
        self.capture("focus");
        self.focused = value;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        self.capture("ime");
        Some(self.rect)
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.capture("accessibility");
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name("Bound leaf")
                .with_children(vec![
                    ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                        .with_name("Synthetic probe")
                        .with_key(42),
                ]),
        )
    }
    fn perform_action(&mut self, _: &ui::a11y::Action, doc: &mut AutoCommit, _: &ObjId) -> bool {
        self.edit(doc, "action");
        true
    }
    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        path: &[u64],
        doc: &mut AutoCommit,
        _: &ObjId,
    ) -> bool {
        assert_eq!(path, &[42]);
        assert_eq!(action, &ui::a11y::Action::Focus);
        self.edit(doc, "synthetic-action");
        true
    }
    fn resource(&self, doc: &AutoCommit, _: &str) -> Option<qos_actions::Resource> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.record.as_str())
        );
        self.capture("resource-resolve");
        None
    }
    #[allow(
        clippy::panic_in_result_fn,
        reason = "callback probe asserts its actual canonical record before returning the toolkit result"
    )]
    fn start_resource_operation(
        &mut self,
        _: qos_actions::InvocationId,
        _: qos_actions::OperationKind,
        _: &str,
        _: &[u8],
        doc: &AutoCommit,
        _: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.record.as_str())
        );
        self.capture("operation-start");
        self.operation_state = Some(LocalState::new(0));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        id: qos_actions::InvocationId,
        cancel: bool,
        doc: &AutoCommit,
        _: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.record.as_str())
        );
        if cancel {
            assert!(
                !LocalState::new(0).writer().try_set(1),
                "cancellation ran outside closed route"
            );
            assert!(!self.operation_state.as_ref().unwrap().writer().try_set(1));
            CANCELLED.set(CANCELLED.get().saturating_add(1));
            assert_closed_reentry();
            assert!(
                !PANIC_CANCEL.replace(false),
                "controlled cancellation unwind"
            );
            Some(qos_actions::InvocationUpdate::Cancelled {
                invocation: id,
                message: None,
            })
        } else {
            self.capture("operation-poll");
            FINISH_OPERATION
                .get()
                .then_some(qos_actions::InvocationUpdate::Cancelled {
                    invocation: id,
                    message: None,
                })
        }
    }
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.record.as_str())
        );
        self.capture("resources");
        vec![]
    }
}
impl Drop for Leaf {
    fn drop(&mut self) {
        assert!(!self.owned_state.writer().try_set(1));
        assert!(!LocalState::new(0).writer().try_set(1));
        CLOSED_DROPS.set(CLOSED_DROPS.get().saturating_add(1));
        assert_closed_reentry();
        if let Some(state) = &self.operation_state {
            assert!(!state.writer().try_set(1));
            assert!(
                !LocalState::new(0).writer().try_set(1),
                "drop escaped closed route"
            );
        }
    }
}
struct Root(Column, String, bool);
impl Root {
    fn mark(&self, label: &'static str) {
        TASKS.with_borrow_mut(|tasks| tasks.push(ui::LatestTask::new(0)));
        CAPTURES
            .with_borrow_mut(|captures| captures.push((self.1.clone(), label, LocalState::new(0))));
    }
}
impl Widget for Root {
    fn sync(&mut self, d: &mut AutoCommit, p: &ObjId) {
        self.0.sync(d, p);
    }
    fn update(&mut self, d: &mut AutoCommit, p: &ObjId) -> bool {
        self.0.update(d, p)
    }
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        h: &ui::Theme,
        m: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.0.build_layout(t, h, m)
    }
    fn apply_layout(&mut self, t: &taffy::TaffyTree<()>, n: taffy::NodeId, o: vello::kurbo::Point) {
        self.0.apply_layout(t, n, o);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        renderer: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::PresenceCtx,
    ) {
        self.0.draw(scene, transform, renderer, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.0.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
    }
    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        assert!(
            !permissions.check(&ui::permissions::PermissionScope::workspace(
                ui::permissions::Verb::Write
            )),
            "draw received outer authority"
        );
        self.mark("draw-permissions");
        self.0.draw(scene, transform, text, theme, presence);
    }
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        self.mark("provider-create");
        vec![]
    }
    fn activate_search_result(&mut self, _: &str, doc: &mut AutoCommit, _: &ObjId) {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.1.as_str())
        );
        self.mark("search-activate");
    }
    fn on_external_event(&mut self, _: &str, doc: &mut AutoCommit, _: &ObjId) {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
            Some(self.1.as_str())
        );
        self.mark("external");
    }
    fn take_control_invocation(&mut self) -> Option<qos_actions::ControlInvocation> {
        self.mark("control-drain");
        QUEUED_CONTROL.with_borrow_mut(Option::take)
    }
    fn mount_native_views(&mut self, host: &ui::widget::native_view::NativeViewHost<'_>) {
        assert!(host.window().is_none());
        self.mark("native-mount");
    }
    fn native_view_frame_start(&mut self) {
        self.mark("native-frame");
    }
    fn unmount_native_views(&mut self) {
        self.mark("native-unmount");
    }
    fn view_state(&self) -> Option<ui::view::ViewContribution> {
        assert!(!PANIC_VIEW.replace(false), "controlled capture unwind");
        self.mark("capture-view");
        Some(if INVALID_VIEW.get() {
            ui::view::ViewContribution {
                scroll: vec![f64::NAN],
                ..Default::default()
            }
        } else {
            ui::view::ViewContribution::default()
        })
    }
    fn restore_view_state(&mut self, _: &ui::view::ViewContribution) {
        self.mark("restore-view");
    }
    fn name(&self) -> &'static str {
        "Root"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.0.get_rect()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.2 { vec![] } else { self.0.children() }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.2 {
            vec![]
        } else {
            self.0.children_mut()
        }
    }
}
impl AppWidget for Root {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Dispatch probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
    fn commands(&self) -> Vec<ui::AppCommand> {
        COMMAND_CALLS.set(COMMAND_CALLS.get().saturating_add(1));
        vec![ui::AppCommand {
            id: "legacy".into(),
            label: "Legacy".into(),
            shortcut: None,
            invoke: Box::new(|_, _| LEGACY_INVOKED.set(true)),
        }]
    }
    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.mark("host-intent");
        None
    }
}
#[allow(clippy::needless_pass_by_value, reason = "fixed factory signature")]
fn factory(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let record = context.record().id.as_str().to_string();
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "marker", record.clone())
                    .map_err(|e| e.to_string())
            })
            .map_err(ui::launch::LaunchError::Record)?;
    }
    let root = Root(
        Column::new().push(Box::new(Leaf {
            record: record.clone(),
            rect: vello::kurbo::Rect::ZERO,
            focused: false,
            operation_state: None,
            owned_state: LocalState::new(0),
            layout_node: None,
        })),
        record,
        LAYOUT_ONLY.get(),
    );
    root.mark("construction");
    Ok(AppComponent::new(
        context,
        Box::new(root),
        qos_actions::ActionRegistry::new(),
    ))
}
fn component(host: &ProbeHost, instance: u64) -> AppComponent {
    let definition = definition(factory);
    let context = host
        .prepare(&definition, instance, &LaunchTarget::New)
        .unwrap();
    definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap()
}
#[test]
fn component_raw_descendants_keep_the_record_and_mount_for_focus_ime_and_actions() {
    let host = ProbeHost::new();
    let mut component = component(&host, 1);
    let checkout = component.record().clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    component
        .mount(move |event| {
            sender.send(event).unwrap();
        })
        .unwrap();
    let mut tile = TileHost::from_component(component);
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = tile.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    // SAFETY: the fixture owns the unchanged tree exclusively; only shared metadata is inspected.
    unsafe {
        bindings.rebuild(&tile, &tree, node, &nodes);
    }
    let mut outer = AutoCommit::new();
    outer.put(ROOT, "marker", "shell").unwrap();
    CAPTURES.with_borrow_mut(Vec::clear);
    TASKS.with_borrow_mut(Vec::clear);
    for &node in nodes.keys() {
        // SAFETY: this live admitted tree is not replaced and no target references escape.
        unsafe {
            invoke_bound_node(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |widget, doc, root| {
                    if widget.name() == "Leaf" {
                        widget.set_focus(true);
                        widget.ime_cursor_rect();
                        widget.perform_action(&ui::a11y::Action::Focus, doc, root);
                    }
                },
            )
            .unwrap();
        }
    }
    let captures = CAPTURES.with_borrow_mut(std::mem::take);
    for label in ["focus", "ime", "action"] {
        assert!(captures.iter().any(|(_, actual, _)| *actual == label));
    }
    for (_, _, state) in captures {
        assert!(state.writer().try_set(1));
        assert!(receiver.try_recv().is_ok());
    }
    assert_eq!(
        outer.get(ROOT, "marker").unwrap().unwrap().0.to_str(),
        Some("shell")
    );
    assert_eq!(
        host.records
            .read(&checkout.id, |doc| doc
                .get(ROOT, "last_callback")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_owned())
            .unwrap(),
        "action"
    );
}

#[test]
fn real_driver_measure_and_focus_walks_capture_only_the_component_mount() {
    let host = ProbeHost::new();
    let mut component = component(&host, 1);
    let (sender, receiver) = std::sync::mpsc::channel();
    component
        .mount(move |event| {
            sender.send(event).unwrap();
        })
        .unwrap();
    let mut outer = ui::wake::WakeScope::new();
    let (outer_sender, outer_receiver) = std::sync::mpsc::channel();
    outer
        .bind(move |event| {
            outer_sender.send(event).unwrap();
        })
        .unwrap();
    let _outer = outer.enter();
    let mut state = ui::State::new();
    state.doc.put(ROOT, "marker", "shell").unwrap();
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    );
    let renderer = ui::TextRenderer::with_font_catalog(&catalog).unwrap();
    CAPTURES.with_borrow_mut(Vec::clear);
    TASKS.with_borrow_mut(Vec::clear);
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
        state,
        Box::new(Column::new().push(Box::new(TileHost::from_component(component)))),
        (400, 300),
        ui::Theme::dark(),
        renderer,
        ui::permissions::UserId("probe-author".into()),
    );
    let _ = driver.focus_order();
    driver.tab(false);
    driver.focus_access_nodes();
    driver.resources();
    QUEUED_CONTROL.with_borrow_mut(|queue| {
        *queue = Some(qos_actions::ControlInvocation {
            address: qos_actions::ActionAddress::new("missing", "probe"),
            arguments: serde_json::Map::default(),
        });
    });
    driver.send(&winit::event::WindowEvent::Focused(true));
    let target = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Synthetic probe"))
        .unwrap()
        .0;
    driver
        .perform_a11y_action(target, &ui::a11y::Action::Focus)
        .unwrap();
    driver
        .perform_named("dispatch-leaf", &ui::a11y::Action::Focus)
        .unwrap();
    let captures = CAPTURES.with_borrow_mut(std::mem::take);
    for label in [
        "sync",
        "update",
        "build",
        "measure",
        "apply",
        "draw",
        "focus-order",
        "focus",
        "accessibility",
        "action",
        "event",
        "synthetic-action",
        "resource-resolve",
    ] {
        assert!(
            captures.iter().any(|(_, actual, _)| *actual == label),
            "missing {label}"
        );
    }
    for (_, label, state) in captures {
        assert!(state.writer().try_set(1));
        assert!(
            receiver.try_recv().is_ok(),
            "{label} did not wake its component"
        );
        assert!(
            outer_receiver.try_recv().is_err(),
            "{label} woke the outer host"
        );
    }
    for task in TASKS.with_borrow_mut(std::mem::take) {
        let completion = task.begin(1);
        assert!(
            std::thread::spawn(move || completion.complete(2))
                .join()
                .unwrap()
        );
        assert!(receiver.try_recv().is_ok());
        assert!(outer_receiver.try_recv().is_err());
    }
}

#[test]
fn same_record_reentry_reuses_the_borrow_and_cross_record_reentry_is_refused() {
    let host = ProbeHost::new();
    let first = component(&host, 1);
    let checkout = first.record().clone();
    let definition = definition(factory);
    let target = crate::support::application::existing(&checkout);
    let context = host.prepare(&definition, 2, &target).unwrap();
    let second = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let third = component(&host, 3);
    let mut root = Column::new()
        .push(Box::new(TileHost::from_component(first)))
        .push(Box::new(TileHost::from_component(second)))
        .push(Box::new(TileHost::from_component(third)));
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = root.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    let mut outer = AutoCommit::new();
    // SAFETY: this fixture exclusively owns an unchanged live tree; each target is a distinct leaf.
    unsafe {
        bindings.rebuild(&root, &tree, node, &nodes);
        let mut leaves = vec![];
        for &node in nodes.keys() {
            if invoke_bound_node(
                &bindings,
                &nodes,
                node,
                &mut outer,
                &ROOT,
                |widget, doc, _| {
                    (widget.name() == "Leaf").then(|| {
                        doc.get(ROOT, "marker")
                            .unwrap()
                            .unwrap()
                            .0
                            .to_str()
                            .unwrap()
                            .to_owned()
                    })
                },
            )
            .unwrap()
            .is_some_and(|marker| {
                leaves.push((node, marker));
                true
            }) {}
        }
        let same: Vec<_> = leaves
            .iter()
            .filter(|(_, marker)| marker == checkout.id.as_str())
            .map(|(node, _)| *node)
            .collect();
        let different = leaves
            .iter()
            .find(|(_, marker)| marker != checkout.id.as_str())
            .unwrap()
            .0;
        assert_eq!(same.len(), 2);
        invoke_bound_node(
            &bindings,
            &nodes,
            same[0],
            &mut outer,
            &ROOT,
            |_, doc, _| {
                invoke_bound_node(
                    &bindings,
                    &nodes,
                    same[1],
                    doc,
                    &ROOT,
                    |widget, doc, root| widget.perform_action(&ui::a11y::Action::Focus, doc, root),
                )
                .unwrap();
                assert!(matches!(
                    invoke_bound_node::<()>(
                        &bindings,
                        &nodes,
                        different,
                        doc,
                        &ROOT,
                        |_, _, _| panic!("cross-record callback executed")
                    ),
                    Err(BoundDispatchError::CrossRecordReentry)
                ));
            },
        )
        .unwrap();
    }
}

#[test]
fn replacement_cancels_admitted_work_and_refuses_the_old_node_before_dereference() {
    let host = ProbeHost::new();
    let mut tile = TileHost::from_component(component(&host, 1));
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    let layout = tile.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    // SAFETY: admission inspects only this matching live layout.
    unsafe {
        bindings.rebuild(&tile, &tree, layout, &nodes);
    }
    let mut outer = AutoCommit::new();
    let invocation = qos_actions::InvocationId::new();
    assert!(matches!(
        tile.start_resource_operation(
            invocation,
            qos_actions::OperationKind::Effect,
            "probe",
            &[],
            &outer,
            &ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    CANCELLED.set(0);
    CLOSED_DROPS.set(0);
    let report = tile.replace_component(component(&host, 1)).pop().unwrap();
    assert!(report.recovery.is_some());
    assert_eq!(CANCELLED.get(), 1);
    assert_eq!(CLOSED_DROPS.get(), 1);
    // SAFETY: each retained actual old pointer has CLOSED admission metadata.
    // Closed refusal occurs before dereference; no stale OPEN delivery is attempted.
    for &node in nodes.keys() {
        unsafe {
            assert!(matches!(
                invoke_bound_node::<()>(
                    &bindings,
                    &nodes,
                    node,
                    &mut outer,
                    &ROOT,
                    |_, _, _| panic!("stale callback executed")
                ),
                Err(BoundDispatchError::ClosedLifetime)
            ));
        }
    }
}

#[test]
fn driver_retains_both_replacement_failures_and_retries_after_the_last_tile_drops() {
    let host = ProbeHost::new();
    let component = component(&host, 1);
    let checkout = component.record().clone();
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    );
    let replace = std::rc::Rc::new(Cell::new(false));
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(ReplacingHost(
            TileHost::from_component(component),
            replace.clone(),
        )),
        (400, 300),
        ui::Theme::dark(),
        ui::TextRenderer::with_font_catalog(&catalog).unwrap(),
        ui::permissions::UserId("probe-author".into()),
    );
    host.records.try_flush().unwrap();
    let record_path = app_state::DocStore::room_record_dir_at(
        &host.root(),
        crate::support::application::APP,
        crate::support::application::ROOM,
        &checkout.id.storage_key(),
    )
    .unwrap()
    .join(app_state::DOCUMENT_FILE);
    std::fs::remove_file(&record_path).unwrap();
    std::fs::create_dir(&record_path).unwrap();
    let view_path = host.view_path(1);
    std::fs::create_dir_all(&view_path).unwrap();
    host.records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "unsaved", "retained").unwrap();
        })
        .unwrap();
    replace.set(true);
    driver.relayout();
    assert_eq!(driver.component_close_reports().len(), 1);
    driver.close_components();
    assert_eq!(
        driver.component_close_reports().len(),
        1,
        "close duplicated recovery"
    );
    let mut reports = driver.take_component_close_reports();
    drop(driver);
    drop(host.records);
    drop(host.owners);
    let report = reports.last_mut().unwrap();
    assert!(report.view_error.is_some());
    assert!(report.record_error.is_some());
    assert!(report.recovery.is_some());
    assert_eq!(
        report
            .record_retry
            .as_ref()
            .unwrap()
            .read(&checkout.id, |doc| doc
                .get(ROOT, "unsaved")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_owned())
            .unwrap(),
        "retained"
    );
    std::fs::remove_dir(view_path).unwrap();
    std::fs::remove_dir(record_path).unwrap();
    report.retry_view().unwrap();
    report.retry_record().unwrap();
    assert!(report.view_retry.is_none());
    assert!(report.record_retry.is_none());
    assert!(report.view_error.is_none());
    assert!(report.record_error.is_none());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table proves the complete root callback ledger against a single mounted instance"
)]
fn root_callback_ledger_uses_real_tile_routes_and_omits_legacy_commands() {
    enum Callback {
        DrawPermissions,
        Providers,
        Search,
        External,
        Control,
        Mount,
        Frame,
        Unmount,
        Capture,
        Restore,
        HostIntent,
        Start,
        Poll,
    }

    let host = ProbeHost::new();
    let mut component = component(&host, 1);
    let (sender, receiver) = std::sync::mpsc::channel();
    component
        .mount(move |event| {
            sender.send(event).unwrap();
        })
        .unwrap();
    let mut outer = ui::wake::WakeScope::new();
    let (tx, rx) = std::sync::mpsc::channel();
    outer
        .bind(move |event| {
            tx.send(event).unwrap();
        })
        .unwrap();
    let _outer = outer.enter();
    let mut tile = TileHost::from_component(component);
    let mut shell = AutoCommit::new();
    shell.put(ROOT, "marker", "shell").unwrap();
    CAPTURES.with_borrow_mut(Vec::clear);
    TASKS.with_borrow_mut(Vec::clear);
    let invocation = qos_actions::InvocationId::new();
    for (callback, label) in [
        (Callback::DrawPermissions, "draw-permissions"),
        (Callback::Providers, "provider-create"),
        (Callback::Search, "search-activate"),
        (Callback::External, "external"),
        (Callback::Control, "control-drain"),
        (Callback::Mount, "native-mount"),
        (Callback::Frame, "native-frame"),
        (Callback::Unmount, "native-unmount"),
        (Callback::Capture, "capture-view"),
        (Callback::Restore, "restore-view"),
        (Callback::HostIntent, "host-intent"),
        (Callback::Start, "operation-start"),
        (Callback::Poll, "operation-poll"),
    ] {
        CAPTURES.with_borrow_mut(Vec::clear);
        TASKS.with_borrow_mut(Vec::clear);
        match callback {
            Callback::DrawPermissions => {
                let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
                    "en-US",
                );
                let mut renderer = ui::TextRenderer::with_font_catalog(&catalog).unwrap();
                tile.children_mut()[0].draw_with_presence(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut renderer,
                    &ui::Theme::dark(),
                    &ui::PresenceCtx::empty(),
                    &ui::permissions::PermissionContext::permissive(),
                );
            }

            Callback::Start => {
                tile.start_resource_operation(
                    invocation,
                    qos_actions::OperationKind::Effect,
                    "probe",
                    &[],
                    &shell,
                    &ROOT,
                )
                .unwrap();
            }
            Callback::Poll => {
                tile.poll_resource_operation(invocation, false, &shell, &ROOT);
            }
            Callback::Providers => {
                tile.search_providers();
            }
            Callback::Search => tile.activate_search_result("result", &mut shell, &ROOT),
            Callback::External => tile.on_external_event("probe", &mut shell, &ROOT),
            Callback::Control => {
                tile.take_control_invocation();
            }
            Callback::Mount => {
                tile.mount_native_views(&ui::widget::native_view::NativeViewHost::headless(
                    ui::platform::PlatformConventions::default(),
                    1.0,
                ));
            }
            Callback::Frame => tile.native_view_frame_start(),
            Callback::Unmount => tile.unmount_native_views(),
            Callback::Capture => {
                tile.view_state();
            }
            Callback::Restore => tile.restore_view_state(&ui::view::ViewContribution::default()),
            Callback::HostIntent => {
                tile.take_host_intent();
            }
        }
        let captures = CAPTURES.with_borrow_mut(std::mem::take);
        assert!(
            captures.iter().any(|(_, actual, _)| *actual == label),
            "missing {label}"
        );
        for (_, _, state) in captures {
            assert!(state.writer().try_set(1));
            assert!(receiver.try_recv().is_ok(), "{label} missed child");
            assert!(rx.try_recv().is_err(), "{label} reached outer");
        }
    }
    COMMAND_CALLS.set(0);
    let ui::widget::TileContent::App(root) = tile.content() else {
        panic!("missing component")
    };
    assert!(root.commands().is_empty());
    assert_eq!(
        COMMAND_CALLS.get(),
        0,
        "canonical path consulted legacy producer"
    );
    assert!(!LEGACY_INVOKED.get());
    let legacy = TileHost::new(ui::widget::TileContent::App(Box::new(Root(
        Column::new(),
        "legacy".into(),
        false,
    ))));
    let ui::widget::TileContent::App(root) = legacy.content() else {
        panic!("missing legacy root")
    };
    let commands = root.commands();
    assert_eq!(commands.len(), 1);
    (commands[0].invoke)(&mut shell, &ROOT);
    assert!(LEGACY_INVOKED.get());
    assert_eq!(COMMAND_CALLS.get(), 1);
}

fn assert_closed_reentry() {
    REENTRY.with_borrow(|entry| {
        if let Some((bindings, nodes, node)) = entry {
            // SAFETY: only actual CLOSED metadata is delivered during owning cleanup.
            // The refused callback neither dereferences nor aliases its current ancestor.
            assert!(matches!(
                unsafe {
                    invoke_bound_node::<()>(
                        bindings,
                        nodes,
                        *node,
                        &mut AutoCommit::new(),
                        &ROOT,
                        |_, _, _| panic!("closed reentry executed"),
                    )
                },
                Err(BoundDispatchError::ClosedLifetime)
            ));
        }
    });
}

#[test]
fn layout_only_descendant_inherits_the_taffy_boundary_and_two_mounts_stay_independent() {
    let host = ProbeHost::new();
    LAYOUT_ONLY.set(true);
    let mut first = component(&host, 1);
    let first_record = first.record().id.as_str().to_owned();
    let mut second = component(&host, 2);
    let (tx1, rx1) = std::sync::mpsc::channel();
    first
        .mount(move |event| {
            tx1.send(event).unwrap();
        })
        .unwrap();
    let (tx2, rx2) = std::sync::mpsc::channel();
    second
        .mount(move |event| {
            tx2.send(event).unwrap();
        })
        .unwrap();
    let mut root = Column::new()
        .push(Box::new(TileHost::from_component(first)))
        .push(Box::new(TileHost::from_component(second)));
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    let layout = root.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
    let mut bindings = BoundNodeMap::new();
    let mut shell = AutoCommit::new();
    shell.put(ROOT, "marker", "shell").unwrap();
    CAPTURES.with_borrow_mut(Vec::clear);
    // SAFETY: fixture owns a live unchanged tree; callbacks never retain target borrows.
    unsafe {
        bindings.rebuild(&root, &tree, layout, &nodes);
        for &node in nodes.keys() {
            invoke_bound_node(
                &bindings,
                &nodes,
                node,
                &mut shell,
                &ROOT,
                |widget, doc, path| {
                    if widget.name() == "Leaf" {
                        widget.perform_action(&ui::a11y::Action::Focus, doc, path);
                    }
                },
            )
            .unwrap();
        }
    }
    let captures = CAPTURES.with_borrow_mut(std::mem::take);
    assert_eq!(captures.len(), 2);
    for (record, _, state) in captures {
        assert!(state.writer().try_set(1));
        if record == first_record {
            assert!(rx1.try_recv().is_ok());
            assert!(rx2.try_recv().is_err());
        } else {
            assert!(rx2.try_recv().is_ok());
            assert!(rx1.try_recv().is_err());
        }
    }
    LAYOUT_ONLY.set(false);
}

#[test]
fn cancellation_unwind_and_ordinary_drop_destroy_operation_captures_under_closed_owner() {
    for panic_cancel in [false, true] {
        let host = ProbeHost::new();
        let mut component = component(&host, 1);
        let (tx, rx) = std::sync::mpsc::channel();
        component
            .mount(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        let mut tile = TileHost::from_component(component);
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = tile.build_layout(&mut tree, &ui::Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        // SAFETY: this admission uses the fixture's live matching topology.
        unsafe {
            bindings.rebuild(&tile, &tree, node, &nodes);
        }
        tile.start_resource_operation(
            qos_actions::InvocationId::new(),
            qos_actions::OperationKind::Effect,
            "probe",
            &[],
            &AutoCommit::new(),
            &ROOT,
        )
        .unwrap();
        REENTRY.with_borrow_mut(|entry| *entry = Some((bindings, nodes, node)));
        PANIC_CANCEL.set(panic_cancel);
        CANCELLED.set(0);
        CLOSED_DROPS.set(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(tile)));
        assert_eq!(result.is_err(), panic_cancel);
        assert_eq!(CANCELLED.get(), 1);
        assert_eq!(CLOSED_DROPS.get(), 1);
        assert!(rx.try_recv().is_err());
        REENTRY.with_borrow_mut(|entry| *entry = None);
    }
}

#[test]
fn terminal_operation_is_removed_before_later_close() {
    let host = ProbeHost::new();
    let mut tile = TileHost::from_component(component(&host, 1));
    let id = qos_actions::InvocationId::new();
    let doc = AutoCommit::new();
    tile.start_resource_operation(
        id,
        qos_actions::OperationKind::Effect,
        "probe",
        &[],
        &doc,
        &ROOT,
    )
    .unwrap();
    FINISH_OPERATION.set(true);
    assert!(
        tile.poll_resource_operation(id, false, &doc, &ROOT)
            .unwrap()
            .is_terminal()
    );
    FINISH_OPERATION.set(false);
    CANCELLED.set(0);
    tile.close_components();
    assert_eq!(CANCELLED.get(), 0, "terminal work was cancelled twice");
}

thread_local! {
    static NESTED: RefCell<Option<AppComponent>> = const { RefCell::new(None) };
    static INVALID_VIEW: Cell<bool> = const { Cell::new(false) };
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "fixed component factory contract"
)]
fn nested_factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let record = context.record().id.as_str().to_owned();
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "marker", record.clone())
                .map_err(|e| e.to_string())
        })
        .map_err(ui::launch::LaunchError::Record)?;
    let child = NESTED.with_borrow_mut(Option::take).unwrap();
    Ok(AppComponent::new(
        context,
        Box::new(Root(
            Column::new().push(Box::new(TileHost::from_component(child))),
            record,
            false,
        )),
        qos_actions::ActionRegistry::new(),
    ))
}
#[test]
fn nested_close_transfers_each_original_report_once_before_roots_are_destroyed() {
    let host = ProbeHost::new();
    let child = component(&host, 2);
    NESTED.with_borrow_mut(|nested| *nested = Some(child));
    let definition = definition(nested_factory);
    let parent = definition
        .instantiate(
            host.prepare(&definition, 1, &LaunchTarget::New).unwrap(),
            LaunchTarget::New,
            RestoreInput::None,
        )
        .unwrap();
    std::fs::create_dir_all(host.view_path(1)).unwrap();
    std::fs::create_dir_all(host.view_path(2)).unwrap();
    let mut tile = TileHost::from_component(parent);
    let mut reports = tile.close_components();
    assert_eq!(reports.len(), 2);
    assert!(tile.close_components().is_empty());
    assert!(tile.take_component_close_reports().is_empty());
    drop(tile);
    let mut instances = reports
        .iter()
        .map(|report| report.recovery.as_ref().unwrap().instance_id)
        .collect::<Vec<_>>();
    instances.sort_unstable();
    assert_eq!(instances, vec![1, 2]);
    for report in &mut reports {
        let id = report.recovery.as_ref().unwrap().instance_id;
        assert!(report.view_error.is_some());
        std::fs::remove_dir(host.view_path(id)).unwrap();
        report.retry_view().unwrap();
    }
}

fn factory_then_error(
    context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let _owned = factory(context, target, restore)?;
    Err(ui::launch::LaunchError::Operation(
        "controlled post-owner error".into(),
    ))
}
#[allow(
    clippy::panic_in_result_fn,
    reason = "fixture deliberately unwinds after installing its guarded component owner"
)]
fn factory_then_unwind(
    context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    let _owned = factory(context, target, restore)?;
    panic!("controlled post-owner unwind")
}
#[test]
fn owned_factory_error_restore_error_suspend_and_unwind_keep_drop_captures_closed() {
    let host = ProbeHost::new();
    for create in [
        factory_then_error as ui::application::ComponentFactory,
        factory_then_unwind,
    ] {
        CLOSED_DROPS.set(0);
        let definition = definition(create);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            definition.instantiate(
                host.prepare(&definition, 1, &LaunchTarget::New).unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
        }));
        assert!(result.is_err() || result.unwrap().is_err());
        assert_eq!(CLOSED_DROPS.get(), 1);
    }
    INVALID_VIEW.set(true);
    CLOSED_DROPS.set(0);
    let definition = definition(factory);
    assert!(
        definition
            .instantiate(
                host.prepare(&definition, 1, &LaunchTarget::New).unwrap(),
                LaunchTarget::New,
                RestoreInput::None
            )
            .is_err()
    );
    assert_eq!(CLOSED_DROPS.get(), 1);
    INVALID_VIEW.set(false);
    let mut component = component(&host, 1);
    CLOSED_DROPS.set(0);
    let first = component.suspend();
    let second = component.close();
    assert_eq!(
        first.recovery.as_ref().unwrap().instance_id,
        second.recovery.as_ref().unwrap().instance_id
    );
    assert_eq!(CLOSED_DROPS.get(), 1);
}

thread_local! { static PANIC_VIEW: Cell<bool> = const { Cell::new(false) }; }
#[test]
fn capture_unwind_closes_the_lifetime_and_drops_root_before_returning_to_host() {
    let host = ProbeHost::new();
    let mut tile = TileHost::from_component(component(&host, 1));
    CLOSED_DROPS.set(0);
    PANIC_VIEW.set(true);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tile.close_components())).is_err()
    );
    assert_eq!(
        CLOSED_DROPS.get(),
        1,
        "close unwind left a live root in the tile"
    );
    tile.on_external_event("refused", &mut AutoCommit::new(), &ROOT);
    assert_eq!(CLOSED_DROPS.get(), 1);
}

struct ReplacingHost(TileHost, std::rc::Rc<Cell<bool>>);
impl Widget for ReplacingHost {
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        self.0.sync(doc, path);
    }
    fn update(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
        if self.1.replace(false) {
            self.0.replace_content(ui::widget::TileContent::Notice {
                title: "Closed".into(),
                detail: "Fixture replacement".into(),
            });
        }
        self.0.update(doc, path)
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let child = self.0.build_layout(tree, theme, nodes);
        tree.new_with_children(taffy::Style::default(), &[child])
            .unwrap()
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.0
            .apply_layout(tree, tree.children(node).unwrap()[0], origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<taffy::NodeId>,
        _: taffy::NodeId,
        _: &taffy::TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "ReplacingHost"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.0.get_rect()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.0]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.0]
    }
}
