//! Prepared-driver tests: a launched component restores, syncs and publishes
//! in the order an edit needs, observed through the headless harness.

use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use crate::application_test_support::*;
use crate::headless::*;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Default)]
struct Observations {
    published: Rc<RefCell<crate::undo::UndoStatus>>,
    syncs: Rc<Cell<usize>>,
    local: Rc<RefCell<Option<crate::LocalState<String>>>>,
}
thread_local! { static OBSERVED: RefCell<Observations> = RefCell::new(Observations::default()); }
struct Observer {
    observed: Observations,
    snapshot: String,
    rect: vello::kurbo::Rect,
    focused: bool,
    motion_delivered: bool,
}
impl Observer {
    fn new(observed: Observations) -> Self {
        Self {
            observed,
            snapshot: String::new(),
            rect: vello::kurbo::Rect::ZERO,
            focused: false,
            motion_delivered: false,
        }
    }
}
impl Widget for Observer {
    crate::provides!(dyn crate::undo::UndoControl, dyn crate::undo::UndoPoints);

    fn set_motion_reduced(&mut self, _: bool) {
        if !std::mem::replace(&mut self.motion_delivered, true)
            && let Some(local) = self.observed.local.borrow().as_ref()
        {
            local.writer().set("initial callback".into());
        }
    }
    fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {
        self.observed
            .syncs
            .set(self.observed.syncs.get().saturating_add(1));
        if let Some(local) = self.observed.local.borrow().as_ref() {
            self.snapshot = local.with(Clone::clone);
        }
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree
            .new_leaf(Style {
                size: Size::from_lengths(80.0, 30.0),
                ..Default::default()
            })
            .unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(layout.size.width),
            origin.y + f64::from(layout.size.height),
        );
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event, .. } = event
            && self.focused
            && event.state == ElementState::Pressed
            && let Some(text) = event.text.as_deref()
        {
            let before = ctx
                .state
                .get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned();
            ctx.state
                .document_mut()
                .put(ROOT, "body", format!("{before}{text}"))
                .unwrap();
            *ctx.handled = true;
        }
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, value: bool) {
        self.focused = value;
    }
    fn name(&self) -> &'static str {
        "runner-observer"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}

impl crate::undo::UndoPoints for Observer {
    fn get_state(&self) -> Option<String> {
        Some(self.snapshot.clone())
    }
}

