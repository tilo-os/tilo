use super::*;
use crate::clock::{Duration, Instant};
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
                && encoding.draw_data[data_offset] == expected)
                .then_some(position);
            data_offset += usize::try_from(tag.info_size()).expect("draw info size fits usize");
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

#[test]
fn portable_frame_composites_real_dropdown_overlay_above_later_siblings() {
    let theme = Theme::dark();
    let blocker = vello::peniko::Color::from_rgb8(0xf1, 0x0d, 0xa7);
    let root = Column::new()
        .push(Box::new(
            crate::widget::Dropdown::new(vec!["First".into(), "Second".into()], "choice").open(),
        ))
        .push(Box::new(
            Container::new().with_fill(blocker).with_size(240.0, 80.0),
        ));
    let frame = frame(Box::new(root));
    let scene = frame.frame_output().scene;

    let blocker_position = encoded_color_positions(scene, blocker)
        .into_iter()
        .next()
        .expect("the later sibling paints its fixture color");
    let overlay_position = encoded_color_positions(scene, theme.tokens.colors.surfaces.raised)
        .into_iter()
        .rfind(|position| *position > blocker_position)
        .expect("the real open dropdown paints its raised menu after the later sibling");

    assert!(
        overlay_position > blocker_position,
        "overlay paint must occupy the top layer"
    );
}

struct OverlayProbe {
    draws: Rc<Cell<usize>>,
    rect: Rect,
}

impl Widget for OverlayProbe {
    fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(100.0),
                    height: Dimension::length(40.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

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

    fn handle_event(
        &mut self,
        _: &WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut automerge::AutoCommit,
        _: &automerge::ObjId,
        _: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "overlay-probe"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

static PORTABLE_HISTORY: crate::history_mode::HistoryDefinition =
    crate::history_mode::HistoryDefinition {
        app_id: "dev.qos.portable-overlay-fixture",
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
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inspections.set(self.inspections.get() + 1);
        None
    }

    fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}
    fn update(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) -> bool {
        self.updates.set(self.updates.get() + 1);
        false
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = t.new_leaf(taffy::Style::default()).unwrap();
        nodes.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }
    fn handle_event(
        &mut self,
        _: &WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut automerge::AutoCommit,
        _: &automerge::ObjId,
        _: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "scheduled"
    }
    fn get_rect(&self) -> Rect {
        Rect::ZERO
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
        .push(Box::new(Button::new("first", "First", None)))
        .push(Box::new(Button::new("last", "Last", None)));
    let modal = Container::new()
        .with_focus_trap()
        .with_child(Box::new(controls));
    let root = Column::new()
        .push(Box::new(Button::new("background", "Background", None)))
        .push(Box::new(modal));
    let mut frame = frame(Box::new(root));
    assert_eq!(
        frame.focus_order().len(),
        2,
        "background excluded by the same native trap policy"
    );
    assert!(
        frame
            .access_node_named("first", None)
            .unwrap()
            .state
            .focused
    );
    frame.key("Tab");
    assert!(frame.access_node_named("last", None).unwrap().state.focused);
    frame.key("Tab");
    assert!(
        frame
            .access_node_named("first", None)
            .unwrap()
            .state
            .focused
    );
    frame.set_modifiers(&["shift".into()]);
    frame.key("Tab");
    assert!(frame.access_node_named("last", None).unwrap().state.focused);
    frame.set_modifiers(&["alt".into()]);
    frame.key("ArrowUp");
    assert!(
        frame
            .access_node_named("first", None)
            .unwrap()
            .state
            .focused
    );
    frame.set_modifiers(&["alt".into()]);
    frame.key("ArrowUp");
    assert!(
        frame
            .access_node_named("first", None)
            .unwrap()
            .state
            .focused
    );
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
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.content.sync(doc, path);
    }
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
    fn apply_layout(&mut self, t: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.content.apply_layout(t, node, origin);
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
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        t: &TaffyTree<()>,
    ) {
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
            *handled = true;
            return;
        }
        self.content
            .handle_event(event, scale, modifiers, doc, path, handled, focus, node, t);
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
        content: Column::new().push(Box::new(Button::new("inside", "Inside", None))),
        open: open.clone(),
        traps: true,
        delivered: Rc::default(),
    };
    let mut frame = frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("background", "Background", None)))
            .push(Box::new(gate)),
    ));
    frame
        .perform_named("background", &crate::a11y::Action::Focus)
        .unwrap();
    open.set(true);
    frame.relayout();
    assert!(
        frame
            .access_node_named("inside", None)
            .unwrap()
            .state
            .focused
    );
    assert!(
        !frame
            .access_node_named("background", None)
            .unwrap()
            .state
            .focused
    );
    open.set(false);
    frame.relayout();
    assert!(
        frame
            .access_node_named("background", None)
            .unwrap()
            .state
            .focused
    );
}
#[test]
fn portable_review_modal_owning_keyboard_receives_tab_and_alt_arrow() {
    let delivered = Rc::new(Cell::new(0));
    let gate = Gate {
        content: Column::new().push(Box::new(Button::new("inside", "Inside", None))),
        open: Rc::new(Cell::new(true)),
        traps: false,
        delivered: delivered.clone(),
    };
    let mut frame = frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("background", "Background", None)))
            .push(Box::new(gate)),
    ));
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
    assert!(
        frame
            .access_node_named("inside", None)
            .unwrap()
            .state
            .focused
    );
    assert!(
        !frame
            .access_node_named("background", None)
            .unwrap()
            .state
            .focused
    );
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
    let mut frame = frame(Box::new(
        Column::new()
            .push(Box::new(Button::new("background", "Background", None)))
            .push(Box::new(gate)),
    ));
    frame.key("Tab");
    assert_eq!(
        delivered.get(),
        1,
        "native traversal declines Tab when the trap has no focusable controls"
    );
    assert!(
        !frame
            .access_node_named("background", None)
            .unwrap()
            .state
            .focused
    );
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
    let inspections=Rc::new(Cell::new(0));
    let mut frame=frame(Box::new(Scheduled {
        inspections: inspections.clone(), animate: Rc::new(Cell::new(false)),
        deadline: Rc::new(Cell::new(None)), updates: Rc::new(Cell::new(0)),
    }));
    inspections.set(0);
    for _ in 0..100 { assert!(!frame.poll_frame().unwrap()); }
    assert_eq!(inspections.get(),0,"a scheduling query must not construct the accessibility snapshot");
    let _=frame.frame_output();
    assert_eq!(inspections.get(),1,"explicit output still supplies current semantics");
}
