//! A person's events through the host's own actions
//! ([`crate::workspace_bots::events`]), as a device, a page and an agent's
//! turn take them: who may set one up and reach it, what a listing says of
//! it, and that the host fires what was set up, once, also across a restart.

use std::time::{Duration, Instant};

use serde_json::json;
use tilo_actions::remote::person_events::{self, Ended, EventView, Events, Sort};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage,
};
use tilo_server_protocol::WorkspaceId;

use super::*;
use crate::workspace_bots::events::PersonEvents;

/// One action on a person's events as `caller` takes it, through what the
/// host's app action server and an agent's turn both answer with.
fn take(
    events: &PersonEvents,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let unanswered = |why: &str| ActionCallError::Unavailable {
        reason: why.to_owned(),
    };
    let workspace = WorkspaceId::parse(workspace).ok_or_else(|| unanswered("not a workspace"))?;
    let arguments = if let serde_json::Value::Object(arguments) = arguments {
        arguments
    } else {
        serde_json::Map::new()
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: person_events::target(action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let answered = events
        .serve(workspace, caller, RemoteActionMessage::Invoke(request))
        .and_then(|answer| answer.wait_timeout(Duration::from_secs(5)).ok().flatten());
    match answered {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Rejected { error } => Err(error),
            other => Err(unanswered(&format!(
                "an action on events ends at once: {other:?}"
            ))),
        },
        _ => Err(unanswered("an action is answered with its receipt")),
    }
}

/// The event `answer` describes.
fn event(answer: Result<serde_json::Value, ActionCallError>) -> EventView {
    serde_json::from_value(answer.expect("the action is taken")).expect("an event is answered")
}

/// The events `caller`'s person keeps.
fn listed(events: &PersonEvents, workspace: &str, caller: &Caller) -> Vec<EventView> {
    let answer = take(events, workspace, caller, person_events::LIST, json!({}));
    serde_json::from_value::<Events>(answer.expect("a person lists their events"))
        .expect("events are answered")
        .events
}

/// A listing of everything a caller is offered: the empty query.
fn everything() -> CatalogueQuery {
    serde_json::from_value(json!({})).expect("the empty query asks for everything")
}

/// The person the workspace is of, on a device of theirs.
fn as_person(cert: &DeviceCert, workspace: &str) -> Caller {
    Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace)
}

/// That person's assistant, on a turn their session was handed to.
fn as_their_agent(cert: &DeviceCert, workspace: &str) -> Caller {
    as_person(cert, workspace)
        .delegated_to("assistant")
        .named("Assistant")
}

/// Another person, on a device of their own.
fn as_another(workspace: &str) -> Caller {
    Caller::authenticated(ADA, [9; 32]).in_workspace(workspace)
}

/// A summary every tenth morning something large came in: an event with a
/// schedule, a question, a count and a note.
fn morning_numbers() -> serde_json::Value {
    json!({
        "name": "Morning numbers",
        "when": "every day at 09:00",
        "onlyIf": "Is anything over a thousand?",
        "every": 10,
        "instruction": "Sum up what came in since yesterday.",
        "purpose": "Keep me on top of the numbers.",
        "style": "Three lines.",
    })
}

/// The events of the person a test's workspace is of, as their callers reach
/// them.
struct Theirs {
    events: PersonEvents,
    workspace: String,
    cert: DeviceCert,
}

impl Theirs {
    /// Their events on the host of `workspaces`, kept under `state`. Nothing
    /// takes what they hand over.
    fn on(
        workspaces: &Workspaces,
        workspace: &str,
        cert: &DeviceCert,
        state: &std::path::Path,
    ) -> Self {
        Self {
            events: PersonEvents::start_with(
                workspaces.clone(),
                state.to_path_buf(),
                Assistant::answering(vec![]),
            ),
            workspace: workspace.to_owned(),
            cert: cert.clone(),
        }
    }

    fn me(&self) -> Caller {
        as_person(&self.cert, &self.workspace)
    }

    fn agent(&self) -> Caller {
        as_their_agent(&self.cert, &self.workspace)
    }

    fn id(&self) -> WorkspaceId {
        WorkspaceId::parse(&self.workspace).expect("a workspace has an id")
    }

    fn take(
        &self,
        caller: &Caller,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        take(&self.events, &self.workspace, caller, action, arguments)
    }

    /// Their events, as they read them on a device of their own.
    fn listed(&self) -> Vec<EventView> {
        listed(&self.events, &self.workspace, &self.me())
    }

    /// The event their assistant sets up as [`morning_numbers`] describes.
    fn set_up(&self) -> EventView {
        event(self.take(&self.agent(), person_events::SET_UP, morning_numbers()))
    }

