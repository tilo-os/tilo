//! A press the pointer made completes across a rebuild that put an equivalent control where it
//! stood: through the headless host and through an app's own surface, and not when the pointer
//! left or the control moved away.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_ui::headless::Driver;
use tilo_ui::widget::Button;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::{EventCtx, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Point, Rect, Vec2};
use winit::event::ElementState;
use winit::event::{MouseButton, WindowEvent};

fn moved(x: f64, y: f64) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(x, y)
}

fn button(state: ElementState) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left)
}

type Staged = Rc<RefCell<Option<(Box<dyn Widget>, f64)>>>;
type Clicks = Rc<RefCell<Vec<&'static str>>>;

/// Shows one child and swaps in the staged replacement on its next structural poll, as an app
/// rebuilding its content from a worker result does.
struct Rebuilds {
    child: Box<dyn Widget>,
    staged: Staged,
    offset: f64,
    replaced: bool,
    rect: Rect,
    placed: Rc<Cell<Rect>>,
}

impl Rebuilds {
    fn new(clicks: &Clicks, staged: &Staged) -> (Self, Rc<Cell<Rect>>) {
        let placed = Rc::new(Cell::new(Rect::ZERO));
        let root = Self {
            child: copy_link(clicks, "first"),
            staged: Rc::clone(staged),
            offset: 0.0,
            replaced: false,
            rect: Rect::ZERO,
            placed: Rc::clone(&placed),
        };
        (root, placed)
    }
}

impl Widget for Rebuilds {
    fn take_structure_dirty(&mut self) -> bool {
        if let Some((child, offset)) = self.staged.borrow_mut().take() {
            self.child = child;
            self.offset = offset;
            self.replaced = true;
        }
        std::mem::take(&mut self.replaced) | self.child.take_structure_dirty()
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[child], node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (400.0, 200.0));
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            let at = origin + Vec2::new(0.0, self.offset);
            self.child.apply_layout(taffy, *child, at);
            self.placed.set(self.child.get_rect());
        }
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.child.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.child.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn name(&self) -> &'static str {
        "Rebuilds"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn copy_link(clicks: &Clicks, generation: &'static str) -> Box<dyn Widget> {
    let clicks = Rc::clone(clicks);
    Box::new(
        Button::new("copy_link", "Copy link")
            .on_click(move || clicks.borrow_mut().push(generation)),
    )
}

fn hovered() -> (Driver, Staged, Clicks) {
    let clicks = Clicks::default();
    let staged = Staged::default();
    let mut harness = Driver::new(
        tilo_ui::State::new(),
        Box::new(Rebuilds::new(&clicks, &staged).0),
        (400, 200),
    );
    let centre = harness.targets()["copy_link"].center();
    harness.send(&moved(centre.x, centre.y));
    (harness, staged, clicks)
}

/// Stage a fresh, equivalent button `offset` below the first and run the frame that swaps it in, as
/// a worker result landing does.
fn rebuild(harness: &mut Driver, staged: &Staged, clicks: &Clicks, offset: f64) {
    *staged.borrow_mut() = Some((copy_link(clicks, "rebuilt"), offset));
    harness.relayout();
    assert!(staged.borrow().is_none(), "the frame swapped the button in");
}

#[test]
fn a_rebuild_between_press_and_release_still_completes_the_click() {
    // A control rebuilt under a pointer that has not moved is pressed
    // where it now stands.
    let (mut harness, staged, clicks) = hovered();
    let before = harness.targets()["copy_link"];
    rebuild(&mut harness, &staged, &clicks, 0.0);
    assert_eq!(harness.targets()["copy_link"], before, "the same place");
    harness.send(&button(ElementState::Pressed));
    harness.send(&button(ElementState::Released));
    assert_eq!(*clicks.borrow(), vec!["rebuilt"]);

    let (mut harness, staged, clicks) = hovered();
    harness.send(&button(ElementState::Pressed));
    rebuild(&mut harness, &staged, &clicks, 0.0);
    harness.send(&button(ElementState::Released));
    assert_eq!(*clicks.borrow(), vec!["rebuilt"]);
    rebuild(&mut harness, &staged, &clicks, 0.0);
    harness.send(&button(ElementState::Released));
    assert_eq!(*clicks.borrow(), vec!["rebuilt"]);
}

#[test]
fn a_control_rebuilt_away_from_the_pointer_is_not_clicked() {
    let (mut harness, staged, clicks) = hovered();
    rebuild(&mut harness, &staged, &clicks, 120.0);
    harness.send(&button(ElementState::Pressed));
    harness.send(&button(ElementState::Released));
    assert!(clicks.borrow().is_empty(), "nothing is under the pointer");

    let (mut harness, staged, clicks) = hovered();
    harness.send(&button(ElementState::Pressed));
    rebuild(&mut harness, &staged, &clicks, 120.0);
    harness.send(&button(ElementState::Released));
    assert!(
        clicks.borrow().is_empty(),
        "a press does not follow its control to another place"
    );
}

/// The live host and an embedding one: the App replays the pointer on its own surface input, not
/// only the headless driver.
#[test]
fn an_app_surface_clicks_a_control_rebuilt_under_a_still_pointer() {
    struct Region;
    impl tilo_ui::app::AppSurface for Region {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(400, 200)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
    }
    let clicks = Clicks::default();
    let staged = Staged::default();
    let (root, placed) = Rebuilds::new(&clicks, &staged);
    let mut app = tilo_ui::App::new_with_controlled_inputs(tilo_ui::State::new(), Box::new(root));
    let mut scene = Scene::new();
    let _ = app.surface_frame(&Region, &mut scene);
    let centre = placed.get().center();
    app.surface_input(&Region, moved(centre.x, centre.y), false);

    *staged.borrow_mut() = Some((copy_link(&clicks, "rebuilt"), 0.0));
    let _ = app.surface_frame(&Region, &mut scene);
    assert!(staged.borrow().is_none(), "the frame swapped the button in");
    app.surface_input(&Region, button(ElementState::Pressed), false);
    *staged.borrow_mut() = Some((copy_link(&clicks, "again"), 0.0));
    let _ = app.surface_frame(&Region, &mut scene);
    app.surface_input(&Region, button(ElementState::Released), false);
    assert_eq!(
        *clicks.borrow(),
        vec!["again"],
        "a rebuild before the press and another before the release"
    );
}

#[test]
fn a_pointer_that_left_the_pressed_control_still_cancels_after_a_rebuild() {
    let (mut harness, staged, clicks) = hovered();
    harness.send(&button(ElementState::Pressed));
    harness.send(&moved(390.0, 190.0));
    rebuild(&mut harness, &staged, &clicks, 0.0);
    harness.send(&button(ElementState::Released));
    assert!(
        clicks.borrow().is_empty(),
        "the pointer left before release"
    );
}
