//! Genuine mapped, mounted record owners. RED refuses unsafe reads after actual drop.
use super::*;
use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use crate::application_test_support::{self as probe, ProbeHost};
use crate::owner_wake::OwnerWakeQueue;
use crate::widget::{
    AppKind, AppWidget, BoundAppWidget, Column, IconDescriptor, TextInput, Widget,
};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Default)]
struct Seen {
    drops: Rc<Cell<usize>>,
    replace: Rc<Cell<bool>>,
    panic_after_replace: Rc<Cell<bool>>,
    preserve_focus: Rc<Cell<bool>>,
    panic_on_build: Rc<Cell<bool>>,
    mapped_before_panic: Rc<Cell<usize>>,
    fail_composition: Rc<Cell<bool>>,
    replace_in_apply: Rc<Cell<bool>>,
    events: Rc<Cell<usize>>,
    wake: Rc<RefCell<Option<crate::WakeHandle>>>,
}
struct Editor {
    field: TextInput,
    drops: Rc<Cell<usize>>,
}
impl Drop for Editor {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
impl Widget for Editor {
    crate::testing::delegate_widget_methods!(field: build_layout, apply_layout, draw, get_rect);
    fn name(&self) -> &'static str {
        "OwnedMappedEditor"
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.field.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.field]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.field]
    }
}
struct Owner {
    editor: Box<Editor>,
    seen: Seen,
    dirty: bool,
}
impl Owner {
    fn editor(seen: &Seen, body: &str) -> Box<Editor> {
        Box::new(Editor {
            drops: seen.drops.clone(),
            field: TextInput::local_with_language("en_US")
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_id("owned-editor")
                .with_accessible_name("Owned editor")
                .with_initial_value(body),
        })
    }
}
impl Widget for Owner {
    crate::testing::delegate_widget_methods!(editor: draw, get_rect);
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.editor.apply_layout(tree, node, origin);
        if self.seen.replace_in_apply.get() {
            let fresh = Self::editor(&self.seen, "retained draft");
            drop(std::mem::replace(&mut self.editor, fresh));
            self.dirty = true;
        }
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.editor.build_layout(tree, theme, nodes);
        if self.seen.panic_on_build.replace(false) {
            self.seen.mapped_before_panic.set(nodes.len());
            self.seen.replace.set(true);
            self.seen.panic_after_replace.set(true);
        }
        node
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.seen.events.set(self.seen.events.get() + 1);
        self.editor.handle_event(event, ctx);
    }
    fn name(&self) -> &'static str {
        "LayoutCustodyOwner"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.editor.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.editor.as_mut()]
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.seen.fail_composition.get() || std::mem::take(&mut self.dirty)
    }
    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, _) = crate::doc::document_ref(state, path);
        assert!(crate::record::identity(doc).is_some());
        if self.seen.replace.replace(false) {
            let body = doc
                .get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned();
            let focused = self.seen.preserve_focus.get() && self.editor.field.is_focused();
            let mut fresh = Self::editor(&self.seen, &body);
            fresh.field.set_focus(focused);
            // This genuinely frees the previously mapped TextInput allocation.
            // RED never reads the old raw pointer after observing this Drop.
            drop(std::mem::replace(&mut self.editor, fresh));
            self.dirty = true;
            assert!(
                !self.seen.panic_after_replace.replace(false),
                "owner failed after mapped child drop"
            );
        }
        self.editor.field.sync(doc, &ROOT);
    }
}
impl AppWidget for Owner {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Mapped owner custody"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("M")
    }
}
thread_local! { static NEXT: RefCell<Option<Seen>> = const { RefCell::new(None) }; }
fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "retained draft")
                .map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(crate::launch::LaunchError::Record)?;
    let seen = NEXT.with_borrow_mut(|next| next.take().unwrap());
    *seen.wake.borrow_mut() = Some(context.wake_handle());
    Ok(AppComponent::new(
        context,
        Box::new(Owner {
            editor: Owner::editor(&seen, "retained draft"),
            seen,
            dirty: false,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}
fn prepared(
    host: &ProbeHost,
    queue: &OwnerWakeQueue,
    id: u64,
) -> (BoundAppWidget, Seen, crate::record::RecordId) {
    let seen = Seen::default();
    NEXT.with_borrow_mut(|next| *next = Some(seen.clone()));
    let definition = probe::definition(factory);
    let context = host.prepare(&definition, id, &LaunchTarget::New).unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let id = component.record().id.clone();
    component.mount_with_owner_queue(queue, |_| {}).unwrap();
    let mut bound = BoundAppWidget::new(component);
    bound.sync(&AutoCommit::new(), &ROOT);
    (bound, seen, id)
}
fn frame(queue: &OwnerWakeQueue, owners: Vec<BoundAppWidget>) -> FrameHarness {
    let mut column = Column::new();
    for owner in owners {
        column = column.push(Box::new(owner));
    }
    let inputs = probe::runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(column),
        (500, 280),
        inputs.theme,
        inputs.text_renderer,
        probe::author(),
    );
    frame.attach_owner_wake_queue(queue.clone()).unwrap();
    frame.service_pending_owner_models().unwrap();
    frame.try_relayout().unwrap();
    assert!(
        !frame.layout_node_ids().is_empty(),
        "real descendant mapping is a setup requirement"
    );
    frame
}
fn close(frame: &mut FrameHarness, count: usize) {
    frame.close_components();
    let reports = frame.take_component_close_reports();
    assert_eq!(reports.len(), count);
    assert!(
        reports.iter().all(|r| r.record_error.is_none()
            && r.view_error.is_none()
            && r.teardown_error.is_none())
    );
}
#[test]
fn mapped_child_drop_retires_old_maps_before_failure_publish_or_next_original_idle_tick() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, id) = prepared(&host, &queue, 9100);
    let mut frame = frame(&queue, vec![owner]);
    frame
        .perform_named("owned-editor", &crate::a11y::Action::Focus)
        .unwrap();
    assert!(
        frame.ime_window().is_some(),
        "real mapped text owner must initially own IME"
    );
    let old_nodes = frame.layout_node_ids();
    let drops = seen.drops.get();
    let passes = frame.relayout_frames;
    host.records
        .with(&id, |doc| {
            doc.put(ROOT, "body", "current canonical replacement")
                .unwrap();
            doc.commit();
        })
        .unwrap();
    seen.replace.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    assert_eq!(
        seen.drops.get(),
        drops + 1,
        "old mapped control must genuinely drop"
    );
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    // RED exits safely before any raw focus/IME/undo inspection of freed memory.
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "RED: freed descendant still has admitted layout nodes {old_nodes:?}"
        );
        return;
    }
    assert_eq!(
        frame.relayout_frames, passes,
        "standalone model service itself cannot require layout or paint"
    );
    assert!(
        frame.ime_window().is_none(),
        "unadmitted retired physical target may not serve failure publication IME"
    );
    frame.idle_control_tick();
    frame.try_relayout().unwrap();
    let ime = frame.ime_window();
    let fresh = frame.named_target_nodes();
    close(&mut frame, 1);
    assert!(
        ime.is_none(),
        "a replacement with no live focus cannot resurrect old focus/caret"
    );
    assert!(fresh.contains_key("owned-editor"));
}
#[test]
fn replacement_then_unwind_keeps_maps_retired_and_requeues_the_actual_current_owner() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9101);
    let mut frame = frame(&queue, vec![owner]);
    let drops = seen.drops.get();
    seen.replace.set(true);
    seen.panic_after_replace.set(true);
    seen.wake.borrow().as_ref().unwrap().wake();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || frame.service_pending_owner_models()
        ))
        .is_err()
    );
    assert_eq!(seen.drops.get(), drops + 1);
    assert!(queue.has_pending());
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "RED: native unwind left freed descendant physically admitted"
        );
        return;
    }
    assert!(frame.ime_window().is_none());
    frame.idle_control_tick();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    frame.try_relayout().unwrap();
    close(&mut frame, 1);
}
#[test]
fn partial_batch_then_busy_canonical_loan_retires_maps_and_keeps_unvisited_owner_work() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (a, sa, _) = prepared(&host, &queue, 9102);
    let busy_host = ProbeHost::new();
    let (b, sb, record) = prepared(&busy_host, &queue, 9103);
    let mut frame = frame(&queue, vec![a, b]);
    let drops = sa.drops.get();
    sa.replace.set(true);
    sa.wake.borrow().as_ref().unwrap().wake();
    sb.wake.borrow().as_ref().unwrap().wake();
    busy_host
        .records
        .with_action_state(&record, |_, _, _| {
            assert!(matches!(
                frame.service_pending_owner_models(),
                Err(crate::bound_dispatch::BoundDispatchError::Record(
                    crate::record::RecordOpenError::Busy
                ))
            ));
        })
        .unwrap();
    assert_eq!(sa.drops.get(), drops + 1);
    assert!(queue.has_pending());
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    if !retired {
        close(&mut frame, 2);
        assert!(
            retired,
            "RED: partial success before Busy left dropped child in maps"
        );
        return;
    }
    assert!(frame.ime_window().is_none());
    frame.idle_control_tick();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 1);
    frame.try_relayout().unwrap();
    close(&mut frame, 2);
}
#[test]
fn unchanged_focused_editor_caret_and_ime_survive_fresh_input_readmission_without_paint_requirement()
 {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9104);
    let mut frame = frame(&queue, vec![owner]);
    frame
        .perform_named("owned-editor", &crate::a11y::Action::Focus)
        .unwrap();
    frame
        .perform_named(
            "owned-editor",
            &crate::a11y::Action::SetSelection {
                anchor: 2,
                focus: 6,
            },
        )
        .unwrap();
    let before = frame.ime_window();
    assert!(before.is_some());
    let drops = seen.drops.get();
    seen.wake.borrow().as_ref().unwrap().wake();
    frame.service_pending_owner_models().unwrap();
    let retired = frame.node_map.is_empty();
    if !retired {
        close(&mut frame, 1);
        assert!(retired, "RED: arbitrary service kept admitted pointer maps");
        return;
    }
    frame
        .perform_named(
            "owned-editor",
            &crate::a11y::Action::SetSelection {
                anchor: 2,
                focus: 6,
            },
        )
        .unwrap();
    let ime = frame.ime_window();
    let focus = frame.focused_access_node();
    close(&mut frame, 1);
    assert_eq!(
        seen.drops.get(),
        drops + 1,
        "only actual close may drop unchanged editor"
    );
    assert_eq!(ime, before);
    let focus = focus.expect("current keyboard owner restored from live retained controls");
    let selection = focus.text_selection.unwrap();
    assert_eq!(selection.anchor, 2);
    assert_eq!(selection.focus, 6);
    assert_eq!(focus.value.as_deref(), Some("retained draft"));
}
#[test]
fn empty_owner_queue_preserves_existing_physical_map_and_text_ime() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, _, _) = prepared(&host, &queue, 9105);
    let mut frame = frame(&queue, vec![owner]);
    frame
        .perform_named("owned-editor", &crate::a11y::Action::Focus)
        .unwrap();
    let nodes = frame.layout_node_ids();
    let ime = frame.ime_window();
    let focus = frame.focused();
    assert_eq!(frame.service_pending_owner_models().unwrap(), 0);
    assert_eq!(frame.layout_node_ids(), nodes);
    assert_eq!(frame.ime_window(), ime);
    assert_eq!(frame.focused(), focus);
    close(&mut frame, 1);
}

