//! The display-less driver runs the app as a window does: a widget that draws itself, animates on the
//! frame clock and takes the pointer is driven, timed and photographed through it, and a control is
//! reached by name, by its accessibility node and by Tab.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::headless::Driver;
use crate::state::State;
use crate::time::{Duration, Instant};
use crate::{widget::Banner, widget::Button, widget::Column, widget::Container, widget::TextInput};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

const WIDTH: u32 = 200;
const HEIGHT: u32 = 100;
const SQUARE: f64 = 20.0;
const SLIDE: Duration = Duration::from_millis(200);
const RED: vello::peniko::Color = vello::peniko::Color::from_rgb8(255, 0, 0);

/// A square it paints itself, which slides across the widget once a click sets it going.
struct Slider {
    rect: Rect,
    cursor: Point,
    started: Option<Instant>,
    clicks: Rc<Cell<u32>>,
}

impl Slider {
    fn progress(&self, now: Instant) -> f64 {
        self.started.map_or(0.0, |started| {
            (now.saturating_duration_since(started).as_secs_f64() / SLIDE.as_secs_f64()).min(1.0)
        })
    }
}

impl Widget for Slider {
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

    tilo_ui_base::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                self.started = Some(crate::time::now());
                self.clicks.set(self.clicks.get() + 1);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.progress(crate::time::now()) < 1.0 && self.started.is_some()
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        let transform = ctx.transform;
        let scene = &mut *ctx.scene;
        let x = self.rect.x0 + self.progress(crate::time::now()) * (self.rect.width() - SQUARE);
        let y = self.rect.y0 + (self.rect.height() - SQUARE) / 2.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            RED,
            None,
            &Rect::new(x, y, x + SQUARE, y + SQUARE),
        );
    }

    fn name(&self) -> &'static str {
        "Slider"
    }
}

/// Whether the pixel at logical `(x, y)` of the frame drawn last is the square's red.
fn red_at(driver: &Driver, x: u32, y: u32) -> bool {
    let rgba = driver.render_rgba();
    let (width, _) = driver.device_size();
    let scale = driver.input_scale();
    let (x, y) = (
        (f64::from(x) * scale) as usize,
        (f64::from(y) * scale) as usize,
    );
    let at = (y * width as usize + x) * 4;
    rgba[at] > 200 && rgba[at + 1] < 60 && rgba[at + 2] < 60
}

#[test]
fn a_widget_that_draws_itself_animates_on_the_frame_clock_and_takes_the_pointer() {
    let start = crate::time::now();
    let clicks = Rc::new(Cell::new(0));
    let slider = Slider {
        rect: Rect::ZERO,
        cursor: Point::new(-1.0, -1.0),
        started: None,
        clicks: clicks.clone(),
    };
    let mut driver = crate::time::testing::with_monotonic_time(start, || {
        Driver::new(State::new(), Box::new(slider), (WIDTH, HEIGHT))
    });
    assert!(red_at(&driver, 10, 50), "the square starts at the left");
    assert!(
        !crate::time::testing::with_monotonic_time(start, || driver.frame_schedule())
            .needs_animation_frame
    );

    crate::time::testing::with_monotonic_time(start, || driver.click_at(100.0, 50.0));
    assert_eq!(
        clicks.get(),
        1,
        "the click reached the widget that drew itself"
    );
    assert!(
        crate::time::testing::with_monotonic_time(start, || driver.frame_schedule())
            .needs_animation_frame,
        "a running slide asks for frames"
    );

    // Halfway through, as a 60 Hz display would show it: 96 ms is 48 % of the way.
    let mut ran = 0;
    for frame in 1..=6 {
        let now = start + Duration::from_millis(16 * frame);
        if crate::time::testing::with_monotonic_time(now, || driver.poll_frame()).unwrap() {
            ran += 1;
        }
    }
    assert_eq!(ran, 6, "every frame of a running animation is owed");
    let halfway = (0.48 * (f64::from(WIDTH) - SQUARE) + SQUARE / 2.0) as u32;
    assert!(
        red_at(&driver, halfway, 50),
        "the square is where 96 ms put it"
    );
    assert!(!red_at(&driver, 10, 50));

    for frame in 7..=15 {
        let now = start + Duration::from_millis(16 * frame);
        crate::time::testing::with_monotonic_time(now, || driver.poll_frame()).unwrap();
    }
    assert!(
        red_at(&driver, WIDTH - 10, 50),
        "the slide ends at the right"
    );
    let rest = start + Duration::from_millis(16 * 16);
    crate::time::testing::with_monotonic_time(rest, || {
        assert!(!driver.frame_schedule().needs_animation_frame);
        assert!(
            !driver.poll_frame().unwrap(),
            "nothing owes a frame once the slide is over"
        );
    });
}

