//! A key typed while the window's content is on its way waits for it, and reaches it once it
//! stands, with the modifiers held when it was pressed: Ctrl+N pressed as an app opens makes the
//! note it asks for.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::widget::ArrivingView;
use tilo_ui_base::{arriving::Arrival, arriving::Arriving, arriving::Source};
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, ModifiersState};

struct Window;
impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(400, 200)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

/// The keys the content took, each with whether Control was held, as the app's own shortcuts
/// read it.
type Took = Rc<RefCell<Vec<(String, bool)>>>;

/// The content of the window once it arrived: it takes every key pressed in it.
struct Content {
    took: Took,
}

impl Widget for Content {
    crate::testing::inert_widget_methods!(leaf_layout);
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && let Key::Character(text) = &key.logical_key
        {
            self.took
                .borrow_mut()
                .push((text.to_string(), ctx.modifiers.state().control_key()));
            *ctx.handled = true;
        }
    }
    fn name(&self) -> &'static str {
        "content"
    }
}

/// A window whose content arrives with `arriving`, and what its content took.
fn window(arriving: &Arriving<(), String>) -> (App, Took) {
    let took = Took::default();
    let shown = Rc::clone(&took);
    let view = ArrivingView::new(arriving, move |()| -> tilo_ui_base::widget::Element {
        Box::new(Content {
            took: Rc::clone(&shown),
        })
    })
    .holding_input();
    (
        App::new_with_controlled_inputs(crate::State::new(), Box::new(view)),
        took,
    )
}

fn frame(app: &mut App) {
    let mut scene = vello::Scene::new();
    app.surface_frame(&Window, &mut scene);
}

/// Ctrl+N as a person types it: Control held, N pressed and let go, Control let go.
fn ctrl_n(app: &mut App) {
    let n = Key::Character("n".into());
    app.set_surface_modifiers(Modifiers::from(ModifiersState::CONTROL));
    app.surface_input(
        &Window,
        tilo_ui_base::synthetic_input::key_input(&n, ElementState::Pressed),
        false,
    );
    app.surface_input(
        &Window,
        tilo_ui_base::synthetic_input::key_input(&n, ElementState::Released),
        false,
    );
    app.set_surface_modifiers(Modifiers::default());
}

tilo_test_env::scenario! {
    fn a_key_typed_before_the_content_arrived_reaches_it;

    phase content_that_stands_takes_the_key_at_once;
    {
        let (mut app, took) = window(&Arriving::ready(()));
        frame(&mut app);
        ctrl_n(&mut app);
        assert_eq!(*took.borrow(), [("n".to_owned(), true)]);
    }

    phase a_key_pressed_while_it_is_on_its_way_waits_and_then_reaches_it;
    {
        let arriving = Arriving::<(), String>::pending(Source::Local);
        let (mut app, took) = window(&arriving);
        frame(&mut app);
        ctrl_n(&mut app);
        frame(&mut app);
        assert!(took.borrow().is_empty(), "nothing stands to take the key yet");
        arriving.stand(Arrival::Ready(()));
        frame(&mut app);
        frame(&mut app);
        assert_eq!(
            *took.borrow(),
            [("n".to_owned(), true)],
            "the key reached the content once it stood, with Control held as it was pressed"
        );
        ctrl_n(&mut app);
        assert_eq!(took.borrow().len(), 2, "keys after it go straight to the content");
    }
}