#[test]
fn readmission_second_sync_drop_then_panic_retires_the_newly_built_map_before_resuming_unwind() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9106);
    let mut frame = frame(&queue, vec![owner]);
    let drops = seen.drops.get();
    seen.wake.borrow().as_ref().unwrap().wake();
    frame.service_pending_owner_models().unwrap();
    // Do not execute readmission against a known dangling RED map.
    let retired = frame.node_map.is_empty();
    if !retired {
        close(&mut frame, 1);
        assert!(retired, "RED: service must retire before readmission");
        return;
    }
    seen.panic_on_build.set(true);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        frame.perform_named("owned-editor", &crate::a11y::Action::Focus)
    }));
    assert!(panic.is_err());
    assert!(
        seen.mapped_before_panic.get() > 0,
        "panic must follow genuine fresh mapping, not pre-build sync"
    );
    assert_eq!(
        seen.drops.get(),
        drops + 1,
        "freshly mapped boxed editor genuinely dropped in second sync"
    );
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    // Never dereference the freshly dangling RED/old-GREEN map.
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "RED: readmission unwind left its newly freed child mapped"
        );
        return;
    }
    assert!(frame.ime_window().is_none());
    frame.idle_control_tick();
    frame.try_relayout().unwrap();
    frame
        .perform_named("owned-editor", &crate::a11y::Action::Focus)
        .unwrap();
    assert!(frame.ime_window().is_some());
    close(&mut frame, 1);
}
#[test]
fn failed_current_composition_refuses_undo_redo_named_a11y_and_root_input_without_outer_fallback() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, record) = prepared(&host, &queue, 9107);
    let mut frame = frame(&queue, vec![owner]);
    frame
        .perform_named("owned-editor", &crate::a11y::Action::Focus)
        .unwrap();
    // Establish a genuine outer undo action that a lost owner binding could wrongly select.
    frame.state.doc.put(ROOT, "outer-only", "before").unwrap();
    frame.state.doc.commit();
    frame.state.doc.put(ROOT, "outer-only", "after").unwrap();
    assert!(
        frame
            .state
            .commit_action_authored(None, None, Some(frame.author.clone()))
    );
    assert_eq!(frame.state.undo_depth(), 1);
    let outer_before = frame.state.doc.base_heads();
    let inner_before = host.records.with(&record, |doc| doc.base_heads()).unwrap();
    seen.wake.borrow().as_ref().unwrap().wake();
    frame.service_pending_owner_models().unwrap();
    let retired = frame.node_map.is_empty();
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "RED: owner maps must retire before failed input admission"
        );
        return;
    }
    seen.fail_composition.set(true);
    assert!(matches!(
        frame.try_relayout(),
        Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
    ));
    let focus_before = frame.focused();
    let instance_before = frame.active_input_instance;
    let activation = frame.check_app_activation();
    frame.prepare_app_activation(9999);
    frame.activate_app_focus(9999);
    let focus_after = frame.focused();
    let instance_after = frame.active_input_instance;
    let undo = frame.perform_undo();
    let redo = frame.perform_redo();
    let named = frame.perform_named(
        "owned-editor",
        &crate::a11y::Action::SetValue("refused".into()),
    );
    let semantic = frame.perform_semantic_named("owned-editor", None, &crate::a11y::Action::Focus);
    let a11y = frame.perform_a11y_action(accesskit::NodeId(1), &crate::a11y::Action::Focus);
    let root = frame.dispatch_to_root(&WindowEvent::Ime(winit::event::Ime::Commit(
        "refused".into(),
    )));
    frame.commit_text("refused");
    frame.idle_control_tick();
    let outer_after = frame.state.doc.base_heads();
    let depth = frame.state.undo_depth();
    let inner_after = host.records.with(&record, |doc| doc.base_heads()).unwrap();
    let events = seen.events.get();
    let retired_after = frame.node_map.is_empty();
    seen.fail_composition.set(false);
    frame.try_relayout().unwrap();
    frame
        .perform_named(
            "owned-editor",
            &crate::a11y::Action::SetSelection {
                anchor: 1,
                focus: 4,
            },
        )
        .unwrap();
    let ime = frame.ime_window();
    frame.commit_text("!");
    let resumed = frame.focused_access_node().and_then(|node| node.value);
    close(&mut frame, 1);
    assert!(activation.unwrap_err().contains("readmission"));
    assert_eq!(focus_after, focus_before);
    assert_eq!(instance_after, instance_before);
    assert!(
        !undo && !redo,
        "failed owner admission cannot select outer undo/redo fallback"
    );
    assert_eq!(outer_after, outer_before);
    assert_eq!(depth, 1);
    assert_eq!(inner_after, inner_before);
    for result in [named, semantic, a11y] {
        assert!(
            result.unwrap_err().contains("CompositionDidNotSettle"),
            "Result ingress must report original composition refusal"
        );
    }
    assert!(!root);
    assert_eq!(events, 0);
    assert!(retired_after);
    assert!(
        ime.is_some(),
        "successful fresh readmission restores the live logical text owner"
    );
    assert!(
        resumed.as_deref().is_some_and(|value| value.contains('!')),
        "same original text ingress resumes after fresh admission"
    );
}