/// A click on a control says the rect of the control it answered: a call it makes carries it,
/// so the person's question stands beside that control, never over it.
#[test]
#[allow(
    clippy::float_cmp,
    reason = "a control's rect is whole logical pixels, told as the node laid it out"
)]
fn a_click_says_the_rect_of_the_control_it_answered() {
    const PRESS: u32 = 77;
    let mut driver = Driver::new(
        State::new(),
        Box::new(Column::new().push(Box::new(Button::new("add", "Add")))),
        (400, 300),
    );
    driver.relayout();
    let at = driver.targets().get("add").copied().expect("the button");
    tilo_ui_base::press::note(PRESS);
    driver.click_named("add").unwrap();
    let told = tilo_actions::remote::press_points::of(PRESS).expect("the click told its control");
    assert_eq!(
        (
            f64::from(told.x),
            f64::from(told.y),
            f64::from(told.w),
            f64::from(told.h)
        ),
        (at.x0, at.y0, at.width(), at.height()),
        "the rect of the button the pointer answered"
    );
}

#[test]
fn a_control_is_reached_by_its_name_its_accessibility_node_and_tab() {
    let presses = Rc::new(Cell::new(0));
    let counted = presses.clone();
    let column = Column::new()
        .push(Box::new(
            Button::new("add", "Add").on_click(move || counted.set(counted.get() + 1)),
        ))
        .push(Box::new(Button::new("other", "Other")));
    let mut driver = Driver::new(State::new(), Box::new(column), (400, 300));

    driver.click_named("add").unwrap();
    assert_eq!(presses.get(), 1, "a click at the named control presses it");

    driver
        .perform_named("add", &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(
        presses.get(),
        2,
        "an assistive technology's activation presses it"
    );

    let node = driver
        .a11y_node_named("Add")
        .expect("the button is in the tree");
    driver
        .perform_a11y_action(node, &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(
        presses.get(),
        3,
        "an action on its exported node presses it"
    );

    let id = driver
        .semantic_tree()
        .find_by_name("Add")
        .expect("the button is in the window's semantics")
        .id
        .clone();
    assert!(
        driver
            .perform_semantic(&id, &tilo_ui_base::a11y::Action::Activate)
            .unwrap(),
        "the button takes the action on its semantic node"
    );
    assert_eq!(
        presses.get(),
        4,
        "an action on its semantic node presses it, as a renderer of the semantics runs it"
    );

    let stops = driver.tab_through();
    let names: Vec<_> = stops
        .iter()
        .filter_map(|(_, name)| name.as_deref())
        .collect();
    assert_eq!(names, ["Add", "Other"], "Tab walks the controls in order");
}

/// A field that asks for the keyboard in the tree it is built into, once, as a page whose view
/// marks a field `autofocus` does.
struct AsksForTheKeyboard {
    rect: Rect,
    focused: bool,
    asked: bool,
}

impl AsksForTheKeyboard {
    fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            focused: false,
            asked: false,
        }
    }
}

