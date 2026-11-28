//! What the display-less driver's frames wait for and keep: a capture settles the animations the
//! window still runs and stops at its bound, a requested layout rebuilds the geometry it changed,
//! a retained tree is resized and recomposed rather than always rebuilt, a part laid out and never
//! drawn measures once, and the tooltip is painted under a resting pointer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, WindowEvent};

use crate::headless::{Driver, Framed};
use crate::state::State;
use crate::{widget::Column, widget::Text};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::TaffyTreeExt as _, widget::Widget};

// ---- frames a capture waits for ----

/// A window that draws up to `frames` times: what the capture must wait out. The target is a cell
/// so a test can start the entrance after the driver mounted, when the window was already at rest.
struct CountedFrames {
    frames: Rc<Cell<usize>>,
    drawn: Rc<Cell<usize>>,
}

impl Widget for CountedFrames {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _text_renderer, _theme, mut env) = ctx.parts();
        self.drawn.set(self.drawn.get().saturating_add(1));
    }

    fn needs_animation_frame(&self) -> bool {
        self.drawn.get() < self.frames.get()
    }

    fn name(&self) -> &'static str {
        "CountedFrames"
    }
}

/// A window whose third draw starts background visual work, which it owes a frame until the work
/// lands.
struct FinalFrameWork {
    draws: Rc<Cell<usize>>,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Widget for FinalFrameWork {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _text_renderer, _theme, mut env) = ctx.parts();
        self.draws.set(self.draws.get().saturating_add(1));
        if self.draws.get() == 3 {
            self.pending
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn has_pending_visual_work(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn needs_animation_frame(&self) -> bool {
        self.draws.get() < 3
    }

    fn name(&self) -> &'static str {
        "FinalFrameWork"
    }
}

/// A written capture is the window a person sees a moment after it opened, so a still-running
/// entrance is waited out; otherwise `--shots` writes a scroll thumb mid-fade or a page drawn at
/// its old width.
#[cfg(feature = "native")]
#[test]
fn writing_a_shot_runs_the_animation_out_first() {
    let frames = Rc::new(Cell::new(0));
    let drawn = Rc::new(Cell::new(0));
    let widget = CountedFrames {
        frames: Rc::clone(&frames),
        drawn: Rc::clone(&drawn),
    };
    let mut driver = Driver::new(State::new(), Box::new(widget), (200, 100));
    // The entrance starts after the window mounted, as one a person's action begins.
    drawn.set(0);
    frames.set(3);
    assert!(
        driver.frame_schedule().needs_animation_frame,
        "the animation is running before the capture"
    );
    let temp = tempfile::tempdir().expect("temporary output root");
    driver.write_shot(&temp.path().join("shot.png"), false);
    assert!(
        drawn.get() >= 3,
        "the capture ran the animation out ({} draw(s))",
        drawn.get()
    );
    assert!(
        !driver.frame_schedule().needs_animation_frame,
        "the written frame is a settled one"
    );
}

/// An expired bound cannot flush another frame: the capture runs the frames the window asks for and
/// stops there, before the frame that would start its visual work.
#[test]
fn a_capture_bound_includes_the_initial_frame_pipeline() {
    let draws = Rc::new(Cell::new(usize::MAX));
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut driver = Driver::new(
        State::new(),
        Box::new(FinalFrameWork {
            draws: Rc::clone(&draws),
            pending: std::sync::Arc::clone(&pending),
        }),
        (200, 100),
    );
    draws.set(0);
    assert!(!driver.settle_for_capture(crate::time::Duration::ZERO));
    assert_eq!(
        draws.get(),
        2,
        "an expired capture cannot flush another frame"
    );
    assert!(!pending.load(std::sync::atomic::Ordering::SeqCst));
}

/// A widget that paints a mark a capture may not keep, as a view that declared `protect capture`
/// or a secret field holding a value does.
struct SecretMark;

impl Widget for SecretMark {
    crate::testing::inert_widget_methods!(leaf_layout, name = "secret-mark");
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _, theme, mut env) = ctx.parts();
        tilo_ui_base::secret_paint::note_secret_painted();
        let rect = Rect::new(0.0, 0.0, 100.0, 50.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &rect,
        );
    }
    fn needs_redraw(&self) -> bool {
        false
    }
}