    /// What `caller` is offered on events, as a catalogue's page reads.
    fn offered(&self, caller: &Caller) -> String {
        serde_json::to_string(&self.events.catalogue(self.id(), caller, &everything()))
            .expect("a page is written")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persons_assistant_sets_an_event_up_and_its_person_reads_the_same() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(
        &workspaces,
        &workspace,
        &cert,
        &scratch.path().join("events"),
    );

    let made = theirs.set_up();
    assert_eq!(made.name, "Morning numbers");
    assert_eq!(made.sort, Sort::Scheduled);
    assert_eq!(made.when, "every day at 09:00");
    assert!(
        made.next_at_ms.is_some(),
        "a schedule says when it fires next"
    );
    assert_eq!(
        made.only_if.as_deref(),
        Some("Is anything over a thousand?")
    );
    assert_eq!((made.every, made.count), (Some(10), 0));
    assert_eq!(made.instruction, "Sum up what came in since yesterday.");
    assert_eq!(made.note["instruction"], "Keep me on top of the numbers.");
    assert_eq!(made.note["style"], "Three lines.");
    assert!(made.on);
    assert_eq!((made.runs, made.last_run.clone()), (0, None));

    // An agent is offered the event actions.
    let offered = theirs.offered(&theirs.agent());
    assert!(offered.contains("\"set-up\""), "{offered}");
    assert!(
        theirs
            .events
            .provider(theirs.id(), &theirs.agent())
            .is_some()
    );

    // The person reads the same on a device of their own.
    assert_eq!(theirs.listed(), vec![made]);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_persons_agent_and_a_bot_set_nothing_up_and_reach_no_event() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(
        &workspaces,
        &workspace,
        &cert,
        &scratch.path().join("events"),
    );
    let made = theirs.set_up();
    let named = json!({"event": made.id});

    // Another person and an agent acting for them set nothing up here, and
    // reach none of this person's events.
    for other in [
        as_another(&workspace),
        as_another(&workspace).delegated_to("scout"),
    ] {
        let refused = theirs.take(&other, person_events::SET_UP, morning_numbers());
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{refused:?}"
        );
        for action in [
            person_events::LIST,
            person_events::PAUSE,
            person_events::CHANGE,
            person_events::REMOVE,
        ] {
            assert!(
                theirs.take(&other, action, named.clone()).is_err(),
                "{action}"
            );
        }
        assert!(theirs.events.provider(theirs.id(), &other).is_none());
        let offered = theirs.offered(&other);
        assert!(!offered.contains("\"set-up\""), "{offered}");
    }
    // Neither does a bot, whoever installed it.
    let bot =
        Caller::bot(GREETER.to_owned(), cert.user_public_key, [5; 32]).in_workspace(&workspace);
    assert!(matches!(
        theirs.take(&bot, person_events::LIST, json!({})),
        Err(ActionCallError::UnknownAction)
    ));
    assert_eq!(theirs.listed(), vec![made], "nothing of it changed");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_is_changed_paused_and_removed_by_its_persons_assistant() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("events");
    let theirs = Theirs::on(&workspaces, &workspace, &cert, &state);
    let agent = theirs.agent();
    let made = theirs.set_up();
    let named = json!({"event": made.id});

    // Changed: what starts it, its filter, its count, what it is told, and
    // its note. What is left out stays.
    let changed = event(theirs.take(
        &agent,
        person_events::CHANGE,
        json!({
            "event": made.id,
            "when": "once at 2030-01-01T09:00:00Z",
            "onlyIf": "",
            "every": 1,
            "instruction": "Say happy new year.",
            "accepted": "Short and warm.",
            "kept": {"seen": 3},
        }),
    ));
    assert_eq!(changed.id, made.id, "it is the same event");
    assert_eq!(changed.when, "once at 2030-01-01T09:00:00Z");
    assert_eq!(changed.next_at_ms, Some(1_893_488_400_000));
    assert_eq!((changed.only_if.clone(), changed.every), (None, None));
    assert_eq!(changed.instruction, "Say happy new year.");
    assert_eq!(changed.note["accepted"], json!(["Short and warm."]));
    assert_eq!(changed.note["kept"], json!({"seen": 3}));
    assert_eq!(changed.note["style"], "Three lines.", "the rest stays");
    // What no host can run is refused in words, and changes nothing.
    let refused = theirs.take(
        &agent,
        person_events::CHANGE,
        json!({"event": made.id, "when": "when someone presses \"ctrl+k\""}),
    );
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("every device off")),
        "{refused:?}"
    );
    assert_eq!(theirs.listed(), vec![changed]);

    // Paused, and let go on.
    let paused = event(theirs.take(&agent, person_events::PAUSE, named.clone()));
    assert!(!paused.on);
    assert_eq!(paused.next_at_ms, None, "a paused event fires at no time");
    let resumed = event(theirs.take(&agent, person_events::RESUME, named.clone()));
    assert!(resumed.on);

    // Removed: gone for its person, with everything it kept.
    let left = theirs.take(&agent, person_events::REMOVE, named.clone());
    assert_eq!(left.ok(), Some(json!({"events": []})));
    assert!(theirs.take(&agent, person_events::PAUSE, named).is_err());
    let kept = state
        .join(theirs.id().to_string())
        .join(hex::encode(cert.user_public_key))
        .join(&made.id);
    assert!(!kept.exists(), "{}", kept.display());
}

