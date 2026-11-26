//! A box the view opens for editing takes the keyboard: the box a person just made, or the one
//! they pressed Enter on.

use super::*;
use automerge::AutoCommit;
use tilo_ui_base::component::Described;
use ui::testing::Dispatch;

/// An editor the view bound to a field, as a slide's text box is.
fn bound_editor(focus: bool) -> BlockEditor {
    let queue = tilo_ui_base::Intents::<tilo_ui_base::component::Fired>::default();
    BlockEditor::from_props(
        "box",
        crate::BlockEditorProps {
            rich: tilo_ui_base::component::Bound {
                edits: Some(tilo_ui_base::component::Intent::new(
                    &queue, "box", "richtext",
                )),
                ..tilo_ui_base::component::Bound::default()
            },
            focus: Some(focus),
            ..crate::BlockEditorProps::default()
        },
    )
}

#[test]
fn the_box_the_view_opens_for_editing_takes_the_keyboard() {
    let (services, _recorder) = ui::process::ProcessServices::recording();
    let _host = ui::process::enter(&services);
    let mut opened = bound_editor(true);
    assert_eq!(
        opened.take_focus_request().as_deref(),
        Some("box"),
        "the editor asks the host for the keyboard"
    );
    assert_eq!(
        opened.take_focus_request(),
        None,
        "and asks once, not every frame"
    );

    // Focused as the host focuses it, the typing is the box's.
    opened.set_blocks(vec![Block::paragraph("Hello")]);
    opened.set_focus(true);
    assert!(opened.is_focused(), "the box holds the keyboard");
    let mut doc = AutoCommit::new();
    let typed = tilo_ui_base::synthetic_input::key_event(
        Key::Character("!".into()),
        Some("!".into()),
        ElementState::Pressed,
    );
    let handled = Dispatch::new()
        .doc(&mut doc)
        .send(
            &mut opened,
            &tilo_ui_base::synthetic_input::keyboard_input(typed),
        )
        .handled;
    assert!(handled, "the typing reached the box the view opened");

    // A box the view did not open for editing asks for nothing: a press is what focuses it.
    let mut shown = bound_editor(false);
    assert_eq!(shown.take_focus_request(), None);
}
