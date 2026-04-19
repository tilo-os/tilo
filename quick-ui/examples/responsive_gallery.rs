use quick_ui::{App, State};
use quick_ui::widget::{Button, Column, Row, Container, Text, Spacer, TextInput};
use quick_ui::i18n::I18nManager;
use std::fs;

fn main() {
    I18nManager::init("en-US");
    I18nManager::add_resource("en-US", "title = Responsive Gallery\nsave = Save Session\nload = Load Session\nwelcome = Welcome to CRDT-Native UI!".to_string());

    let state_file = "session.crdt";
    
    // Try to load existing session, or start new
    let mut state = if let Ok(data) = fs::read(state_file) {
        State::load(&data).unwrap_or_else(|_| State::new())
    } else {
        State::new()
    };

    // Initialize state if new or missing keys
    {
        use automerge::transaction::Transactable;
        use automerge::ReadDoc;
        let _ = state.doc.put(&state.root, "request_save", false);
        let _ = state.doc.put(&state.root, "request_load", false);
        if state.doc.get(&state.root, "text_val").unwrap().is_none() {
            let text_id = state.doc.put_object(&state.root, "text_val", automerge::ObjType::Text).unwrap();
            let _ = state.doc.splice_text(&text_id, 0, 0, "Edit me and restart!");
        }
    }

    let root_widget = Box::new(
        Column::new()
            .push(Box::new(
                Row::new()
                    .push(Box::new(Text::new("title").with_size(24.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new("btn_save", "save", Some("request_save"))))
                    .push(Box::new(Button::new("btn_load", "load", Some("request_load"))))
            ))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(
                        Column::new()
                            .push(Box::new(Text::new("welcome")))
                            .push(Box::new(TextInput::new(vec![]).with_id("inp_1").with_on_change_path("text_val")))
                    ))
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Button::new("b1", "Wrap Item 1", None)))
                    .push(Box::new(Button::new("b2", "Wrap Item 2", None)))
                    .push(Box::new(Button::new("b3", "Wrap Item 3", None)))
                    .push(Box::new(Button::new("b4", "Wrap Item 4", None)))
                    .push(Box::new(Button::new("b5", "Wrap Item 5", None)))
            ))
    );

    let app = App::new(state, root_widget);
    app.run();
}
