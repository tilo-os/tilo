//! Original checked activation: actual Scene ink versus final focus and IME.
//! TEST-only. No production ordering or presentation-credit contract is changed.
//! The marker is real probe Widget.draw ink in the encoded Scene, not raster
//! pixels, a screenshot, physical display presentation, or a GPU receipt.
use super::*;
use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use crate::application_test_support::{self as probe, ProbeHost};
use crate::owner_wake::OwnerWakeQueue;
use crate::view::{ViewContribution, capability::ViewState};
use crate::widget::{AppKind, AppWidget, BoundAppWidget, Column, IconDescriptor, TextInput};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const OLD: u64 = 95201;
const SELECTED: u64 = 95202;
const ABSENT: u64 = 95299;

#[derive(Clone, Default)]
struct Seen {
    restore_from_sync: Rc<Cell<bool>>,
    restored_syncs: Rc<Cell<usize>>,
    draws: Rc<Cell<usize>>,
    draw_focus: Rc<Cell<bool>>,
    draw_ime: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<&'static str>>>,
    panic_next_sync: Rc<Cell<bool>>,
    replace_on_blur: Rc<Cell<bool>>,
    replacements: Rc<Cell<usize>>,
    structure_dirty: Rc<Cell<bool>>,
    dirty_blur_after_sync: Rc<Cell<bool>>,
    dirty_each_blur: Rc<Cell<bool>>,
    dirty_blurs: Rc<Cell<usize>>,
    panic_blur_after_sync: Rc<Cell<bool>>,
    panic_blur: Rc<Cell<bool>>,
    panic_next_draw: Rc<Cell<bool>>,
    refocus_after_draw: Rc<Cell<bool>>,
    blur_after_draw: Rc<Cell<bool>>,
}
struct Owner {
    field: Box<TextInput>,
    seen: Seen,
    id: u64,
}
fn marker(id: u64, focused: bool) -> vello::peniko::Color {
    match (id == OLD, focused) {
        (true, true) => vello::peniko::Color::from_rgb8(0xf1, 0x0d, 0xa7),
        (true, false) => vello::peniko::Color::from_rgb8(0x05, 0xad, 0xbc),
        (false, true) => vello::peniko::Color::from_rgb8(0x23, 0xca, 0x39),
        (false, false) => vello::peniko::Color::from_rgb8(0x14, 0x19, 0xd2),
    }
}
// Draw data offsets come from the scene stream, not the separate info buffer.
fn scene_has(scene: &Scene, color: vello::peniko::Color) -> bool {
    use vello_encoding::{DrawMonoid, DrawTag, Monoid};
    let expected = vello_encoding::DrawColor::from(color).rgba;
    let encoding = scene.encoding();
    let mut offset = 0;
    let mut found = false;
    for tag in &encoding.draw_tags {
        if !matches!(
            *tag,
            DrawTag::NOP
                | DrawTag::COLOR
                | DrawTag::LINEAR_GRADIENT
                | DrawTag::RADIAL_GRADIENT
                | DrawTag::SWEEP_GRADIENT
                | DrawTag::IMAGE
                | DrawTag::BLUR_RECT
                | DrawTag::BEGIN_CLIP
                | DrawTag::END_CLIP
        ) {
            return false;
        }
        let words = usize::try_from(DrawMonoid::new(*tag).scene_offset).unwrap();
        let Some(data) = encoding.draw_data.get(offset..offset + words) else {
            return false;
        };
        found |= *tag == DrawTag::COLOR && data.first() == Some(&expected);
        offset += words;
    }
    found && offset == encoding.draw_data.len()
}
#[test]
fn encoded_scene_color_reader_uses_scene_offsets_after_mixed_draws() {
    use vello_encoding::DrawTag;
    let color = marker(SELECTED, true);
    let mut scene = Scene::new();
    let encoding = scene.encoding_mut();
    encoding.draw_tags = vec![
        DrawTag::BEGIN_CLIP,
        DrawTag::LINEAR_GRADIENT,
        DrawTag::RADIAL_GRADIENT,
        DrawTag::SWEEP_GRADIENT,
        DrawTag::IMAGE,
        DrawTag::BLUR_RECT,
        DrawTag::END_CLIP,
        DrawTag::COLOR,
    ];
    encoding.draw_data = vec![0; 2 + 5 + 7 + 5 + 3 + 5];
    encoding
        .draw_data
        .push(vello_encoding::DrawColor::from(color).rgba);
    assert!(scene_has(&scene, color));
    assert!(!scene_has(&scene, marker(SELECTED, false)));
    scene.encoding_mut().draw_data.pop();
    assert!(!scene_has(&scene, color), "short stream must refuse");
    scene.encoding_mut().draw_tags[0] = DrawTag(u32::MAX);
    assert!(!scene_has(&scene, color), "unknown tag must refuse");
}
// Test wrappers expose one child, so build one corresponding parent node.
fn apply_wrapper(child: &mut dyn Widget, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
    let child_node = tree.children(node).unwrap()[0];
    child.apply_layout(
        tree,
        child_node,
        crate::widget::layout_rect(tree, node, origin).origin(),
    );
}
impl Widget for Owner {
    crate::testing::delegate_widget_methods!(field: get_rect);
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.field.build_layout(tree, theme, nodes);
        let node = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Style::default()
            },
            &[child],
        );
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        apply_wrapper(self.field.as_mut(), tree, node, origin);
    }
    fn name(&self) -> &'static str {
        "ActivationSceneOwner"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.field.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.field.as_mut()]
    }
    fn set_focus(&mut self, focused: bool) {
        self.field.set_focus(focused);
        if !focused {
            assert!(
                !self.seen.panic_blur.replace(false),
                "activation setter fixture unwind"
            );
            if self.seen.replace_on_blur.replace(false) {
                // Replace the ACTUAL mapped heap allocation. The truthful
                // structure flag must retire its pointer before another visit.
                let replacement = Box::new(
                    TextInput::local_with_language("en_US")
                        .with_spellcheck(false)
                        .with_autocorrect(false)
                        .with_id(if self.id == OLD {
                            "activation-old"
                        } else {
                            "activation-selected"
                        })
                        .with_initial_value("retained body"),
                );
                // Free the old pointee, rather than replacing its value in place:
                // this control exercises retirement of actual mapped storage.
                drop(std::mem::replace(&mut self.field, replacement));
                self.seen.replacements.set(self.seen.replacements.get() + 1);
                self.seen.structure_dirty.set(true);
            }
            if self.seen.dirty_each_blur.get() {
                self.seen.dirty_blurs.set(self.seen.dirty_blurs.get() + 1);
                self.seen.structure_dirty.set(true);
            }
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        let child = self.field.take_structure_dirty();
        self.seen.structure_dirty.replace(false) || child
    }
    fn sync(&mut self, model: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        let (doc, root) = crate::doc::document_ref(model, path);
        assert!(
            crate::record::identity(doc).is_some(),
            "original canonical owner admission"
        );
        assert_eq!(
            doc.get(ROOT, "body").unwrap().unwrap().0.as_str(),
            Some("retained body")
        );
        self.seen.events.borrow_mut().push("sync");
        assert!(
            !self.seen.panic_next_sync.replace(false),
            "activation sync fixture unwind"
        );
        self.field.sync(doc, root);
        if self.seen.restore_from_sync.get() {
            // Legal opaque local-state restoration in the ORIGINAL sync callback.
            self.field.set_focus(true);
            self.seen
                .restored_syncs
                .set(self.seen.restored_syncs.get() + 1);
            self.seen.events.borrow_mut().push("restore_sync");
        }
        if self.seen.dirty_blur_after_sync.get() {
            self.seen.dirty_each_blur.set(true);
        }
        if self.seen.panic_blur_after_sync.replace(false) {
            self.seen.panic_blur.set(true);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let focused = self.field.is_focused();
        self.seen.draw_focus.set(focused);
        self.seen
            .draw_ime
            .set(self.field.ime_cursor_rect().is_some());
        self.seen.draws.set(self.seen.draws.get() + 1);
        self.seen.events.borrow_mut().push("draw");
        self.field.draw(scene, transform, text, theme, presence);
        let rect = self.field.get_rect();
        assert!(
            rect.width() >= 8.0 && rect.height() >= 8.0,
            "actual visible field allocation"
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            marker(self.id, focused),
            None,
            &Rect::new(rect.x0 + 1.0, rect.y0 + 1.0, rect.x0 + 7.0, rect.y0 + 7.0),
        );
        assert!(
            !self.seen.panic_next_draw.replace(false),
            "activation draw fixture unwind"
        );
        if self.seen.refocus_after_draw.get() {
            self.field.set_focus(true);
        }
        if self.seen.blur_after_draw.get() {
            self.field.set_focus(false);
        }
    }
}
impl AppWidget for Owner {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Activation scene fixture"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}
thread_local! { static NEXT: RefCell<Option<(Seen, u64, &'static str)>> = const { RefCell::new(None) }; }
fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "retained body")
                .map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(crate::launch::LaunchError::Record)?;
    let (seen, id, name) = NEXT.with_borrow_mut(|next| next.take().unwrap());
    Ok(AppComponent::new(
        context,
        Box::new(Owner {
            field: Box::new(
                TextInput::local_with_language("en_US")
                    .with_spellcheck(false)
                    .with_autocorrect(false)
                    .with_id(name)
                    .with_initial_value("retained body"),
            ),
            seen,
            id,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}
fn prepared(
    host: &ProbeHost,
    queue: &OwnerWakeQueue,
    id: u64,
    name: &'static str,
) -> (BoundAppWidget, Seen) {
    let seen = Seen::default();
    NEXT.with_borrow_mut(|next| *next = Some((seen.clone(), id, name)));
    let definition = probe::definition(factory);
    let context = host.prepare(&definition, id, &LaunchTarget::New).unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.mount_with_owner_queue(queue, |_| {}).unwrap();
    (BoundAppWidget::new(component), seen)
}
#[derive(Clone, Default)]
struct Tail {
    restore_view: Rc<Cell<bool>>,
    view_restores: Rc<Cell<usize>>,
    fail_composition: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<&'static str>>>,
}
struct Pair {
    column: Column,
    tail: Tail,
}
impl Widget for Pair {
    crate::provides!(dyn ViewState);
    crate::testing::delegate_widget_methods!(column: draw, get_rect);
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.column.build_layout(tree, theme, nodes);
        let node = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Style::default()
            },
            &[child],
        );
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        apply_wrapper(&mut self.column, tree, node, origin);
    }
    fn name(&self) -> &'static str {
        "ActivationScenePair"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.column]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.column]
    }
    fn take_structure_dirty(&mut self) -> bool {
        let child = self.column.take_structure_dirty();
        self.tail.fail_composition.get() || child
    }
}
impl ViewState for Pair {
    fn view_state(&self) -> Option<ViewContribution> {
        Some(ViewContribution::default())
    }
    fn restore_view_state(&mut self, _: &ViewContribution) {
        if self.tail.restore_view.get() {
            self.tail
                .view_restores
                .set(self.tail.view_restores.get() + 1);
            self.tail.events.borrow_mut().push("restore_view");
            // Current owned reference through original BoundAppWidget.set_focus
            // admission; no saved raw alias, document loan, or forced map.
            self.column.children_mut()[0].set_focus(true);
        }
    }
}
struct Fixture {
    frame: FrameHarness,
    _host: ProbeHost,
    old: Seen,
    selected: Seen,
    tail: Tail,
}
impl Fixture {
    fn new() -> Self {
        Self::with_history(false)
    }
    fn with_history(managed_history: bool) -> Self {
        let host = ProbeHost::new();
        let queue = OwnerWakeQueue::default();
        let (old, old_seen) = prepared(&host, &queue, OLD, "activation-old");
        let (selected, selected_seen) = prepared(&host, &queue, SELECTED, "activation-selected");
        let tail = Tail {
            events: old_seen.events.clone(),
            ..Tail::default()
        };
        let history_sources = managed_history.then(|| {
            vec![
                crate::history_mode::HistorySourceRegistration::owned(
                    "old",
                    probe::APP,
                    crate::history_mode::HistoryOwner::component(old.component.callback_binding()),
                )
                .unwrap(),
                crate::history_mode::HistorySourceRegistration::owned(
                    "selected",
                    probe::APP,
                    crate::history_mode::HistoryOwner::component(
                        selected.component.callback_binding(),
                    ),
                )
                .unwrap(),
            ]
        });
        let root = Pair {
            column: Column::new().push(Box::new(old)).push(Box::new(selected)),
            tail: tail.clone(),
        };
        // Box all mapped owners before construction. Only owning boxes move.
        let inputs = probe::runner_inputs();
        let mut frame = if managed_history {
            let registration = crate::history_mode::HistoryRegistration::new(
                &COMPONENT_HISTORY,
                history_sources.unwrap(),
            )
            .unwrap();
            let mut state = State::new();
            state
                .doc
                .put(ROOT, "body", "history owner composition")
                .unwrap();
            state.commit_action(None, None);
            FrameHarness::from_managed_root_with_controlled_inputs(
                state,
                crate::history_mode::ManagedRoot::new(Box::new(root), registration),
                (500, 280),
                inputs.theme,
                inputs.text_renderer,
                probe::author(),
            )
            .unwrap()
        } else {
            FrameHarness::new_with_controlled_inputs(
                State::new(),
                Box::new(root),
                (500, 280),
                inputs.theme,
                inputs.text_renderer,
                probe::author(),
            )
        };
        frame.attach_owner_wake_queue(queue).unwrap();
        frame.service_pending_owner_models().unwrap();
        frame.try_relayout().unwrap();
        frame.prepare_app_activation(OLD);
        frame.activate_app_focus(OLD);
        let old_node = frame.named_target_nodes()["activation-old"];
        assert!(frame.inspect_node(old_node, |w| w.is_focused()).unwrap());
        assert!(
            frame.ime_window().is_some(),
            "original mapped TextInput owns IME"
        );
        Self {
            frame,
            _host: host,
            old: old_seen,
            selected: selected_seen,
            tail,
        }
    }
    fn close(&mut self) {
        self.tail.fail_composition.set(false);
        self.old.panic_next_sync.set(false);
        self.frame.close_components();
        let reports = self.frame.take_component_close_reports();
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| r.record_error.is_none()
            && r.view_error.is_none()
            && r.teardown_error.is_none()));
    }
}
fn assert_consistent_return(f: &mut Fixture, instance: u64) {
    let old_node = f.frame.named_target_nodes()["activation-old"];
    let final_focus = f.frame.inspect_node(old_node, |w| w.is_focused()).unwrap();
    let final_ime = f
        .frame
        .inspect_node(old_node, |w| w.ime_cursor_rect().is_some())
        .unwrap();
    let caret = f
        .frame
        .inspect_node(old_node, |w| crate::undo::get_state(w))
        .unwrap();
    let scene = f.frame.frame_render_input().scene;
    let focus_ink = scene_has(scene, marker(OLD, true));
    let blur_ink = scene_has(scene, marker(OLD, false));
    assert_ne!(
        focus_ink, blur_ink,
        "actual encoded Scene has exactly one old-owner marker"
    );
    assert_eq!(
        focus_ink,
        f.old.draw_focus.get(),
        "encoding agrees with actual draw-time observation"
    );
    assert!(
        !final_focus && !final_ime,
        "original postdraw blur cleared actual outgoing focus/IME"
    );
    assert!(
        caret.is_some(),
        "blur preserves actual TextInput caret state"
    );
    assert_eq!(f.frame.active_input_instance, Some(instance));
    if instance == SELECTED {
        let node = f.frame.focused().unwrap();
        assert_eq!(
            f.frame.bindings.binding_for(node).unwrap().instance_id,
            SELECTED
        );
        assert!(
            f.frame.ime_window().is_some(),
            "selected actual text owner owns IME"
        );
    } else {
        assert!(f.frame.focused().is_none() && f.frame.ime_window().is_none());
    }
    {
        let events = f.old.events.borrow();
        let restore = events
            .iter()
            .rposition(|event| matches!(*event, "restore_sync" | "restore_view"))
            .unwrap();
        let draw = events.iter().rposition(|event| *event == "draw").unwrap();
        assert!(
            restore < draw,
            "original restoration really precedes the last actual draw: {events:?}"
        );
    }
    // Sample all live observations BEFORE cleanup/lifetime revocation.
    let drawn_focus = f.old.draw_focus.get();
    let drawn_ime = f.old.draw_ime.get();
    f.close();
    assert_eq!(
        drawn_focus, final_focus,
        "returned Scene focus predates original postdraw blur"
    );
    assert_eq!(
        focus_ink, final_focus,
        "encoded ink and final focus must agree on return"
    );
    assert_eq!(
        drawn_ime, final_ime,
        "draw-time caret/IME state must agree with final field state"
    );
}
#[test]
fn sync_restored_outgoing_owner_scene_matches_selected_activation_return() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    let restores = f.old.restored_syncs.get();
    f.old.restore_from_sync.set(true);
    assert!(f.frame.check_app_activation().is_ok());
    f.frame.activate_app_focus(SELECTED);
    assert!(
        f.old.restored_syncs.get() > restores,
        "original final sync really restored local focus"
    );
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn sync_restored_outgoing_owner_scene_matches_absent_target_return() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(ABSENT);
    f.old.restore_from_sync.set(true);
    f.frame.activate_app_focus(ABSENT);
    assert!(f.old.restored_syncs.get() > 0);
    assert_consistent_return(&mut f, ABSENT);
}
#[test]
fn original_view_restoration_scene_matches_selected_activation_return() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.tail.restore_view.set(true);
    let restores = f.tail.view_restores.get();
    f.frame.request_layout();
    f.frame.activate_app_focus(SELECTED);
    assert!(
        f.tail.view_restores.get() > restores,
        "original build path restored actual ViewState"
    );
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn early_blur_is_undone_by_original_sync_control() {
    let mut f = Fixture::new();
    f.old.restore_from_sync.set(true);
    let node = f.frame.named_target_nodes()["activation-old"];
    f.frame.mutate_node(node, |w| w.set_focus(false)).unwrap();
    assert!(!f.frame.inspect_node(node, |w| w.is_focused()).unwrap());
    f.frame.try_relayout().unwrap();
    let node = f.frame.named_target_nodes()["activation-old"];
    assert!(f.frame.inspect_node(node, |w| w.is_focused()).unwrap());
    assert!(f.old.draw_focus.get());
    assert!(scene_has(
        f.frame.frame_render_input().scene,
        marker(OLD, true)
    ));
    f.close();
}
#[test]
fn ordinary_completed_frame_matches_focus_and_real_ink_control() {
    let mut f = Fixture::new();
    f.old.restore_from_sync.set(true);
    f.frame.try_relayout().unwrap();
    let node = f.frame.named_target_nodes()["activation-old"];
    assert!(f.frame.inspect_node(node, |w| w.is_focused()).unwrap());
    assert!(f.old.draw_focus.get() && f.old.draw_ime.get());
    assert!(scene_has(
        f.frame.frame_render_input().scene,
        marker(OLD, true)
    ));
    f.close();
}
#[test]
fn closed_destination_is_not_restored_as_a_keyboard_owner_control() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    let node = f.frame.named_target_nodes()["activation-selected"];
    let closing = f.frame.bindings.binding_for(node).unwrap().closing.clone();
    let draws = f.selected.draws.get();
    closing.set(true); // original live binding gate; allocation remains owned
    f.frame.activate_app_focus(SELECTED);
    assert!(f.frame.focused().is_none());
    assert!(f.frame.ime_window().is_none());
    assert_eq!(
        f.selected.draws.get(),
        draws,
        "closed adapter cannot invoke destination draw"
    );
    closing.set(false);
    f.close();
}
#[test]
fn failed_and_unwound_composition_refuse_activation_before_target_callbacks_control() {
    for unwind in [false, true] {
        let mut f = Fixture::new();
        f.frame.request_layout();
        if unwind {
            f.old.panic_next_sync.set(true);
        } else {
            f.tail.fail_composition.set(true);
        }
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.frame.try_relayout()));
        if unwind {
            assert!(result.is_err());
        } else {
            assert!(matches!(
                result.unwrap(),
                Err(crate::widget::WidgetLayoutError::CompositionDidNotSettle)
            ));
        }
        assert!(
            f.frame.node_map.is_empty()
                && f.frame.bindings.binding_for(f.frame.child_node).is_none()
        );
        assert!(f.frame.owner_service_layout_retired && f.frame.layout_requested);
        let draws = f.selected.draws.get();
        let instance = f.frame.active_input_instance;
        // Keep the genuine refusal live while calling original activation.
        if unwind {
            f.old.panic_next_sync.set(true);
        }
        let activation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f.frame.activate_app_focus(SELECTED)
        }));
        if unwind {
            assert!(activation.is_err());
        } else {
            assert!(activation.is_ok());
        }
        assert_eq!(f.frame.active_input_instance, instance);
        assert_eq!(f.selected.draws.get(), draws);
        assert!(f.frame.node_map.is_empty() && f.frame.layout_requested);
        f.tail.fail_composition.set(false);
        f.old.panic_next_sync.set(false);
        f.frame.try_relayout().unwrap();
        assert!(!f.frame.layout_requested);
        f.close();
    }
}
static COMPONENT_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: probe::APP,
        scopes: &[
            crate::history_mode::HistoryScopeDefinition {
                key: "old",
                title: "Old owner",
                presentation: crate::history_mode::HistoryPresentation::Domain {
                    create: crate::history_mode::test_preview,
                },
                content_schema: None,
            },
            crate::history_mode::HistoryScopeDefinition {
                key: "selected",
                title: "Selected owner",
                presentation: crate::history_mode::HistoryPresentation::Domain {
                    create: crate::history_mode::test_preview,
                },
                content_schema: None,
            },
        ],
    };
