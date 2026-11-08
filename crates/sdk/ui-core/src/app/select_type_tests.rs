//! Select and type: a printable key nothing in the window took, with a
//! selection in focus, opens a send-to box about it with the key in its
//! message.

use super::*;
use crate::presence::capability::PublishesPresence;
use crate::shared_selection::{SharedSelection, set_send_host_available, take_open_send_box};

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
    let _ = take_open_send_box();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(SelectedRow));
    app.init_layout();
    set_send_host_available(false);
    assert!(
        !app.handle_unconsumed_typed_key(&typed("s")),
        "without a desktop agent a key is nobody's"
    );
    set_send_host_available(true);
    assert!(
        !app.handle_unconsumed_typed_key(&typed(" ")),
        "a space starts nothing"
    );
    assert!(app.handle_unconsumed_typed_key(&typed("s")));
    set_send_host_available(false);
    let opened = take_open_send_box().expect("the box was asked for");
    assert_eq!(
        opened.draft.as_deref(),
        Some("s"),
        "the key begins the message"
    );
    assert_eq!(
        opened
            .about
            .as_ref()
            .and_then(|about| about.anchor.place.clone()),
        Some(tilo_actions::SelectionPlace::item("row-3"))
    );
    assert!(take_open_send_box().is_none(), "asked for once");
}

/// A report asked of this window is a box about what it picked, addressed to
/// the place the shell named: the person writes what went wrong and sends it.
#[cfg(feature = "native")]
#[test]
fn a_report_asked_of_a_window_opens_the_box_about_what_it_picked() {
    use crate::floating_surface::{FloatingSurfaceHostMessage, Recipient, RecipientKind};

    let _ = take_open_send_box();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(SelectedRow));
    app.init_layout();
    let destination = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: vec![destination.clone()],
        }])
    );
    let opened = take_open_send_box().expect("the box was asked for");
    assert_eq!(
        opened
            .about
            .as_ref()
            .and_then(|about| about.anchor.place.clone()),
        Some(tilo_actions::SelectionPlace::item("row-3")),
        "what the window picked goes with the report"
    );
    assert_eq!(
        opened.to,
        vec![destination],
        "and it goes where the shell said"
    );
    assert_eq!(opened.draft, None, "the words are left to write");
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
    let _ = take_open_send_box();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Plain));
    app.init_layout();
    set_send_host_available(true);
    assert!(!app.handle_unconsumed_typed_key(&typed("s")));
    set_send_host_available(false);
    assert!(take_open_send_box().is_none());
}
