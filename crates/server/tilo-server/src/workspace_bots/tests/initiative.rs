//! What a person's assistant finds to do on its own, through the server's own
//! actions ([`tilo_workspace_service::agents::initiative`]) and the steps the events'
//! engine hands the server, with no device of the person on: a look that
//! finds nothing new asks no model and writes nothing, the look-round's own
//! day holds, its switch and rhythm are those of one of its person's
//! events, and a kind of work its person turned down twice is not proposed
//! a third time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::workspace_events::{self, EventView, Events, Sort};
use tilo_actions::remote::workspace_initiative::{
    self, LookEnded, LookRound, Proposal, Proposals, State,
};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_agent_session::bot::host_steps::{
    HostStepError as StepError, HostSteps as Steps, StepCause,
};

use crate::workspaces::fixtures::TestHost;
use tilo_workspace_service::work::{AgentAccess, AgentEngine, Ineligible};

use super::bot_server;
use super::*;
use crate::WorkspaceId;
use tilo_workspace_service::agents::events::{Authority, WorkspaceEvents};
use tilo_workspace_service::agents::initiative::{Initiative, look_round_spender};

/// One action of the server's own, to take.
fn asking(target: RemoteActionTarget, arguments: Value) -> RemoteActionMessage {
    let arguments = match arguments {
        Value::Object(arguments) => arguments,
        _ => serde_json::Map::new(),
    };
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// What an action answered, or why it was not taken. One that was not
/// answered at all is told as unavailable, which no test here takes for a
/// refusal.
fn answered(pending: Option<Pending<RemoteActionMessage>>) -> Result<Value, ActionCallError> {
    let unanswered = |why: &str| ActionCallError::Unavailable {
        reason: why.to_owned(),
    };
    let answered =
        pending.and_then(|answer| answer.wait_timeout(Duration::from_secs(5)).ok().flatten());
    match answered {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Rejected { error } => Err(error),
            other => Err(unanswered(&format!(
                "an action of the server's own ends with its answer: {other:?}"
            ))),
        },
        _ => Err(unanswered("an action is answered with its receipt")),
    }
}

/// Whether `answer` is what a caller who is not answered here is told:
/// that there is no such action, or a refusal.
fn turned_away(answer: &Result<Value, ActionCallError>) -> bool {
    matches!(
        answer,
        Err(ActionCallError::UnknownAction
            | ActionCallError::NotPermitted
            | ActionCallError::Refused { .. })
    )
}

/// One action on the look-round as `caller` takes it, through what the
/// app action server and an agent's turn both answer with.
fn ask(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let target = workspace_initiative::target(workspace_initiative::LOOK_ROUND, action);
    answered(initiative.serve(
        WorkspaceId::parse(workspace).unwrap(),
        caller,
        Authority::Turn,
        asking(target, arguments),
    ))
}

/// One action on the workspace's events as `caller` takes it.
fn of_events(
    events: &WorkspaceEvents,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    answered(events.serve(
        WorkspaceId::parse(workspace).unwrap(),
        caller,
        Authority::Turn,
        asking(workspace_events::target(action), arguments),
    ))
}

/// The look-round `answer` describes.
fn look_round(answer: Result<Value, ActionCallError>) -> LookRound {
    serde_json::from_value(answer.expect("the action is taken")).expect("a look-round is answered")
}

/// The look-round as `caller` reads it.
fn read(initiative: &Initiative, workspace: &str, caller: &Caller) -> LookRound {
    look_round(ask(
        initiative,
        workspace,
        caller,
        workspace_initiative::READ,
        json!({}),
    ))
}