static HISTORY: crate::history_mode::HistoryDefinition = crate::history_mode::HistoryDefinition {
    app_id: "dev.tilo.controlled-history-fixture",
    scopes: &[crate::history_mode::HistoryScopeDefinition {
        key: "document",
        title: "Activation history control",
        presentation: crate::history_mode::HistoryPresentation::Domain {
            create: crate::history_mode::test_preview,
        },
        content_schema: None,
    }],
};
#[test]
fn original_history_preflight_preserves_suspended_live_caret_control() {
    let registration = crate::history_mode::HistoryRegistration::new(
        &HISTORY,
        vec![
            crate::history_mode::HistorySourceRegistration::HostDocument {
                scope: "document",
                resource: crate::history_mode::HistoryResource::new(
                    "controlled-profile",
                    "controlled-workspace",
                    HISTORY.app_id,
                    crate::RecordId::for_tab(95210),
                ),
            },
        ],
    )
    .unwrap();
    let mut state = State::new();
    state.doc.put(ROOT, "body", "history body").unwrap();
    state.commit_action(None, None);
    let mut frame = FrameHarness::from_managed_root_with_controlled_inputs(
        state,
        crate::history_mode::ManagedRoot::new(
            Box::new(
                Column::new().push(Box::new(
                    TextInput::local()
                        .with_id("history-live-field")
                        .with_initial_value("unfinished"),
                )),
            ),
            registration,
        ),
        (900, 650),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        probe::author(),
    )
    .unwrap();
    frame
        .perform_named("history-live-field", &crate::a11y::Action::Focus)
        .unwrap();
    frame.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
        "composition".into(),
        Some((11, 11)),
    )));
    let node = frame.named_target_nodes()["history-live-field"];
    let caret = frame
        .inspect_node(node, |w| crate::undo::get_state(w))
        .unwrap();
    frame
        .perform_named("history-open", &crate::a11y::Action::Activate)
        .unwrap();
    assert!(frame.history.as_ref().unwrap().captures_input());
    let input_sequence = frame.input_sequence.get();
    let before = frame.relayout_frames();
    // Same preflight gate used by DesktopHandle.navigate; do not bypass History
    // by calling activation after a refusal.
    assert!(frame.check_app_activation().is_err());
    assert_eq!(frame.input_sequence.get(), input_sequence);
    assert_eq!(frame.relayout_frames(), before);
    frame
        .perform_named("history-open", &crate::a11y::Action::Activate)
        .unwrap();
    let node = frame.named_target_nodes()["history-live-field"];
    assert_eq!(
        frame
            .inspect_node(node, |w| crate::undo::get_state(w))
            .unwrap(),
        caret
    );
    assert!(frame.ime_window().is_some());
    frame.close_components();
    assert!(frame.take_component_close_reports().is_empty());
}