impl Widget for AsksForTheKeyboard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: taffy::Dimension::length(80.0),
                height: taffy::Dimension::length(20.0),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn name(&self) -> &'static str {
        "AsksForTheKeyboard"
    }

    fn test_id(&self) -> Option<&str> {
        Some("field")
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn take_focus_request(&mut self) -> Option<String> {
        (!std::mem::replace(&mut self.asked, true)).then(|| "field".to_owned())
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

#[test]
fn a_widget_that_asks_for_the_keyboard_when_it_arrives_gets_it() {
    let mut driver = Driver::new(
        State::new(),
        Box::new(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(crate::widget::Text::new("waiting"))),
        ),
        (WIDTH, HEIGHT),
    );
    assert!(driver.focused().is_none());

    // A page arrives with a field that asks for the keyboard in the tree it is built into.
    let replaced = driver
        .with_widget_mut(|page: &mut Container| {
            page.replace_child(Box::new(AsksForTheKeyboard::new()))
        })
        .expect("the page is the driver's root");
    assert!(replaced.is_some(), "the page swapped its view");
    let focused = driver
        .with_widget_mut(|field: &mut AsksForTheKeyboard| field.focused)
        .expect("the field is in the tree");
    assert!(focused, "the field that asked for the keyboard has it");
    assert!(
        driver.focused().is_some(),
        "the window knows where the keyboard is"
    );

    // A rebuild keeps the keyboard where it was.
    driver.recompose().unwrap();
    let focused = driver
        .with_widget_mut(|field: &mut AsksForTheKeyboard| field.focused)
        .expect("the field is still in the tree");
    assert!(focused, "a rebuild keeps the keyboard where it was");
}

/// A driver fits the stack a test thread has by default: built, shown as a window with its bar,
/// pressed and moved by value, as a test's helpers move it, on a thread of 2 MiB.
#[test]
fn a_driver_runs_a_window_on_a_default_test_thread_stack() {
    let ran = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let presses = Rc::new(Cell::new(0));
            let counted = presses.clone();
            let column = Column::new().push(Box::new(
                Button::new("add", "Add").on_click(move || counted.set(counted.get() + 1)),
            ));
            let mut driver = Driver::new(State::new(), Box::new(column), (400, 300));
            driver.present_in_window_bar();
            let mut moved = vec![driver];
            let mut driver = moved.pop().expect("the driver moved back out");
            driver.click_named("add").unwrap();
            driver.relayout();
            presses.get()
        })
        .expect("a thread starts")
        .join()
        .expect("the driver ran within the thread's stack");
    assert_eq!(ran, 1, "the window's button pressed once");
}

// ---- the pointer shape a surface asks for ----

/// A text field's I-beam where the pointer is over it, the arrow where nothing asks for another:
/// the one rule every host shares.
#[test]
fn a_text_field_shows_the_text_cursor_through_the_shared_rule() {
    use winit::window::CursorIcon;
    let root = Column::new().push(Box::new(TextInput::new(vec![
        tilo_ui_base::PathSegment::Field("draft".into()),
    ])));
    let mut driver = Driver::new(State::new(), Box::new(root), (240, 160));
    driver.relayout();
    let over = (1..80)
        .map(|y| Point::new(60.0, f64::from(y)))
        .find(|&p| driver.cursor_at(p) == Some(CursorIcon::Text))
        .expect("the field asks for the text cursor");
    let scale = driver.input_scale();
    driver.send_host_event(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(over.x * scale, over.y * scale),
    });
    assert_eq!(driver.pointer_cursor(), CursorIcon::Text);
    driver.send_host_event(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(60.0 * scale, 150.0 * scale),
    });
    assert_eq!(driver.pointer_cursor(), CursorIcon::Default);
}

/// A card that builds its contents again while it draws, as a panel opening the card it was asked
/// for does, and says so: what it let go of is kept aside, so a read through the old nodes finds
/// them out of the tree instead of in freed memory.
struct RebuildsWhileDrawing {
    child: Box<dyn Widget>,
    let_go: Vec<Box<dyn Widget>>,
    rebuild: bool,
    replaced: bool,
    rect: Rect,
}

fn draft_card() -> Box<dyn Widget> {
    Box::new(crate::widget::Row::new().push(Box::new(TextInput::new(vec![
        tilo_ui_base::PathSegment::Field("draft".into()),
    ]))))
}

