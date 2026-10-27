//! Each Word Processor window shows the document it opened: a resource of
//! its own, never the app's record, so no window mirrors another.

use ui::framework::testing::TestApp;
use word_processor::framework::WordProcessorApp;

#[test]
fn a_new_window_shows_a_document_of_its_own() {
    let mut app = TestApp::<WordProcessorApp>::new().expect("word processor");
    app.flush().expect("saved");
    let beside = app.open_window().expect("a second window");
    assert!(
        !app.record().is_package_primary() && !beside.record().is_package_primary(),
        "a window's document is never the app's record"
    );
    assert_ne!(
        beside.record(),
        app.record(),
        "a second window shows a document of its own"
    );
}