#[test]
fn demand_only_fresh_map_second_sync_unwind_retires_before_input_retry() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9120);
    let mut frame = frame(&queue, vec![owner]);
    assert!(
        !queue.has_pending(),
        "no owner wake may stand in for layout demand"
    );
    let drops = seen.drops.get();
    seen.panic_on_build.set(true);
    frame.request_layout();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frame.try_relayout()));
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    let pending = frame.frame_schedule().queued_work;
    // Never inspect a mapped widget until retirement has been proven.
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "demand-only second-sync unwind must retire newly built maps"
        );
        return;
    }
    let focused = frame.perform_named("owned-editor", &crate::a11y::Action::Focus);
    let admitted = !frame.node_map.is_empty() && frame.ime_window().is_some();
    close(&mut frame, 1);
    assert!(failed.is_err());
    assert!(seen.mapped_before_panic.get() > 0);
    assert!(seen.drops.get() > drops);
    assert!(pending && focused.is_ok() && admitted);
}

#[test]
fn demand_only_truthful_apply_replacement_error_retires_before_original_input() {
    let host = ProbeHost::new();
    let queue = OwnerWakeQueue::default();
    let (owner, seen, _) = prepared(&host, &queue, 9121);
    let mut frame = frame(&queue, vec![owner]);
    assert!(!queue.has_pending());
    let drops = seen.drops.get();
    seen.replace_in_apply.set(true);
    frame.request_layout();
    let failed = frame.try_relayout();
    let retired = frame.node_map.is_empty() && !frame.layout_built;
    let pending = frame.frame_schedule().queued_work;
    if !retired {
        close(&mut frame, 1);
        assert!(
            retired,
            "last-pass apply replacement error must retire partial mapped tree"
        );
        return;
    }
    let refused = frame.perform_named("owned-editor", &crate::a11y::Action::Focus);
    assert!(
        frame.node_map.is_empty(),
        "failed input readmission cannot expose partial maps"
    );
    seen.replace_in_apply.set(false);
    let resumed = frame.perform_named("owned-editor", &crate::a11y::Action::Focus);
    let admitted = !frame.node_map.is_empty() && frame.ime_window().is_some();
    close(&mut frame, 1);
    assert!(matches!(
        failed,
        Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
    ));
    assert!(seen.drops.get() > drops);
    assert!(pending && refused.is_err() && resumed.is_ok() && admitted);
}
