use quick_ui::{App, State};
use quick_ui::widget::Button;

fn main() {
    let state = State::new();
    let root_widget = Box::new(Button::new("btn_1", "Click Me", Some("counter")));
    
    let app = App::new(state, root_widget);
    app.run();
}