impl crate::undo::UndoControl for Observer {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> crate::undo::UndoStatus) {
        *self.observed.published.borrow_mut() = status();
    }
}
impl crate::widget::AppWidget for Observer {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Observer"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("O")
    }
}
fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| doc.put(ROOT, "body", "").map_err(|error| error.to_string()))
        .map_err(crate::launch::LaunchError::Record)?;
    let observed = OBSERVED.with_borrow(Clone::clone);
    *observed.local.borrow_mut() = Some(crate::LocalState::new("construction".into()));
    observed
        .local
        .borrow()
        .as_ref()
        .unwrap()
        .writer()
        .set("before mount".into());
    Ok(AppComponent::new(
        context,
        Box::new(Observer::new(observed)),
        tilo_actions::ActionRegistry::new(),
    ))
}
fn prepared() -> (
    ProbeHost,
    FrameHarness,
    Observations,
    crate::record::RecordCheckout,
) {
    let host = ProbeHost::new();
    let definition = definition(factory);
    let observed = Observations::default();
    OBSERVED.with_borrow_mut(|slot| *slot = observed.clone());
    let context = host
        .prepare_with_permissions(&definition, 11, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let checkout = context.record().clone();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let driver = FrameHarness::from_component(component, (320, 180), runner_inputs()).unwrap();
    (host, driver, observed, checkout)
}
#[test]
fn prepared_driver_publishes_undo_status_and_guards_wakes() {
    {
        let (host, mut driver, bound, checkout) = prepared();
        let outer = Observations::default();
        // Preserve the owned bound adapter as a child; this test adds ordinary host chrome.
        let component_root =
            std::mem::replace(&mut driver.root, Box::new(crate::widget::Column::new()));
        driver.root = Box::new(
            crate::widget::Column::new()
                .push(Box::new(Observer::new(outer.clone())))
                .push(component_root),
        );
        driver.state.doc.put(ROOT, "outer", true).unwrap();
        driver
            .state
            .commit_action_authored_with_context(None, None, Some(author()), None);
        driver
            .state
            .undo_as(Some(&author()), None, None, None)
            .unwrap();
        driver.relayout();
        let outer_baseline = driver.state.undo_status(Some(&author()));
        assert!(!outer_baseline.can_undo && outer_baseline.can_redo);
        let node = *driver
            .node_map
            .keys()
            .find(|node| driver.bindings.binding_ref(**node).is_some())
            .unwrap();
        driver.set_focused(Some(node));
        driver.type_text("first");
        let selected = driver.undo_status();
        assert!(selected.can_undo && !selected.can_redo);
        assert_eq!(*bound.published.borrow(), selected);
        assert_eq!(*outer.published.borrow(), outer_baseline);
        driver.type_text(" second");
        for _ in 0..3 {
            assert!(driver.undo_status().can_undo);
        }
        assert_eq!(body(&host.records, &checkout), "first second");
        assert!(driver.perform_undo());
        assert_eq!(body(&host.records, &checkout), "");
        assert!(!driver.undo_status().can_undo && driver.undo_status().can_redo);
        assert!(driver.perform_redo());
        assert_eq!(body(&host.records, &checkout), "first second");
        assert_eq!(*outer.published.borrow(), outer_baseline);
        assert_eq!(driver.state.undo_status(Some(&author())), outer_baseline);
        driver.set_focused(None);
        assert_eq!(driver.undo_status(), outer_baseline);
    }

    {
        let mut outer_scope = crate::WakeScope::new();
        let outer_wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = outer_wakes.clone();
        outer_scope
            .bind(move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .unwrap();
        let _outer = outer_scope.enter();
        let (_host, mut driver, observed, _) = prepared();
        assert_eq!(
            crate::undo::get_state(driver.root.as_ref()),
            Some("initial callback".into())
        );
        assert_eq!(outer_wakes.load(std::sync::atomic::Ordering::Relaxed), 0);
        let writer = observed.local.borrow().as_ref().unwrap().writer();
        let before = observed.syncs.get();
        writer.set("pending".into());
        let _ = driver.inspection();
        assert_eq!(
            observed.syncs.get(),
            before,
            "read boundary must not perform a frame"
        );
        let _ = driver.action_registry_mut();
        assert!(observed.syncs.get() > before);
        assert_eq!(
            crate::undo::get_state(driver.root.as_ref()),
            Some("pending".into())
        );
        writer.set("queued before close".into());
        driver.close_components();
        assert!(!writer.try_set("stale".into()));
        assert!(!driver.component_wake.as_ref().unwrap().dirty.get());
        let (_next, successor, next_observed, _) = prepared();
        let successor_syncs = next_observed.syncs.get();
        assert!(successor_syncs > 0, "initial composition was synchronized");
        assert!(!writer.try_set("late successor result".into()));
        assert_eq!(
            crate::undo::get_state(successor.root.as_ref()),
            Some("initial callback".into())
        );
        assert_eq!(
            next_observed.syncs.get(),
            successor_syncs,
            "stale wake must not synchronize the successor"
        );
        outer_scope.close();
    }
}

#[test]
fn duplicate_host_modifiers_do_not_sync_the_document_tree() {
    let (_host, mut driver, observed, _) = prepared();
    let before = observed.syncs.get();
    for _ in 0..100 {
        driver.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
    }
    assert_eq!(
        observed.syncs.get(),
        before,
        "unchanged modifiers must not relayout"
    );
    let shifted = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
    driver.send_host_event(&WindowEvent::ModifiersChanged(shifted));
    let changed = observed.syncs.get();
    assert!(
        changed > before,
        "real modifier transitions still reach widgets"
    );
    driver.send_host_event(&WindowEvent::ModifiersChanged(shifted));
    assert_eq!(observed.syncs.get(), changed);
    driver.send_host_event(&WindowEvent::ModifiersChanged(
        winit::event::Modifiers::default(),
    ));
    assert!(
        observed.syncs.get() > changed,
        "modifier release still reaches widgets"
    );
}
