use generated_app::{App, Code};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;

#[test]
fn app_generates_an_accessible_visual_from_semantic_input() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .perform_named("qr-input", &Action::SetValue("你好 Rust GUI".into()))
        .expect("the IME-capable input remains addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("qr-input")
                .role(Role::TextInput)
                .value("你好 Rust GUI"),
        )
        .expect("input semantics match");
    app.harness()
        .expect(
            &SemanticExpectation::named("qr-output")
                .role(Role::Image)
                .name_contains("你好 Rust GUI"),
        )
        .expect("the visual output has payload-specific alternative text");
    app.invoke(Code::RESET.invoke()).expect("reset");
    assert_eq!(app.state(), Code::default());
}