/// A capture an agent takes leaves out what a view showed under `protect capture`: the shot is the
/// window's ground, with nothing of the view on it.
#[cfg(feature = "native")]
#[test]
fn a_written_capture_leaves_out_a_secret() {
    let mut driver = Driver::new(State::new(), Box::new(SecretMark), (200, 100));
    assert!(
        driver.capture_protected(),
        "what it shows is withheld from captures"
    );
    let temp = tempfile::tempdir().expect("temporary output root");
    let shot = temp.path().join("shot.png");
    driver.write_shot(&shot, false);

    let blank = Driver::new(
        State::new(),
        Box::new(tilo_ui_base::widget::Spacer::new()),
        (200, 100),
    );
    let ground = temp.path().join("ground.png");
    blank.render_png(&ground);
    assert_eq!(
        std::fs::read(&shot).expect("the shot was written"),
        std::fs::read(&ground).expect("the ground was written"),
        "the capture is the window's ground, with nothing of the view on it"
    );
}

// ---- a retained tree: resized, recomposed, changed through its root ----
struct Probe {
    width: f32,
    rect: Rect,
    syncs: usize,
    presses: usize,
    follows: Cell<bool>,
}

impl Probe {
    fn new(width: f32) -> Self {
        Self {
            width,
            rect: Rect::ZERO,
            follows: Cell::new(false),
            syncs: 0,
            presses: 0,
        }
    }
}

impl Widget for Probe {
    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.syncs = self.syncs.saturating_add(1);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(self.width, 40.0),
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.presses = self.presses.saturating_add(1);
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn composition_follows_allocation(&self) -> bool {
        self.follows.get()
    }
}

#[test]
fn framed_root_runs_a_frame_on_mount_and_recomposes_after_mutable_access() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    assert!(probe.syncs > 0, "mount runs the production sync pass");
    assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);

    Framed::harness_mut(&mut probe).relayout();
    assert!(
        !Framed::harness(&probe).last_relayout().rebuilt,
        "an untouched root keeps its retained tree"
    );

    probe.width = 200.0;
    Framed::harness_mut(&mut probe).relayout();
    assert!(Framed::harness(&probe).last_relayout().rebuilt);
    assert!((probe.rect.width() - 200.0).abs() < f64::EPSILON);
}

/// A resize re-solves the retained tree against the new window size and composes nothing again,
/// unless a widget composes from its allocation.
#[test]
fn a_resize_recomposes_only_for_widgets_that_compose_from_their_allocation() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    let ids = Framed::harness(&probe).layout_node_ids();
    let frames = Framed::harness(&probe).rebuilt_frames();
    Framed::harness_mut(&mut probe).resize((360, 180)).unwrap();
    let driver = Framed::harness(&probe);
    assert!(
        !driver.last_relayout().rebuilt,
        "resize recomposed the tree"
    );
    assert!(driver.last_relayout().measured, "resize re-solved sizes");
    assert_eq!(driver.layout_node_ids(), ids, "the taffy tree was kept");
    assert_eq!(driver.rebuilt_frames(), frames);

    probe.follows.set(true);
    Framed::harness_mut(&mut probe).resize((320, 160)).unwrap();
    assert!(
        Framed::harness(&probe).last_relayout().rebuilt,
        "a widget that composes from its allocation is composed again"
    );
}

#[test]
fn recompose_rebuilds_an_unchanged_tree() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    Framed::harness_mut(&mut probe).recompose().unwrap();
    assert!(Framed::harness(&probe).last_relayout().rebuilt);
    assert!((probe.rect.width() - 120.0).abs() < f64::EPSILON);
}

#[test]
fn dispatch_to_root_reaches_the_root_and_keeps_the_retained_tree() {
    let mut probe = Framed::new(State::new(), Probe::new(120.0), (400, 200), Theme::dark());
    let handled = Framed::harness_mut(&mut probe)
        .dispatch_to_root(&tilo_ui_base::synthetic_input::mouse_press());
    assert!(handled);
    assert_eq!(probe.presses, 1);

    Framed::harness_mut(&mut probe).relayout();
    assert!(!Framed::harness(&probe).last_relayout().rebuilt);
}

