//! Select and type: a printable key nothing in the window took, with a
//! selection in focus, opens the desktop's agent composer on the selection
//! with the key in its field.

use super::*;
use crate::presence::capability::PublishesPresence;
use crate::shared_selection::{SharedSelection, set_agent_host_available, take_agent_requests};

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
fn a_key_nothing_took_over_a_selection_opens_the_composer_with_it() {
    let _ = take_agent_requests();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(SelectedRow));
    app.init_layout();
    set_agent_host_available(false);
    assert!(
        !app.handle_unconsumed_typed_key(&typed("s")),
        "without a desktop agent a key is nobody's"
    );
    set_agent_host_available(true);
    assert!(
        !app.handle_unconsumed_typed_key(&typed(" ")),
        "a space starts nothing"
    );
    assert!(app.handle_unconsumed_typed_key(&typed("s")));
    set_agent_host_available(false);
    let asked = take_agent_requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].draft.as_deref(), Some("s"));
    assert_eq!(asked[0].ask, None, "begun, not asked");
    assert_eq!(
        asked[0]
            .about
            .as_ref()
            .and_then(|about| about.anchor.place.clone()),
        Some(tilo_actions::SelectionPlace::item("row-3"))
    );
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
    let _ = take_agent_requests();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Plain));
    app.init_layout();
    set_agent_host_available(true);
    assert!(!app.handle_unconsumed_typed_key(&typed("s")));
    set_agent_host_available(false);
    assert!(take_agent_requests().is_empty());
}
