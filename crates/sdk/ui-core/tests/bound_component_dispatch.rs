//! Controlled callback routing fixtures: in-memory widgets, explicit persistence only.
use automerge::ROOT;
use std::collections::HashMap;
use tilo_ui::bound_dispatch::{BoundDispatchError, BoundNodeMap, invoke_bound_node};
use tilo_ui::widget::{AppModel, ModelPath};
use tilo_ui::widget::{Button, Widget};

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn provider_payloads_enter_and_drop_under_their_originating_component() {
    // Case `owned_search_provider_methods_and_early_drop_enter_the_originating_component`.
    {
        use crate::support::application::{enable_provider_probe, instantiate_probe};
        let host = ProbeHost::new();
        let observed = enable_provider_probe();
        let mut component =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = wakes.clone();
        component
            .mount(move |_| {
                sink.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .unwrap();
        let tile = TileHost::from_component(component);
        let providers = tilo_ui::search::capability::search_providers(&tile);
        wakes.store(0, std::sync::atomic::Ordering::Relaxed);
        let provider = &providers[0];
        assert_eq!(provider.name(), "Probe");
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "name is inert copied metadata"
        );
        assert_eq!(provider.action_directory_generation(), Some(4));
        assert!(
            provider.action_directory_cursor_is_current(&tilo_ui::search::SearchCursor::default())
        );
        assert_eq!(provider.search("").len(), 1);
        assert!(provider.search_limited("", 0).is_empty());
        let page = provider.search_page(&"".into(), None, 1, 1);
        assert_eq!(page.results.len(), 1);
        assert!(page.next.is_some());
        assert_eq!(page.inspected, 1);
        provider.resolve_action_resource(&tilo_actions::ActionAddress::new("probe:shared", "edit"));
        assert!(observed.borrow().methods.iter().all(|(_, active)| *active));
        drop(providers);
        assert_eq!(observed.borrow().drops, vec![(true, true)]);
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::Relaxed),
            7,
            "all six live methods and early destruction wake the captured owner"
        );
    }

    // Case `provider_payload_drops_on_close_even_when_the_proxy_outlives_replacement`.
    {
        use crate::support::application::{enable_provider_probe, existing, instantiate_probe};
        let host = ProbeHost::new();
        let observed = enable_provider_probe();
        let component =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = component.record().clone();
        let mut tile = TileHost::from_component(component);
        let providers = tilo_ui::search::capability::search_providers(&tile);
        tilo_ui::component_lifecycle::close_components(&mut tile);
        assert_eq!(observed.borrow().drops, vec![(false, false)]);
        let mut replacement =
            instantiate_probe(&host, 11, existing(&checkout), RestoreInput::None).unwrap();
        let replacement_wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = replacement_wakes.clone();
        replacement
            .mount(move |_| {
                sink.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .unwrap();
        assert!(tile.replace_component(replacement).is_empty());
        replacement_wakes.store(0, std::sync::atomic::Ordering::Relaxed);
        let count = observed.borrow().methods.len();
        let provider = &providers[0];
        assert_eq!(provider.name(), "Probe");
        assert_eq!(provider.action_directory_generation(), None);
        assert!(
            !provider.action_directory_cursor_is_current(&tilo_ui::search::SearchCursor::default())
        );
        assert!(provider.search("").is_empty());
        assert!(provider.search_limited("", 1).is_empty());
        let page = provider.search_page(&"".into(), None, 1, 1);
        assert!(page.results.is_empty() && page.next.is_none() && page.inspected == 0);
        assert!(
            provider
                .resolve_action_resource(&tilo_actions::ActionAddress::new("probe:shared", "edit"))
                .is_none()
        );
        assert_eq!(observed.borrow().methods.len(), count);
        assert_eq!(
            replacement_wakes.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        drop(providers);
        assert_eq!(observed.borrow().drops.len(), 1);
    }

    // Case `provider_destructor_unwind_keeps_payloads_under_the_closed_owner`.
    {
        use crate::support::application::{enable_provider_probe, instantiate_probe};
        let host = ProbeHost::new();
        let observed = enable_provider_probe();
        let component =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let mut tile = TileHost::from_component(component);
        let mut providers = tilo_ui::search::capability::search_providers(&tile);
        providers.extend(tilo_ui::search::capability::search_providers(&tile));
        observed.borrow_mut().panic_on_drop = true;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tilo_ui::component_lifecycle::close_components(&mut tile)
        }));
        observed.borrow_mut().panic_on_drop = false;
        assert!(result.is_err());
        assert_eq!(
            observed.borrow().drops,
            vec![(false, false), (false, false)]
        );
        assert!(
            providers
                .iter()
                .all(|provider| provider.search("").is_empty())
        );
        drop(providers);
        assert_eq!(observed.borrow().drops.len(), 2);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn component_mounts_capture_their_descendants_for_dispatch_and_layout() {
    // Case `cleared_or_unadmitted_nodes_never_dispatch_as_outer_widgets`.
    {
        let mut root = Button::new("probe", "Probe");
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = root.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
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
            let mut other = Button::new("other", "Other");
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

    // Case `component_raw_descendants_keep_the_record_and_mount_for_focus_ime_and_actions`.
    {
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
        let node = tile.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
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
                            widget.perform_action(
                                &tilo_ui::a11y::Action::Focus,
                                &[],
                                &mut tilo_ui::widget::Edits::lend(doc),
                                root,
                            );
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
            outer.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some("shell")
        );
        assert_eq!(
            host.records
                .read(&checkout.id, |doc| doc
                    .get(ROOT, "last_callback")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str()
                    .unwrap()
                    .to_owned())
                .unwrap(),
            "action"
        );
    }

    // Case `real_driver_measure_and_focus_walks_capture_only_the_component_mount`.
    {
        let host = ProbeHost::new();
        let mut component = component(&host, 1);
        let (sender, receiver) = std::sync::mpsc::channel();
        component
            .mount(move |event| {
                sender.send(event).unwrap();
            })
            .unwrap();
        let mut outer = tilo_ui::wake::WakeScope::new();
        let (outer_sender, outer_receiver) = std::sync::mpsc::channel();
        outer
            .bind(move |event| {
                outer_sender.send(event).unwrap();
            })
            .unwrap();
        let _outer = outer.enter();
        let mut state = tilo_ui::State::new();
        state.doc.put(ROOT, "marker", "shell").unwrap();
        let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
            tilo_ui::font_catalog::checkout_root(),
            "en-US",
        );
        let renderer = tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap();
        CAPTURES.with_borrow_mut(Vec::clear);
        TASKS.with_borrow_mut(Vec::clear);
        let mut driver = tilo_ui::headless::FrameHarness::new_with_controlled_inputs(
            state,
            Box::new(Column::new().push(Box::new(TileHost::from_component(component)))),
            (400, 300),
            tilo_ui::Theme::dark(),
            renderer,
            tilo_ui::permissions::UserId("probe-author".into()),
        );
        let _ = driver.focus_order();
        driver.tab(false);
        driver.focus_access_nodes();
        driver.resources();
        QUEUED_CONTROL.with_borrow_mut(|queue| {
            *queue = Some(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("missing", "probe"),
                arguments: serde_json::Map::default(),
            });
        });
        driver.send(&winit::event::WindowEvent::Focused(true));
        // A control naming nothing the window has is said over it, never
        // dropped unseen; Escape dismisses the notice as any palette.
        assert!(
            driver.refusal_shown().is_some(),
            "the missing control's refusal is shown"
        );
        driver.key("Escape");
        assert_eq!(driver.refusal_shown(), None);
        // What the app invokes on its own, for nobody's gesture, is refused
        // without a word over the window: the person pressed nothing.
        let failures = driver.control_failures().len();
        QUEUED_CONTROL.with_borrow_mut(|queue| {
            *queue = Some(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("missing", "on-its-own"),
                arguments: serde_json::Map::default(),
            });
        });
        QUEUED_ORIGIN.with(|origin| origin.set(tilo_ui::widget::controls::Origin::App));
        driver.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(
            driver.refusal_shown(),
            None,
            "the app's own refused invocation is not said as the person's"
        );
        assert_eq!(
            driver.control_failures().len(),
            failures + 1,
            "the app's own refused invocation is still kept as a failure"
        );
        let target = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Synthetic probe"))
            .unwrap()
            .0;
        driver
            .perform_a11y_action(target, &tilo_ui::a11y::Action::Focus)
            .unwrap();
        driver
            .perform_named("dispatch-leaf", &tilo_ui::a11y::Action::Focus)
            .unwrap();
        let captures = CAPTURES.with_borrow_mut(std::mem::take);
        for label in [
            "sync",
            "update",
            "build",
            "measure",
            "apply",
            "draw",
            "draw-permissions",
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

    // Case `layout_only_descendant_inherits_the_taffy_boundary_and_two_mounts_stay_independent`.
    {
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
        let layout = root.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
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
                            widget.perform_action(
                                &tilo_ui::a11y::Action::Focus,
                                &[],
                                &mut tilo_ui::widget::Edits::lend(doc),
                                path,
                            );
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
}

use crate::support::application::{ProbeHost, definition};
use automerge::{AutoCommit, ObjId, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use tilo_ui::application::{
    AppComponent, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use tilo_ui::host_operation::{poll_resource_operation, start_operation};
use tilo_ui::local_state::LocalState;
use tilo_ui::widget::{AppKind, AppWidget, Column, IconDescriptor, TileHost};
type RefusedNode = (
    BoundNodeMap,
    HashMap<taffy::NodeId, *mut dyn Widget>,
    taffy::NodeId,
);
thread_local! {
    static CAPTURES: RefCell<Vec<(String, &'static str, LocalState<u32>)>> = const { RefCell::new(vec![]) };
    static TASKS: RefCell<Vec<tilo_ui::LatestTask<u32>>> = const { RefCell::new(vec![]) };
    static LAYOUT_ONLY: Cell<bool> = const { Cell::new(false) };
    static PANIC_CANCEL: Cell<bool> = const { Cell::new(false) };
    static FINISH_OPERATION: Cell<bool> = const { Cell::new(false) };
    static REENTRY: RefCell<Option<RefusedNode>> = const { RefCell::new(None) };
    static QUEUED_CONTROL: RefCell<Option<tilo_actions::ControlInvocation>> = const { RefCell::new(None) };
    static QUEUED_ORIGIN: Cell<tilo_ui::widget::controls::Origin> = const { Cell::new(tilo_ui::widget::controls::Origin::Person) };
    static LEGACY_INVOKED: Cell<bool> = const { Cell::new(false) };
    static LEGACY_INTENT: RefCell<Option<tilo_ui::record::RecordCheckout>> = const { RefCell::new(None) };
    static LEGACY_SCOPES: RefCell<Vec<tilo_ui::record::RecordId>> = const { RefCell::new(vec![]) };
    static LEGACY_MUTABLE: Cell<bool> = const { Cell::new(false) };
    static LEGACY_DOCUMENTS: RefCell<Vec<(String, bool)>> = const { RefCell::new(vec![]) };
    static LEGACY_START: Cell<Option<bool>> = const { Cell::new(None) };
    static LEGACY_START_CALLS: Cell<usize> = const { Cell::new(0) };
    static EXPECTED_WRITE: Cell<bool> = const { Cell::new(false) };
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
        TASKS.with_borrow_mut(|tasks| tasks.push(tilo_ui::LatestTask::new(0)));
    }
    /// Check `doc` is this leaf's record and note the callback `label`.
    fn observe(&self, doc: &AutoCommit, label: &'static str) {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.record.as_str())
        );
        self.capture(label);
    }
    fn edit(&self, doc: &mut tilo_ui::widget::Edits<'_>, label: &'static str) {
        self.observe(doc, label);
        crate::support::application::write_fixture_string(doc, "last_callback", label);
    }
}
impl Widget for Leaf {
    tilo_ui::provides!(
        dyn tilo_ui::resources::PublishesResources,
        dyn tilo_ui::host_operation::ResourceOperations
    );

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        let (doc, _) = tilo_ui::doc::document_ref(doc, path);
        self.observe(doc, "sync");
    }
    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let (doc, _) = tilo_ui::doc::document_ref(doc, path);
        self.observe(doc, "update");
        false
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        _: &tilo_ui::Theme,
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
        _: &mut tilo_ui::TextRenderer,
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
        _: &mut tilo_ui::TextRenderer,
        _: &tilo_ui::Theme,
        _: &tilo_ui::PresenceCtx,
    ) {
        self.capture("draw");
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        ctx: &mut tilo_ui::widget::EventCtx<'_>,
    ) {
        assert_eq!(
            self.layout_node,
            Some(ctx.node_id),
            "adapter shifted event topology"
        );
        self.edit(&mut ctx.state, "event");
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
    fn accessibility(&self) -> Option<tilo_ui::a11y::AccessNode> {
        self.capture("accessibility");
        Some(
            tilo_ui::a11y::AccessNode::new(tilo_ui::a11y::Role::Button)
                .with_name("Bound leaf")
                .with_children(vec![
                    tilo_ui::a11y::AccessNode::new(tilo_ui::a11y::Role::Button)
                        .with_name("Synthetic probe")
                        .with_key(42),
                ]),
        )
    }
    fn perform_action(
        &mut self,
        action: &tilo_ui::a11y::Action,
        key_path: &[u64],
        doc: &mut tilo_ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (doc, _) = tilo_ui::doc::document(doc, path);
        if key_path.is_empty() {
            self.edit(doc, "action");
            return true;
        }
        assert_eq!(key_path, &[42]);
        assert_eq!(action, &tilo_ui::a11y::Action::Focus);
        self.edit(doc, "synthetic-action");
        true
    }
}

impl tilo_ui::host_operation::ResourceOperations for Leaf {
    #[allow(
        clippy::panic_in_result_fn,
        reason = "callback probe asserts its actual canonical record before returning the toolkit result"
    )]
    fn start_operation(
        &mut self,
        op: &tilo_ui::host_operation::ResourceOperation<'_>,
        doc: &mut tilo_ui::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<tilo_ui::host_operation::ResourceOperationStart>, String> {
        if op.unscoped().is_none() {
            return Ok(None);
        }
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.record.as_str())
        );
        self.capture("operation-start");
        self.operation_state = Some(LocalState::new(0));
        Ok(Some(
            tilo_ui::host_operation::ResourceOperationStart::Accepted,
        ))
    }

    fn poll_resource_operation(
        &mut self,
        id: tilo_actions::InvocationId,
        cancel: bool,
        doc: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
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
            Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation: id,
                message: None,
            })
        } else {
            self.capture("operation-poll");
            FINISH_OPERATION
                .get()
                .then_some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation: id,
                    message: None,
                })
        }
    }
}

