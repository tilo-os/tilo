//! How long a call is held for its action. A turn's own server holds a call that names no wait
//! until the action has ended; a call that names a wait, and an outside client's call that
//! names none, are answered `pending` as they always were.
use super::*;

/// The longest a tool took in the turn that showed the fault: three times the default wait.
const FIFTEEN_SECONDS: std::time::Duration = std::time::Duration::from_secs(15);
const AN_HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// A turn's server over `notes`, as a workspace's server makes one for a person's ask.
fn turn(notes: &Notes) -> McpServer<Notes> {
    let caller = Caller::delegated_agent("assistant", [1; 32], [2; 32]).in_workspace("workspace");
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

/// What the tool `call` names answered on `server` while the person of the action `asked` of
/// `notes` is asked to allow it and answers only after that long, whether the call failed, and
/// how long it was held.
fn while_asked(
    notes: &Notes,
    server: &McpServer<Notes>,
    asked: (&'static str, std::time::Duration),
    call: (&str, Value),
) -> (String, bool, std::time::Duration) {
    waited(async {
        let started = tokio::time::Instant::now();
        *notes.person_asked.lock().unwrap() =
            Some((asked.0, started.checked_add(asked.1).unwrap()));
        let (text, failed) = tool(server, call.0, call.1).await;
        (text, failed, started.elapsed())
    })
}

/// The id an answer names for `tilo_status`.
fn named_id(text: &str) -> &str {
    text.split("\"invocation_id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("no id for tilo_status: {text}"))
}

/// A turn's call whose person is asked to allow it is answered `asked` at once, without an
/// error: a person who may not be there is no tool to wait for. The answer says where they
/// are asked, that nothing happened yet, what to do meanwhile, and the id `tilo_status` takes;
/// `tilo_status` says the same for as long as they have not answered.
#[test]
fn a_turns_call_whose_person_is_asked_is_answered_asked_at_once() {
    let notes = Notes::new(true);
    let server = turn(&notes);
    let (text, failed, held) = while_asked(
        &notes,
        &server,
        ("rename", AN_HOUR),
        ("tilo_do", rename(&json!({}))),
    );
    assert!(
        held < std::time::Duration::from_secs(1),
        "held for {held:?} although its person is asked: {text}"
    );
    assert!(!failed, "its person being asked is no failure: {text}");
    assert!(
        text.starts_with(&format!(
            "asked: {ASKED_ON_DEVICES} Go on with what does not need it"
        )),
        "{text}"
    );
    assert!(text.contains("do not make this call again"), "{text}");
    let id = named_id(&text).to_owned();
    assert!(uuid::Uuid::parse_str(&id).is_ok(), "{text}");
    let (status, failed, held) = waited(async {
        let started = tokio::time::Instant::now();
        let (status, failed) = tool(&server, "tilo_status", json!({"invocation_id": id})).await;
        (status, failed, started.elapsed())
    });
    assert!(!failed && status.starts_with("asked: "), "{status}");
    assert!(held < std::time::Duration::from_secs(1), "{held:?}");
}

/// The typed answer of such a call says that it is pending and what it waits for.
#[test]
fn a_turns_typed_call_whose_person_is_asked_says_what_it_waits_for() {
    let notes = Notes::new(true);
    let server = turn(&notes);
    let (typed, failed, held) = waited(async {
        let started = tokio::time::Instant::now();
        *notes.person_asked.lock().unwrap() =
            Some(("rename", started.checked_add(AN_HOUR).unwrap()));
        let (typed, failed) = answer(&server, "tilo_do", rename(&json!({}))).await;
        (typed, failed, started.elapsed())
    });
    assert!(
        held < std::time::Duration::from_secs(1),
        "{held:?}: {typed}"
    );
    assert!(!failed, "{typed}");
    assert_eq!(typed["status"], "pending", "{typed}");
    assert_eq!(typed["waiting_for"], ASKED_ON_DEVICES, "{typed}");
}

/// A turn's call that names a wait asked to be held for it: it is, also while its person is
/// asked, and is then told `asked`, without an error.
#[test]
fn a_turns_call_that_names_a_wait_keeps_it_while_its_person_is_asked() {
    let notes = Notes::new(true);
    let (text, failed, held) = while_asked(
        &notes,
        &turn(&notes),
        ("rename", AN_HOUR),
        ("tilo_do", rename(&json!({"wait_ms": 30_000}))),
    );
    assert!(
        held >= std::time::Duration::from_secs(30) && held < std::time::Duration::from_secs(60),
        "answered after {held:?}"
    );
    assert!(!failed && text.starts_with("asked: "), "{text}");
}

/// A person's own client keeps the wait it has, and is told `asked` after it.
#[test]
fn an_outside_clients_call_keeps_its_wait_while_its_person_is_asked() {
    let notes = Notes::new(true);
    let (text, failed, held) = while_asked(
        &notes,
        &agent(&notes),
        ("rename", FIFTEEN_SECONDS),
        ("tilo_do", rename(&json!({}))),
    );
    assert!(
        held >= std::time::Duration::from_millis(DEFAULT_WAIT_MS) && held < FIFTEEN_SECONDS,
        "answered after {held:?}"
    );
    assert!(!failed && text.starts_with("asked: "), "{text}");
}

/// A person who allows it while the call still waits for them is not said to be asked: the
/// call is answered with what the action did.
#[test]
fn a_call_whose_person_allows_it_within_its_wait_is_answered_with_what_it_did() {
    let notes = Notes::new(true);
    let (text, failed, held) = while_asked(
        &notes,
        &agent(&notes),
        ("rename", std::time::Duration::from_secs(2)),
        ("tilo_do", rename(&json!({}))),
    );
    assert!(!failed && text.starts_with("ok"), "{text}");
    assert!(
        held >= std::time::Duration::from_secs(2)
            && held < std::time::Duration::from_millis(DEFAULT_WAIT_MS),
        "answered after {held:?}"
    );
}

/// A turn is told that a call its person must allow first answers `asked` at once.
#[test]
fn a_turn_is_told_that_a_call_its_person_must_allow_answers_asked() {
    let notes = Notes::new(true);
    let listed =
        waited(turn(&notes).handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))).unwrap();
    let told = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "tilo_do")
        .and_then(|tool| tool["description"].as_str())
        .unwrap()
        .to_owned();
    assert!(
        told.contains("answers `asked` at once where your person must allow it first"),
        "{told}"
    );
}

