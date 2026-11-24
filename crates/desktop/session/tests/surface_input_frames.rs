//! A window's host presents on its own frame clock: the events of one keystroke reach the tree
//! and the tree runs one frame for them, the frame the display's clock draws, rather than one
//! frame per event. A drag that reaches the tree with no input of its own asks for a frame too, and
//! a tree on a second display sees it where it is on that display.

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_session_core::surface::{KeyModifiers, ShellSurface as _, SurfaceKey};

use tilo_session::platform::surfaces::{Placement, WidgetSurface};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{EventCtx, TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, WindowEvent};

/// A control that keeps what it is typed, as a field does.
#[derive(Default)]
struct Field {
    typed: Rc<std::cell::RefCell<String>>,
    rect: ui::kurbo::Rect,
    modal: bool,
}

impl Widget for Field {
    fn name(&self) -> &'static str {
        "Field"
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn modal_capture(&self) -> Option<ui::kurbo::Rect> {
        self.modal.then_some(self.rect)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: ui::taffy::Size {
                    width: ui::taffy::Dimension::percent(1.0),
                    height: ui::taffy::Dimension::length(40.0),
                },
                ..Style::default()
            },
            nodes,
            self,
        )
    }

    ui::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && let Some(text) = &event.text
        {
            self.typed.borrow_mut().push_str(text);
            *ctx.handled = true;
        }
    }
}

/// One key as the compositor sends it, with the text it types on the press.
fn key(pressed: bool, text: &str) -> SurfaceKey {
    let keysym = u32::from(text.bytes().next().unwrap_or(b' '));
    SurfaceKey {
        keycode: keysym,
        at: std::time::Instant::now(),
        pressed,
        repeat: false,
        keysym,
        unmodified_keysym: keysym,
        latin_keysym: keysym,
        text: Some(text.to_owned()),
        modifiers: KeyModifiers::default(),
    }
}

#[test]
fn an_open_modal_claims_its_layout_bounds_without_painted_pixels() {
    let mut host = WidgetSurface::new(
        Box::new(Field {
            modal: true,
            ..Field::default()
        }),
        Placement {
            origin: (120, 80),
            size: (320, 240),
            scale: 1.5,
        },
        false,
    );
    assert!(
        host.prepare_frame(),
        "the field is laid out on a transparent surface"
    );
    assert!(
        host.accepts_pointer(160.0, 115.0),
        "the field keeps a press in the open modal, even on a transparent frame"
    );
    assert!(
        !host.accepts_pointer(160.0, 135.0),
        "a press outside the modal still goes to the surface behind it"
    );
}

#[test]
fn a_surface_that_loses_the_keyboard_draws_again_without_its_carets() {
    let _settled = ui::motion::testing::settled();
    let holds = Rc::new(Cell::new(true));
    let field = Field::default();
    let mut host = WidgetSurface::new(
        Box::new(field),
        Placement {
            origin: (0, 0),
            size: (320, 240),
            scale: 1.0,
        },
        false,
    )
    .with_keyboard({
        let holds = Rc::clone(&holds);
        Rc::new(move || holds.get())
    });
    assert!(host.prepare_frame(), "the window's first frame draws it");
    assert!(
        host.host_mut().keyboard_held(),
        "the field holds the keyboard"
    );
    let drawn = host.revision();

    // Another surface took the keyboard: the tree keeps its text, gives up the caret, and draws
    // again, so the frame the person sees has no caret in it.
    holds.set(false);
    assert!(
        host.prepare_frame(),
        "the surface draws the frame its caret left"
    );
    assert_eq!(host.revision(), drawn + 1);
    assert!(!host.host_mut().keyboard_held());

    // The keyboard comes back: the caret comes back with it.
    holds.set(true);
    assert!(host.prepare_frame(), "the caret's frame is drawn again");
    assert!(host.host_mut().keyboard_held());
    assert_eq!(host.revision(), drawn + 2);
}

#[test]
fn a_keystroke_reaches_one_frame_of_the_hosts_clock() {
    let _settled = ui::motion::testing::settled();
    let field = Field::default();
    let typed = Rc::clone(&field.typed);
    let mut host = WidgetSurface::new(
        Box::new(field),
        Placement {
            origin: (0, 0),
            size: (320, 240),
            scale: 1.0,
        },
        false,
    );
    assert!(host.prepare_frame(), "the window's first frame draws it");
    let drawn = host.revision();
    // The key down and the key up of one character: the tree takes them in at once, and the
    // frame the host's clock runs draws what they typed.
    host.key(&key(true, "a"));
    host.key(&key(false, "a"));
    assert_eq!(typed.borrow().as_str(), "a", "the key reached the field");
    assert_eq!(
        host.revision(),
        drawn,
        "input drew no frame of its own: the host's clock draws the frame"
    );
    assert!(host.wants_frame(), "the typing asked for a frame");
    assert!(
        host.prepare_frame(),
        "the clock's frame takes the keystroke in"
    );
    assert_eq!(host.revision(), drawn + 1, "one frame, not one per event");
    assert!(!host.prepare_frame(), "and the window rests after it");
}