impl tilo_ui::resources::PublishesResources for Leaf {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.record.as_str())
        );
        self.capture("resources");
        vec![]
    }
    fn resource(&self, doc: &AutoCommit, _: &str) -> Option<tilo_actions::Resource> {
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.record.as_str())
        );
        self.capture("resource-resolve");
        None
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
        TASKS.with_borrow_mut(|tasks| tasks.push(tilo_ui::LatestTask::new(0)));
        CAPTURES
            .with_borrow_mut(|captures| captures.push((self.1.clone(), label, LocalState::new(0))));
    }
}
impl Widget for Root {
    tilo_ui::provides!(
        dyn tilo_ui::widget::controls::ActionControls,
        dyn tilo_ui::search::capability::SearchSource,
        dyn tilo_ui::record::capability::RecordScoped,
        dyn tilo_ui::view::capability::ViewState,
        dyn tilo_ui::host_operation::ResourceOperations
    );

    fn sync(&mut self, d: &AppModel, p: &ModelPath) {
        self.0.sync(d, p);
    }
    fn update(&mut self, d: &AppModel, p: &ModelPath) -> bool {
        self.0.update(d, p)
    }
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        h: &tilo_ui::Theme,
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
        renderer: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::PresenceCtx,
    ) {
        assert!(
            presence.permits(&tilo_ui::permissions::PermissionScope::workspace(
                tilo_ui::permissions::ActionKind::Undoable
            )) == EXPECTED_WRITE.get(),
            "draw received outer authority"
        );
        self.mark("draw-permissions");
        self.0.draw(scene, transform, renderer, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui::widget::EventCtx<'_>,
    ) {
        self.0.handle_event(event, ctx);
    }
    fn on_external_event(
        &mut self,
        _: &str,
        doc: &mut tilo_ui::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        let (doc, _) = tilo_ui::doc::document(doc, path);
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.1.as_str())
        );
        self.mark("external");
    }
    fn test_id(&self) -> Option<&str> {
        Some("dispatch-root")
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
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.0);
        true
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.2 {
            vec![]
        } else {
            self.0.children_mut()
        }
    }
}