// These are candidate correctness fences. Replacement tests run only with the
// immediate retirement guard: the old implementation kept freed node pointers.
fn assert_retired_activation(f: &Fixture) {
    assert!(f.frame.node_map.is_empty());
    assert!(f.frame.owner_service_layout_retired && f.frame.layout_requested);
    assert_eq!(f.frame.pending_app_activation, Some(SELECTED));
    assert!(f.frame.last_layout_viewport.is_none());
    assert!(
        f.frame.presence.viewport.get().is_none(),
        "temporary draw viewport is restored"
    );
    assert!(
        f.frame
            .frame_render_input()
            .scene
            .encoding()
            .draw_tags
            .is_empty(),
        "no partial attempted Scene survives activation refusal"
    );
}
#[test]
fn setter_replacement_rebuilds_before_another_mapped_target() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.old.replace_on_blur.set(true);
    f.frame.activate_app_focus(SELECTED);
    assert_eq!(f.old.replacements.get(), 1);
    assert!(f.frame.last_relayout.rebuilt);
    assert!(f.frame.pending_app_activation.is_none());
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn setter_topology_oscillation_refuses_with_existing_eight_bound() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.old.dirty_blur_after_sync.set(true);
    f.frame.activate_app_focus(SELECTED);
    assert_eq!(
        f.old.dirty_blurs.get(),
        8,
        "original eight-pass settlement bound"
    );
    assert_retired_activation(&f);
    f.old.dirty_blur_after_sync.set(false);
    f.old.dirty_each_blur.set(false);
    f.frame.try_relayout().unwrap();
    assert!(f.frame.pending_app_activation.is_none());
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn outgoing_setter_unwind_retires_maps_and_preserves_retry() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.old.panic_blur_after_sync.set(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.frame.activate_app_focus(SELECTED);
    }));
    assert!(result.is_err());
    assert_retired_activation(&f);
    f.frame.try_relayout().unwrap();
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn draw_unwind_does_not_publish_partial_activation() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.old.panic_next_draw.set(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.frame.activate_app_focus(SELECTED);
    }));
    assert!(result.is_err());
    assert_retired_activation(&f);
    f.frame.try_relayout().unwrap();
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn late_draw_refocus_is_refused_and_fresh_retry_settles() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.old.refocus_after_draw.set(true);
    f.frame.activate_app_focus(SELECTED);
    assert!(
        !f.old.draw_focus.get(),
        "actual ink was emitted before late refocus"
    );
    assert_retired_activation(&f);
    f.old.refocus_after_draw.set(false);
    f.frame.try_relayout().unwrap();
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn new_selection_and_terminal_close_cancel_prior_retry_metadata() {
    for terminal_close in [false, true] {
        let mut f = Fixture::new();
        f.frame.prepare_app_activation(SELECTED);
        f.old.restore_from_sync.set(true);
        f.old.refocus_after_draw.set(true);
        f.frame.activate_app_focus(SELECTED);
        assert_retired_activation(&f);
        f.old.refocus_after_draw.set(false);
        if terminal_close {
            f.close();
            assert!(f.frame.pending_app_activation.is_none());
            assert!(f.frame.node_map.is_empty());
        } else {
            f.frame.prepare_app_activation(ABSENT);
            f.frame.activate_app_focus(ABSENT);
            assert!(f.frame.pending_app_activation.is_none());
            assert_consistent_return(&mut f, ABSENT);
        }
    }
}
#[test]
fn closed_outgoing_binding_is_never_reopened_by_retry() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    let node = f.frame.named_target_nodes()["activation-old"];
    let closing = f.frame.bindings.binding_for(node).unwrap().closing.clone();
    let draws = f.old.draws.get();
    let syncs = f.old.restored_syncs.get();
    f.old.restore_from_sync.set(true);
    closing.set(true);
    f.frame.activate_app_focus(SELECTED);
    assert_eq!(f.old.draws.get(), draws);
    assert_eq!(f.old.restored_syncs.get(), syncs);
    assert!(f.frame.pending_app_activation.is_none());
    assert_eq!(f.frame.active_input_instance, Some(SELECTED));
    closing.set(false);
    f.close();
}

