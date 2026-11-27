//! An app's native input probe reports the fixture's state as it stands, without changing it.
use tilo_ui_app::App;
use tilo_ui_app::State;
use tilo_ui_app::widget::Text;

#[test]
fn effect_probe_reads_current_fixture_state_without_changing_it() {
    let offset = std::rc::Rc::new(std::cell::Cell::new(12_u32));
    let observed = offset.clone();
    let mut app = App::new(State::new(), Box::new(Text::new("probe")));
    assert!(app.native_input_snapshot_for_test().is_none());
    app.set_native_input_probe(move || serde_json::json!({"offset": observed.get()}));
    assert_eq!(app.native_input_snapshot_for_test().unwrap()["offset"], 12);
    offset.set(24);
    assert_eq!(app.native_input_snapshot_for_test().unwrap()["offset"], 24);
    assert_eq!(offset.get(), 24);
}