/// What the test app ends a call with whose person is asked about it in the conversation, as
/// a workspace's service words it.
const ASKED_THERE: &str = "Your person is asked about this in the conversation, and it is done \
     when they allow it there. Nothing ran yet: tell them it waits for their answer, and do not \
     make this call again.";
/// What it ends one with where the question could not be put.
const NOT_ASKED_THERE: &str = "Your person could not be asked about this in the conversation, \
     so nothing ran.";

impl Notes {
    /// `update`, or, where the call `id` is of the action whose person is asked about it in
    /// the conversation, how that call ends: nothing done, under the code of a confirmation
    /// that is required, with the detail that says a question was put, or without it where
    /// none could be.
    pub(super) fn or_asked(
        &self,
        id: &tilo_actions::InvocationId,
        update: InvocationUpdate,
    ) -> InvocationUpdate {
        let Some((asked, put)) = *self.asked_there.lock().unwrap() else {
            return update;
        };
        let called = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.invocation == *id && request.target.address.action == asked);
        if !called {
            return update;
        }
        InvocationUpdate::Failed {
            invocation: *id,
            error: tilo_actions::ActionFailure {
                code: tilo_actions::remote::CONFIRMATION_REQUIRED_CODE.to_owned(),
                message: if put { ASKED_THERE } else { NOT_ASKED_THERE }.to_owned(),
                details: put.then(tilo_actions::remote::person_asking::asked_detail),
                retryable: false,
            },
        }
    }
}

/// A turn's call whose person is asked about it in the conversation ends there with nothing
/// done. It is answered `asked`, without an error, in the service's own sentence and with what
/// to do meanwhile, and its typed answer says what it waits for. The same end where no
/// question could be put stays the failure it is.
#[test]
fn a_turns_call_whose_person_is_asked_in_the_conversation_is_answered_asked() {
    let notes = Notes::new(true);
    let server = turn(&notes);
    *notes.asked_there.lock().unwrap() = Some(("rename", true));
    let (text, failed) = waited(tool(&server, "tilo_do", rename(&json!({}))));
    assert!(!failed, "its person being asked is no failure: {text}");
    assert!(
        text.starts_with(&format!(
            "asked: {ASKED_THERE} Go on with what does not need it."
        )),
        "{text}"
    );
    let (typed, failed) = waited(answer(&server, "tilo_do", rename(&json!({}))));
    assert!(!failed, "{typed}");
    assert_eq!(typed["status"], "pending", "{typed}");
    assert_eq!(typed["waiting_for"], ASKED_THERE, "{typed}");

    *notes.asked_there.lock().unwrap() = Some(("rename", false));
    let (text, failed) = waited(tool(&server, "tilo_do", rename(&json!({}))));
    assert!(failed, "nobody was asked, and nothing follows: {text}");
    assert!(
        text.starts_with(&format!(
            "failed: action_confirmation_required: {NOT_ASKED_THERE}"
        )),
        "{text}"
    );
}