impl tilo_ui::host_operation::ResourceOperations for Root {
    fn start_operation(
        &mut self,
        op: &tilo_ui::host_operation::ResourceOperation<'_>,
        doc: &mut tilo_ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<tilo_ui::host_operation::ResourceOperationStart>, String> {
        if op.unscoped().is_none() {
            return tilo_ui::host_operation::start_in_children(self, op, doc, path);
        }
        if !LEGACY_MUTABLE.get() {
            if let Some(started) = tilo_ui::host_operation::start_in_children(self, op, doc, path)?
            {
                return Ok(Some(started));
            }
            if let Some(accept) = LEGACY_START.get() {
                LEGACY_START_CALLS.set(LEGACY_START_CALLS.get().saturating_add(1));
                return Ok(accept.then(|| {
                    tilo_ui::host_operation::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                }));
            }
            return start_operation(&mut self.0, op, doc, path);
        }
        let marker = doc
            .get(ROOT, "marker")
            .map_err(|e| e.to_string())?
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        LEGACY_DOCUMENTS.with_borrow_mut(|seen| seen.push((marker, path == &ROOT)));
        crate::support::application::write_fixture_flag(doc, "legacy-follow-up");
        Ok(Some(
            tilo_ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl tilo_ui::view::capability::ViewState for Root {
    fn view_state(&self) -> Option<tilo_ui::view::ViewContribution> {
        assert!(!PANIC_VIEW.replace(false), "controlled capture unwind");
        self.mark("capture-view");
        Some(if INVALID_VIEW.get() {
            tilo_ui::view::ViewContribution {
                scroll: vec![f64::NAN],
                ..Default::default()
            }
        } else {
            tilo_ui::view::ViewContribution::default()
        })
    }
    fn restore_view_state(&mut self, _: &tilo_ui::view::ViewContribution) {
        self.mark("restore-view");
    }
}

impl tilo_ui::record::capability::RecordScoped for Root {
    fn record_intent(&self) -> Option<tilo_ui::record::RecordCheckout> {
        LEGACY_INTENT.with_borrow(Clone::clone)
    }
    fn set_record_scope(&mut self, record: &tilo_ui::record::RecordId) {
        if LEGACY_MUTABLE.get() {
            LEGACY_SCOPES.with_borrow_mut(|scopes| scopes.push(record.clone()));
        }
    }
}

impl tilo_ui::search::capability::SearchSource for Root {
    fn search_providers(&self) -> Vec<Box<dyn tilo_ui::search::SearchProvider>> {
        self.mark("provider-create");
        vec![]
    }
    fn activate_search_result(
        &mut self,
        _: &str,
        doc: &mut tilo_ui::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        let (doc, _) = tilo_ui::doc::document(doc, path);
        assert_eq!(
            doc.get(ROOT, "marker").unwrap().unwrap().0.as_str(),
            Some(self.1.as_str())
        );
        self.mark("search-activate");
    }
}

impl tilo_ui::widget::controls::ActionControls for Root {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.mark("control-drain");
        let taken = QUEUED_CONTROL.with_borrow_mut(Option::take);
        if taken.is_some() {
            tilo_ui::widget::controls::note_drained_origin(
                QUEUED_ORIGIN
                    .with(|origin| origin.replace(tilo_ui::widget::controls::Origin::Person)),
            );
        }
        taken
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
    fn commands(&self) -> Vec<tilo_ui::AppCommand> {
        COMMAND_CALLS.set(COMMAND_CALLS.get().saturating_add(1));
        vec![tilo_ui::AppCommand {
            id: "legacy".into(),
            label: "Legacy".into(),
            shortcut: None,
            invoke: Box::new(|_, _| LEGACY_INVOKED.set(true)),
        }]
    }
    fn take_host_intent(&mut self) -> Option<tilo_ui::widget::AppHostIntent> {
        self.mark("host-intent");
        None
    }
}
#[allow(clippy::needless_pass_by_value, reason = "fixed factory signature")]
fn factory(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, tilo_ui::launch::LaunchError> {
    let record = context.record().id.as_str().to_string();
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "marker", record.clone())
                    .map_err(|e| e.to_string())
            })
            .map_err(tilo_ui::launch::LaunchError::Record)?;
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
        tilo_actions::ActionRegistry::new(),
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

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn reentry_and_replacement_respect_record_borrows_and_retry_failures() {
    // Case `same_record_reentry_reuses_the_borrow_and_cross_record_reentry_is_refused`.
    {
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
        let node = root.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
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
                                .as_str()
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
                        |widget, doc, root| {
                            widget.perform_action(
                                &tilo_ui::a11y::Action::Focus,
                                &[],
                                &mut tilo_ui::widget::Edits::lend(doc),
                                root,
                            )
                        },
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

    // Case `replacement_cancels_admitted_work_and_refuses_the_old_node_before_dereference`.
    {
        let host = ProbeHost::new();
        let mut tile = TileHost::from_component(component(&host, 1));
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let layout = tile.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
        let mut bindings = BoundNodeMap::new();
        // SAFETY: admission inspects only this matching live layout.
        unsafe {
            bindings.rebuild(&tile, &tree, layout, &nodes);
        }
        let mut outer = AutoCommit::new();
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            start_operation(
                &mut tile,
                &tilo_ui::host_operation::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    "probe",
                    &[]
                ),
                &mut tilo_ui::widget::Edits::lend(&mut outer),
                &ROOT
            )
            .unwrap(),
            Some(tilo_ui::host_operation::ResourceOperationStart::Accepted)
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

    // Case `driver_retains_both_replacement_failures_and_retries_after_the_last_tile_drops`.
    {
        let host = ProbeHost::new();
        let component = component(&host, 1);
        let checkout = component.record().clone();
        let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
            tilo_ui::font_catalog::checkout_root(),
            "en-US",
        );
        let replace = std::rc::Rc::new(Cell::new(false));
        let mut driver = tilo_ui::headless::FrameHarness::new_with_controlled_inputs(
            tilo_ui::State::new(),
            Box::new(ReplacingHost(
                TileHost::from_component(component),
                replace.clone(),
            )),
            (400, 300),
            tilo_ui::Theme::dark(),
            tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap(),
            tilo_ui::permissions::UserId("probe-author".into()),
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
                    .as_str()
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table proves the complete root callback ledger against a single mounted instance"
)]
fn driver_tiles_route_root_callbacks_and_paint_with_current_permissions() {
    // Case `root_callback_ledger_uses_real_tile_routes_and_omits_legacy_commands`.
    {
        enum Callback {
            DrawPermissions,
            Providers,
            Search,
            External,
            Control,
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
        let mut outer = tilo_ui::wake::WakeScope::new();
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
        let invocation = tilo_actions::InvocationId::new();
        for (callback, label) in [
            (Callback::DrawPermissions, "draw-permissions"),
            (Callback::Providers, "provider-create"),
            (Callback::Search, "search-activate"),
            (Callback::External, "external"),
            (Callback::Control, "control-drain"),
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
                    let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
                        tilo_ui::font_catalog::checkout_root(),
                        "en-US",
                    );
                    let mut renderer = tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap();
                    let presence = tilo_ui::PresenceCtx::empty();
                    presence.with_permissions(
                        tilo_ui::permissions::PermissionContext::permissive(),
                        || {
                            tile.draw(
                                &mut vello::Scene::new(),
                                vello::kurbo::Affine::IDENTITY,
                                &mut renderer,
                                &tilo_ui::Theme::dark(),
                                &presence,
                            );
                        },
                    );
                }

                Callback::Start => {
                    start_operation(
                        &mut tile,
                        &tilo_ui::host_operation::ResourceOperation::local(
                            invocation,
                            tilo_actions::OperationKind::Effect,
                            "probe",
                            &[],
                        ),
                        &mut tilo_ui::widget::Edits::lend(&mut shell),
                        &ROOT,
                    )
                    .unwrap();
                }
                Callback::Poll => {
                    poll_resource_operation(&mut tile, invocation, false, &shell, &ROOT);
                }
                Callback::Providers => {
                    tilo_ui::search::capability::search_providers(&tile);
                }
                Callback::Search => tilo_ui::search::capability::activate_search_result(
                    &mut tile,
                    "result",
                    &mut tilo_ui::widget::Edits::lend(&mut shell),
                    &ROOT,
                ),
                Callback::External => tile.on_external_event(
                    "probe",
                    &mut tilo_ui::widget::Edits::lend(&mut shell),
                    &ROOT,
                ),
                Callback::Control => {
                    // A tile with its own document owner is drained through that
                    // owner's control custody, never by the generic outer drain.
                    assert!(
                        tilo_ui::widget::controls::take_control_invocation(&mut tile).is_none()
                    );
                    assert!(CAPTURES.with_borrow_mut(std::mem::take).is_empty());
                    continue;
                }
                Callback::Capture => {
                    tilo_ui::view::capability::view_state(&tile);
                }
                Callback::Restore => {
                    tilo_ui::view::capability::restore_view_state(
                        &mut tile,
                        &tilo_ui::view::ViewContribution::default(),
                    );
                }
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
        let tilo_ui::widget::TileContent::App(root) = tile.content() else {
            panic!("missing component")
        };
        assert!(root.commands().is_empty());
        assert_eq!(
            COMMAND_CALLS.get(),
            0,
            "canonical path consulted legacy producer"
        );
        assert!(!LEGACY_INVOKED.get());
        let legacy = TileHost::new(tilo_ui::widget::TileContent::App(Box::new(Root(
            Column::new(),
            "legacy".into(),
            false,
        ))));
        let tilo_ui::widget::TileContent::App(root) = legacy.content() else {
            panic!("missing legacy root")
        };
        let commands = root.commands();
        assert_eq!(commands.len(), 1);
        (commands[0].invoke)(&mut shell, &ROOT);
        assert!(LEGACY_INVOKED.get());
        assert_eq!(COMMAND_CALLS.get(), 1);
    }

    // Case `driver_tile_paint_observes_current_component_permissions`.
    {
        let host = ProbeHost::new();
        let definition = definition(factory);
        let permissions = crate::support::application::permissions();
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records: host.records.clone(),
                views: host.views,
                device_id: "D".into(),
                instance_id: 1,
                permissions: permissions.clone(),
                author: crate::support::application::author(),
                action_context: crate::support::application::provenance(),
            },
            &LaunchTarget::New,
        )
        .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        CAPTURES.with_borrow_mut(Vec::clear);
        let mut driver = controlled_driver(
            Box::new(Column::new().push(Box::new(TileHost::from_component(component)))),
            (400, 300),
        );
        assert!(
            CAPTURES.with_borrow(|c| c.iter().any(|(_, label, _)| *label == "draw-permissions")),
            "ordinary FrameHarness paint did not reach the permission-aware root"
        );
        *permissions.borrow_mut() = tilo_ui::permissions::PermissionContext::permissive();
        EXPECTED_WRITE.set(true);
        CAPTURES.with_borrow_mut(Vec::clear);
        driver.relayout();
        assert!(
            CAPTURES.with_borrow(|c| c.iter().any(|(_, label, _)| *label == "draw-permissions")),
            "later paint did not read the current permission snapshot"
        );
        EXPECTED_WRITE.set(false);
    }

    // Case `canonical_fill_geometry_matches_legacy_tile_at_multiple_viewports`.
    {
        for viewport in [(400, 300), (900, 600)] {
            let host = ProbeHost::new();
            let definition = definition(fill_factory);
            let component = definition
                .instantiate(
                    host.prepare(&definition, 1, &LaunchTarget::New).unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let canonical = controlled_driver(
                Box::new(Column::new().push(Box::new(TileHost::from_component(component)))),
                viewport,
            );
            // A bare root draws under the driver's own permissive authority.
            EXPECTED_WRITE.set(true);
            let legacy = controlled_driver(
                Box::new(Column::new().push(Box::new(TileHost::new(
                    tilo_ui::widget::TileContent::App(Box::new(Root(
                        Column::new().with_no_padding(),
                        String::new(),
                        false,
                    ))),
                )))),
                viewport,
            );
            EXPECTED_WRITE.set(false);
            let actual = canonical.targets()["dispatch-root"];
            let expected = legacy.targets()["dispatch-root"];
            assert!(expected.width() > f64::from(viewport.0) / 2.0);
            assert!(
                expected.x0 > 0.0 && expected.y0 > 0.0,
                "fixture exercises a nonzero host origin"
            );
            assert_eq!(
                actual, expected,
                "canonical wrapper changed fill geometry at {viewport:?}"
            );
        }
    }
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

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn operation_captures_are_destroyed_under_the_closed_owner_on_every_exit() {
    // Case `cancellation_unwind_and_ordinary_drop_destroy_operation_captures_under_closed_owner`.
    {
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
            let node = tile.build_layout(&mut tree, &tilo_ui::Theme::dark(), &mut nodes);
            let mut bindings = BoundNodeMap::new();
            // SAFETY: this admission uses the fixture's live matching topology.
            unsafe {
                bindings.rebuild(&tile, &tree, node, &nodes);
            }
            start_operation(
                &mut tile,
                &tilo_ui::host_operation::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    "probe",
                    &[],
                ),
                &mut tilo_ui::widget::Edits::lend(&mut AutoCommit::new()),
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

    // Case `terminal_operation_is_removed_before_later_close`.
    {
        let host = ProbeHost::new();
        let mut tile = TileHost::from_component(component(&host, 1));
        let id = tilo_actions::InvocationId::new();
        let mut doc = AutoCommit::new();
        start_operation(
            &mut tile,
            &tilo_ui::host_operation::ResourceOperation::local(
                id,
                tilo_actions::OperationKind::Effect,
                "probe",
                &[],
            ),
            &mut tilo_ui::widget::Edits::lend(&mut doc),
            &ROOT,
        )
        .unwrap();
        FINISH_OPERATION.set(true);
        assert!(
            poll_resource_operation(&mut tile, id, false, &doc, &ROOT)
                .unwrap()
                .is_terminal()
        );
        FINISH_OPERATION.set(false);
        CANCELLED.set(0);
        tilo_ui::component_lifecycle::close_components(&mut tile);
        assert_eq!(CANCELLED.get(), 0, "terminal work was cancelled twice");
    }

    // Case `owned_factory_error_restore_error_suspend_and_unwind_keep_drop_captures_closed`.
    {
        let host = ProbeHost::new();
        for create in [
            factory_then_error as tilo_ui::application::ComponentFactory,
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

    // Case `capture_unwind_closes_the_lifetime_and_drops_root_before_returning_to_host`.
    {
        let host = ProbeHost::new();
        let mut tile = TileHost::from_component(component(&host, 1));
        CLOSED_DROPS.set(0);
        PANIC_VIEW.set(true);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tilo_ui::component_lifecycle::close_components(&mut tile)
            }))
            .is_err()
        );
        assert_eq!(
            CLOSED_DROPS.get(),
            1,
            "close unwind left a live root in the tile"
        );
        tile.on_external_event(
            "refused",
            &mut tilo_ui::widget::Edits::lend(&mut AutoCommit::new()),
            &ROOT,
        );
        assert_eq!(CLOSED_DROPS.get(), 1);
    }
}

thread_local! {
    static NESTED_REPLACE: RefCell<Option<std::rc::Rc<Cell<bool>>>> = const { RefCell::new(None) };
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
) -> Result<AppComponent, tilo_ui::launch::LaunchError> {
    let record = context.record().id.as_str().to_owned();
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "marker", record.clone())
                .map_err(|e| e.to_string())
        })
        .map_err(tilo_ui::launch::LaunchError::Record)?;
    let child = NESTED.with_borrow_mut(Option::take).unwrap();
    let tile = TileHost::from_component(child);
    let child: Box<dyn Widget> = if let Some(replace) = NESTED_REPLACE.with_borrow_mut(Option::take)
    {
        Box::new(ReplacingHost(tile, replace))
    } else {
        Box::new(tile)
    };
    Ok(AppComponent::new(
        context,
        Box::new(Root(Column::new().push(child), record, false)),
        tilo_actions::ActionRegistry::new(),
    ))
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one nested replacement must preserve live parent, independent recovery ownership and durable retry"
)]
fn nested_close_replacement_and_mount_failures_report_each_original_once() {
    // Case `nested_close_transfers_each_original_report_once_before_roots_are_destroyed`.
    {
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
        let mut reports = tilo_ui::component_lifecycle::close_components(&mut tile);
        assert_eq!(reports.len(), 2);
        assert!(tilo_ui::component_lifecycle::close_components(&mut tile).is_empty());
        assert!(tilo_ui::component_lifecycle::take_component_close_reports(&mut tile).is_empty());
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

    // Case `prepared_runner_nested_mount_failure_retains_each_original_report_once`.
    {
        for native in [false, true] {
            let host = ProbeHost::new();
            NESTED.with_borrow_mut(|nested| *nested = Some(component(&host, 2)));
            let definition = definition(nested_factory);
            let mut parent = definition
                .instantiate(
                    host.prepare(&definition, 1, &LaunchTarget::New).unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            parent.mount(|_| {}).unwrap();
            for id in [1, 2] {
                std::fs::create_dir_all(host.view_path(id)).unwrap();
            }
            let paths = [host.view_path(1), host.view_path(2)];
            drop(host.views);
            let error = if native {
                tilo_ui::App::from_component(parent, crate::support::application::runner_inputs())
                    .err()
                    .unwrap()
            } else {
                tilo_ui::headless::FrameHarness::from_component(
                    parent,
                    (320, 180),
                    crate::support::application::runner_inputs(),
                )
                .err()
                .unwrap()
            };
            let tilo_ui::launch::LaunchError::ComponentSetup { recovery, .. } = error else {
                panic!("missing setup recovery");
            };
            let mut reports = recovery.into_close_reports().unwrap();
            assert_eq!(reports.len(), 2);
            reports.sort_by_key(|report| report.recovery.as_ref().unwrap().instance_id);
            for (index, report) in reports.iter_mut().enumerate() {
                assert_eq!(report.recovery.as_ref().unwrap().instance_id, [1, 2][index]);
                assert!(report.view_error.is_some());
                std::fs::remove_dir(&paths[index]).unwrap();
                report.retry_view().unwrap();
                assert!(paths[index].is_file());
            }
        }
    }

    // Case `nested_replacement_reports_reach_driver_while_parent_remains_open`.
    {
        let child_host = ProbeHost::new();
        let child = component(&child_host, 2);
        let checkout = child.record().clone();
        child_host.records.try_flush().unwrap();
        let record_path = app_state::DocStore::room_record_dir_at(
            &child_host.root(),
            crate::support::application::APP,
            crate::support::application::ROOM,
            &checkout.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE);
        std::fs::remove_file(&record_path).unwrap();
        std::fs::create_dir(&record_path).unwrap();
        let view_path = child_host.view_path(2);
        std::fs::create_dir_all(&view_path).unwrap();
        child_host
            .records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "unsaved", "nested-retained").unwrap();
            })
            .unwrap();
        NESTED.with_borrow_mut(|n| *n = Some(child));
        let replace = std::rc::Rc::new(Cell::new(false));
        NESTED_REPLACE.with_borrow_mut(|n| *n = Some(replace.clone()));
        let parent_host = ProbeHost::new();
        let definition = definition(nested_factory);
        let mut parent = definition
            .instantiate(
                parent_host
                    .prepare(&definition, 1, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        parent
            .mount(move |event| {
                sender.send(event).unwrap();
            })
            .unwrap();
        let mut driver = controlled_driver(Box::new(TileHost::from_component(parent)), (400, 300));
        replace.set(true);
        CAPTURES.with_borrow_mut(Vec::clear);
        driver.relayout();
        assert_eq!(
            driver.component_close_reports().len(),
            1,
            "open parent hid its replaced child's recovery"
        );
        let mut reports = driver.take_component_close_reports();
        assert!(
            driver.take_component_close_reports().is_empty(),
            "nested report transferred twice"
        );
        let live = CAPTURES.with_borrow(|c| {
            c.iter()
                .find(|(_, label, _)| *label == "draw-permissions")
                .unwrap()
                .2
                .clone()
        });
        assert!(
            live.writer().try_set(1),
            "parent was closed to obtain child recovery"
        );
        assert!(receiver.try_recv().is_ok());
        drop(driver);
        assert!(!live.writer().try_set(2));
        drop(child_host.records);
        drop(child_host.owners);
        drop(child_host.views);
        drop(parent_host);
        let report = reports.last_mut().unwrap();
        assert_eq!(report.recovery.as_ref().unwrap().instance_id, 2);
        assert!(report.view_error.is_some() && report.record_error.is_some());
        assert_eq!(
            report
                .record_retry
                .as_ref()
                .unwrap()
                .read(&checkout.id, |doc| {
                    doc.get(ROOT, "unsaved")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .unwrap(),
            "nested-retained"
        );
        std::fs::remove_dir(view_path).unwrap();
        std::fs::remove_dir(record_path).unwrap();
        report.retry_view().unwrap();
        report.retry_record().unwrap();
        assert!(report.view_retry.is_none() && report.record_retry.is_none());
        let mut owners = tilo_ui::record::RecordOwners::new(child_host.temp.path().join("state"));
        let saved = owners
            .open(
                crate::support::application::APP,
                crate::support::application::ROOM,
            )
            .unwrap();
        saved.try_open_record(&checkout).unwrap();
        assert_eq!(
            saved
                .read(&checkout.id, |doc| {
                    doc.get(ROOT, "unsaved")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .unwrap(),
            "nested-retained"
        );
    }
}

fn factory_then_error(
    context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, tilo_ui::launch::LaunchError> {
    let _owned = factory(context, target, restore)?;
    Err(tilo_ui::launch::LaunchError::Operation(
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
) -> Result<AppComponent, tilo_ui::launch::LaunchError> {
    let _owned = factory(context, target, restore)?;
    panic!("controlled post-owner unwind")
}

thread_local! { static PANIC_VIEW: Cell<bool> = const { Cell::new(false) }; }

struct ReplacingHost(TileHost, std::rc::Rc<Cell<bool>>);
impl Widget for ReplacingHost {
    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        if self.1.replace(false) {
            self.0
                .replace_content(tilo_ui::widget::TileContent::Notice {
                    title: "Closed".into(),
                    detail: "Fixture replacement".into(),
                });
        }
        self.0.update(doc, path)
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui::Theme,
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
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
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

// Registration-only uniqueness must not mistake this retained wrapper for
// its sole registered inline child. Name/address are deliberately identical.
#[repr(transparent)]
struct SameNameUnregisteredWrapper(Button);
impl Widget for SameNameUnregisteredWrapper {
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui::Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.0.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.0.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
    fn name(&self) -> &str {
        self.0.name()
    }
    fn test_id(&self) -> Option<&'static str> {
        Some("ambiguous-wrapper")
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

fn controlled_driver(
    root: Box<dyn Widget>,
    viewport: (u32, u32),
) -> tilo_ui::headless::FrameHarness {
    let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
        tilo_ui::font_catalog::checkout_root(),
        "en-US",
    );
    tilo_ui::headless::FrameHarness::new_with_controlled_inputs(
        tilo_ui::State::new(),
        root,
        viewport,
        tilo_ui::Theme::dark(),
        tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap(),
        tilo_ui::permissions::UserId("probe-author".into()),
    )
}

fn fill_factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, tilo_ui::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(tilo_ui::launch::LaunchError::Record)?;
    Ok(AppComponent::new(
        context,
        Box::new(Root(Column::new().with_no_padding(), String::new(), false)),
        tilo_actions::ActionRegistry::new(),
    ))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one live tile proves successful intent transition and retention across missing, mismatched and busy requests"
)]
fn legacy_mutable_admission_edits_only_its_record_once() {
    // Case `mutable_container_traversal_invokes_legacy_fallback_once`.
    {
        for accept in [true, false] {
            LEGACY_START.set(Some(accept));
            LEGACY_START_CALLS.set(0);
            let mut tree =
                Column::new().push(Box::new(tilo_ui::widget::Row::new().push(Box::new(
                    tilo_ui::widget::Container::new().with_child(Box::new(TileHost::new(
                        tilo_ui::widget::TileContent::App(Box::new(Root(
                            Column::new(),
                            String::new(),
                            false,
                        ))),
                    ))),
                ))));
            let result = start_operation(
                &mut tree,
                &tilo_ui::host_operation::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    "legacy",
                    &[],
                ),
                &mut tilo_ui::widget::Edits::lend(&mut AutoCommit::new()),
                &ROOT,
            )
            .unwrap();
            assert_eq!(result.is_some(), accept);
            assert_eq!(
                LEGACY_START_CALLS.get(),
                1,
                "legacy accepting/declining handler was revisited"
            );
            LEGACY_START.set(None);
        }
    }

    // Case `legacy_bound_mutable_admission_edits_only_its_record_and_refuses_missing_or_busy`.
    {
        let host = ProbeHost::new();
        let checkout = tilo_ui::record::RecordCheckout::new(
            tilo_ui::record::RecordId::minted(),
            tilo_ui::record::RecordKind::new("probe"),
        );
        host.records
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "marker", "bound-B")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let missing = tilo_ui::record::RecordCheckout::new(
            tilo_ui::record::RecordId::minted(),
            tilo_ui::record::RecordKind::new("probe"),
        );
        let mut outer = AutoCommit::new();
        outer.put(ROOT, "marker", "outer-A").unwrap();
        let outer_path = outer
            .put_object(ROOT, "outer-path", automerge::ObjType::Map)
            .unwrap();
        LEGACY_MUTABLE.set(true);
        LEGACY_DOCUMENTS.with_borrow_mut(Vec::clear);
        let mut outcomes = vec![];
        for requested in [&checkout, &missing, &checkout] {
            let mut tile = TileHost::new(tilo_ui::widget::TileContent::App(Box::new(Root(
                Column::new(),
                String::new(),
                false,
            ))));
            tile.bind_record(host.records.clone(), requested.clone());
            let mut tree = Column::new().push(Box::new(tilo_ui::widget::Row::new().push(
                Box::new(tilo_ui::widget::Container::new().with_child(Box::new(tile))),
            )));
            let mut invoke = || {
                start_operation(
                    &mut tree,
                    &tilo_ui::host_operation::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        "legacy.mutable",
                        &[],
                    ),
                    &mut tilo_ui::widget::Edits::lend(&mut outer),
                    &outer_path,
                )
            };
            let result = if outcomes.len() == 2 {
                host.records.with(&checkout.id, |_| invoke()).unwrap()
            } else {
                invoke()
            };
            outcomes.push(result);
        }
        LEGACY_MUTABLE.set(false);
        assert_eq!(
            LEGACY_DOCUMENTS.with_borrow(Clone::clone),
            vec![("bound-B".to_string(), true)],
            "bound callback must run once with its record and ROOT; missing/busy bindings must not execute"
        );
        assert!(matches!(
            outcomes[0],
            Ok(Some(
                tilo_ui::host_operation::ResourceOperationStart::Completed(_)
            ))
        ));
        assert_eq!(
            outcomes[1].as_ref().unwrap_err(),
            &tilo_ui::record::RecordOpenError::MissingRecord.to_string()
        );
        assert_eq!(
            outcomes[2].as_ref().unwrap_err(),
            &tilo_ui::record::RecordOpenError::Busy.to_string()
        );
        assert!(outer.get(ROOT, "legacy-follow-up").unwrap().is_none());
        assert!(
            host.records
                .read(&checkout.id, |doc| doc
                    .get(ROOT, "legacy-follow-up")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_bool()
                    .unwrap())
                .unwrap()
        );
    }

    // Case `legacy_mutable_admission_follows_new_intent_only_after_checked_resolution`.
    {
        let host = ProbeHost::new();
        let first = tilo_ui::record::RecordCheckout::new(
            tilo_ui::record::RecordId::minted(),
            tilo_ui::record::RecordKind::new("probe"),
        );
        let next = tilo_ui::record::RecordCheckout::new(
            tilo_ui::record::RecordId::minted(),
            tilo_ui::record::RecordKind::new("probe"),
        );
        let missing = tilo_ui::record::RecordCheckout::new(
            tilo_ui::record::RecordId::minted(),
            tilo_ui::record::RecordKind::new("probe"),
        );
        for (checkout, marker) in [(&first, "bound-B"), (&next, "selected-C")] {
            host.records
                .reserve_record(checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "marker", marker)
                        .map_err(|error| error.to_string())
                })
                .unwrap();
        }
        LEGACY_MUTABLE.set(true);
        LEGACY_DOCUMENTS.with_borrow_mut(Vec::clear);
        LEGACY_SCOPES.with_borrow_mut(Vec::clear);
        let mut tile = TileHost::new(tilo_ui::widget::TileContent::App(Box::new(Root(
            Column::new(),
            String::new(),
            false,
        ))));
        tile.bind_record(host.records.clone(), first.clone());
        let mut tree = Column::new().push(Box::new(tilo_ui::widget::Row::new().push(Box::new(
            tilo_ui::widget::Container::new().with_child(Box::new(tile)),
        ))));
        let mut outer = AutoCommit::new();
        outer.put(ROOT, "marker", "outer-A").unwrap();
        let mut invoke = || {
            start_operation(
                &mut tree,
                &tilo_ui::host_operation::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    "legacy.mutable",
                    &[],
                ),
                &mut tilo_ui::widget::Edits::lend(&mut outer),
                &ROOT,
            )
        };
        LEGACY_INTENT.with_borrow_mut(|intent| *intent = Some(next.clone()));
        let selected = invoke();
        LEGACY_INTENT.with_borrow_mut(|intent| *intent = Some(missing));
        let absent = invoke();
        LEGACY_INTENT.with_borrow_mut(|intent| {
            *intent = Some(tilo_ui::record::RecordCheckout::new(
                next.id.clone(),
                tilo_ui::record::RecordKind::new("wrong-kind"),
            ));
        });
        let mismatched = invoke();
        LEGACY_INTENT.with_borrow_mut(|intent| *intent = Some(first.clone()));
        let busy = host.records.with(&next.id, |_| invoke()).unwrap();
        LEGACY_INTENT.with_borrow_mut(|intent| *intent = None);
        let retained = invoke();
        LEGACY_MUTABLE.set(false);
        assert!(matches!(
            selected,
            Ok(Some(
                tilo_ui::host_operation::ResourceOperationStart::Completed(_)
            ))
        ));
        assert_eq!(
            absent.as_ref().unwrap_err(),
            &tilo_ui::record::RecordOpenError::MissingRecord.to_string()
        );
        assert_eq!(
            mismatched.as_ref().unwrap_err(),
            &tilo_ui::record::RecordOpenError::IdentityMismatch.to_string()
        );
        assert_eq!(
            busy.as_ref().unwrap_err(),
            &tilo_ui::record::RecordOpenError::Busy.to_string()
        );
        assert!(matches!(
            retained,
            Ok(Some(
                tilo_ui::host_operation::ResourceOperationStart::Completed(_)
            ))
        ));
        assert_eq!(
            LEGACY_DOCUMENTS.with_borrow(Clone::clone),
            vec![
                ("selected-C".to_string(), true),
                ("selected-C".to_string(), true)
            ]
        );
        assert_eq!(
            LEGACY_SCOPES.with_borrow(Clone::clone),
            vec![first.id.clone(), next.id.clone()],
            "failed intent changed live scope"
        );
        assert!(outer.get(ROOT, "legacy-follow-up").unwrap().is_none());
        assert!(
            host.records
                .read(&first.id, |doc| doc
                    .get(ROOT, "legacy-follow-up")
                    .unwrap()
                    .is_none())
                .unwrap()
        );
        assert!(
            host.records
                .read(&next.id, |doc| doc
                    .get(ROOT, "legacy-follow-up")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_bool()
                    .unwrap())
                .unwrap()
        );
    }
}

#[test]
fn bound_resources_list_and_look_up_what_the_component_permissions_admit() {
    use crate::support::application::action_probe_factory;
    use tilo_ui::permissions::{PermissionContext, Role};
    let host = ProbeHost::new();
    let definition = definition(action_probe_factory);
    let driver = |instance, role| {
        let permissions = std::rc::Rc::new(std::cell::RefCell::new(
            PermissionContext::member("probe-author", role).at_unix_time(100),
        ));
        let context = host
            .prepare_with_permissions(&definition, instance, &LaunchTarget::New, permissions)
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let Ok(driver) = tilo_ui::headless::FrameHarness::from_component(
            component,
            (200, 100),
            tilo_ui::application::ComponentRunnerInputs {
                theme: tilo_ui::Theme::dark(),
                text_renderer: tilo_ui::TextRenderer::new(),
            },
        ) else {
            panic!("component runner");
        };
        driver
    };
    let actions = |resources: &[tilo_actions::Resource]| {
        resources
            .iter()
            .flat_map(|resource| resource.actions.iter().map(|action| action.id.clone()))
            .collect::<Vec<_>>()
    };

    let owner = driver(21, Role::Owner);
    let listed = owner.bound_resources(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.as_str(), "probe:shared");
    assert!(actions(&listed).contains(&"edit".to_owned()));
    let exact = owner.bound_resources(Some("probe:shared")).unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(actions(&exact), actions(&listed));
    assert!(
        owner
            .bound_resources(Some("probe:missing"))
            .unwrap()
            .is_empty()
    );

    // What the door would refuse is not offered: Write actions leave a viewer's listing.
    let viewer = driver(22, Role::Viewer);
    let exact = viewer.bound_resources(Some("probe:shared")).unwrap();
    let offered = actions(&exact);
    assert!(offered.contains(&"view".to_owned()), "{offered:?}");
    for write in ["edit", "prompt", "typed"] {
        assert!(!offered.contains(&write.to_owned()), "{offered:?}");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one, two and zero authorized targets share the same control and outer-history oracle"
)]
fn driver_controls_resolve_bound_targets_to_their_record_owner() {
    // Case `task4_driver_qualified_equal_resource_ids_use_their_record_and_view_owner`.
    {
        use crate::support::application::{action_probe_factory, existing, operation_observations};
        let host = ProbeHost::new();
        let definition = definition(action_probe_factory);
        let permissions = std::rc::Rc::new(std::cell::RefCell::new(
            tilo_ui::permissions::PermissionContext::member(
                "probe-author",
                tilo_ui::permissions::Role::Owner,
            )
            .at_unix_time(100),
        ));
        let first = definition
            .instantiate(
                host.prepare_with_permissions(
                    &definition,
                    11,
                    &LaunchTarget::New,
                    permissions.clone(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = first.record().clone();
        let target = existing(&checkout);
        let second = definition
            .instantiate(
                host.prepare_with_permissions(&definition, 12, &target, permissions)
                    .unwrap(),
                target,
                RestoreInput::None,
            )
            .unwrap();
        let column = tilo_ui::widget::Column::new()
            .push(Box::new(TileHost::from_component(first)))
            .push(Box::new(TileHost::from_component(second)));
        let mut driver = controlled_driver(Box::new(column), (200, 100));
        let outer = driver.state_mut().doc.get_heads();
        let address = |instance_id, action| tilo_ui::bound_dispatch::BoundActionAddress {
            instance_id,
            address: tilo_actions::ActionAddress::new("probe:shared", action),
        };
        driver
            .invoke_bound_action(&address(12, "view"), &serde_json::Map::new())
            .unwrap();
        let observed = operation_observations();
        assert!(observed[0].borrow().is_empty());
        assert_eq!(*observed[1].borrow(), vec!["View"]);
        host.records
            .read_history(&checkout.id, |doc, history| {
                assert!(
                    history
                        .next_undo_in(doc, &crate::support::application::author(), 12)
                        .is_none()
                );
            })
            .unwrap();
        driver
            .invoke_bound_action(&address(11, "edit"), &serde_json::Map::new())
            .unwrap();
        host.records
            .with_history(&checkout.id, |doc, history| {
                let author = crate::support::application::author();
                assert_eq!(
                    history
                        .next_undo_in(doc, &author, 11)
                        .unwrap()
                        .author
                        .as_ref(),
                    Some(&author)
                );
                assert!(history.next_undo_in(doc, &author, 12).is_none());
                history.undo_in(doc, &author, 11, None, None, None).unwrap();
                assert!(history.next_undo_in(doc, &author, 11).is_none());
                assert_eq!(
                    doc.get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("initialized")
                );
            })
            .unwrap();
        assert_eq!(driver.state_mut().doc.get_heads(), outer);
    }

    // Case `task4_driver_unqualified_control_selects_or_refuses_bound_targets`.
    {
        use crate::support::application::{ProbeAction, action_probe_factory, author};
        use automerge::ReadDoc;
        #[derive(Clone)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for authorized in [1, 2, 0] {
            let diagnostics = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let writer = diagnostics.clone();
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::WARN)
                .with_writer(move || Capture(writer.clone()))
                .finish();
            let _subscriber = tracing::subscriber::set_default(subscriber);
            let host = ProbeHost::new();
            let definition = definition(action_probe_factory);
            let mut column = tilo_ui::widget::Column::new();
            let mut records = Vec::new();
            for instance in 11..13 {
                let allowed = instance < 11_u64.saturating_add(authorized);
                let permissions = std::rc::Rc::new(std::cell::RefCell::new(
                    if allowed {
                        tilo_ui::permissions::PermissionContext::member(
                            "probe-author",
                            tilo_ui::permissions::Role::Owner,
                        )
                    } else {
                        tilo_ui::permissions::PermissionContext::stranger("probe-author")
                    }
                    .at_unix_time(100),
                ));
                let component = definition
                    .instantiate(
                        host.prepare_with_permissions(
                            &definition,
                            instance,
                            &LaunchTarget::New,
                            permissions,
                        )
                        .unwrap(),
                        LaunchTarget::New,
                        RestoreInput::None,
                    )
                    .unwrap();
                records.push(component.record().clone());
                column = column.push(Box::new(TileHost::from_component(component)));
            }
            let resource = tilo_actions::Resource::new("probe:shared", "probe", "Shared probe")
                .with_action(
                    tilo_actions::ActionSpec::immediate(
                        "edit",
                        "Edit",
                        "probe.edit",
                        &ProbeAction("edited".into()),
                    )
                    .unwrap(),
                );
            let button = Button::new("outer-edit", "Outer edit")
                .on_action(tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap());
            column = column.push(Box::new(button));
            let mut driver = controlled_driver(Box::new(column), (300, 200));
            if authorized == 0 {
                driver
                    .action_registry_mut()
                    .register::<ProbeAction>("probe.edit");
            }
            let outer = driver.state_mut().doc.get_heads();
            driver
                .perform_named("outer-edit", &tilo_ui::a11y::Action::Activate)
                .unwrap();
            for (index, checkout) in records.iter().enumerate() {
                host.records
                    .read_history(&checkout.id, |doc, history| {
                        // An outer control never reselects a component owner
                        // from its address, however many would authorize it.
                        let changed = false;
                        assert_eq!(
                            doc.get(ROOT, "body").unwrap().unwrap().0.as_str(),
                            Some(if changed { "edited" } else { "initialized" })
                        );
                        assert_eq!(
                            history
                                .next_undo_in(
                                    doc,
                                    &author(),
                                    11_u64.saturating_add(u64::try_from(index).unwrap())
                                )
                                .is_some(),
                            changed
                        );
                    })
                    .unwrap();
            }
            assert_eq!(driver.state_mut().doc.get_heads(), outer);
            assert!(!driver.state_mut().can_undo_as(None));
            let diagnostics = String::from_utf8(diagnostics.lock().unwrap().clone()).unwrap();
            assert!(diagnostics.contains("unknown_action"), "{diagnostics}");
        }
        assert_same_name_inline_wrapper_refused();
    }
}

fn assert_same_name_inline_wrapper_refused() {
    let clicked = std::rc::Rc::new(Cell::new(false));
    let observer = clicked.clone();
    let wrapper = Box::new(SameNameUnregisteredWrapper(
        Button::new("ambiguous-child", "Child").on_click(move || observer.set(true)),
    ));
    assert_eq!(
        std::ptr::from_ref(wrapper.as_ref()).cast::<()>(),
        std::ptr::from_ref(&wrapper.0).cast::<()>(),
    );
    assert_eq!(wrapper.name(), wrapper.0.name());
    let mut driver = controlled_driver(wrapper, (300, 200));
    let before = (
        driver.state_mut().doc.base_heads(),
        driver.state_mut().doc.pending_ops(),
    );
    for id in ["ambiguous-wrapper", "ambiguous-child"] {
        assert!(
            driver
                .perform_named(id, &tilo_ui::a11y::Action::Activate)
                .is_err()
        );
        assert!(
            driver
                .expect(&tilo_ui::headless::SemanticExpectation::named(id))
                .is_err()
        );
    }
    assert!(!clicked.get());
    assert_eq!(
        (
            driver.state_mut().doc.base_heads(),
            driver.state_mut().doc.pending_ops()
        ),
        before
    );
    assert!(!driver.state_mut().can_undo_as(None));
    driver.close_components();
}
