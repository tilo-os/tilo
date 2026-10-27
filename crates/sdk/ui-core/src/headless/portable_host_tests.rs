use super::*;
use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, Column, Container, Widget};
use std::{cell::Cell, rc::Rc};

fn encoded_color_positions(scene: &Scene, color: vello::peniko::Color) -> Vec<usize> {
    let expected = vello_encoding::DrawColor::from(color).rgba;
    let encoding = scene.encoding();
    let mut data_offset = 0;
    encoding
        .draw_tags
        .iter()
        .enumerate()
        .filter_map(|(position, tag)| {
            let found = (*tag == vello_encoding::DrawTag::COLOR
                && encoding.draw_data.get(data_offset) == Some(&expected))
            .then_some(position);
            // The scene data a tag carries (the shader's `scene_offset`), which
            // differs from its info size for images and blurred rects.
            data_offset += usize::try_from((tag.0 >> 2) & 0x07).expect("draw data size fits usize");
            found
        })
        .collect()
}

fn frame(root: Box<dyn Widget>) -> FrameHarness {
    FrameHarness::new_with_controlled_inputs(
        State::new(),
        root,
        (400, 240),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("portable-review".into()),
    )
}

use crate::synthetic_input::cursor_moved;

fn focused(frame: &FrameHarness, name: &str) -> bool {
    frame.access_node_named(name, None).unwrap().state.focused
}

fn target_and_other() -> Column {
    Column::new()
        .push(Box::new(Button::new("target", "Target")))
        .push(Box::new(Button::new("other", "Other")))
}

fn behind_background(content: impl Widget + 'static) -> FrameHarness {
    frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("background", "Background")))
            .push(Box::new(content)),
    ))
}

#[test]
fn portable_frame_composites_real_dropdown_overlay_above_later_siblings() {
    let theme = Theme::dark();
    let blocker = vello::peniko::Color::from_rgb8(0xf1, 0x0d, 0xa7);
    let root = Column::new()
        .push(Box::new(
            crate::widget::Dropdown::new(vec!["First".into(), "Second".into()], "choice").open(),
        ))
        .push(Box::new(Blocker {
            color: blocker,
            rect: Rect::ZERO,
        }));
    let frame = frame(Box::new(root));
    let scene = frame.frame_output().scene;

    let blocker_position = encoded_color_positions(scene, blocker)
        .into_iter()
        .next()
        .expect("the later sibling paints its fixture color");
    // The open list is the floating material (HIG §9.2): the panel ground,
    // opaque or as the tint over a blurred backdrop.
    let ground = crate::chrome::panel_ground(&theme);
    let tint = ground.with_alpha(theme.tokens.colors.surfaces.material_opacity);
    let overlay_position = encoded_color_positions(scene, ground)
        .into_iter()
        .chain(encoded_color_positions(scene, tint))
        .filter(|position| *position > blocker_position)
        .max()
        .expect("the real open dropdown paints its menu after the later sibling");

    assert!(
        overlay_position > blocker_position,
        "overlay paint must occupy the top layer"
    );
}

/// A later sibling that paints one fixture colour over its rect.
struct Blocker {
    color: vello::peniko::Color,
    rect: Rect,
}

impl Widget for Blocker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let style = taffy::Style {
            size: taffy::Size::from_lengths(240.0_f32, 80.0_f32),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: vello::kurbo::Affine,
        _text: &mut crate::text_render::TextRenderer,
        _theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.color,
            None,
            &self.rect,
        );
    }
    fn name(&self) -> &'static str {
        "Blocker"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

struct OverlayProbe {
    draws: Rc<Cell<usize>>,
    rect: Rect,
}

impl Widget for OverlayProbe {
    crate::testing::inert_widget_methods!(name = "overlay-probe");
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size::from_lengths(100.0, 40.0),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    crate::rect_geometry!();

    fn draw_overlay(
        &mut self,
        _: &mut Scene,
        _: Affine,
        _: &mut TextRenderer,
        _: &Theme,
        _: &PresenceCtx,
    ) {
        self.draws.set(self.draws.get() + 1);
    }
}

static PORTABLE_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "so.tilo.portable-overlay-fixture",
        scopes: &[crate::history_mode::HistoryScopeDefinition {
            key: "document",
            title: "Document",
            presentation: crate::history_mode::HistoryPresentation::External {
                reason: "fixture history is unavailable",
            },
            content_schema: None,
        }],
    };

