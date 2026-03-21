use quick_ui::{App, State};
use quick_ui::widget::{Button, Column, Toggle, Checkbox, Slider, TextInput};
use quick_ui::state::RemoteCursor;
use vello::peniko::Color;

fn main() {
    let mut state = State::new();
    
    // Initialize CRDT Document State
    {
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "toggle_1", true);
        let _ = state.doc.put(&state.root, "check_1", false);
        let _ = state.doc.put(&state.root, "slider_1", 0.75);
        let text_id = state.doc.put_object(&state.root, "text_1", automerge::ObjType::Text).unwrap();
        let _ = state.doc.splice_text(&text_id, 0, 0, "Hello Gallery!");
    }
    
    // Add a fake remote user cursor overlay to demonstrate collaboration rendering
    state.collab.cursors.insert("user_2".into(), RemoteCursor {
        x: 100.0,
        y: 100.0,
        color: Color::rgb8(255, 100, 100),
        name: "Alice".into()
    });

    // Build the Widget Tree
    let root_widget = Box::new(
        Column::new()
            .push(Box::new(Button::new("btn_1", "Click Me", Some("counter"))))
            .push(Box::new(Toggle::new("tog_1", "toggle_1")))
            .push(Box::new(Checkbox::new("chk_1", "check_1")))
            .push(Box::new(Slider::new("sld_1", "slider_1")))
            .push(Box::new(TextInput::new("txt_1", "text_1")))
    );
    
    // Run the Application
    let app = App::new(state, root_widget);
    app.run();
}