/// A moment `in_ms` from now, as a trigger's line and in ms since the epoch.
fn in_a_moment(in_ms: i64) -> (String, i64) {
    let at_ms = tilo_clock::unix_now_ms_i64().saturating_add(in_ms);
    (format!("once at {at_ms} ms"), at_ms)
}

/// Wait until `done` answers something, or fail when it never does.
async fn until<T>(what: &str, mut done: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(answer) = done() {
            return answer;
        }
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reminder_the_assistant_set_up_fires_once_also_when_the_host_starts_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("person-events");
    let assistant = Assistant::answering(vec![]);
    let events = PersonEvents::start_with(workspaces.clone(), state.clone(), assistant.clone());
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);

    let (when, at_ms) = in_a_moment(400);
    let made = event(take(
        &events,
        &workspace,
        &theirs,
        person_events::SET_UP,
        json!({
            "name": "Call home",
            "when": when,
            "instruction": "Say that it is time to call home.",
        }),
    ));
    assert_eq!(made.sort, Sort::Scheduled);
    assert_eq!(made.next_at_ms, Some(at_ms));
    assert!(assistant.taken().is_empty(), "nothing before its moment");

    // The moment comes: the host hands the instruction to the assistant of
    // the person the event is of, with no device of theirs asking.
    let taken = until("the reminder never fired", || {
        Some(assistant.taken()).filter(|taken| !taken.is_empty())
    })
    .await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].person, hex::encode(cert.user_public_key));
    assert_eq!(
        taken[0].workspace,
        WorkspaceId::parse(&workspace).unwrap().to_string()
    );
    assert_eq!(taken[0].instruction, "Say that it is time to call home.");
    assert_eq!(taken[0].cause.installation, made.id);
    assert!(
        tilo_clock::unix_now_ms_i64() >= at_ms,
        "it fired at its moment, not before"
    );
    // Its person reads that it ran, and that it has no next time.
    let ran = until("the run was never kept", || {
        listed(&events, &workspace, &me)
            .into_iter()
            .find(|event| event.last_run.is_some())
    })
    .await;
    assert_eq!(
        ran.last_run.as_ref().map(|last| last.ended),
        Some(Ended::Ran)
    );
    assert_eq!((ran.runs, ran.next_at_ms), (1, None));

    // The host starts again: it still keeps the event, and hands nothing
    // over a second time.
    drop(events);
    let events = PersonEvents::start_with(workspaces.clone(), state, assistant.clone());
    let kept = listed(&events, &workspace, &me);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!((kept[0].id.as_str(), kept[0].runs), (made.id.as_str(), 1));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(assistant.taken().len(), 1);
    assert_eq!(listed(&events, &workspace, &me)[0].runs, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_event_fires_nothing_and_a_refused_run_says_why() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![Err(HostStepError::Refused(
        "this person keeps no assistant".into(),
    ))]);
    let events = PersonEvents::start_with(
        workspaces.clone(),
        scratch.path().join("person-events"),
        assistant.clone(),
    );
    let theirs = as_their_agent(&cert, &workspace);
    let set_up = |name: &str, when: String| {
        event(take(
            &events,
            &workspace,
            &theirs,
            person_events::SET_UP,
            json!({"name": name, "when": when, "instruction": "Say hello."}),
        ))
    };

    // Paused before its moment, it never fires, also not once it goes on.
    let quiet = set_up("Quiet", in_a_moment(300).0);
    let named = json!({"event": quiet.id});
    take(
        &events,
        &workspace,
        &theirs,
        person_events::PAUSE,
        named.clone(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());
    let resumed = event(take(
        &events,
        &workspace,
        &theirs,
        person_events::RESUME,
        named,
    ));
    assert!(resumed.on);
    assert_eq!(resumed.runs, 0);

    // One whose instruction nothing takes says so where its person reads it.
    let refused = set_up("Loud", in_a_moment(200).0);
    let ended = until("the refused run was never kept", || {
        listed(&events, &workspace, &theirs)
            .into_iter()
            .find(|event| event.id == refused.id)
            .and_then(|event| event.last_run)
    })
    .await;
    assert_eq!(ended.ended, Ended::Refused);
    assert!(
        ended
            .why
            .as_deref()
            .is_some_and(|why| why.contains("this person keeps no assistant")),
        "{ended:?}"
    );
    assert_eq!(
        assistant.taken().len(),
        1,
        "only the one that was on asked: {:?}",
        assistant.taken()
    );
}