/// A drop target shows where a drag would land however the drag reaches it: one carried here
/// from another of the person's devices moves on the toolkit's drag bus with no input of the
/// tree's, and the host still draws the line over the target, and takes it away again.
#[test]
fn a_drag_that_reaches_the_tree_without_its_input_shows_where_it_lands() {
    let _settled = ui::motion::testing::settled();
    let target = ui::widget::DropTarget::new(
        "window",
        Box::new(Field::default()),
        ui::component::Event::None,
    );
    let mut host = WidgetSurface::new(
        Box::new(target),
        Placement {
            origin: (0, 0),
            size: (320, 240),
            scale: 1.0,
        },
        true,
    );
    assert!(host.prepare_frame(), "the first frame draws the tree");
    assert!(!host.prepare_frame(), "and the tree rests after it");
    let drawn = host.revision();

    // A window's record goes on over the target from elsewhere: nothing the tree was handed moved.
    ui::widget::drag_bus::publish(ui::widget::drag_bus::InFlight {
        source: "carried-here".into(),
        payload: ui::dnd::DragPayload::Custom {
            kind: ui::widget::RESOURCE_PAYLOAD.to_owned(),
            blob: b"notes.page:one".to_vec(),
        },
        label: "Notes".into(),
        pointer: ui::kurbo::Point::new(100.0, 20.0),
        runs: Vec::new(),
    });
    assert!(host.wants_frame(), "the drag asks the tree for a frame");
    assert!(
        host.prepare_frame(),
        "the frame draws the drop line over the target"
    );
    assert!(host.revision() > drawn);
    assert!(
        !host.prepare_frame(),
        "and rests while the drag stands still"
    );

    // The drag goes on past the tree: the line goes with it.
    ui::widget::drag_bus::moved(
        ui::kurbo::Point::new(400.0, 20.0),
        ("carried-here".to_owned(), ui::kurbo::Rect::ZERO),
    );
    assert!(
        host.prepare_frame(),
        "the frame takes the drop line away again"
    );
    ui::widget::drag_bus::end(None);
    assert!(
        !host.prepare_frame(),
        "a drag ending away from the target changes nothing it shows"
    );
}

/// A drop target on each of two displays side by side, as a desktop shows one per display: a drag
/// carried here over the second display, in the desktop's own pixels, draws the line over the
/// second display's target alone and lands there, not on the first display's.
#[test]
fn a_drag_over_the_second_display_shows_and_lands_on_that_display() {
    let _settled = ui::motion::testing::settled();
    let host = |x: i32| {
        let mut host = WidgetSurface::new(
            Box::new(ui::widget::DropTarget::new(
                "window",
                Box::new(Field::default()),
                ui::component::Event::None,
            )),
            Placement {
                origin: (x, 0),
                size: (320, 240),
                scale: 1.0,
            },
            true,
        );
        assert!(host.prepare_frame(), "the first frame draws the tree");
        assert!(!host.prepare_frame(), "and the tree rests after it");
        host
    };
    let mut first = host(0);
    let mut second = host(320);
    let payload = ui::dnd::DragPayload::Custom {
        kind: ui::widget::RESOURCE_PAYLOAD.to_owned(),
        blob: b"notes.page:one".to_vec(),
    };
    // The drag comes in away from both displays' targets, and both settle with it in flight.
    ui::widget::drag_bus::publish(ui::widget::drag_bus::InFlight {
        source: "carried-here".into(),
        payload: payload.clone(),
        label: "Notes".into(),
        pointer: ui::kurbo::Point::new(5000.0, 5000.0),
        runs: Vec::new(),
    });
    first.prepare_frame();
    second.prepare_frame();
    assert!(!first.prepare_frame() && !second.prepare_frame());

    // It moves over the second display.
    let over_second = ui::kurbo::Point::new(420.0, 20.0);
    ui::widget::drag_bus::moved(
        over_second,
        ("carried-here".to_owned(), ui::kurbo::Rect::ZERO),
    );
    assert!(
        !first.prepare_frame(),
        "the first display shows no line: the drag is not over it"
    );
    assert!(
        second.prepare_frame(),
        "the second display draws the line over its target"
    );

    // Let go there: the first display's pass comes first and takes nothing.
    ui::widget::drag_bus::end(Some(ui::widget::Landed {
        source: "carried-here".into(),
        payload,
        pointer: over_second,
    }));
    first.prepare_frame();
    assert!(
        ui::widget::drag_bus::take("probe", ui::kurbo::Rect::new(-1e6, -1e6, 1e6, 1e6))
            .is_some_and(|landed| {
                // Put back for the second display's pass.
                ui::widget::drag_bus::end(Some(landed));
                true
            }),
        "the first display left the drop for the display it was let go over"
    );
    second.prepare_frame();
    assert!(
        ui::widget::drag_bus::take("probe", ui::kurbo::Rect::new(-1e6, -1e6, 1e6, 1e6)).is_none(),
        "the second display's target took the drop"
    );
}

/// A tree that asks for animation frames that come out the same, as a hint does that moves
/// nothing this surface draws.
struct Still;

impl Widget for Still {
    fn name(&self) -> &'static str {
        "Still"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), nodes, self)
    }

    fn needs_animation_frame(&self) -> bool {
        true
    }
}

/// An animation frame that changed no pixel still takes its display frame: the next is due a
/// display frame later, not at once, so a still animation does not turn the shell's loop as fast as
/// it can and slow every app opening beside it.
#[test]
fn an_animation_frame_that_changes_nothing_waits_for_the_next_display_frame() {
    let placement = Placement {
        origin: (0, 0),
        size: (320, 240),
        scale: 1.0,
    };
    let mut host = WidgetSurface::new(Box::new(Still), placement, false);
    assert!(host.prepare_frame(), "the first frame draws the tree");
    // The display holds each frame a second from here on; the grid of the first frame has passed.
    let frame = std::time::Duration::from_secs(1);
    host.set_frame_interval(frame);
    let turn = std::time::Instant::now();
    assert!(!host.prepare_frame(), "the frame shows nothing new");
    let due = host.next_deadline().expect("the animation asks for frames");
    assert!(due >= turn + frame, "the next frame waits for the display");
}
