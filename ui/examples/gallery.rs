use quick_ui::{App, State, Theme};
use quick_ui::widget::{Button, Column, Toggle, Checkbox, Slider, TextInput, Text, Widget};
use quick_ui::state::RemoteCursor;
use quick_ui::i18n::I18nManager;
use vello::peniko::Color;

fn main() {
    // 1. Initialize i18n
    I18nManager::init("en-US");
    I18nManager::add_resource("en-US", "btn_1 = Click Me\ncounter = Count is { $count }".to_string());
    I18nManager::add_resource("de-DE", "btn_1 = Klick Mich\ncounter = Zähler ist { $count }".to_string());

    let mut state = State::new();
    
    // 2. Initialize CRDT Document State
    {
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "toggle_1", true);
        let _ = state.doc.put(&state.root, "check_1", false);
        let _ = state.doc.put(&state.root, "slider_1", 0.75);
        let text_id = state.doc.put_object(&state.root, "text_1", automerge::ObjType::Text).unwrap();
        let _ = state.doc.splice_text(&text_id, 0, 0, "Hello Gallery!");
        let _ = state.doc.put(&state.root, "request_theme_cycle", false);
    }
    
    // Add a fake remote user cursor
    state.collab.cursors.insert("user_2".into(), RemoteCursor {
        x: 100.0,
        y: 100.0,
        color: Color::rgb8(255, 100, 100),
        name: "Alice".into()
    });

    // 3. Build the Widget Tree
    let root_widget = Box::new(
        Column::new()
            .push(Box::new(Button::new("btn_1", "btn_1", Some("counter")))) // Uses i18n key "btn_1"
            .push(Box::new(Text::new("counter")))
            .push(Box::new(Button::new("btn_cycle", "Cycle Theme", Some("request_theme_cycle"))))
            .push(Box::new(Toggle::new("tog_1", "toggle_1")))
            .push(Box::new(Checkbox::new("chk_1", "check_1")))
            .push(Box::new(Slider::new("sld_1", "slider_1")))
            .push(Box::new(TextInput::new("txt_1", "text_1")))
    );
    
    // 4. Run the Application
    let app = App::new(state, root_widget);
    app.run();
}