impl Widget for RebuildsWhileDrawing {
    fn name(&self) -> &str {
        "rebuilds-while-drawing"
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
            self.child.apply_layout(taffy, child, origin);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
        if std::mem::take(&mut self.rebuild) {
            self.let_go
                .push(std::mem::replace(&mut self.child, draft_card()));
            self.replaced = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.replaced)
    }
}

/// The layout maps' pointers to widgets the live tree no longer holds: none once a frame took up
/// what a widget rebuilt while drawing.
fn out_of_tree(driver: &Driver) -> usize {
    fn walk(widget: &dyn Widget, live: &mut Vec<usize>) {
        live.push(std::ptr::from_ref(widget).cast::<()>() as usize);
        for child in widget.children() {
            walk(child, live);
        }
    }
    let mut live = Vec::new();
    walk(driver.app().root_widget.as_ref(), &mut live);
    driver
        .app()
        .layout
        .node_map
        .values()
        .filter(|pointer| !live.contains(&(pointer.cast::<()>() as usize)))
        .count()
}

/// A widget that built its children again while the frame drew leaves the frame's layout maps
/// naming the ones it let go of; input read afterwards rebuilds them and reaches no freed widget.
#[test]
fn input_after_a_widget_rebuilt_while_drawing_reads_no_widget_it_let_go_of() {
    let root = RebuildsWhileDrawing {
        child: draft_card(),
        let_go: Vec::new(),
        rebuild: false,
        replaced: false,
        rect: Rect::ZERO,
    };
    let mut driver = Driver::for_root(State::new(), root, (240, 160), Theme::dark());
    driver.set_coalesce_input_frames(true);
    driver.relayout();
    assert_eq!(out_of_tree(&driver), 0);
    // The frame the guard runs on drop draws the card rebuilt, after laying out the old one.
    driver.root_mut::<RebuildsWhileDrawing>().rebuild = true;
    assert!(
        out_of_tree(&driver) > 0,
        "the frame drew a rebuilt card after laying out the old one"
    );
    driver.send_host_event(&WindowEvent::ModifiersChanged(
        winit::event::Modifiers::default(),
    ));
    assert_eq!(
        out_of_tree(&driver),
        0,
        "input read the layout maps through widgets that were let go of"
    );
    let _ = driver.pointer_cursor();
}

// ---- a painted affordance a tool finds and presses ----

/// A widget that paints one named affordance inside its own rect instead of composing a child for
/// it, as a strip's labels and a window's tabs do.
struct Painted {
    rect: Rect,
    presses: Rc<Cell<usize>>,
}

impl Widget for Painted {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size::from_lengths(200.0, 100.0),
            ..Default::default()
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
            self.presses.set(self.presses.get().saturating_add(1));
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "Painted"
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        vec![(
            "painted_part".to_owned(),
            Rect::new(20.0, 20.0, 100.0, 80.0),
        )]
    }
}

#[test]
fn a_painted_affordance_is_a_target_and_a_press_on_it_reaches_the_widget() {
    let presses = Rc::new(Cell::new(0));
    let mut driver = Driver::for_root(
        State::new(),
        Painted {
            rect: Rect::ZERO,
            presses: Rc::clone(&presses),
        },
        (320, 200),
        Theme::dark(),
    );
    driver.relayout();
    let targets = driver.targets();
    assert_eq!(
        targets.get("painted_part").copied(),
        Some(Rect::new(20.0, 20.0, 100.0, 80.0)),
        "the part the widget paints is named where it stands: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    driver
        .click_named("painted_part")
        .expect("a press on the named part");
    assert_eq!(
        presses.get(),
        1,
        "the press at the part's center reached the widget that paints it"
    );
}

// ---- text input: selection keys and touch ----

fn controlled_field(name: &str, value: &str) -> Driver {
    Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(TextInput::local().with_id(name).with_initial_value(value)),
        (400, 200),
        Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_ui_base::permissions::MemberId::person("fixture-user"),
    )
}