#[test]
fn portable_frame_hides_live_overlay_with_compact_history_surface() {
    let draws = Rc::new(Cell::new(0));
    let registration = crate::history_mode::HistoryRegistration::new(
        &PORTABLE_HISTORY,
        vec![
            crate::history_mode::HistorySourceRegistration::Unversioned {
                scope: "document",
                reason: "fixture history is unavailable",
            },
        ],
    )
    .unwrap();
    let mut frame = FrameHarness::from_managed_root(
        State::new(),
        crate::history_mode::ManagedRoot::new(
            Box::new(OverlayProbe {
                draws: draws.clone(),
                rect: Rect::ZERO,
            }),
            registration,
        ),
        (400, 240),
    )
    .unwrap();
    assert!(draws.get() > 0, "the live surface paints its overlay");

    frame.click_named("history-open").unwrap();
    let draws_after_open = draws.get();
    frame.relayout();

    assert_eq!(
        draws.get(),
        draws_after_open,
        "compact History replaces the live surface, including its overlay"
    );
}
struct Scheduled {
    inspections: Rc<Cell<usize>>,
    animate: Rc<Cell<bool>>,
    deadline: Rc<Cell<Option<Instant>>>,
    updates: Rc<Cell<usize>>,
}
impl Widget for Scheduled {
    crate::testing::inert_widget_methods!(leaf_layout, name = "scheduled");
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inspections.set(self.inspections.get() + 1);
        None
    }

    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        self.updates.set(self.updates.get() + 1);
        false
    }
    fn needs_redraw(&self) -> bool {
        false
    }
    fn needs_animation_frame(&self) -> bool {
        self.animate.get()
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        self.deadline.get()
    }
}
#[test]
fn portable_review_clean_animation_and_delayed_wake_drive_frames() {
    let animate = Rc::new(Cell::new(false));
    let deadline = Rc::new(Cell::new(None));
    let updates = Rc::new(Cell::new(0));
    let mut frame = frame(Box::new(Scheduled {
        inspections: Rc::new(Cell::new(0)),
        animate: animate.clone(),
        deadline: deadline.clone(),
        updates: updates.clone(),
    }));
    assert!(!frame.poll_frame().unwrap());
    animate.set(true);
    assert!(frame.frame_output().needs_animation_frame);
    let before = updates.get();
    assert!(
        frame.poll_frame().unwrap(),
        "clean widgets still need animation updates"
    );
    assert!(updates.get() > before);
    animate.set(false);
    deadline.set(Some(Instant::now() + Duration::from_secs(60)));
    assert_eq!(frame.frame_output().next_wake_deadline, deadline.get());
    assert!(
        !frame.poll_frame().unwrap(),
        "future deadlines must not spin"
    );
    deadline.set(Some(Instant::now() - Duration::from_millis(1)));
    assert!(
        frame.poll_frame().unwrap(),
        "due deadlines must update a clean tree"
    );
}
#[test]
fn portable_review_trap_confines_tab_shift_tab_and_spatial_focus() {
    let controls = Column::new()
        .push(Box::new(Button::new("first", "First")))
        .push(Box::new(Button::new("last", "Last")));
    let modal = Container::new()
        .with_focus_trap()
        .with_child(Box::new(controls));
    let root = Column::new()
        .push(Box::new(Button::new("background", "Background")))
        .push(Box::new(modal));
    let mut frame = frame(Box::new(root));
    assert_eq!(
        frame.focus_order().len(),
        2,
        "background excluded by the same native trap policy"
    );
    assert!(focused(&frame, "first"));
    frame.key("Tab");
    assert!(focused(&frame, "last"));
    frame.key("Tab");
    assert!(focused(&frame, "first"));
    frame.set_modifiers(&["shift".into()]);
    frame.key("Tab");
    assert!(focused(&frame, "last"));
    frame.set_modifiers(&["alt".into()]);
    frame.key("ArrowUp");
    assert!(focused(&frame, "first"));
    frame.set_modifiers(&["alt".into()]);
    frame.key("ArrowUp");
    assert!(focused(&frame, "first"));
}

