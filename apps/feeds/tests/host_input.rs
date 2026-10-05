//! The Add feed dialog through the real host loop: its address field takes
//! typed text after the host lays out between every event.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use ui::framework::testing::TestApp;
use ui::headless::FrameHarness;

fn value_of(harness: &FrameHarness, name: &str) -> Option<String> {
    harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some(name))
        .and_then(|control| control.value)
}

#[test]
fn the_add_feed_field_takes_typed_text() {
    let mut app = TestApp::<feeds::FeedsShell>::new().unwrap();
    let harness = app.harness();
    harness.relayout();
    let add = harness
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Add subscription"))
        .and_then(|control| control.bounds)
        .expect("the Add subscription button is placed")
        .center();
    harness.click_at(add.x, add.y);
    harness.relayout();
    assert!(
        value_of(harness, "Feed URL").is_some(),
        "the dialog opened with its field"
    );
    // A browser page delivers text only to a focused field it can place its
    // input-method editor over.
    assert!(
        harness.ime_window().is_some(),
        "the field holds the host's focus: {:?}",
        harness.focused_access_node()
    );
    // A page's input-method editor commits the text.
    harness.commit_text("https://example.com/feed");
    assert_eq!(
        value_of(harness, "Feed URL").as_deref(),
        Some("https://example.com/feed")
    );
}