#[test]
fn select_all_on_a_russian_source_selects_the_field() {
    let mut driver = controlled_field("name", "hello");
    driver.relayout();
    driver.set_platform_conventions(tilo_ui_base::platform::PlatformConventions::new(
        tilo_ui_base::platform::Platform::Linux,
    ));
    driver
        .click_named("name")
        .expect("the field takes the pointer");
    assert_eq!(
        driver.focused(),
        driver.named_target_nodes().get("name").copied(),
        "the field holds the keyboard before the selection key"
    );
    driver.set_modifiers(&["ctrl".to_owned()]);
    let mut select_all = tilo_ui_base::synthetic_input::key_event(
        winit::keyboard::Key::Character("ф".into()),
        Some("ф".into()),
        winit::event::ElementState::Pressed,
    );
    select_all.physical_key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
    driver.send_host_event(&tilo_ui_base::synthetic_input::keyboard_input(select_all));
    driver.set_modifiers(&[]);
    driver.send_host_event(&WindowEvent::Ime(winit::event::Ime::Commit("я".into())));
    assert_eq!(
        driver
            .access_node_named("name", None)
            .unwrap()
            .value
            .as_deref(),
        Some("я"),
        "Ctrl+A selected the field, so the next letter replaced it"
    );
}

#[test]
fn a_focus_asked_of_the_field_takes_the_keyboard_it_types_into() {
    let submitted = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    let field = TextInput::local()
        .with_id("field")
        .on_submit(move |text| sink.borrow_mut().push(text.to_owned()));
    let mut driver = Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(field),
        (400, 200),
        Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_ui_base::permissions::MemberId::person("fixture-user"),
    );
    driver.relayout();
    driver
        .perform_named("field", &tilo_ui_base::a11y::Action::Focus)
        .expect("the field takes a focus asked of it");
    assert_eq!(
        driver.focused(),
        driver.named_target_nodes().get("field").copied(),
        "the field holds the keyboard the focus asked for"
    );
    driver.type_text("hello");
    driver.key("Enter");
    assert_eq!(
        *submitted.borrow(),
        ["hello".to_owned()],
        "what was typed reached the field, and Enter submitted it"
    );
}

#[test]
fn a_tap_places_the_caret_and_a_double_tap_selects_the_word() {
    let caret_at = |taps: usize| {
        let mut driver = controlled_field("field", "hello world");
        driver.relayout();
        let rect = driver.targets()["field"];
        let (x, y) = (rect.x0 + 24.0, rect.center().y);
        if taps == 1 {
            driver.tap_at(x, y);
        } else {
            driver.double_tap_at(x, y);
        }
        driver
            .access_node_named("field", None)
            .unwrap()
            .text_selection
            .expect("a focused field reports its caret")
    };

    let caret = caret_at(1);
    assert_eq!(caret.anchor, caret.focus, "one tap selected {caret:?}");
    assert!(caret.focus > 0 && caret.focus < 5, "{caret:?}");
    assert_eq!(
        caret_at(2),
        tilo_ui_base::a11y::AccessibleTextSelection {
            anchor: 0,
            focus: 5
        }
    );
}

// ---- themes and the controls a window retains ----

/// A theme change repaints everything on the next frame, retained caches included: a window
/// switched from dark to light renders exactly what a window opened in light renders.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn switching_theme_repaints_retained_controls_as_a_fresh_window_would() {
    fn controls() -> Box<dyn Widget> {
        Box::new(
            Column::new()
                .push(Box::new(
                    Column::new()
                        .push(Box::new(crate::widget::Checkbox::bound("a", "a")))
                        .push(Box::new(crate::widget::Checkbox::bound("b", "b"))),
                ))
                .push(Box::new(crate::widget::ProgressBar::new(0.4))),
        )
    }
    let mut switched = Driver::new(State::new(), controls(), (240, 160));
    switched.relayout();
    switched.set_theme(Theme::light());
    let fresh = Driver::new_with_theme(State::new(), controls(), (240, 160), Theme::light());
    let (switched, fresh) = (switched.render_rgba(), fresh.render_rgba());
    assert!(!fresh.is_empty());
    let differing = switched
        .chunks_exact(4)
        .zip(fresh.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differing, 0,
        "{differing} pixels still show the dark theme after switching to light"
    );
}

