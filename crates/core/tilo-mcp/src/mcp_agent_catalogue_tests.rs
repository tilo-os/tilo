//! What looking an app up costs a session: the catalogue that says which apps run and which are
//! installed is read once for a name, and the app a start made is known by the call after it.
//! Before, a call on an app started without a window found no running provider in the catalogue,
//! so it asked the workspace to start the app again — a start invocation for every read an agent
//! made, which is what made a store read cost tens of milliseconds.

use super::*;

/// How many calls the catalogue is held to answering for: one session names the same app again
/// and again, so what it found is kept.
#[test]
fn a_session_reads_the_catalogue_once_for_an_app_it_names_again() {
    // Installed and not running: a workspace before anything opened, and what an agent's first
    // call meets. The read starts it without a window.
    let notes = Notes::new(false);
    run(async {
        let server = agent(&notes);
        let (first, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(!failed, "{first}");
        let asked = notes.take_asked();
        assert!(
            !asked.is_empty(),
            "the first call reads the catalogue: {first}"
        );
        notes.invoked.lock().unwrap().clear();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            let (text, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
            assert!(!failed && !text.is_empty(), "{text}");
        }
        let again = notes.take_asked();
        assert!(
            again.is_empty(),
            "100 calls read the catalogue again: {again:?}"
        );
        let starts: Vec<_> = notes
            .invoked()
            .into_iter()
            .filter(|(_, action, _)| action == "start")
            .collect();
        assert!(
            starts.is_empty(),
            "an app started once is started again by a call through it: {starts:?}"
        );
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(1),
            "100 reads took {took:?}"
        );
    });
}

/// A name no app answers is asked about once, not once a call: an agent that reads a path whose
/// app is gone gets the same refusal from what the session already learned.
#[test]
fn a_name_no_app_answers_asks_the_catalogue_once() {
    let notes = Notes::new(true);
    run(async {
        let server = agent(&notes);
        let (first, _) = tool(&server, "tilo_read", json!({ "path": "gone/Todo" })).await;
        assert!(
            !notes.take_asked().is_empty(),
            "the first call reads the catalogue: {first}"
        );
        let started = std::time::Instant::now();
        for _ in 0..100 {
            let (text, _) = tool(&server, "tilo_read", json!({ "path": "gone/Todo" })).await;
            assert!(!text.is_empty());
        }
        assert!(
            notes.take_asked().is_empty(),
            "100 calls read the catalogue again for a name it already answered"
        );
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(1),
            "100 refusals took {took:?}"
        );
    });
}