#[test]
fn selected_late_blur_is_refused_before_frame_is_accepted() {
    let mut f = Fixture::new();
    f.frame.prepare_app_activation(SELECTED);
    f.old.restore_from_sync.set(true);
    f.selected.restore_from_sync.set(true);
    f.selected.blur_after_draw.set(true);
    f.frame.activate_app_focus(SELECTED);
    assert!(
        f.selected.draw_focus.get(),
        "selected ink was focused before late blur"
    );
    assert_retired_activation(&f);
    f.selected.blur_after_draw.set(false);
    f.frame.try_relayout().unwrap();
    let node = f.frame.named_target_nodes()["activation-selected"];
    assert!(
        f.frame
            .inspect_node(node, |widget| widget.is_focused())
            .unwrap()
    );
    assert!(f.frame.ime_window().is_some());
    assert!(scene_has(
        f.frame.frame_render_input().scene,
        marker(SELECTED, true)
    ));
    assert!(!scene_has(
        f.frame.frame_render_input().scene,
        marker(SELECTED, false)
    ));
    assert_consistent_return(&mut f, SELECTED);
}
#[test]
fn original_history_return_completes_with_activation_intent_and_matching_live_ink() {
    let mut f = Fixture::with_history(true);
    f.frame.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
        "unfinished".into(),
        Some((10, 10)),
    )));
    let old_node = f.frame.named_target_nodes()["activation-old"];
    let caret = f
        .frame
        .inspect_node(old_node, |widget| crate::undo::get_state(widget))
        .unwrap();
    f.frame
        .perform_named("history-open", &crate::a11y::Action::Activate)
        .unwrap();
    assert!(f.frame.history.as_ref().unwrap().captures_input());
    assert!(f.frame.check_app_activation().is_err());
    // The actual History button's production request is queued separately
    // from layout. Its original return now runs WITH activation intent.
    f.frame.history.as_ref().unwrap().request_close();
    f.frame.activate_app_focus(OLD);
    assert!(!f.frame.history.as_ref().unwrap().captures_input());
    assert!(!f.frame.history.as_ref().unwrap().returning_live());
    assert!(
        !f.frame
            .history
            .as_ref()
            .unwrap()
            .has_settled_control_return(),
        "the original postdraw return tail releases its control lease"
    );
    assert!(f.frame.pending_app_activation.is_none());
    let old_node = f.frame.named_target_nodes()["activation-old"];
    let focused = f
        .frame
        .inspect_node(old_node, |widget| widget.is_focused())
        .unwrap();
    assert!(focused && f.old.draw_focus.get());
    assert_eq!(
        f.frame
            .inspect_node(old_node, |widget| crate::undo::get_state(widget))
            .unwrap(),
        caret
    );
    assert!(f.frame.ime_window().is_some());
    assert!(scene_has(
        f.frame.frame_render_input().scene,
        marker(OLD, focused)
    ));
    assert!(!scene_has(
        f.frame.frame_render_input().scene,
        marker(OLD, !focused)
    ));
    let node = f.frame.focused().unwrap();
    assert_eq!(f.frame.bindings.binding_for(node).unwrap().instance_id, OLD);
    f.close();
}
