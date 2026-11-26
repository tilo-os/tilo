//! A framework app runs on the stack a test thread starts with. Opening it, searching it, showing
//! its History, launching it again and capturing it keep nothing on the stack that two mebibytes
//! in an unoptimised build can't hold: an app or a driver moved by value through the helpers that
//! open one took its whole size again in each of their frames, until the thread overflowed.

use tilo_app::widget::search_request::SearchRequest;
use tilo_app_test_host::TestApp;

use crate::kinds::{Tally, TallyApp};

/// What `cargo test` gives each test's thread unless `RUST_MIN_STACK` says otherwise.
const TEST_THREAD_STACK: usize = 2 * 1024 * 1024;

#[test]
fn an_app_opens_searches_shows_its_history_relaunches_and_captures_on_a_test_threads_stack() {
    let name = std::thread::Builder::new()
        .stack_size(TEST_THREAD_STACK)
        .spawn(|| {
            let mut app = TestApp::<TallyApp>::new().unwrap();
            app.invoke(Tally::RENAME.with("visitors")).unwrap();
            app.harness()
                .open_search(&SearchRequest {
                    query: Some("Bump".into()),
                    ..SearchRequest::this_app()
                })
                .unwrap();
            assert!(app.harness().palette_shown(), "the window's search shows");
            app.harness().key("Escape");
            app.harness().open_history().unwrap();
            let app = app.relaunch(&[]).unwrap();
            let capture = TestApp::<TallyApp>::capture(None).unwrap();
            drop(capture);
            app.state().name
        })
        .expect("a thread starts")
        .join()
        .expect("the app ran within the thread's stack");
    assert_eq!(name, "visitors", "the relaunched app reads its record");
}