/// A retained modal with explicit native Widget focus/modal contracts. Its real
/// controls keep stable semantic identities while the host rebuilds Taffy ids.
struct Gate {
    content: Column,
    open: Rc<Cell<bool>>,
    traps: bool,
    delivered: Rc<Cell<usize>>,
}
impl Widget for Gate {
    crate::testing::delegate_widget_methods!(content: sync, apply_layout);
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.content.build_layout(t, theme, nodes);
        nodes.insert(node, self as *mut dyn Widget);
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.open.get() {
            self.content.draw(scene, transform, text, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open.get() {
            return;
        }
        if (!self.traps || self.content.children().is_empty())
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        state: ElementState::Pressed,
                        logical_key: Key::Named(NamedKey::Tab | NamedKey::ArrowUp),
                        ..
                    },
                    ..
                }
            )
        {
            self.delivered.set(self.delivered.get() + 1);
            *ctx.handled = true;
            return;
        }
        self.content.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.open.get() {
            self.content.children()
        } else {
            vec![]
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open.get() {
            self.content.children_mut()
        } else {
            vec![]
        }
    }
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.content);
        true
    }
    fn name(&self) -> &'static str {
        "gate"
    }
    fn get_rect(&self) -> Rect {
        self.content.get_rect()
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.open.get().then(|| self.get_rect())
    }
    fn traps_focus(&self) -> bool {
        self.open.get() && self.traps
    }
}
#[test]
fn portable_review_modal_enter_leave_restores_semantic_focus() {
    let open = Rc::new(Cell::new(false));
    let gate = Gate {
        content: Column::new().push(Box::new(Button::new("inside", "Inside"))),
        open: open.clone(),
        traps: true,
        delivered: Rc::default(),
    };
    let mut frame = behind_background(gate);
    frame
        .perform_named("background", &crate::a11y::Action::Focus)
        .unwrap();
    open.set(true);
    frame.relayout();
    assert!(focused(&frame, "inside"));
    assert!(!focused(&frame, "background"));
    open.set(false);
    frame.relayout();
    assert!(focused(&frame, "background"));
}

/// Counts every time the focus-order walk asks whether it takes focus.
struct FocusAsked {
    asked: Rc<Cell<usize>>,
}
impl Widget for FocusAsked {
    crate::testing::inert_widget_methods!(leaf_layout, name = "focus-asked");
    fn is_focusable(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        false
    }
}

#[test]
fn untrapped_frames_skip_the_focus_walk_and_a_trap_still_engages_at_once() {
    let asked = Rc::new(Cell::new(0));
    let open = Rc::new(Cell::new(false));
    let gate = Gate {
        content: Column::new().push(Box::new(Button::new("inside", "Inside"))),
        open: open.clone(),
        traps: true,
        delivered: Rc::default(),
    };
    let mut frame = frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("background", "Background")))
            .push(Box::new(FocusAsked {
                asked: asked.clone(),
            }))
            .push(Box::new(gate)),
    ));
    frame
        .perform_named("background", &crate::a11y::Action::Focus)
        .unwrap();
    frame.relayout();
    asked.set(0);
    let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
    for _ in 0..5 {
        frame.relayout();
        frame.dispatch_to_root(&wheel);
        frame.relayout();
    }
    assert_eq!(
        asked.get(),
        0,
        "untrapped idle and scroll frames walk no focus order, as the live App between rebuilds"
    );
    // The gate opens without a structure change, so no rebuild announces it.
    open.set(true);
    frame.relayout();
    assert!(
        focused(&frame, "inside"),
        "a trap engages the frame it appears"
    );
    assert!(!focused(&frame, "background"));
    open.set(false);
    frame.relayout();
    assert!(
        focused(&frame, "background"),
        "closing the trap restores the focus it saved"
    );
}
#[test]
fn portable_review_modal_owning_keyboard_receives_tab_and_alt_arrow() {
    let delivered = Rc::new(Cell::new(0));
    let gate = Gate {
        content: Column::new().push(Box::new(Button::new("inside", "Inside"))),
        open: Rc::new(Cell::new(true)),
        traps: false,
        delivered: delivered.clone(),
    };
    let mut frame = behind_background(gate);
    frame
        .perform_named("inside", &crate::a11y::Action::Focus)
        .unwrap();
    frame.key("Tab");
    frame.set_modifiers(&["shift".into()]);
    frame.key("Tab");
    frame.set_modifiers(&["alt".into()]);
    frame.key("ArrowUp");
    assert_eq!(
        delivered.get(),
        3,
        "modal-owned keys must reach ordinary widget dispatch"
    );
    assert!(focused(&frame, "inside"));
    assert!(!focused(&frame, "background"));
}