// ---- the pen behind each pointer event ----

/// Records what each pointer event it handles says about the pen.
struct PenProbe {
    rect: Rect,
    seen: Rc<std::cell::RefCell<Vec<(&'static str, Option<tilo_ui_base::widget::PenSample>)>>>,
}

impl Widget for PenProbe {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        let kind = match event {
            WindowEvent::CursorMoved { .. } => "move",
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => "press",
            WindowEvent::MouseInput { .. } => "release",
            WindowEvent::PenSample { .. } => "sample",
            _ => return,
        };
        self.seen.borrow_mut().push((kind, ctx.pen()));
    }

    fn name(&self) -> &'static str {
        "PenProbe"
    }
}

fn pen_sample(pressure: f64, eraser: bool) -> WindowEvent {
    WindowEvent::PenSample {
        device_id: winit::event::DeviceId::dummy(),
        pressure,
        tilt: (25.0, -40.0),
        eraser,
    }
}

fn pen(pressure: f64, eraser: bool) -> Option<tilo_ui_base::widget::PenSample> {
    Some(tilo_ui_base::widget::PenSample {
        pressure,
        tilt: (25.0, -40.0),
        eraser,
    })
}

#[test]
fn a_widget_reads_the_pen_behind_each_pointer_event_and_none_for_a_mouse() {
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut driver = Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(PenProbe {
            rect: Rect::ZERO,
            seen: Rc::clone(&seen),
        }),
        (320, 200),
        Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_ui_base::permissions::MemberId::person("pen-review"),
    );
    let button = |state| tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left);
    for event in [
        pen_sample(0.0, false),
        tilo_ui_base::synthetic_input::cursor_moved(40.0, 50.0),
        pen_sample(0.3, false),
        button(ElementState::Pressed),
        pen_sample(0.8, false),
        tilo_ui_base::synthetic_input::cursor_moved(60.0, 70.0),
        pen_sample(0.0, false),
        button(ElementState::Released),
    ] {
        driver.send_host_event(&event);
    }
    driver.send_host_event(&pen_sample(0.5, true));
    driver.send(&tilo_ui_base::synthetic_input::cursor_moved(80.0, 90.0));
    driver.send_host_event(&tilo_ui_base::synthetic_input::cursor_moved(100.0, 110.0));
    driver.send_host_event(&button(ElementState::Pressed));

    assert_eq!(
        *seen.borrow(),
        [
            ("move", pen(0.0, false)),
            ("press", pen(0.3, false)),
            ("move", pen(0.8, false)),
            ("release", pen(0.0, false)),
            ("move", pen(0.5, true)),
            ("move", None),
            ("press", None),
        ],
        "the samples themselves never reach a widget as events"
    );
}

// ---- a named control with no place ----

/// A widget that holds content it never lays out, as a page whose frame never placed it reads:
/// only the accessibility box around its control is left, and that box sits at the origin.
struct Unplaced {
    content: Banner,
    rect: Rect,
}

impl Widget for Unplaced {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size::from_lengths(240.0, 160.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.content);
    }

    fn name(&self) -> &'static str {
        "Unplaced"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[test]
fn a_named_control_with_no_laid_out_place_is_no_target_and_is_never_pressed_at_the_origin() {
    let content =
        Banner::warning("No page").with_secondary_action(Button::new("unplaced.control", "Go on"));
    let mut driver = Driver::for_root(
        State::new(),
        Unplaced {
            content,
            rect: Rect::ZERO,
        },
        (240, 160),
        Theme::dark(),
    );
    driver.relayout();
    assert!(
        !driver.targets().contains_key("unplaced.control"),
        "a control nothing laid out has no place to press: {:?}",
        driver.targets().get("unplaced.control")
    );
    let error = driver
        .click_named("unplaced.control")
        .expect_err("no place to click");
    assert!(
        error.contains("no addressable widget"),
        "the click names what it could not reach, and presses nothing: {error}"
    );
}