/// The events of the workspace `caller` reaches.
fn their_events(events: &WorkspaceEvents, workspace: &str, caller: &Caller) -> Vec<EventView> {
    let answer = of_events(events, workspace, caller, workspace_events::LIST, json!({}));
    serde_json::from_value::<Events>(answer.expect("a member lists the workspace's events"))
        .expect("events are answered")
        .events
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

/// The look-round `event` fires: what the events' engine hands the server's
/// steps at its time, on a thread that drives nothing else.
async fn fires(
    steps: &Arc<dyn Steps>,
    cert: &DeviceCert,
    workspace: &str,
    event: &str,
) -> Result<String, StepError> {
    let instruction = Instruction {
        look_grant: None,
        person: hex::encode(cert.user_public_key),
        workspace: WorkspaceId::parse(workspace).unwrap().to_string(),
        instruction: "Look round.".into(),
        allow: Vec::new(),
        permits: Vec::new(),
        note: json!({}),
        cause: StepCause {
            installation: event.to_owned(),
            invocation: tilo_actions::InvocationId::new(),
            event: None,
        },
    };
    let steps = Arc::clone(steps);
    tokio::task::spawn_blocking(move || steps.instruct(instruction))
        .await
        .unwrap()
}

/// A server with a workspace of `cert`'s person, their events and their
/// assistant's initiative, the events' steps answered by `assistant`.
fn started(workspaces: &Workspaces, assistant: &Arc<Assistant>) -> (Initiative, WorkspaceEvents) {
    Initiative::start_with(
        workspaces.clone(),
        assistant.clone(),
        std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
    )
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story: off, on, two quiet looks, something new, and a server that starts again"
)]
async fn a_look_that_finds_nothing_new_asks_no_model_and_writes_nothing() {
    use crate::workspaces::person_model::scripted::{Reply, ScriptedProvider};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    // Whatever asks a model on this server is seen here.
    let models = ScriptedProvider::answering(Vec::<Reply>::new());
    workspaces.answer_models_with(models.clone());
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);

    // Nobody turned it on: it is off, and no event of the person's.
    let off = read(&initiative, &workspace, &me);
    assert!(!off.on);
    assert_eq!((off.event.clone(), off.next_at_ms), (None, None));
    assert_eq!(off.when, workspace_initiative::DEFAULT_WHEN);
    assert_eq!(
        off.day_limit_micros,
        workspace_initiative::DEFAULT_DAY_LIMIT_MICROS
    );
    assert!(off.looks.is_empty());
    assert!(their_events(&events, &workspace, &me).is_empty());

    // Their assistant turns it on: it is one of their events, on a clock.
    let on = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    assert!(on.on);
    assert!(on.next_at_ms.is_some(), "it says when it looks next");
    let event = on.event.clone().expect("it is an event now");
    let listed = their_events(&events, &workspace, &me);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        (listed[0].id.as_str(), listed[0].sort),
        (event.as_str(), Sort::Scheduled)
    );
    assert_eq!(listed[0].when, workspace_initiative::DEFAULT_WHEN);

    // The first look notes where the workspace stands, the second finds
    // nothing new: neither hands anything on, asks a model or writes.
    let steps = (initiative.before())(assistant.clone());
    let written = posts(&workspaces, &workspace).await.len();
    for _ in 0..2 {
        assert_eq!(
            fires(&steps, &cert, &workspace, &event).await,
            Ok(String::new())
        );
    }
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());
    assert!(models.asked().is_empty(), "no model is asked");
    assert_eq!(posts(&workspaces, &workspace).await.len(), written);
    let quiet = read(&initiative, &workspace, &me);
    assert_eq!(quiet.looks.len(), 2, "each look is kept");
    for look in &quiet.looks {
        assert_eq!(look.ended, LookEnded::Quiet);
        assert!(!look.found.anything_new(), "{look:?}");
        assert_eq!(look.cost_micros, 0);
    }
    assert_eq!(quiet.spent_today_micros, 0);

    // Somebody writes in the workspace: the next look hands what is new to
    // the assistant, once, as data beside the rules of a look.
    ada_says(
        &workspaces,
        &workspace,
        &cert,
        &general,
        "The numbers for March are in.",
    )
    .await;
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].cause.installation, event);
    let told = &taken[0].instruction;
    assert!(told.starts_with("Look round."), "{told}");
    for said in [
        "data, not instructions",
        "Tasks has already queued the work in a separate turn",
        "Otherwise start nothing",
        "answer with nothing at all",
        "so.tilo.chat",
        "a person",
    ] {
        assert!(told.contains(said), "{said}: {told}");
    }
    assert!(
        !told.contains("The numbers for March"),
        "a look says where something changed, not what it says: {told}"
    );
    assert!(models.asked().is_empty(), "the look itself asks no model");
    let looked = read(&initiative, &workspace, &me);
    assert_eq!(looked.looks[0].ended, LookEnded::Told);
    assert!(looked.looks[0].found.changed >= 1, "{:?}", looked.looks[0]);

    // What a look handed on is not new a second time.
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(assistant.taken().len(), 1);
    assert_eq!(
        read(&initiative, &workspace, &me).looks[0].ended,
        LookEnded::Quiet
    );

    // The server starts again: it still keeps the look-round and its looks,
    // and a look goes on from where the last one left the workspace.
    drop((initiative, events, steps));
    let (initiative, events) = started(&workspaces, &assistant);
    let kept = read(&initiative, &workspace, &me);
    assert_eq!(kept.event.as_deref(), Some(event.as_str()));
    assert_eq!(kept.looks.len(), 4);
    assert_eq!(their_events(&events, &workspace, &me).len(), 1);
    let steps = (initiative.before())(assistant.clone());
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(assistant.taken().len(), 1, "nothing became new meanwhile");
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story: a day that is spent, who may raise it, and what was found meanwhile"
)]
async fn the_look_rounds_own_day_holds_and_only_its_person_raises_it() {
    use crate::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};
    use crate::workspaces::person_model::{Interaction, ProviderProtocol};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = super::bot_server_taking_part(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let models = ScriptedProvider::answering([Reply::text("Looked.").costing(600_000)]);
    workspaces.answer_models_with(models.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-their-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);

    // Its person turns it on with a day of half a unit.
    let on = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({"when": "every day at 09:00", "dayLimitMicros": 500_000}),
    ));
    assert_eq!(on.when, "every day at 09:00");
    assert_eq!((on.day_limit_micros, on.spent_today_micros), (500_000, 0));
    let event = on.event.clone().expect("it is an event now");
    let steps = (initiative.before())(assistant.clone());
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );

    // A turn a look started spends the look-round's own day, as whoever
    // carries out the assistant's turn counts its model calls.
    let spender = look_round_spender(
        &WorkspaceId::parse(&workspace).unwrap().to_string(),
        &event,
        "Look-round",
        &tilo_workspace_service::agents::today(),
        on.day_limit_micros,
    );
    let model = workspaces
        .workspace_model(WorkspaceId::parse(&workspace).unwrap(), Interaction::Task)
        .spending_as(spender);
    tokio::task::spawn_blocking(move || {
        model.say_blocking(
            ("Look round.", "Something changed."),
            64,
            Instant::now() + Duration::from_secs(10),
            &|| false,
        )
    })
    .await
    .unwrap()
    .expect("the person's model answers the turn");
    assert_eq!(
        read(&initiative, &workspace, &me).spent_today_micros,
        600_000
    );

    // Something is new, and its day is spent: the look starts nothing.
    ada_says(
        &workspaces,
        &workspace,
        &cert,
        &general,
        "An order came in.",
    )
    .await;
    for _ in 0..2 {
        assert_eq!(
            fires(&steps, &cert, &workspace, &event).await,
            Ok(String::new())
        );
    }
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());
    assert_eq!(models.asked().len(), 1, "no model is asked past the limit");
    let spent = read(&initiative, &workspace, &me);
    assert_eq!(spent.looks[0].ended, LookEnded::OverLimit);
    assert!(spent.looks[0].found.changed >= 1, "{:?}", spent.looks[0]);
    assert_eq!(
        spent.looks.len(),
        2,
        "a day that stays spent is said once: {:?}",
        spent.looks
    );

    // Its assistant lowers the limit and is refused raising it.
    let refused = ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::CHANGE,
        json!({"dayLimitMicros": 2_000_000}),
    );
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("only the person")),
        "{refused:?}"
    );
    let lowered = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::CHANGE,
        json!({"dayLimitMicros": 400_000}),
    ));
    assert_eq!(lowered.day_limit_micros, 400_000);

    // Its person raises it: what was found while the day was spent is
    // still new, and is handed on now.
    let raised = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::CHANGE,
        json!({"dayLimitMicros": 2_000_000}),
    ));
    assert_eq!(raised.day_limit_micros, 2_000_000);
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(assistant.taken().len(), 1);
    let told = read(&initiative, &workspace, &me);
    assert_eq!(
        told.looks.iter().map(|look| look.ended).collect::<Vec<_>>(),
        [LookEnded::Told, LookEnded::OverLimit, LookEnded::Quiet]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story: who is answered, a rhythm's bounds, and one switch wherever it is pressed"
)]
async fn the_look_round_is_one_of_its_persons_events_with_their_switch_and_rhythm() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let id = WorkspaceId::parse(&workspace).unwrap();
    let everything: CatalogueQuery = serde_json::from_value(json!({})).unwrap();

    // A member and their assistant are offered it; nobody outside the
    // workspace is.
    let offered = serde_json::to_string(
        &initiative
            .catalogue(id, &theirs, &Authority::Turn, &everything)
            .await,
    )
    .unwrap();
    assert!(offered.contains("\"turn-on\""), "{offered}");
    assert!(
        initiative
            .provider(id, &me, &Authority::Turn)
            .await
            .is_some()
    );
    let bot =
        Caller::bot(GREETER.to_owned(), cert.user_public_key, [5; 32]).in_workspace(&workspace);
    for other in [
        as_another(&workspace),
        as_another(&workspace).delegated_to("scout"),
        bot,
    ] {
        for action in [workspace_initiative::READ, workspace_initiative::TURN_ON] {
            let answer = ask(&initiative, &workspace, &other, action, json!({}));
            assert!(turned_away(&answer), "{action}: {answer:?}");
        }
        assert!(
            initiative
                .provider(id, &other, &Authority::Turn)
                .await
                .is_none()
        );
        let offered = serde_json::to_string(
            &initiative
                .catalogue(id, &other, &Authority::Turn, &everything)
                .await,
        )
        .unwrap();
        assert!(!offered.contains("\"turn-on\""), "{offered}");
    }

    // A rhythm under five minutes, and a moment that is no rhythm, are
    // refused in words, and set nothing up.
    for (when, why) in [
        ("every 60 seconds", "300 seconds"),
        ("once at 2030-01-01T09:00:00Z", "rhythm"),
    ] {
        let refused = ask(
            &initiative,
            &workspace,
            &theirs,
            workspace_initiative::TURN_ON,
            json!({"when": when}),
        );
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains(why)),
            "{when}: {refused:?}"
        );
    }
    assert!(their_events(&events, &workspace, &me).is_empty());

    let on = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({"when": "every 600 seconds"}),
    ));
    assert_eq!(on.when, "every 600 seconds");
    let event = on.event.expect("it is an event now");
    let named = json!({"event": event});

    // Off pauses the event; resumed among the events, it is on again: one
    // switch, wherever it is pressed.
    let off = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_OFF,
        json!({}),
    ));
    assert!(!off.on);
    assert_eq!(
        (off.event.as_deref(), off.next_at_ms),
        (Some(event.as_str()), None)
    );
    assert!(!their_events(&events, &workspace, &me)[0].on);
    of_events(
        &events,
        &workspace,
        &me,
        workspace_events::RESUME,
        named.clone(),
    )
    .unwrap();
    assert!(read(&initiative, &workspace, &theirs).on);
    of_events(
        &events,
        &workspace,
        &me,
        workspace_events::PAUSE,
        named.clone(),
    )
    .unwrap();
    assert!(!read(&initiative, &workspace, &theirs).on);

    // On again is the same event, not a second one, at the rhythm asked.
    let again = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({"when": "every day at 09:00"}),
    ));
    assert!(again.on);
    assert_eq!(again.event.as_deref(), Some(event.as_str()));
    let listed = their_events(&events, &workspace, &me);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].when, "every day at 09:00");
    let slower = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::CHANGE,
        json!({"when": "every 3600 seconds"}),
    ));
    assert_eq!(slower.when, "every 3600 seconds");

    // Removed among the events, it is no look-round any more; turned on
    // again, it is a new event.
    of_events(&events, &workspace, &me, workspace_events::REMOVE, named).unwrap();
    let gone = read(&initiative, &workspace, &me);
    assert!(!gone.on);
    assert_eq!(gone.event, None);
    let anew = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    assert!(anew.on);
    let replaced = anew.event.unwrap();
    assert_ne!(replaced, event);
    of_events(
        &events,
        &workspace,
        &me,
        workspace_events::REMOVE,
        json!({"event": replaced}),
    )
    .unwrap();
    let current = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({}),
    ))
    .event
    .unwrap();
    assert_ne!(current, replaced);

    // A Chat turn handed off by any of these installations still spends the
    // original account. An unstarted job of a removed installation cannot run.
    let person = hex::encode(cert.user_public_key);
    let place = id.to_string();
    let day = tilo_workspace_service::agents::today();
    let expected = look_round_spender(&place, &event, "Look-round", &day, anew.day_limit_micros);
    for author in [&event, &replaced, &current] {
        assert_eq!(
            initiative
                .day_of_turn(&place, &person, author, &day)
                .unwrap()
                .as_ref(),
            Some(&expected)
        );
    }
    let steps = (initiative.before())(assistant.clone());
    assert!(matches!(
        fires(&steps, &cert, &workspace, &replaced).await,
        Err(StepError::Refused(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_looks_when_the_look_round_fires_and_hands_other_events_on_as_they_are() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let on = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    let event = on.event.clone().expect("it is an event now");

    // Its person moves it to a moment a blink away, as they may any event
    // of theirs, and has a reminder beside it at the same moment.
    let when = format!("once at {} ms", tilo_clock::unix_now_ms_i64() + 400);
    of_events(
        &events,
        &workspace,
        &me,
        workspace_events::CHANGE,
        json!({"event": event, "when": when}),
    )
    .unwrap();
    of_events(
        &events,
        &workspace,
        &theirs,
        workspace_events::SET_UP,
        json!({
            "name": "Call home",
            "when": when,
            "instruction": "Say that it is time to call home.",
        }),
    )
    .unwrap();

    // The moment comes. The server looked before it handed the look-round's
    // instruction on and, with nothing new, handed nothing on; the reminder
    // reached the assistant as it was set up.
    let looked = until("the look-round never looked", || {
        Some(read(&initiative, &workspace, &me)).filter(|read| !read.looks.is_empty())
    })
    .await;
    assert_eq!(looked.looks.len(), 1, "{:?}", looked.looks);
    assert_eq!(looked.looks[0].ended, LookEnded::Quiet);
    let taken = until("the reminder never fired", || {
        Some(assistant.taken()).filter(|taken| !taken.is_empty())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(assistant.taken().len(), 1, "{:?}", assistant.taken());
    assert_eq!(taken[0].instruction, "Say that it is time to call home.");
    assert_ne!(taken[0].cause.installation, event);
}

/// The test host runs only the signing sessions its keepers have granted here.
struct HeldAgents {
    workspace: String,
    agents: std::collections::BTreeMap<([u8; 32], String), AgentAccess>,
}

impl AgentEngine for HeldAgents {
    fn access<'a>(
        &'a self,
        workspace: &'a str,
        signer: &'a str,
        keeper: &'a [u8; 32],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentAccess, Ineligible>> + Send + 'a>,
    > {
        let access = if workspace == self.workspace {
            self.agents.get(&(*keeper, signer.to_owned())).cloned()
        } else {
            None
        };
        Box::pin(std::future::ready(access.ok_or(Ineligible::CannotRunAgent)))
    }
}

/// Each keeper creates their own roster entry. The stand-in for the hosted signer
/// reports only these exact workspace, agent and keeper combinations to the scheduler.
async fn holds_assistants(host: &TestHost, workspace: &str, keepers: &[(&DeviceCert, &str)]) {
    use tilo_app_id::apps::{ASSISTANT, CHAT, SHORTCUTS, TASKS};
    use tilo_server_protocol::delegation::DelegateScope;
    use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};
    let mut roster = workspace_agents_document(workspace);
    let mut agents = std::collections::BTreeMap::new();
    for &(cert, id) in keepers {
        roster = roster.as_author(&hex::encode(cert.user_public_key));
        roster
            .apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    name: tilo_resource::NonBlank::parse("Assistant").unwrap(),
                    id: Some(id.to_owned()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .unwrap();
        let created_at = roster
            .state()
            .unwrap()
            .personal_assistant(&hex::encode(cert.user_public_key))
            .unwrap()
            .created_at;
        let key = ed25519_dalek::SigningKey::from_bytes(&rand::random());
        agents.insert(
            (cert.user_public_key, id.to_owned()),
            AgentAccess::Hosted {
                device: key.verifying_key().to_bytes(),
                created_at,
                scope: DelegateScope {
                    workspaces: vec![WorkspaceId::parse(workspace).unwrap()],
                    apps: [CHAT, TASKS, ASSISTANT, SHORTCUTS]
                        .map(str::to_owned)
                        .into(),
                },
            },
        );
    }
    let bytes = roster.doc_mut().save();
    let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    host.workspaces()
        .author_workspace_document(
            workspace,
            host.workspaces().serving_certificate(),
            (tilo_workspace_agent::OWNER, document.as_str()),
            |doc| {
                tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    host.run_agents(Arc::new(HeldAgents {
        workspace: workspace.to_owned(),
        agents,
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_assistant_and_its_server_did_themselves_is_not_news_to_a_look() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    // A server that takes its own count of what an agent spent.
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let person = hex::encode(cert.user_public_key);
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let on = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    let event = on.event.expect("it is an event now");
    let steps = (initiative.before())(assistant.clone());
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );

    // The server counts what a turn of the assistant cost, on the workspace's
    // list of its agents, and the person writes to their assistant in the
    // thread of the two. Both are the assistant's own business: a look that
    // took them for news would start the turn that makes the next ones.
    let kept = tilo_workspace_service::agents::assistant::assistant_of(
        &workspaces,
        &workspace,
        &cert.user_public_key,
    )
    .await
    .unwrap()
    .expect("the person keeps an assistant");
    tilo_workspace_service::agents::count_spend(
        &workspaces,
        &workspace,
        &kept,
        &tilo_workspace_service::agents::today(),
        1_500,
    )
    .await
    .expect("the server counts what the assistant spent");
    let thread = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("assistant"),
    ]);
    post_in(
        &workspaces,
        &workspace,
        &cert,
        (
            &person,
            thread.as_str(),
            "Thank you, that was what I meant.",
        ),
    )
    .await;
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());
    let quiet = read(&initiative, &workspace, &me);
    assert_eq!(quiet.looks[0].ended, LookEnded::Quiet);
    assert_eq!(quiet.looks[0].found.changed, 0, "{:?}", quiet.looks[0]);

    // What somebody else writes in the workspace still is news.
    ada_says(
        &workspaces,
        &workspace,
        &cert,
        &general,
        "The delivery is late.",
    )
    .await;
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(assistant.taken().len(), 1, "{:?}", assistant.taken());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_look_found_and_nothing_took_is_said_and_still_new_at_the_next_look() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    // Nothing takes the first instruction handed over, as on a server that
    // starts no turn of anyone's assistant.
    let assistant = Assistant::answering(vec![Err(StepError::Unavailable(
        "this server starts no turn of a person's assistant".into(),
    ))]);
    let (initiative, _events) = started(&workspaces, &assistant);
    let me = as_person(&cert, &workspace);
    let on = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    let event = on.event.expect("it is an event now");
    let steps = (initiative.before())(assistant.clone());
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    ada_says(
        &workspaces,
        &workspace,
        &cert,
        &general,
        "The printer is out of paper.",
    )
    .await;

    // The look says why it handed nothing on, and its invocation is told to take
    // the step again.
    let failed = fires(&steps, &cert, &workspace, &event).await;
    assert!(
        matches!(&failed, Err(StepError::Unavailable(why)) if why.contains("starts no turn")),
        "{failed:?}"
    );
    let said = read(&initiative, &workspace, &me);
    assert_eq!(said.looks[0].ended, LookEnded::Failed);
    assert!(
        said.looks[0]
            .why
            .as_deref()
            .is_some_and(|why| why.contains("starts no turn")),
        "{:?}",
        said.looks[0]
    );
    assert!(said.looks[0].found.changed >= 1, "{:?}", said.looks[0]);

    // What it found is still new at the next look, and is handed on then.
    assert_eq!(
        fires(&steps, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(assistant.taken().len(), 2, "handed over again");
    let told = read(&initiative, &workspace, &me).looks[0].clone();
    assert_eq!(told.ended, LookEnded::Told);

    // Chat records the event that actually asked on behalf of this person.
    let person = hex::encode(cert.user_public_key);
    let place = WorkspaceId::parse(&workspace).unwrap().to_string();
    let day = tilo_workspace_service::agents::today();
    let program = assistant.taken().last().unwrap().cause.installation.clone();
    assert_eq!(
        initiative
            .day_of_turn(&place, &person, &program, &day)
            .unwrap(),
        Some(look_round_spender(
            &place,
            &event,
            "Look-round",
            &day,
            workspace_initiative::DEFAULT_DAY_LIMIT_MICROS
        ))
    );
    assert_eq!(
        initiative
            .day_of_turn(&place, &person, "another-program", &day)
            .unwrap(),
        None
    );
    assert_eq!(
        initiative
            .day_of_turn(&place, "../somebody", &program, &day)
            .unwrap(),
        None
    );
}

/// One action on what was proposed as `caller` takes it.
fn on_proposals(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let target = workspace_initiative::target(workspace_initiative::PROPOSALS, action);
    answered(initiative.serve(
        WorkspaceId::parse(workspace).unwrap(),
        caller,
        Authority::Turn,
        asking(target, arguments),
    ))
}

/// The proposal `answer` describes.
fn proposal(answer: Result<Value, ActionCallError>) -> Proposal {
    serde_json::from_value(answer.expect("the action is taken")).expect("a proposal is answered")
}

/// What was proposed, as `caller` reads it.
fn proposed(initiative: &Initiative, workspace: &str, caller: &Caller) -> Proposals {
    let answer = on_proposals(
        initiative,
        workspace,
        caller,
        workspace_initiative::LIST,
        json!({}),
    );
    serde_json::from_value(answer.expect("a person lists what was proposed"))
        .expect("proposals are answered")
}

/// Whether `answer` was refused in words that say `why`.
fn refused_for(answer: &Result<Value, ActionCallError>, why: &str) -> bool {
    matches!(answer, Err(ActionCallError::Refused { reason }) if reason.contains(why))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_its_person_turned_down_twice_is_not_proposed_a_third_time() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let compare = |initiative: &Initiative, title: &str| {
        on_proposals(
            initiative,
            &workspace,
            &theirs,
            workspace_initiative::PROPOSE,
            json!({
                "kind": "Compare price pages",
                "title": title,
                "why": "Two of the pages you watch changed since Monday.",
            }),
        )
    };
    let turned_down = |caller: &Caller, proposal: &str, why: Value| {
        let mut arguments = json!({"proposal": proposal});
        if !why.is_null() {
            arguments["why"] = why;
        }
        on_proposals(
            &initiative,
            &workspace,
            caller,
            workspace_initiative::DECLINE,
            arguments,
        )
    };

    // The assistant puts a piece of work in front of its person: it waits.
    let first = proposal(compare(&initiative, "Compare the three shops' price pages"));
    assert_eq!(first.kind, "compare-price-pages");
    assert_eq!(first.state, State::Proposed);
    assert_eq!(
        first.why,
        "Two of the pages you watch changed since Monday."
    );
    assert!(
        first.task.is_none() && first.event.is_none(),
        "this server files no task: {first:?}"
    );
    assert_eq!(
        proposed(&initiative, &workspace, &me).proposals,
        vec![first.clone()]
    );

    // Their person turns it down.
    let declined = proposal(turned_down(&me, &first.id, json!("I look at them myself.")));
    assert_eq!(declined.state, State::Declined);
    assert!(declined.decided_at_ms.is_some());

    let second = proposal(compare(&initiative, "Compare them again"));
    proposal(turned_down(&me, &second.id, Value::Null));
    // A retried decline does not count the same proposal twice.
    proposal(turned_down(&me, &second.id, Value::Null));
    let listed = proposed(&initiative, &workspace, &me);
    assert_eq!(listed.kinds.len(), 1);
    assert_eq!(listed.kinds[0].declined, 2);
    assert!(listed.kinds[0].quiet);
    assert!(refused_for(
        &compare(&initiative, "Third comparison"),
        "turned down"
    ));
    let named = json!({"kind": "Compare price pages"});
    let denied = on_proposals(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::PROPOSE_AGAIN,
        named.clone(),
    );
    assert!(refused_for(&denied, "only the person"));

    // Another proposal stays waiting, and persists across restart.
    let other = proposal(on_proposals(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::PROPOSE,
        json!({
            "kind": "review-open-branches",
            "title": "Review the two open branches",
            "why": "Both have waited for a review since Friday.",
        }),
    ));
    assert_eq!(other.state, State::Proposed);
    drop((initiative, events));
    let (initiative, _events) = started(&workspaces, &assistant);
    assert_eq!(proposed(&initiative, &workspace, &me).proposals.len(), 3);
    assert!(refused_for(
        &compare(&initiative, "Third comparison"),
        "turned down"
    ));
    let reset = on_proposals(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::PROPOSE_AGAIN,
        named,
    )
    .unwrap();
    assert!(
        serde_json::from_value::<Proposals>(reset)
            .unwrap()
            .kinds
            .is_empty()
    );
    let third = proposal(compare(&initiative, "Third comparison"));
    assert_eq!(third.state, State::Proposed);
    let withdrawn = proposal(on_proposals(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::DECLINE,
        json!({"proposal": third.id}),
    ));
    assert_eq!(withdrawn.state, State::Declined);
    assert!(proposed(&initiative, &workspace, &me).kinds.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_its_person_gives_the_nod_and_always_lets_a_kind_start_alone() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let take = |caller: &Caller, action: &str, arguments: Value| {
        on_proposals(&initiative, &workspace, caller, action, arguments)
    };
    let file = |title: &str| {
        take(
            &theirs,
            workspace_initiative::PROPOSE,
            json!({
                "kind": "file-receipts",
                "title": title,
                "why": "Four receipts arrived and none is filed.",
            }),
        )
    };
    let march = proposal(file("File March's receipts"));
    let named = json!({"proposal": march.id});

    // A proposal is the assistant's to make and its person's to accept:
    // neither does the other's part, and nobody else reaches either.
    let own = take(
        &me,
        workspace_initiative::PROPOSE,
        json!({"kind": "file-receipts", "title": "Mine", "why": "Because."}),
    );
    assert!(refused_for(&own, "assistant's to make"), "{own:?}");
    let nod = take(&theirs, workspace_initiative::ACCEPT, named.clone());
    assert!(refused_for(&nod, "only the person"), "{nod:?}");
    for other in [
        as_another(&workspace),
        as_another(&workspace).delegated_to("scout"),
    ] {
        for action in [
            workspace_initiative::LIST,
            workspace_initiative::ACCEPT,
            workspace_initiative::DECLINE,
        ] {
            let answer = take(&other, action, named.clone());
            assert!(turned_away(&answer), "{action}: {answer:?}");
        }
    }
    assert_eq!(
        proposed(&initiative, &workspace, &me).proposals[0].state,
        State::Proposed,
        "nothing of it changed"
    );
    // What each is offered says the same.
    let id = WorkspaceId::parse(&workspace).unwrap();
    let everything: CatalogueQuery = serde_json::from_value(json!({})).unwrap();
    let to_agent = serde_json::to_string(
        &initiative
            .catalogue(id, &theirs, &Authority::Turn, &everything)
            .await,
    )
    .unwrap();
    let to_person = serde_json::to_string(
        &initiative
            .catalogue(id, &me, &Authority::Turn, &everything)
            .await,
    )
    .unwrap();
    assert!(
        to_agent.contains("\"propose\"") && !to_agent.contains("\"accept\""),
        "{to_agent}"
    );
    assert!(
        to_person.contains("\"accept\"") && !to_person.contains("\"propose\""),
        "{to_person}"
    );

    for action in [
        workspace_initiative::ASK_FIRST,
        workspace_initiative::PROPOSE_AGAIN,
    ] {
        assert!(to_person.contains(action) && !to_agent.contains(action));
    }
    let standing = json!({"proposal": march.id, "always": true});
    let accepted = proposal(take(&me, workspace_initiative::ACCEPT, standing.clone()));
    assert_eq!(accepted.state, State::Accepted);
    assert!(
        proposed(&initiative, &workspace, &me)
            .kinds
            .iter()
            .any(|kind| kind.kind == "file-receipts" && kind.always)
    );
    let late = take(
        &me,
        workspace_initiative::DECLINE,
        json!({"proposal": march.id}),
    );
    assert!(refused_for(&late, "begun already"), "{late:?}");

    assert_eq!(
        proposal(file("File April's receipts")).state,
        State::Proposed,
        "this service has no tracker in which to queue the work"
    );
    let event = proposal(take(
        &theirs,
        workspace_initiative::PROPOSE,
        json!({
            "kind":"file-receipts","title":"File monthly","why":"Receipts recur",
            "event":{"name":"File receipts","when":"every day at 18:00", "instruction":"File today's receipts."}
        }),
    ));
    assert_eq!(
        event.state,
        State::Proposed,
        "scheduled work requires its own approval"
    );
    let kind = json!({"kind": "file-receipts"});
    let denied = take(&theirs, workspace_initiative::ASK_FIRST, kind.clone());
    assert!(refused_for(&denied, "only the person"));
    take(&me, workspace_initiative::ASK_FIRST, kind).unwrap();
    // Replaying the original acceptance after it was withdrawn cannot re-grant permission.
    let replay = take(&me, workspace_initiative::ACCEPT, standing);
    assert!(refused_for(&replay, "only a waiting proposal"));
    assert_eq!(proposal(file("File May's receipts")).state, State::Proposed);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proposed_event_is_set_up_only_once_its_person_accepted_it_from_what_the_server_kept() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);
    let take = |caller: &Caller, action: &str, arguments: Value| {
        on_proposals(&initiative, &workspace, caller, action, arguments)
    };

    // What comes round again is proposed as an event: a card to look at.
    let card = proposal(take(
        &theirs,
        workspace_initiative::PROPOSE,
        json!({
            "kind": "weekly-numbers",
            "title": "Sum up the orders every Monday",
            "why": "You asked for this on three Mondays in a row.",
            "event": {
                "name": "Monday numbers",
                "when": "every Monday at 09:00",
                "instruction": "Sum up last week's orders.",
                "style": "Three lines.",
            },
        }),
    ));
    assert_eq!(card.state, State::Proposed);
    assert_eq!(card.set_up, None);
    let event = card.event.clone().expect("it carries the event to look at");
    assert_eq!(event.name, "Monday numbers");
    assert_eq!(event.instruction, "Sum up last week's orders.");
    assert!(
        their_events(&events, &workspace, &me).is_empty(),
        "nothing is set up before the nod"
    );

    // One no server can set up is refused when it is proposed.
    let never = take(
        &theirs,
        workspace_initiative::PROPOSE,
        json!({
            "kind": "weekly-numbers",
            "title": "Say hello soon",
            "why": "Because.",
            "event": {"name": "Soon", "when": "in two minutes", "instruction": "Say hello."},
        }),
    );
    assert!(refused_for(&never, "once at"), "{never:?}");

    // Its person accepts: the event is there, set up from what the server kept,
    // with no reach of its own.
    let named = json!({"proposal": card.id});
    let accepted = proposal(take(&me, workspace_initiative::ACCEPT, named.clone()));
    assert_eq!(accepted.state, State::Accepted);
    let running = their_events(&events, &workspace, &me);
    assert_eq!(running.len(), 1, "{running:?}");
    assert_eq!(accepted.set_up.as_deref(), Some(running[0].id.as_str()));
    assert_eq!(
        (
            running[0].name.as_str(),
            running[0].when.as_str(),
            running[0].instruction.as_str()
        ),
        (
            "Monday numbers",
            event.when.as_str(),
            "Sum up last week's orders."
        )
    );
    assert_eq!(running[0].note["style"], "Three lines.");

    // A nod given again sets nothing up a second time.
    take(&me, workspace_initiative::ACCEPT, named).unwrap();
    assert_eq!(their_events(&events, &workspace, &me).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn retrying_a_saved_event_approval_after_restart_installs_it_exactly_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let (workspace, _) = chat_workspace(workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(workspaces, &assistant);
    let me = as_person(&cert, &workspace);
    let card = proposal(on_proposals(
        &initiative,
        &workspace,
        &as_their_agent(&cert, &workspace),
        workspace_initiative::PROPOSE,
        json!({
            "kind": "weekly-orders", "title": "Review the orders", "why": "Orders arrive weekly.",
            "event": {"name": "Review orders", "when": "every Monday at 09:00", "instruction": "Review this week's orders."}
        }),
    ));
    let file = workspaces
        .workspace_root(&workspace)
        .join("initiative")
        .join(hex::encode(cert.user_public_key))
        .join("initiative.json");
    drop((initiative, events));
    server.shutdown().await;

    // The approval reached disk, but the host did not record a receipt from Events.
    let mut pending: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let row = &mut pending["book"]["proposals"][0];
    assert_eq!(row["id"], card.id);
    row["state"] = json!(State::Accepted);
    row["decidedAtMs"] = json!(card.proposed_at_ms);
    pending["book"]["applying"][&card.id] = json!({});
    let pending = serde_json::to_vec(&pending).unwrap();
    let retry = |initiative: &Initiative| {
        on_proposals(
            initiative,
            &workspace,
            &me,
            workspace_initiative::ACCEPT,
            json!({"proposal": card.id}),
        )
    };
    let mut installed = None;
    for before in [0, 1] {
        // On the second restart the same approval is pending, but Events already committed it.
        std::fs::write(&file, &pending).unwrap();
        let server = bot_server(scratch.path(), &key, &cert).await;
        let (initiative, events) = started(server.workspaces(), &assistant);
        assert_eq!(their_events(&events, &workspace, &me).len(), before);
        let accepted = proposal(retry(&initiative));
        let running = their_events(&events, &workspace, &me);
        assert_eq!(
            running.len(),
            1,
            "the retry recovers exactly one installation"
        );
        assert_eq!(accepted.set_up.as_deref(), Some(running[0].id.as_str()));
        if let Some(first_id) = &installed {
            assert_eq!(&running[0].id, first_id);
            assert_eq!(running[0].name, "My updated order review");
            of_events(
                &events,
                &workspace,
                &me,
                workspace_events::REMOVE,
                json!({"event": first_id}),
            )
            .unwrap();
            proposal(retry(&initiative));
            assert!(
                their_events(&events, &workspace, &me).is_empty(),
                "a completed approval cannot reinstall an event its person removed"
            );
        } else {
            of_events(
                &events,
                &workspace,
                &me,
                workspace_events::CHANGE,
                json!({"event": running[0].id, "name": "My updated order review"}),
            )
            .unwrap();
        }
        installed = accepted.set_up;
        drop((initiative, events));
        server.shutdown().await;
    }

    // The person removed the native event while the approval was still missing its receipt.
    std::fs::write(&file, pending).unwrap();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let (initiative, events) = started(server.workspaces(), &assistant);
    let recovered = proposal(retry(&initiative));
    assert_eq!(recovered.state, State::Accepted);
    assert_eq!(recovered.set_up, installed);
    assert_eq!(proposal(retry(&initiative)), recovered);
    assert!(their_events(&events, &workspace, &me).is_empty());
    drop((initiative, events));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_member_governs_their_own_look_round_and_proposals() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);

    let on = look_round(ask(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::TURN_ON,
        json!({}),
    ));
    assert!(on.on);
    assert_eq!(their_events(&events, &workspace, &me).len(), 1);

    let first = proposal(on_proposals(
        &initiative,
        &workspace,
        &theirs,
        workspace_initiative::PROPOSE,
        json!({"kind": "review", "title": "Review my day", "why": "A deadline is close."}),
    ));
    let editor = member(
        &workspaces,
        &cert,
        &workspace,
        63,
        tilo_server_protocol::workspaces::Role::Editor,
    )
    .await;
    let by_editor = as_person(&editor, &workspace);
    let event = on.event.as_deref().expect("the user's event exists");
    assert!(events.set_up_by(&workspace, event, &cert.user_public_key));
    assert!(!events.set_up_by(&workspace, event, &editor.user_public_key));
    let empty = read(&initiative, &workspace, &by_editor);
    assert_eq!(empty.event, None);
    assert!(!empty.on);
    assert!(
        proposed(&initiative, &workspace, &by_editor)
            .proposals
            .is_empty()
    );
    for action in [workspace_initiative::ACCEPT, workspace_initiative::DECLINE] {
        assert!(turned_away(&on_proposals(
            &initiative,
            &workspace,
            &by_editor,
            action,
            json!({"proposal": first.id})
        )));
    }
    let other = look_round(ask(
        &initiative,
        &workspace,
        &by_editor,
        workspace_initiative::TURN_ON,
        json!({"dayLimitMicros": 75_000}),
    ));
    assert_ne!(other.event, on.event);
    assert_eq!(other.day_limit_micros, 75_000);
    let off = look_round(ask(
        &initiative,
        &workspace,
        &by_editor,
        workspace_initiative::TURN_OFF,
        json!({}),
    ));
    assert!(!off.on);
    let mine = read(&initiative, &workspace, &me);
    assert!(mine.on);
    assert_eq!(mine.event, on.event);
    assert_eq!(
        mine.day_limit_micros,
        workspace_initiative::DEFAULT_DAY_LIMIT_MICROS
    );
    assert_eq!(proposed(&initiative, &workspace, &me).proposals, [first]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_new_service_recovers_only_the_owners_recorded_schedule_and_proposals() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let editor = member(
        &workspaces,
        &cert,
        &workspace,
        63,
        tilo_server_protocol::workspaces::Role::Editor,
    )
    .await;
    let me = as_person(&cert, &workspace);
    let other = as_person(&editor, &workspace);
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let original = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({"dayLimitMicros": 8181}),
    ));
    let proposal = proposal(on_proposals(
        &initiative,
        &workspace,
        &as_their_agent(&cert, &workspace),
        workspace_initiative::PROPOSE,
        json!({"kind": "read", "title": "Read my report", "why": "The report arrived."}),
    ));
    drop((initiative, events));

    // Restore the workspace-wide format used by the preceding release. The
    // native event's stored author independently identifies the real owner.
    let root = workspaces.workspace_root(&workspace);
    let person = hex::encode(cert.user_public_key);
    let private = root
        .join("initiative")
        .join(&person)
        .join("initiative.json");
    let mut old: Value = serde_json::from_slice(&std::fs::read(&private).unwrap()).unwrap();
    old["person"] = json!(person);
    old.as_object_mut().unwrap().remove("spendingEvent");
    let shared = root.join("initiative.json");
    std::fs::write(&shared, serde_json::to_vec(&old).unwrap()).unwrap();
    std::fs::remove_file(&private).unwrap();

    let (initiative, events) = started(&workspaces, &assistant);
    assert!(
        proposed(&initiative, &workspace, &other)
            .proposals
            .is_empty()
    );
    let restored = read(&initiative, &workspace, &me);
    assert_eq!(restored.event, original.event);
    assert_eq!(restored.day_limit_micros, 8181);
    assert_eq!(proposed(&initiative, &workspace, &me).proposals, [proposal]);
    drop((initiative, events));

    // A second person could resume the event and overwrite the old person
    // field. Its author still owns its schedule; nobody inherits an ambiguous book.
    old["person"] = json!(hex::encode(editor.user_public_key));
    std::fs::write(&shared, serde_json::to_vec(&old).unwrap()).unwrap();
    let (initiative, _) = started(&workspaces, &assistant);
    assert_eq!(read(&initiative, &workspace, &me).event, original.event);
    assert_eq!(read(&initiative, &workspace, &other).event, None);
    for caller in [&me, &other] {
        assert!(
            proposed(&initiative, &workspace, caller)
                .proposals
                .is_empty()
        );
    }

    // An old look already handed to Chat survives removal of its event. The
    // original owner can no longer be verified, so nobody runs it unbudgeted.
    drop(initiative);
    let removed = uuid::Uuid::new_v4().to_string();
    old["event"] = json!(removed);
    std::fs::write(&shared, serde_json::to_vec(&old).unwrap()).unwrap();
    let (initiative, _) = started(&workspaces, &assistant);
    let day = tilo_workspace_service::agents::today();
    let place = WorkspaceId::parse(&workspace).unwrap().to_string();
    let unrelated = uuid::Uuid::new_v4().to_string();
    for person in [person, hex::encode(editor.user_public_key)] {
        assert!(
            initiative
                .day_of_turn(&place, &person, &removed, &day)
                .is_err()
        );
        assert_eq!(
            initiative
                .day_of_turn(&place, &person, &unrelated, &day)
                .unwrap(),
            None
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_and_their_private_chat_can_look_now_without_enabling_a_schedule() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let (workspace, _) = chat_workspace(server.workspaces(), &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let person = hex::encode(cert.user_public_key);
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(server.workspaces(), &assistant);
    let me = as_person(&cert, &workspace);
    let op = workspace_initiative::LOOK_NOW;
    let target = workspace_initiative::target(workspace_initiative::LOOK_ROUND, op);
    let request = asking(target, json!({}));
    // Losing the receipt and sending the very same signed invocation starts only one turn.
    let send = || {
        answered(initiative.serve(
            WorkspaceId::parse(&workspace).unwrap(),
            &me,
            Authority::Turn,
            request.clone(),
        ))
    };
    let first = look_round(send());
    assert!(!first.on && first.event.is_none() && first.next_at_ms.is_none());
    assert_eq!(first.looks[0].ended, LookEnded::Told);
    assert_eq!(look_round(send()), first);
    let handed = assistant.taken();
    assert_eq!(handed.len(), 1);
    assert!(
        handed[0]
            .instruction
            .contains("propose concrete tasks for today")
    );
    assert!(their_events(&events, &workspace, &me).is_empty());

    // A group conversation or a different agent cannot ask what belongs to this person's
    // assistant. The real agent in the person's direct Chat receives the look in this turn.
    let place = |agent, conversation| {
        as_person(&cert, &workspace).delegated_to(agent).on_turn(
            tilo_actions::policy::turn::Turn::new(conversation, "a-message"),
            Vec::new(),
        )
    };
    let direct = chat::Chat::direct_id([
        tilo_actions::policy::MemberId::person(person),
        tilo_actions::policy::MemberId::agent("assistant"),
    ]);
    let address = chat::summons::conversation_address(&direct);
    for unauthorized in [
        place("assistant", "elsewhere"),
        place("another", &address),
        as_another(&workspace),
    ] {
        assert!(turned_away(&ask(
            &initiative,
            &workspace,
            &unauthorized,
            op,
            json!({})
        )));
    }
    let current = look_round(ask(
        &initiative,
        &workspace,
        &place("assistant", &address),
        op,
        json!({}),
    ));
    assert_eq!(current.looks.len(), 1);
    assert_eq!(
        read(&initiative, &workspace, &me).looks,
        first.looks,
        "the delegated look leaves the person’s cursor and history intact"
    );
    assert_eq!(
        assistant.taken().len(),
        1,
        "the assistant keeps its current turn"
    );
    assert!(matches!(
        ask(&initiative, &workspace, &me, op, json!({"as": "another"})),
        Err(ActionCallError::InvalidRequest { .. })
    ));

    // The same budget applies with the trigger off, regardless of how the check is invoked.
    look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::CHANGE,
        json!({"dayLimitMicros": 0}),
    ));
    let refused = ask(&initiative, &workspace, &me, op, json!({}));
    assert!(
        matches!(refused, Err(ActionCallError::Refused { ref reason }) if reason.contains("day limit")),
        "{refused:?}"
    );
    assert_eq!(
        read(&initiative, &workspace, &me).looks[0].ended,
        LookEnded::OverLimit
    );
    assert_eq!(assistant.taken().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_agent_change_never_raises_the_owners_reduced_limit() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(&workspaces, &assistant);
    let person = as_person(&cert, &workspace);
    let agent = as_their_agent(&cert, &workspace);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let change = |caller: Caller, limit: u64| {
        let (initiative, workspace, barrier) =
            (initiative.clone(), workspace.clone(), Arc::clone(&barrier));
        tokio::task::spawn_blocking(move || {
            barrier.wait();
            ask(
                &initiative,
                &workspace,
                &caller,
                workspace_initiative::CHANGE,
                json!({"dayLimitMicros": limit}),
            )
        })
    };
    let (person_answer, agent_answer) = tokio::join!(change(person.clone(), 0), change(agent, 100));
    assert_eq!(look_round(person_answer.unwrap()).day_limit_micros, 0);
    let agent_answer = agent_answer.unwrap();
    assert!(agent_answer.is_ok() || refused_for(&agent_answer, "only the person"));
    assert_eq!(read(&initiative, &workspace, &person).day_limit_micros, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_immediate_and_a_scheduled_look_cannot_advance_the_same_cursor_together() {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

    struct Held {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }
    impl Steps for Held {
        fn instruct(&self, _: Instruction) -> Result<String, StepError> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Ok(String::new())
        }
        fn judge(&self, _: Judgement) -> Result<Value, StepError> {
            panic!("the look does not judge anything")
        }
    }

    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let (workspace, _) = chat_workspace(server.workspaces(), &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let (entered, entering) = sync_channel(1);
    let (release, releasing) = sync_channel(1);
    let holds: Arc<dyn Steps> = Arc::new(Held {
        entered,
        release: Mutex::new(releasing),
    });
    let (initiative, _events) = Initiative::start_with(
        server.workspaces().clone(),
        holds.clone(),
        Arc::new(crate::workspace_bots::installed::EventRunner),
    );
    let me = as_person(&cert, &workspace);
    let event = look_round(ask(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        json!({}),
    ))
    .event
    .unwrap();
    let pending = initiative.serve(
        WorkspaceId::parse(&workspace).unwrap(),
        &me,
        Authority::Turn,
        asking(
            workspace_initiative::target(
                workspace_initiative::LOOK_ROUND,
                workspace_initiative::LOOK_NOW,
            ),
            json!({}),
        ),
    );
    tokio::task::spawn_blocking(move || entering.recv_timeout(Duration::from_secs(10)))
        .await
        .unwrap()
        .unwrap();
    let scheduled = (initiative.before())(holds);
    let arrival = fires(&scheduled, &cert, &workspace, &event).await;
    assert!(
        matches!(arrival, Err(StepError::Unavailable(why)) if why.contains("already in progress"))
    );
    release.send(()).unwrap();
    assert_eq!(look_round(answered(pending)).looks.len(), 1);
    assert_eq!(
        fires(&scheduled, &cert, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(
        read(&initiative, &workspace, &me).looks[0].ended,
        LookEnded::Quiet
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partly_saved_look_round_waits_for_its_owners_limit() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let assistant = Assistant::answering(vec![]);
    let (initiative, events) = started(&workspaces, &assistant);
    let me = as_person(&cert, &workspace);
    let turn_on = |initiative: &Initiative| {
        look_round(ask(
            initiative,
            &workspace,
            &me,
            workspace_initiative::TURN_ON,
            json!({"dayLimitMicros": 0}),
        ))
    };
    let event = turn_on(&initiative).event.unwrap();
    drop((initiative, events));

    // A stopped host left the scheduler's write on disk. The person's record,
    // which holds the limit, had not been published when the machine stopped.
    let root = workspaces.workspace_root(&workspace);
    std::fs::remove_file(
        root.join("initiative")
            .join(hex::encode(cert.user_public_key))
            .join("initiative.json"),
    )
    .unwrap();
    let (initiative, events) = started(&workspaces, &assistant);
    let steps = (initiative.before())(assistant.clone());
    assert!(matches!(
        fires(&steps, &cert, &workspace, &event).await,
        Err(StepError::Unavailable(_))
    ));
    assert!(assistant.taken().is_empty());
    let retried = turn_on(&initiative);
    assert_eq!(retried.event.as_deref(), Some(event.as_str()));
    assert_eq!(retried.day_limit_micros, 0);
    assert_eq!(their_events(&events, &workspace, &me).len(), 1);
}

/// Two native Tasks creates on the same chain part, each due and assigned to
/// the editor, as its owner posts them through the app.
fn due_task_changes(
    workspace: &str,
    owner: &DeviceCert,
    editor: &DeviceCert,
) -> (collab_types::document::DocumentId, [DocumentMutation; 2]) {
    use collab_types::document::{DocumentId, chain_resource_id};
    let chain = DocumentId::derived("issues", &DocumentId::workspace_default());
    let part = chain.part(0);
    let mut page = Replica::open(
        &AppCatalogue::native(),
        tilo_app_id::apps::TASKS,
        workspace,
        &MemberId::person(hex::encode(owner.user_public_key)),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let creates = ["First due task", "Newly restricted task"].map(|title| {
        page.invoke(
            &tilo_actions::api::ActionAddress::new(chain_resource_id(&chain), "create"),
            json!({
                "title": title, "due_ms": 1,
                "assignee": hex::encode(editor.user_public_key)
            })
            .as_object()
            .unwrap(),
            false,
        )
        .unwrap();
        page.take_document_mutations()
            .unwrap()
            .into_iter()
            .find(|(id, _)| id == &part)
            .expect("Tasks wrote its issue part")
            .1
    });
    (part, creates)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persons_next_look_honours_the_revoked_read_of_a_tasks_record() {
    use tilo_action_policy::{Grant, Grantee};
    use tilo_app_id::apps::TASKS;
    let scratch = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = bot_server(scratch.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _) = chat_workspace(workspaces, &owner).await;
    let editor = member(
        workspaces,
        &owner,
        &workspace,
        74,
        tilo_server_protocol::workspaces::Role::Editor,
    )
    .await;
    holds_assistants(
        &server,
        &workspace,
        &[(&owner, "assistant"), (&editor, "editor-assistant")],
    )
    .await;
    let editor_as_person = as_person(&editor, &workspace);
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(workspaces, &assistant);
    let check = || {
        look_round(ask(
            &initiative,
            &workspace,
            &editor_as_person,
            workspace_initiative::LOOK_NOW,
            json!({}),
        ))
    };
    let scheduled = look_round(ask(
        &initiative,
        &workspace,
        &as_person(&owner, &workspace),
        workspace_initiative::TURN_ON,
        json!({}),
    ))
    .event
    .unwrap();
    let steps = (initiative.before())(assistant.clone());
    let owner_looks = async || {
        fires(&steps, &owner, &workspace, &scheduled).await.unwrap();
    };
    owner_looks().await;
    check();

    let (part, [first, second]) = due_task_changes(&workspace, &owner, &editor);
    let write = async |change: &DocumentMutation| {
        workspaces
            .author_workspace_document(&workspace, &owner, (TASKS, part.as_str()), |doc| {
                tilo_automerge::ApplyIncoming::load_incoming(doc, change.data())
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    };
    write(&first).await;
    let editor_may_read = async || {
        workspaces
            .state(&workspace)
            .await
            .unwrap()
            .check_document_read(
                editor.device_public_key,
                TASKS,
                part.as_str(),
                tilo_clock::wall(),
            )
            .is_ok()
    };
    assert!(editor_may_read().await);
    let before = check();
    assert_eq!(before.looks[0].found.due, 1);
    assert_eq!(before.looks[0].found.open, 1);
    assert!(
        assistant
            .taken()
            .last()
            .unwrap()
            .instruction
            .contains("First due task")
    );
    owner_looks().await;

    let governing_record = collab_types::document_access_target(TASKS, part.as_str()).unwrap();
    let deny = Grant::new(
        "editor-cannot-read-issues",
        Grantee::person(hex::encode(editor.user_public_key)),
        collab_types::document_node(&workspace, TASKS, governing_record),
        KindSet::READ,
        Answer::Deny,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_workspace_grants(&workspace, &owner, [73; 32], vec![deny])
        .await
        .unwrap();
    assert!(!editor_may_read().await);
    check();
    write(&second).await;
    let after = check();
    assert_eq!(
        (after.looks[0].found.due, after.looks[0].found.open),
        (0, 0)
    );
    assert_eq!(after.looks[0].found.changed, 0);
    let restricted = assistant.taken().pop().unwrap();
    assert_eq!(restricted.person, hex::encode(editor.user_public_key));
    assert!(!restricted.instruction.contains(part.as_str()));
    assert!(!restricted.instruction.contains("Newly restricted task"));

    owner_looks().await;
    let visible = assistant.taken().pop().unwrap();
    assert_eq!(visible.person, hex::encode(owner.user_public_key));
    assert!(visible.instruction.contains(part.as_str()));
    assert!(visible.instruction.contains("Newly restricted task"));
}

async fn set_reader_bound(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    rules: &[tilo_action_policy::Rule],
) {
    use tilo_workspace_agent::{Bound, WorkspaceAgents, workspace_agents};
    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let bound = rules.iter().map(Bound::of).collect();
    workspaces
        .author_workspace_document(
            workspace,
            cert,
            (tilo_workspace_agent::OWNER, &list),
            |doc| {
                let mut roster =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                        .as_author(&hex::encode(cert.user_public_key))
                        .in_app(tilo_workspace_agent::OWNER, "Assistant");
                roster
                    .apply(
                        WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
                            agent: tilo_resource::Id::from_raw("assistant"),
                            bound,
                        }),
                    )
                    .map_err(|error| error.to_string())?;
                std::mem::swap(doc, roster.doc_mut());
                Ok(())
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_chat_turn_sees_only_its_granted_records_without_advancing_the_persons_cursor() {
    use tilo_app_id::apps::TASKS;
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let (workspace, _) = chat_workspace(workspaces, &cert).await;
    holds_assistants(&server, &workspace, &[(&cert, "assistant")]).await;
    let person = hex::encode(cert.user_public_key);
    let assistant = Assistant::answering(vec![]);
    let (initiative, _events) = started(workspaces, &assistant);
    let me = as_person(&cert, &workspace);
    let manual = |by: &Caller| {
        look_round(ask(
            &initiative,
            &workspace,
            by,
            workspace_initiative::LOOK_NOW,
            json!({}),
        ))
    };
    let baseline = manual(&me);
    assert!(!baseline.looks.is_empty());
    let delegated = as_their_agent(&cert, &workspace);
    assert!(read(&initiative, &workspace, &delegated).looks.is_empty());
    let (part, [change, _]) = due_task_changes(&workspace, &cert, &cert);
    workspaces
        .author_workspace_document(&workspace, &cert, (TASKS, part.as_str()), |doc| {
            tilo_automerge::ApplyIncoming::load_incoming(doc, change.data())
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let direct = chat::Chat::direct_id([MemberId::person(person), MemberId::agent("assistant")]);
    let by_agent = |bound| {
        me.clone().delegated_to("assistant").on_turn(
            tilo_actions::policy::turn::Turn::new(
                chat::summons::conversation_address(&direct),
                "reading",
            ),
            bound,
        )
    };
    let denied = tilo_action_policy::Rule::new(
        collab_types::document_node(&workspace, TASKS, part.as_str()),
        KindSet::READ,
        Answer::Deny,
    );
    set_reader_bound(workspaces, &workspace, &cert, std::slice::from_ref(&denied)).await;
    let narrow = manual(&by_agent(vec![denied]));
    assert_eq!(narrow.looks[0].found.due, 0);
    assert_eq!(narrow.looks[0].found.open, 0);
    assert_eq!(read(&initiative, &workspace, &me).looks, baseline.looks);
    let requested = manual(&me);
    assert_eq!(requested.looks[0].found.due, 0);
    assert_eq!(requested.looks[0].found.open, 0);
    set_reader_bound(workspaces, &workspace, &cert, &[]).await;
    let broad = manual(&by_agent(Vec::new()));
    assert_eq!(broad.looks[0].found.due, 1);
    assert_eq!(broad.looks[0].found.open, 1);
    assert_eq!(read(&initiative, &workspace, &me).looks, requested.looks);
    assert_eq!(manual(&me).looks[0].found.due, 1);
}