// ---- a layout request for the same viewport ----

#[derive(Clone, Default)]
struct Demand {
    width: Rc<Cell<f32>>,
    rect: Rc<Cell<Rect>>,
    builds: Rc<Cell<usize>>,
    refuse: Rc<Cell<bool>>,
}

struct Intrinsic(Demand);

impl Widget for Intrinsic {
    fn name(&self) -> &str {
        "DeferredIntrinsic"
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.0.builds.set(self.0.builds.get().saturating_add(1));
        let node = tree.leaf_node(Style {
            size: Size::from_lengths(self.0.width.get(), 20.0),
            ..Style::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).expect("the actual leaf has a layout");
        self.0.rect.set(Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(layout.size.width),
            origin.y + f64::from(layout.size.height),
        ));
    }

    fn get_rect(&self) -> Rect {
        self.0.rect.get()
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.0.refuse.get()
    }
}

/// A request for a layout at the viewport it already has still builds the changed intrinsic
/// geometry fresh, and the window owes nothing once it did.
#[test]
fn a_same_viewport_request_rebuilds_changed_intrinsic_geometry() {
    let demand = Demand::default();
    demand.width.set(30.0);
    // The root fills the window, as every app's root does; the leaf inside it keeps its own
    // intrinsic width, which the request changes.
    let root = Column::new().push(Box::new(Intrinsic(demand.clone())));
    let mut driver = Driver::new(State::new(), Box::new(root), (200, 100));
    assert!(!driver.frame_schedule().queued_work);
    let before = demand.rect.get().width();
    demand.width.set(75.0);
    driver.request_layout();
    assert!(driver.poll_frame().unwrap());
    assert_eq!(before, 30.0);
    assert_eq!(demand.rect.get().width(), 75.0);
    assert!(!driver.frame_schedule().queued_work);
}

// ---- the drawn tooltip ----

struct Pane {
    rect: Rect,
}

impl Widget for Pane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(
            "Groceries and the whole week's list",
        ))
    }

    fn name(&self) -> &'static str {
        "Pane"
    }
}

#[test]
fn a_widget_trees_input_reveals_the_tip_under_a_resting_pointer() {
    let mut driver = Driver::new(
        State::new(),
        Box::new(Pane { rect: Rect::ZERO }),
        (400, 300),
    );
    driver.relayout();
    let resting = Point::new(120.0, 80.0);
    let scale = driver.input_scale();
    let settled = crate::time::Instant::now();
    crate::time::testing::with_monotonic_time(settled, || {
        driver.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(resting.x * scale, resting.y * scale),
        });
    });
    assert!(
        driver.drawn_tooltip().is_none(),
        "the pointer has just arrived: nothing is named yet"
    );
    for millis in [700, 1100] {
        crate::time::testing::with_monotonic_time(
            settled + crate::time::Duration::from_millis(millis),
            || {
                driver.poll_frame().expect("a frame after the dwell");
            },
        );
    }
    assert_eq!(
        driver.drawn_tooltip().map(|(text, _)| text),
        Some("Groceries and the whole week's list"),
        "what the pointer rests on names itself once the dwell has run"
    );
}

// ---- a part laid out and never drawn ----

/// A container that lays out both its parts and paints, and rests, only the first: a shut API's
/// contents, a collapsed band's body.
struct HiddenPart {
    shown: Text,
    hidden: Text,
    rect: Rect,
}

