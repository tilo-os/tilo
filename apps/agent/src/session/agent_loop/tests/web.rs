//! Searching the web and reading a page are the agent's own steps: the run
//! asks them itself, in process, and its turn's record says what was asked
//! and which page was read.

use super::*;

use tilo_web_search::Engine;
use tilo_web_search::fixtures::{FixtureServer, Route};

/// The engine answers the library records, served to the agent from a server
/// on this machine.
const BRAVE: &str =
    include_str!("../../../../../../crates/platform/tilo-web-search/tests/fixtures/brave.html");
const BING: &str =
    include_str!("../../../../../../crates/platform/tilo-web-search/tests/fixtures/bing.html");

/// A page a read answers with.
const PAGE: &str = "<!doctype html><html><head><title>Ada Lovelace</title></head><body><main>\
                    <h1>Ada</h1><p>Ada Lovelace wrote the first published algorithm.</p>\
                    </main></body></html>";

/// The engine answers and the page, on a server of the test's own.
fn server() -> FixtureServer {
    FixtureServer::serve(vec![
        Route::new("/brave", "text/html", BRAVE),
        Route::new("/bing", "text/html", BING),
        Route::new("/ada", "text/html", PAGE),
    ])
    .expect("a server on this machine")
}

/// Let the agent's web tools ask `server` instead of the web.
fn look_at(f: &mut Fixture, server: &FixtureServer) {
    let mut web = crate::tools::web::Web::new(f.session.cfg.content.clone());
    web.look_elsewhere(vec![
        (Engine::Brave, server.at("/brave")),
        (Engine::Bing, server.at("/bing")),
    ]);
    f.session.web = Some(web);
}

/// Every progress note the run told the person, in order.
fn notes(sent: &[CollabRequest]) -> Vec<String> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::ReportTaskRun {
                update: runtime_collab::TaskRunUpdate::Progress { message, .. },
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// A run that looks something up: it searches, reads the page that matters,
/// and answers with what it read. Both are steps of the run — the person is
/// told what is being searched and which page is being read, the model reads
/// their answers, and the turn's record keeps both.
#[test]
fn a_search_and_a_read_are_steps_of_the_run() {
    let server = server();
    let mut f = fixture(
        "What did Ada Lovelace write?",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![
            turn(
                &[(
                    crate::tools::web::SEARCH_WEB,
                    serde_json::json!({"query": "Ada Lovelace first algorithm"}),
                )],
                16,
            ),
            turn(
                &[(
                    crate::tools::web::READ_PAGE,
                    serde_json::json!({"url": server.at("/ada")}),
                )],
                16,
            ),
            turn(
                &[finish(
                    "Ada Lovelace wrote the first published algorithm.",
                    true,
                )],
                16,
            ),
        ],
    );
    look_at(&mut f, &server);
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Ada Lovelace wrote the first published algorithm.".into()
        }
    );

    // The person was told what was searched and which page was read, as each
    // step began.
    let told = notes(&f.sent.borrow());
    assert!(
        told.iter()
            .any(|note| note
                == "Searching the web for \u{201c}Ada Lovelace first algorithm\u{201d}…"),
        "{told:?}"
    );
    assert!(
        told.iter()
            .any(|note| note == &format!("Reading {}…", server.at("/ada"))),
        "{told:?}"
    );

    // The model read what the tools answered: the merge's results, and the
    // page's text.
    let bodies = f.script.bodies.lock().unwrap();
    let after_search = bodies[1].to_string();
    assert!(
        after_search.contains("rust-lang.org") || after_search.contains("example"),
        "the search's results reached the model: {after_search}"
    );
    let after_read = bodies[2].to_string();
    assert!(
        after_read.contains("wrote the first published algorithm"),
        "the page's text reached the model: {after_read}"
    );
    assert!(
        tool_names(&bodies[0]).contains(&crate::tools::web::SEARCH_WEB.to_owned()),
        "the search is a tool the model is offered"
    );
    drop(bodies);

    // Both are steps of the turn's record, by what they asked and reached.
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    let steps = records
        .last()
        .and_then(|record| record["work"]["trace"].as_array().cloned())
        .expect("the reply's steps");
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(steps[0]["resource"], "web.search");
    assert_eq!(
        steps[0]["label"],
        "Searching the web for \u{201c}Ada Lovelace first algorithm\u{201d}\u{2026}"
    );
    assert_eq!(steps[0]["outcome"], "applied");
    assert_eq!(steps[1]["resource"], "web.page");
    assert_eq!(
        steps[1]["label"],
        format!("Reading {}\u{2026}", server.at("/ada"))
    );
    assert_eq!(steps[1]["outcome"], "applied");
}

/// A search the engines could not answer is the step's own failure: the model
/// is told what went wrong, the run does not claim the step succeeded, and
/// the turn's record says it was refused.
#[test]
fn a_failed_read_is_a_refused_step() {
    let server = server();
    let mut f = fixture(
        "What is at the example address?",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![
            turn(
                &[(
                    crate::tools::web::READ_PAGE,
                    serde_json::json!({"url": server.at("/missing")}),
                )],
                16,
            ),
            turn(&[finish("The page is not there.", true)], 16),
        ],
    );
    look_at(&mut f, &server);
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap();
    let result = bodies[1].to_string();
    assert!(result.contains("failed"), "{result}");
    assert!(result.contains("404"), "{result}");
    drop(bodies);
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    let steps = records
        .last()
        .and_then(|record| record["work"]["trace"].as_array().cloned())
        .expect("the reply's steps");
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0]["resource"], "web.page");
    assert_eq!(steps[0]["outcome"], "refused", "{steps:?}");
}
