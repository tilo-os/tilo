//! What the person asks of a server, in its entry: the request, the answer,
//! and the newest answers kept.

use tilo_server_protocol::transport::{ServerAnswer, ServerOperation, ServerOperationResult};

use super::*;
use tilo_resource::Replica;

fn request_id(index: usize) -> String {
    format!("{index:032x}")
}

fn answered(summary: &str) -> String {
    serde_json::to_string(&ServerOperationResult {
        operation: "status".into(),
        done: true,
        summary: summary.into(),
        answer: ServerAnswer::Nothing,
    })
    .unwrap()
}

fn ask(replica: &mut Replica<Servers>, id: &str, operation: &str) {
    replica
        .apply(Servers::ASK_OPERATION.with((
            device(),
            id.to_owned(),
            operation.to_owned(),
            String::new(),
            0,
        )))
        .unwrap();
}

/// What the person asked of a server is in its entry until the server answers
/// it, and what it answered reads back as what was asked and what it did.
#[test]
fn an_operation_is_asked_and_answered_in_the_entry() {
    let mut replica = enrolled();
    let id = request_id(1);
    ask(&mut replica, &id, "status");
    let state = replica.state().unwrap();
    let server = state.servers.get(&device()).unwrap();
    assert_eq!(server.pending().count(), 1);
    assert!(server.history().is_empty(), "nothing answered yet");
    let (named, asked) = server.pending().next().unwrap();
    assert_eq!(named.as_str(), id, "a request is named by its id");
    assert_eq!(asked.operation(), Some(ServerOperation::Status));

    replica
        .apply(Servers::ANSWER_OPERATION.with((device(), id, answered("up 4s"))))
        .unwrap();
    let state = replica.state().unwrap();
    let server = state.servers.get(&device()).unwrap();
    assert_eq!(server.pending().count(), 0);
    let history = server.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].operation, "status");
    assert_eq!(history[0].summary, "up 4s");
    assert!(history[0].done);
}

/// Only the newest answers are kept: a status asked every minute for a
/// year does not grow the person's document without bound.
#[test]
fn the_newest_answers_are_what_stays() {
    let mut replica = enrolled();
    for index in 0..(HISTORY_KEPT + 3) {
        let id = request_id(index);
        ask(&mut replica, &id, "status");
        replica
            .apply(Servers::ANSWER_OPERATION.with((device(), id, answered("read"))))
            .unwrap();
    }
    let state = replica.state().unwrap();
    let server = state.servers.get(&device()).unwrap();
    assert_eq!(server.asked.len(), HISTORY_KEPT);
    assert_eq!(
        server
            .asked
            .iter()
            .last()
            .map(|(id, _)| id.as_str().to_owned()),
        Some(request_id(HISTORY_KEPT + 2)),
        "the newest answer is the one kept last"
    );
}

#[test]
fn an_operation_that_is_no_operation_is_refused() {
    let mut replica = enrolled();
    assert!(
        replica
            .apply(Servers::ASK_OPERATION.with((
                device(),
                request_id(1),
                "reboot-now".into(),
                String::new(),
                0,
            )))
            .is_err()
    );
    assert!(
        replica
            .apply(Servers::ASK_OPERATION.with((
                device(),
                "not an id".into(),
                "status".into(),
                String::new(),
                0,
            )))
            .is_err(),
        "a request is named by a hex id"
    );
    ask(&mut replica, &request_id(2), "status");
    assert!(
        replica
            .apply(
                Servers::ANSWER_OPERATION.with((device(), request_id(2), "not a result".into(),))
            )
            .is_err(),
        "an answer is an operation's result"
    );
    assert!(
        replica
            .apply(Servers::ANSWER_OPERATION.with((
                device(),
                request_id(9),
                answered("nothing was asked"),
            )))
            .is_err(),
        "an answer belongs to a request"
    );
}
