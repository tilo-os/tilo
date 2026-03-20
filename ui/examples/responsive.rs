use quick_ui::{App, State};
use quick_ui::widget::{Button, Column, Toggle, Checkbox};

fn main() {
    let mut state = State::new();
    
    // Initialize state
    {
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "toggle_1", true);
        let _ = state.doc.put(&state.root, "check_1", false);
    }

    let root_widget = Box::new(
        Column::new()
            .push(Box::new(Button::new("btn_1", "Click Me", Some("counter"))))
            .push(Box::new(Toggle::new("tog_1", "toggle_1")))
            .push(Box::new(Checkbox::new("chk_1", "check_1")))
    );
    
    let app = App::new(state, root_widget);
    app.run();
}