impl Widget for HiddenPart {
    fn name(&self) -> &str {
        "HiddenPart"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let shown = self.shown.build_layout(taffy, theme, nodes);
        let hidden = self.hidden.build_layout(taffy, theme, nodes);
        let style = Style {
            flex_direction: taffy::FlexDirection::Column,
            ..Style::default()
        };
        let node = taffy.parent_node(style, &[shown, hidden]);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let children = taffy.children(node).unwrap_or_default();
        let origin = self.rect.origin();
        if let [shown, hidden] = children.as_slice() {
            self.shown.apply_layout(taffy, *shown, origin);
            self.hidden.apply_layout(taffy, *hidden, origin);
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.shown.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }

    fn mark_drawn(&mut self) {
        self.shown.mark_drawn();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shown, &self.hidden]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.shown, &mut self.hidden]
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The layout pass answers a widget's measure, drawn or not: a part a container lays out and does
/// not paint asks once for the change it measures, not on every frame after.
#[test]
fn a_part_laid_out_and_never_drawn_asks_once_for_its_measure() {
    let label = tilo_ui_base::LocalState::new(String::from("Draft"));
    let mut driver = Driver::new(
        State::new(),
        Box::new(HiddenPart {
            shown: Text::new("Title"),
            hidden: Text::from_state(&label, Clone::clone),
            rect: Rect::ZERO,
        }),
        (400, 200),
    );
    driver.relayout();
    driver.relayout();
    label.set(String::from("A draft that says a good deal more"));
    driver.relayout();
    assert!(
        driver.last_relayout().marked_leaves >= 1,
        "the change is measured where it is laid out: {:?}",
        driver.last_relayout()
    );
    driver.relayout();
    assert_eq!(
        driver.last_relayout().marked_leaves,
        0,
        "the layout pass answered it; no draw has to: {:?}",
        driver.last_relayout()
    );
}

// ---- a host that keeps the pixels it presented ----

/// A cell of a tree drawn where it changed: it paints a block and counts each draw, asking for
/// another while `changed` stands, as a row whose text moved does.
struct Cell {
    drew: Rc<std::cell::Cell<usize>>,
    changed: Rc<std::cell::Cell<bool>>,
}

impl Widget for Cell {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: Size {
                    width: taffy::Dimension::length(60.0),
                    height: taffy::Dimension::length(40.0),
                },
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        self.drew.set(self.drew.get().saturating_add(1));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.text_primary,
            None,
            &self.rect,
        );
    }

    fn name(&self) -> &'static str {
        "Cell"
    }

    fn needs_redraw(&self) -> bool {
        self.changed.get()
    }

    fn mark_drawn(&mut self) {
        self.changed.set(false);
    }
}

/// A driver asked to keep the pixels it presents draws only the cell that changed, and names
/// where: the frame before it had no target, so that one draws whole.
#[test]
fn a_driver_that_keeps_its_pixels_draws_only_the_cell_that_changed() {
    use crate::headless::Painted;

    let drew: [Rc<std::cell::Cell<usize>>; 2] = [Rc::default(), Rc::default()];
    let changed: [Rc<std::cell::Cell<bool>>; 2] = [Rc::default(), Rc::default()];
    let cell = |index: usize| {
        Box::new(Cell {
            drew: Rc::clone(&drew[index]),
            changed: Rc::clone(&changed[index]),
        }) as Box<dyn Widget>
    };
    let column = Column::new()
        .with_no_padding()
        .with_gap(0.0)
        .push(cell(0))
        .push(cell(1));
    let column = column.into_widget();
    let mut framed = Framed::new(State::new(), column, (400, 200), Theme::dark());
    let driver = Framed::harness_mut(&mut framed);
    driver.draws_only_damage(true);
    driver.relayout();
    assert_eq!(
        (drew[0].get(), drew[1].get()),
        (1, 1),
        "the first frame has no frame before it: both cells are drawn"
    );
    assert!(
        matches!(driver.take_painted_damage(), Painted::Whole),
        "a target nothing was presented on is painted whole"
    );

    // The second cell changes: the frame draws it and leaves the first as the target holds it.
    changed[1].set(true);
    driver.woken();
    driver.poll_frame().unwrap();
    assert_eq!(
        drew[0].get(),
        1,
        "the cell that did not change is not drawn"
    );
    assert_eq!(drew[1].get(), 2, "the cell that changed is drawn again");
    let Painted::Damage(regions) = driver.take_painted_damage() else {
        panic!("the frame names where it changed");
    };
    assert!(!regions.is_empty(), "the changed cell damages its place");
    let first_cell = Point::new(20.0, 20.0);
    assert!(
        regions.iter().all(|region| !region.contains(first_cell)),
        "nothing the damage names repaints the cell that did not change: {regions:?}"
    );
    assert!(
        regions
            .iter()
            .any(|region| region.contains(Point::new(20.0, 60.0))),
        "the damage covers the cell that changed: {regions:?}"
    );
}
