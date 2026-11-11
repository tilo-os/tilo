//! A window's host presents on its own frame clock: the events of one keystroke reach the tree
//! and the tree runs one frame for them, the frame the display's clock draws, rather than one
//! frame per event.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_session_core::surface::{HostKey, KeyModifiers, SurfaceHost as _};

use tilo_surface_host::{Placement, WidgetHost};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{EventCtx, TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, WindowEvent};

/// A control that keeps what it is typed, as a field does.
#[derive(Default)]
struct Field {
    typed: Rc<std::cell::RefCell<String>>,
    rect: ui::kurbo::Rect,
}

impl Widget for Field {
    fn name(&self) -> &'static str {
        "Field"
    }

    fn is_focusable(&self) -> bool {
        true
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
fn key(pressed: bool, text: &str) -> HostKey {
    let keysym = u32::from(text.bytes().next().unwrap_or(b' '));
    HostKey {
        keycode: keysym,
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
fn a_surface_that_loses_the_keyboard_draws_again_without_its_carets() {
    let _settled = ui::motion::testing::settled();
    let holds = Rc::new(Cell::new(true));
    let field = Field::default();
    let mut host = WidgetHost::new(
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
    let mut host = WidgetHost::new(
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