#[test]
fn portable_review_empty_trap_forwards_unhandled_tab_without_background_focus() {
    let delivered = Rc::new(Cell::new(0));
    let gate = Gate {
        content: Column::new(),
        open: Rc::new(Cell::new(true)),
        traps: true,
        delivered: delivered.clone(),
    };
    let mut frame = behind_background(gate);
    frame.key("Tab");
    assert_eq!(
        delivered.get(),
        1,
        "native traversal declines Tab when the trap has no focusable controls"
    );
    assert!(!focused(&frame, "background"));
}

#[test]
fn portable_review_scrollbar_hold_clears_damage_and_sleeps_until_deadline() {
    use crate::widget::{Scrollable, Spacer};
    let mut frame = frame(Box::new(
        Scrollable::new_vertical()
            .with_height(160.0)
            .with_child(Box::new(Spacer::fixed(1200.0))),
    ));
    frame
        .root
        .as_any_mut()
        .unwrap()
        .downcast_mut::<Scrollable>()
        .unwrap()
        .scroll_by(0.0, 40.0);
    frame.relayout();
    let output = frame.frame_output();
    assert!(
        !output.needs_redraw,
        "composed scrollbar damage must be marked drawn"
    );
    assert!(
        !output.needs_animation_frame,
        "opaque scrollbar hold is not an animation"
    );
    assert!(
        output
            .next_wake_deadline
            .is_some_and(|deadline| deadline > Instant::now())
    );
    assert!(
        !frame.poll_frame().unwrap(),
        "opaque hold sleeps until its fade deadline"
    );
}

#[test]
fn idle_frame_poll_does_not_materialize_accessibility_tree() {
    let inspections = Rc::new(Cell::new(0));
    let mut frame = frame(Box::new(Scheduled {
        inspections: inspections.clone(),
        animate: Rc::new(Cell::new(false)),
        deadline: Rc::new(Cell::new(None)),
        updates: Rc::new(Cell::new(0)),
    }));
    inspections.set(0);
    for _ in 0..100 {
        assert!(!frame.poll_frame().unwrap());
    }
    assert_eq!(
        inspections.get(),
        0,
        "a scheduling query must not construct the accessibility snapshot"
    );
    let _ = frame.frame_output();
    assert_eq!(
        inspections.get(),
        1,
        "explicit output still supplies current semantics"
    );
}

