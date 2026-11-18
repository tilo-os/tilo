//! How long a call is held for its action. A turn's own server holds a call that names no wait
//! until the action has ended; a call that names a wait, and an outside client's call that
//! names none, are answered `pending` as they always were.
use super::*;

/// The longest a tool took in the turn that showed the fault: three times the default wait.
const FIFTEEN_SECONDS: std::time::Duration = std::time::Duration::from_secs(15);
const AN_HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// A turn's server over `notes`, as a workspace's server makes one for a person's ask.
fn turn(notes: &Notes) -> McpServer<Notes> {
    let caller = Caller::local_person("ada")
        .in_workspace("workspace")
        .delegated_to("assistant");
    McpServer::for_turn(notes.clone(), caller)
        .unwrap()
        .with_agent_session("turn:test")
}

/// A rename of the Todo note, with what else the call names.
fn rename(more: &Value) -> Value {
    let mut call = json!({"path": "notes/Todo#rename", "args": {"title": "Later"}});
    if let (Some(call), Some(more)) = (call.as_object_mut(), more.as_object()) {
        call.extend(more.clone());
    }
    call
}

/// What `future` came to, on a clock that moves only when everything waits.
fn waited<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(future)
}

/// What the tool `call` names answered on `server` while the action `late` of `notes` took that
/// long to end, whether the call failed, and how long it was held.
fn held(
    notes: &Notes,
    server: &McpServer<Notes>,
    late: (&'static str, std::time::Duration),
    call: (&str, Value),
) -> (String, bool, std::time::Duration) {
    waited(async {
        let started = tokio::time::Instant::now();
        *notes.late.lock().unwrap() = Some((late.0, started.checked_add(late.1).unwrap()));
        let (text, failed) = tool(server, call.0, call.1).await;
        (text, failed, started.elapsed())
    })
}

/// A turn's call of an action that takes fifteen seconds, and of one that takes an hour, is
/// answered with what the action did, in that one call: no clock of the tools' own ends it.
#[test]
fn a_turns_call_is_held_until_its_action_has_ended() {
    for takes in [FIFTEEN_SECONDS, AN_HOUR] {
        let notes = Notes::new(true);
        let (text, failed, waited) = held(
            &notes,
            &turn(&notes),
            ("rename", takes),
            ("tilo_do", rename(&json!({}))),
        );
        assert!(!failed, "an action of {takes:?}: {text}");
        assert!(text.starts_with("ok"), "an action of {takes:?}: {text}");
        assert!(waited >= takes, "answered after {waited:?} of {takes:?}");
    }
}

/// A turn's read of a note whose app takes fifteen seconds to answer is the note.
#[test]
fn a_turns_read_is_held_until_its_app_has_answered() {
    let notes = Notes::new(true);
    let (text, failed, waited) = held(
        &notes,
        &turn(&notes),
        ("read", FIFTEEN_SECONDS),
        ("tilo_read", json!({"path": "notes/Todo"})),
    );
    assert!(!failed, "{text}");
    assert!(text.contains("milk"), "{text}");
    assert!(waited >= FIFTEEN_SECONDS, "answered after {waited:?}");
}

/// A turn's call that names a wait asked to be answered after it: `pending`, with the id
/// `tilo_status` takes, as before.
#[test]
fn a_turns_call_that_names_a_wait_is_answered_pending_after_it() {
    let notes = Notes::new(true);
    let (text, failed, waited) = held(
        &notes,
        &turn(&notes),
        ("rename", AN_HOUR),
        ("tilo_do", rename(&json!({"wait_ms": 30_000}))),
    );
    assert!(failed && text.starts_with("pending "), "{text}");
    assert!(
        waited >= std::time::Duration::from_secs(30) && waited < std::time::Duration::from_secs(60),
        "answered after {waited:?}"
    );
}

/// A person's own client that names no wait is answered `pending` after the default, as
/// before: it has a clock of its own and follows with `tilo_status`.
#[test]
fn an_outside_clients_call_that_names_no_wait_is_answered_pending_as_before() {
    let notes = Notes::new(true);
    let (text, failed, waited) = held(
        &notes,
        &agent(&notes),
        ("rename", FIFTEEN_SECONDS),
        ("tilo_do", rename(&json!({}))),
    );
    assert!(failed && text.starts_with("pending "), "{text}");
    assert!(
        waited >= std::time::Duration::from_millis(DEFAULT_WAIT_MS) && waited < FIFTEEN_SECONDS,
        "answered after {waited:?}"
    );
}

/// A turn is told that its calls are held until they have ended; an outside client is told of
/// the default it still has.
#[test]
fn a_turn_is_told_that_its_calls_are_held_until_they_have_ended() {
    let notes = Notes::new(true);
    let told = |server: &McpServer<Notes>| {
        let listed =
            waited(server.handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))).unwrap();
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "tilo_do")
            .and_then(|tool| tool["description"].as_str())
            .unwrap()
            .to_owned()
    };
    let turns = told(&turn(&notes));
    assert!(turns.contains("held until its action has ended"), "{turns}");
    assert!(!turns.contains("default 5000"), "{turns}");
    let outside = told(&agent(&notes));
    assert!(
        outside.contains("past wait_ms (default 5000), `pending <id>` for tilo_status"),
        "{outside}"
    );
}
