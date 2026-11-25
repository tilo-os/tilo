//! What looking an app up costs a session: the catalogue that says which apps run and which are
//! installed is read once for a name, and the app a start made is known by the call after it.
//! Before, a call on an app started without a window found no running provider in the catalogue,
//! so it asked the workspace to start the app again — a start invocation for every read an agent
//! made, which is what made a store read cost tens of milliseconds.

use super::*;

/// What the workspace's catalogue was asked: which apps run and which are installed. What an
/// app answers about its own resources once it is found is not the catalogue, and a call that
/// reads a resource asks the app every time.
fn catalogue_asks(asked: &[String]) -> Vec<&String> {
    asked
        .iter()
        .filter(|ask| ask.as_str() == "apps" || ask.starts_with(WORKSPACE))
        .collect()
}

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
            catalogue_asks(&again).is_empty(),
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

/// A grant the workspace withdraws is refused by the next call: what a session keeps is where
/// the app runs, never that this caller may use it.
#[test]
fn a_grant_withdrawn_while_the_app_runs_is_refused_by_the_next_call() {
    let notes = Notes::new(false);
    let server = agent(&notes);
    run(async {
        // The agent may open Notes: the read starts it, and the session keeps where it runs.
        let (text, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(!failed, "the first read is the agent's: {text}");
        assert!(
            notes
                .invoked()
                .iter()
                .any(|(_, action, _)| action == "start"),
            "the read started Notes, which the calls after it reach"
        );
        // The workspace withdraws the grant while Notes keeps running.
        *notes.workspace.lock().unwrap() = Installed::NotesClosedToAgent;
        // The next call reaches Notes, whose connection admits it again as the workspace
        // decides it: refused, though the session knows where the app runs.
        let (text, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(
            failed,
            "the call after the grant was withdrawn was answered: {text}"
        );
        assert!(text.contains("not_permitted"), "{text}");
        // And the one after it: nothing the refusal left keeps the agent in.
        let (text, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(failed && text.contains("not_permitted"), "{text}");
    });
}

/// A grant given after a call is the next call's: a refusal the session kept would hold the
/// agent out of an app its person has just allowed it.
#[test]
fn a_grant_given_after_a_call_is_not_refused_from_what_the_session_kept() {
    let notes = Notes::new(false).installed(Installed::NotesClosedToAgent);
    let server = agent(&notes);
    run(async {
        let (text, _) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(
            text.contains("may not open"),
            "Notes is not this agent's to open yet: {text}"
        );
        // The workspace allows it.
        *notes.workspace.lock().unwrap() = Installed::Openable;
        let (text, failed) = tool(&server, "tilo_read", json!({ "path": "notes/Todo" })).await;
        assert!(
            !failed,
            "the call after the grant was given was refused: {text}"
        );
    });
}
