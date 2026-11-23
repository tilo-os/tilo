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
    assert_ne!(anew.event.as_deref(), Some(event.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_looks_when_the_look_round_fires_and_hands_other_events_on_as_they_are() {
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

/// `person` keeps an assistant in `workspace`, as their own device makes
/// one: an agent in the workspace's list of its agents.
async fn keeps_an_assistant(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    acts_for: &str,
) {
    use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};
    assert!(
        cert == workspaces.governor(),
        "the server writes the list as its governor"
    );
    workspaces
        .author_agents(workspace, |doc| {
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .as_author(acts_for);
            let applied = made
                .apply(
                    WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                        name: tilo_resource::NonBlank::parse("Assistant").unwrap(),
                        id: Some("assistant".into()),
                        model: None,
                        mcp_client: None,
                    }),
                )
                .map(drop)
                .map_err(|error| error.to_string());
            std::mem::swap(doc, made.doc_mut());
            applied
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_assistant_and_its_server_did_themselves_is_not_news_to_a_look() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    // A server that takes its own count of what an agent spent.
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let person = hex::encode(cert.user_public_key);
    keeps_an_assistant(&workspaces, &workspace, &cert, &person).await;
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

    // The turn that look started counts on the look-round's own day: the
    // message that asks for it is the one its handing over names by the
    // look's id, and no other message is.
    let person = hex::encode(cert.user_public_key);
    let place = WorkspaceId::parse(&workspace).unwrap().to_string();
    let day = tilo_workspace_service::agents::today();
    let asks = chat::cause::message_id(
        &tilo_actions::policy::MemberId::person(person.clone()),
        &told.id,
    )
    .to_string();
    assert_eq!(
        initiative.day_of_turn(&place, &person, &asks, &day),
        Some(look_round_spender(
            &place,
            &event,
            "Look-round",
            &day,
            workspace_initiative::DEFAULT_DAY_LIMIT_MICROS,
        ))
    );
    assert_eq!(
        initiative.day_of_turn(&place, &person, "another-message", &day),
        None
    );
    assert_eq!(
        initiative.day_of_turn(&place, "../somebody", &asks, &day),
        None,
        "what is no person's root names nobody here"
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
    assert_eq!(proposed(&initiative, &workspace, &me).proposals.len(), 2);
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

    // Their person nods, and work that has begun is not turned down afterwards.
    let accepted = proposal(take(
        &me,
        workspace_initiative::ACCEPT,
        json!({"proposal": march.id}),
    ));
    assert_eq!(accepted.state, State::Accepted);
    let late = take(
        &me,
        workspace_initiative::DECLINE,
        json!({"proposal": march.id}),
    );
    assert!(refused_for(&late, "begun already"), "{late:?}");

    // A further proposal of the same kind still waits for its person's nod.
    assert_eq!(
        proposal(file("File April's receipts")).state,
        State::Proposed
    );
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
async fn another_member_reads_and_stops_the_workspaces_look_round() {
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

    // Another member of the workspace reads the same look-round and stops it:
    // it is the workspace's, and its event is paused with it.
    let editor = member(
        &workspaces,
        &cert,
        &workspace,
        63,
        tilo_server_protocol::workspaces::Role::Editor,
    )
    .await;
    let by_editor = as_person(&editor, &workspace);
    let same = read(&initiative, &workspace, &by_editor);
    assert_eq!(same.event, on.event);
    assert_eq!(same.when, on.when);
    let off = look_round(ask(
        &initiative,
        &workspace,
        &by_editor,
        workspace_initiative::TURN_OFF,
        json!({}),
    ));
    assert!(!off.on);
    assert!(
        !read(&initiative, &workspace, &me).on,
        "it is the workspace's look-round"
    );
    assert!(
        !their_events(&events, &workspace, &me)[0].on,
        "its event is paused with it"
    );
}