#[test]
fn inert_pointer_motion_skips_relayout_while_hover_and_presses_still_render() {
    // Two retained buttons; the column below them is empty space with no
    // hover state. (A childless Container is a leaf that never opts into
    // incremental redraw, so it is deliberately not used as the inert area.)
    let mut frame = frame(Box::new(target_and_other()));
    let device_id = winit::event::DeviceId::dummy();
    let button = frame.targets()["target"];
    let other = frame.targets()["other"];
    let filler_point = (button.center().x, other.max_y() + 40.0);
    assert!(
        filler_point.1 < 240.0,
        "inert point stays inside the 400x240 frame"
    );

    frame.send_host_event(&cursor_moved(filler_point.0, filler_point.1));
    let schedule = frame.frame_schedule();
    assert!(
        frame.last_send_skipped_relayout(),
        "a move over inert space must not run the compose pass; redraw={} animation={} wake={:?} dirty={:?}",
        schedule.needs_redraw,
        schedule.needs_animation_frame,
        schedule.next_wake_deadline.map(|w| w <= Instant::now()),
        frame.redraw_sources()
    );
    frame.send_host_event(&cursor_moved(filler_point.0 + 8.0, filler_point.1 + 3.0));
    assert!(
        frame.last_send_skipped_relayout(),
        "repeated inert motion stays skipped"
    );

    frame.send_host_event(&cursor_moved(button.center().x, button.center().y));
    assert!(
        !frame.last_send_skipped_relayout(),
        "hover onto a button is paint and produces a frame"
    );
    frame.send_host_event(&cursor_moved(button.center().x + 1.0, button.center().y));
    assert!(
        frame.last_send_skipped_relayout(),
        "motion inside the hovered button changes nothing"
    );

    frame.send_host_event(&WindowEvent::MouseInput {
        device_id,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    assert!(
        !frame.last_send_skipped_relayout(),
        "a press is handled input"
    );
    frame.send_host_event(&WindowEvent::MouseInput {
        device_id,
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    assert!(!frame.last_send_skipped_relayout());

    frame.send_host_event(&cursor_moved(filler_point.0, filler_point.1));
    assert!(
        !frame.last_send_skipped_relayout(),
        "leaving the button repaints its resting state"
    );
    frame.send_host_event(&cursor_moved(filler_point.0 + 2.0, filler_point.1));
    assert!(frame.last_send_skipped_relayout());
}

#[test]
fn hover_only_frames_run_zero_layout_passes() {
    // Inert pointer motion must neither rebuild nor re-solve nor re-apply the
    // retained tree: the frame counter stays put and the last relayout is the
    // one the hover produced.
    let mut frame = frame(Box::new(target_and_other()));
    let button = frame.targets()["target"];
    let other = frame.targets()["other"];
    let filler = (button.center().x, other.max_y() + 40.0);
    let ids = frame.layout_node_ids();

    frame.send_host_event(&cursor_moved(button.center().x, button.center().y));
    assert!(
        !frame.last_send_skipped_relayout(),
        "hover onto a button paints"
    );
    let hover = frame.last_relayout();
    assert!(
        !hover.rebuilt,
        "a hover change re-paints a retained tree: {hover:?}"
    );
    let frames = frame.relayout_frames();

    for step in 0..3 {
        frame.send_host_event(&cursor_moved(
            button.center().x + 1.0 + f64::from(step),
            button.center().y,
        ));
    }
    assert_eq!(
        frame.relayout_frames(),
        frames,
        "motion inside the hovered button runs no layout pass"
    );
    frame.send_host_event(&cursor_moved(filler.0, filler.1));
    assert_eq!(
        frame.relayout_frames(),
        frames + 1,
        "leaving the button paints its resting state once"
    );
    assert!(
        !frame.last_relayout().rebuilt,
        "the leave repaints the retained tree"
    );
    for step in 0..3 {
        frame.send_host_event(&cursor_moved(filler.0 + f64::from(step), filler.1));
    }
    assert_eq!(
        frame.relayout_frames(),
        frames + 1,
        "inert motion over empty space runs no layout pass"
    );
    assert_eq!(
        frame.layout_node_ids(),
        ids,
        "hover frames keep the retained tree"
    );
    frame.verify_retained_geometry().unwrap();
}

#[test]
fn typing_into_a_block_editor_re_measures_only_the_edited_line() {
    use crate::widget::BlockEditor;
    let mut editor = BlockEditor::from_markdown("alpha\n\nbeta\n\ngamma");
    editor.set_focus(true);
    let mut frame = frame(Box::new(editor));
    // The first key switches the interaction environment to keyboard, which
    // recomposes once (widgets may size differently per modality); settle
    // that before measuring an ordinary typed key.
    frame.type_text("w");
    frame.relayout();
    frame.set_retained_geometry_check(1);
    let ids = frame.layout_node_ids();
    let focused = frame.focused();
    assert!(
        focused.is_some(),
        "the editor's focused line owns keyboard focus"
    );

    // The press edits the line; its release changes nothing measured.
    let x = keys::parse_key("x");
    frame.send_host_event(&crate::synthetic_input::key_input(
        &x,
        ElementState::Pressed,
    ));
    let typed = frame.last_relayout();
    frame.send_host_event(&crate::synthetic_input::key_input(
        &x,
        ElementState::Released,
    ));
    assert!(
        !frame.last_relayout().measured,
        "the key's release measures nothing: {:?}",
        frame.last_relayout()
    );
    assert!(
        !typed.rebuilt,
        "a character edit keeps the composed tree: {typed:?}"
    );
    assert!(
        typed.measured,
        "a character edit re-solves sizes: {typed:?}"
    );
    assert_eq!(
        typed.marked_leaves, 1,
        "only the edited line is re-measured: {typed:?}"
    );
    assert_eq!(
        frame.layout_node_ids(),
        ids,
        "node ids survive a retained relayout"
    );
    assert_eq!(
        frame.focused(),
        focused,
        "focus survives a retained relayout"
    );
    frame.verify_retained_geometry().unwrap();

    // Splitting the block adds a line but no widget: the block stack is one
    // measured leaf, so the editor restacks its lines in a relayout, and
    // the composed tree (its node ids, focus) survives. The retained
    // geometry check compares the result with a fresh composition.
    let rebuilt_before = frame.rebuilt_frames();
    let enter = keys::parse_key("Enter");
    frame.send_host_event(&crate::synthetic_input::key_input(
        &enter,
        ElementState::Pressed,
    ));
    let split = frame.last_relayout();
    frame.send_host_event(&crate::synthetic_input::key_input(
        &enter,
        ElementState::Released,
    ));
    assert_eq!(
        frame.rebuilt_frames(),
        rebuilt_before,
        "inserting a block keeps the composition"
    );
    assert!(
        split.measured,
        "inserting a block re-solves sizes: {split:?}"
    );
    assert_eq!(
        frame.layout_node_ids(),
        ids,
        "a split adds no layout node and keeps the others"
    );
    assert_eq!(frame.focused(), focused, "focus survives a split");
    frame.verify_retained_geometry().unwrap();
}

/// A resize re-solves the retained tree against the new size; only a widget
/// that composes from its allocation is composed again. Either way the result
/// is what a fresh composition at that size gives.
#[test]
fn viewport_change_re_solves_and_matches_a_fresh_composition() {
    let mut frame = frame(Box::new(target_and_other()));
    let ids = frame.layout_node_ids();
    frame.resize((360, 200)).unwrap();
    assert!(
        !frame.last_relayout().rebuilt && frame.last_relayout().measured,
        "a viewport change re-solves the retained tree"
    );
    assert_eq!(frame.layout_node_ids(), ids, "the tree was kept");
    frame.verify_retained_geometry().unwrap();

    frame.set_full_relayout(Some(true));
    frame.relayout();
    frame.set_full_relayout(None);
    assert!(frame.last_relayout().rebuilt);
    frame.verify_retained_geometry().unwrap();
}

#[test]
fn full_relayout_valve_forces_the_rebuild_path() {
    let root = Column::new().push(Box::new(Button::new("target", "Target")));
    let mut frame = frame(Box::new(root));
    frame.relayout();
    assert!(!frame.last_relayout().rebuilt, "retained by default");
    frame.set_full_relayout(Some(true));
    frame.relayout();
    assert!(
        frame.last_relayout().rebuilt,
        "the valve rebuilds every frame"
    );
    assert_eq!(
        frame.last_relayout().marked_dirty,
        0,
        "a fresh tree has nothing to invalidate"
    );
    frame.set_full_relayout(None);
    frame.relayout();
    assert!(
        !frame.last_relayout().rebuilt,
        "clearing the override restores retention"
    );
}

#[test]
fn raw_root_dispatch_maps_history_composition_to_retained_input_and_returns_focus() {
    let registration = crate::history_mode::HistoryRegistration::new(
        &PORTABLE_HISTORY,
        vec![
            crate::history_mode::HistorySourceRegistration::Unversioned {
                scope: "document",
                reason: "fixture history is unavailable",
            },
        ],
    )
    .unwrap();
    let value = crate::local_state::LocalState::new(String::new());
    let root = Column::new().push(Box::new(
        crate::widget::TextInput::from_state(&value).with_id("raw-input"),
    ));
    let mut frame = FrameHarness::from_managed_root(
        State::new(),
        crate::history_mode::ManagedRoot::new(Box::new(root), registration),
        (600, 360),
    )
    .unwrap();
    let rect = *frame
        .targets()
        .get("raw-input")
        .expect("retained input target");
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
    let center = rect.center();
    let modifiers = winit::event::Modifiers::default();
    let frames = frame.relayout_frames();
    let heads = frame.state.doc.get_heads();
    let focused = frame.focused;
    frame.dispatch_to_root_outcome(&cursor_moved(center.x, center.y), &modifiers);
    let result = frame.dispatch_to_root_outcome(&crate::synthetic_input::mouse_press(), &modifiers);
    assert!(
        result.handled,
        "wrapped input receives the actual pointer press"
    );
    let input_node = result.focus_target.expect("input requests focus");
    assert!(frame.node_map.contains_key(&input_node));
    assert_ne!(
        input_node, frame.child_node,
        "focus belongs to the retained input"
    );
    assert_eq!(
        frame.focused, focused,
        "raw dispatch reports focus without host routing"
    );
    assert_eq!(
        frame.relayout_frames(),
        frames,
        "raw dispatch runs no frame"
    );
    assert_eq!(frame.state.doc.get_heads(), heads);
    assert!(value.get().is_empty());
}

#[test]
fn history_composition_keeps_app_modal_typing_and_focus_inside_the_dialog() {
    use crate::widget::{ModalDialog, TextInput};
    let registration = crate::history_mode::HistoryRegistration::new(
        &PORTABLE_HISTORY,
        vec![
            crate::history_mode::HistorySourceRegistration::Unversioned {
                scope: "document",
                reason: "fixture history is unavailable",
            },
        ],
    )
    .unwrap();
    let value = crate::local_state::LocalState::new(String::new());
    let dialog = ModalDialog::new(
        "Add item",
        Column::new()
            .push(Box::new(
                TextInput::from_state(&value)
                    .with_id("dialog-input")
                    .with_accessible_name("Item name"),
            ))
            .push(Box::new(Button::new("dialog-last", "Last"))),
    );
    let root = Column::new()
        .push(Box::new(Button::new("background", "Background")))
        .push(Box::new(dialog));
    let mut frame = FrameHarness::from_managed_root(
        State::new(),
        crate::history_mode::ManagedRoot::new(Box::new(root), registration),
        (600, 400),
    )
    .unwrap();
    frame
        .perform_named("background", &crate::a11y::Action::Focus)
        .unwrap();
    frame
        .with_widget_mut::<ModalDialog<Column>, _>(|dialog| dialog.set_open(true))
        .unwrap();
    assert_eq!(
        frame.focused_access_node().unwrap().name.as_deref(),
        Some("Item name")
    );
    frame.type_text("Harbour");
    assert_eq!(value.get(), "Harbour");
    for _ in 0..6 {
        frame.tab(false);
        assert!(matches!(
            frame.focused_access_node().unwrap().name.as_deref(),
            Some("Item name" | "Last")
        ));
    }
    frame
        .with_widget_mut::<ModalDialog<Column>, _>(|dialog| dialog.set_open(false))
        .unwrap();
    assert!(focused(&frame, "background"));
}

/// A layout that does not settle skips its frame and nothing else, as the
/// browser host and the native one run it: the failure is reported once
/// rather than on every frame that fails again, input still reaches the
/// controls, and the next frame whose layout settles paints.
#[test]
fn a_frame_whose_layout_does_not_settle_is_skipped_and_the_next_paints() {
    let looping = Rc::new(Cell::new(false));
    let policy = {
        let looping = looping.clone();
        move |ctx: &mut crate::widget::ManualContext<'_>| {
            // While looping, a size that flips with the space offered.
            let short = looping.get() && ctx.available().height > 75.0;
            Ok(taffy::Size {
                width: ctx.available().width,
                height: if short { 50.0 } else { 100.0 },
            })
        }
    };
    let mut frame = frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("target", "Target")))
            .push(Box::new(crate::widget::Manual::new(policy))),
    ));
    frame.relayout_at((400, 240)).expect("a settled frame");

    looping.set(true);
    let error = frame
        .recompose()
        .expect_err("the flipping size never settles");
    assert!(
        matches!(error, crate::widget::WidgetLayoutError::DidNotSettle { .. }),
        "{error}"
    );
    // The next frame the harness runs itself, as input's, fails too: it is
    // skipped, and its report kept for a host whose log is not `tracing`'s.
    frame.relayout();
    let report = frame
        .take_skip_report()
        .expect("the first failure is reported");
    assert!(report.contains("did not settle"), "{report}");
    assert_eq!(frame.take_skip_report(), None, "it is handed over once");
    // The host skipping its own frame at once after is not reported again.
    assert_eq!(
        frame.skip_frame(&error),
        None,
        "a failure right after is not reported again"
    );

    frame
        .click_named("target")
        .expect("the button is still there");
    assert!(
        focused(&frame, "target"),
        "a press still lands while frames are skipped"
    );

    looping.set(false);
    frame.recompose().expect("the layout settles again");
    assert!(
        !frame
            .frame_render_input()
            .scene
            .encoding()
            .draw_tags
            .is_empty(),
        "the frame after paints"
    );
    assert!(focused(&frame, "target"));
}
