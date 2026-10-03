//! The artwork as a document a person names, starts, opens, saves and
//! exports, and the steps that leaves in its history.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

fn labels(app: &TestApp<App>) -> Vec<String> {
    app.versions()
        .iter()
        .map(|version| format!("{version:?}"))
        .collect()
}

#[test]
fn naming_the_artwork_is_one_step_of_history() {
    let mut app = TestApp::<App>::new().unwrap();
    let before = app.versions().len();
    app.harness().click_named("canvas-title").unwrap();
    app.harness().type_text("Dawn");
    app.harness().key("Enter");
    app.harness().key("Tab");
    assert_eq!(app.state().title, "Dawn");
    let after = labels(&app);
    assert_eq!(
        after.len() - before,
        1,
        "typing a title and pressing Enter is one step: {after:#?}"
    );
}
