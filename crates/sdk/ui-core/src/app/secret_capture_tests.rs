use super::*;
use crate::widget::{Column, TextInput};

fn drawn(field: TextInput) -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Column::new().push(Box::new(field))),
    );
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    run_layout_pass(&mut app, root, 600, 400, 1.0, false, None).unwrap();
    draw_frame_content(&mut app, &mut Scene::new(), 600, 400, 1.0, None);
    app
}

#[test]
fn a_window_showing_a_typed_secret_is_withheld_from_captures() {
    // Case `a_typed_password_withholds_the_window_masked_or_shown`.
    for revealed in [false, true] {
        let mut field = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        if revealed {
            field.toggle_reveal();
        }
        let app = drawn(field);
        assert!(!app.capture_protected(), "the app asked for nothing");
        assert!(app.wants_capture_protection(), "revealed: {revealed}");
    }

    // Case `an_empty_password_or_plain_text_withholds_nothing`.
    for field in [
        TextInput::local().with_secret(true),
        TextInput::local().with_initial_value("hunter2"),
    ] {
        assert!(!drawn(field).wants_capture_protection());
    }

    // Case `an_app_that_asked_stays_withheld`.
    let mut app = drawn(TextInput::local());
    app.set_capture_protected(true);
    assert!(app.wants_capture_protection());
}
