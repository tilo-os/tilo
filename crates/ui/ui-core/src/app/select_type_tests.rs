//! Select and type: a printable key nothing in the window took, with a
//! selection in focus, opens a send-to box about it with the key in its
//! message.

use super::*;
use crate::presence::capability::PublishesPresence;
use crate::shared_selection::{SharedSelection, set_send_host_available};

/// A list whose third row is selected.
struct SelectedRow;

impl Widget for SelectedRow {
    crate::provides!(dyn PublishesPresence);
    crate::testing::inert_widget_methods!(leaf_layout);
    fn name(&self) -> &'static str {
        "selected-row"
    }
}

impl PublishesPresence for SelectedRow {
    fn shared_selection(&self) -> Option<SharedSelection> {
        Some(
            SharedSelection::of_text("Invoice from Ada")
                .at(tilo_actions::SelectionPlace::item("row-3")),
        )
    }
}

fn typed(text: &str) -> winit::event::WindowEvent {
    crate::synthetic_input::char_key(text)
}

#[cfg(feature = "native")]
#[test]
fn a_key_nothing_took_over_a_selection_opens_the_box_with_it() {
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(SelectedRow));
    app.init_layout();
    set_send_host_available(false);
    assert!(
        !app.handle_unconsumed_typed_key(&typed("s")),
        "without a shell agent a key is nobody's"
    );
    set_send_host_available(true);
    assert!(
        !app.handle_unconsumed_typed_key(&typed(" ")),
        "a space starts nothing"
    );
    assert!(app.handle_unconsumed_typed_key(&typed("s")));
    set_send_host_available(false);
    assert!(app.window_box.is_open(), "the box was asked for");
    let opened = app.window_box.send_to();
    assert_eq!(opened.text(), "s", "the key begins the message");
    assert_eq!(
        opened
            .subjects()
            .first()
            .and_then(|about| about.anchor.place.clone()),
        Some(tilo_actions::SelectionPlace::item("row-3"))
    );
}

/// A report asked of this window is a box about what it picked, addressed to
/// the place the shell named: the person writes what went wrong and sends it.
#[cfg(feature = "native")]
#[test]
fn a_report_asked_of_a_window_opens_the_box_about_what_it_picked() {
    use crate::floating_surface::{FloatingSurfaceHostMessage, Recipient, RecipientKind};

    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(SelectedRow));
    app.init_layout();
    let destination = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: vec![destination.clone()],
            pointing: false,
        }])
    );
    assert!(app.window_box.is_open(), "the box was asked for");
    let opened = app.window_box.send_to();
    assert_eq!(
        opened
            .subjects()
            .first()
            .and_then(|about| about.anchor.place.clone()),
        Some(tilo_actions::SelectionPlace::item("row-3")),
        "what the window picked goes with the report"
    );
    assert_eq!(
        opened.recipients(),
        [destination],
        "and it goes where the shell said"
    );
    assert_eq!(opened.text(), "", "the words are left to write");
}

#[cfg(feature = "native")]
#[test]
fn with_nothing_selected_a_key_asks_nothing() {
    struct Plain;
    impl Widget for Plain {
        crate::testing::inert_widget_methods!(leaf_layout);
        fn name(&self) -> &'static str {
            "plain"
        }
    }
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Plain));
    app.init_layout();
    set_send_host_available(true);
    assert!(!app.handle_unconsumed_typed_key(&typed("s")));
    set_send_host_available(false);
    assert!(!app.window_box.is_open());
}

/// Only the field the keys go to paints a caret: while the box stands, the
/// window's own fields keep their place and paint none.
#[cfg(feature = "native")]
#[test]
fn the_window_paints_no_caret_under_the_box_that_has_the_keys() {
    use std::cell::Cell;
    use std::rc::Rc;
    struct CaretProbe(Rc<Cell<Option<bool>>>);
    impl Widget for CaretProbe {
        crate::testing::inert_widget_methods!(leaf_layout);
        fn name(&self) -> &'static str {
            "caret-probe"
        }
        fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (_, _, _, _, mut env) = ctx.parts();
            self.0.set(Some(crate::caret_owner::shown()));
        }
    }
    let carets = Rc::new(Cell::new(None));
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(CaretProbe(Rc::clone(&carets))));
    app.init_layout();
    let draw = |app: &mut App| {
        carets.set(None);
        draw_frame_content(app, &mut vello::Scene::new(), 600, 400, 1.0, None);
        carets.get().expect("the window drew its widgets")
    };
    assert!(draw(&mut app), "the window has the keys: a caret");
    app.open_send_box(None, None, None, &[]);
    assert!(!draw(&mut app), "the box has the keys: its caret only");
    app.window_box.close();
    assert!(draw(&mut app), "the keys came back, and so did the caret");
}
