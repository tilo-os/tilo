//! A workspace's events through the host's own actions
//! ([`tilo_workspace_service::agents::events`]), as a device and an agent's turn take
//! them: who may set one up and reach it, what a listing says of it, that
//! every member the workspace's policy lets sees and changes the same ones,
//! and that the host fires what was set up, once, also across a restart.

use std::time::{Duration, Instant};

use serde_json::json;
use tilo_actions::MemberId;
use tilo_actions::remote::workspace_events::{self, Ended, EventView, Events, Sort};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::Role;

use super::*;
use tilo_workspace_service::agents::events::{Authority, WorkspaceEvents};

/// One action on a workspace's events as `caller` on `certificate`'s device
/// takes it, through what the host's app action server answers with.
async fn take_on(
    events: &WorkspaceEvents,
    workspace: &str,
    certificate: &DeviceCert,
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
        target: workspace_events::target(action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let Some(answer) = events.serve(
        workspace,
        caller,
        Authority::of(certificate),
        RemoteActionMessage::Invoke(request),
    ) else {
        return Err(unanswered("an action on events is answered"));
    };
    let Some(answered) = answer.await else {
        return Err(unanswered("an action on events answered nothing"));
    };
    match answered {
        RemoteActionMessage::Receipt(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Rejected { error } => Err(error),
            other => Err(unanswered(&format!(
                "an action on events ends at once: {other:?}"
            ))),
        },
        _ => Err(unanswered("an action is answered with its receipt")),
    }
}

/// One action on a workspace's events as a turn this server carries out for
/// `caller`'s member takes it: what an assistant's own tool call is.
async fn take_as_turn(
    events: &WorkspaceEvents,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let Some(workspace) = WorkspaceId::parse(workspace) else {
        return Err(ActionCallError::UnknownAction);
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: workspace_events::target(action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    match events
        .invoke(workspace, caller, &Authority::Turn, &request)
        .await
        .status
    {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => Err(ActionCallError::Unavailable {
            reason: format!("an action in a turn ends at once: {other:?}"),
        }),
    }
}

/// The event `answer` describes.
fn event(answer: Result<serde_json::Value, ActionCallError>) -> EventView {
    serde_json::from_value(answer.expect("the action is taken")).expect("an event is answered")
}

/// The events of `workspace` whose policy lets `caller` reach them.
async fn listed(
    events: &WorkspaceEvents,
    workspace: &str,
    certificate: &DeviceCert,
    caller: &Caller,
) -> Vec<EventView> {
    let answer = take_on(
        events,
        workspace,
        certificate,
        caller,
        workspace_events::LIST,
        json!({}),
    )
    .await;
    serde_json::from_value::<Events>(answer.expect("a member lists the workspace's events"))
        .expect("events are answered")
        .events
}

/// A listing of everything a caller is offered: the empty query.
fn everything() -> CatalogueQuery {
    serde_json::from_value(json!({})).expect("the empty query asks for everything")
}

/// A member on a device of theirs, in `workspace`.
fn as_person(cert: &DeviceCert, workspace: &str) -> Caller {
    Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace)
}

/// That member's assistant, on a turn their session was handed to.
fn as_their_agent(cert: &DeviceCert, workspace: &str) -> Caller {
    as_person(cert, workspace)
        .delegated_to("assistant")
        .named("Assistant")
}

/// Another person, on a device of their own: no member of the workspace.
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

/// The events of the workspace a test's host governs, as its members reach
/// them.
struct Theirs {
    events: WorkspaceEvents,
    workspace: String,
    cert: DeviceCert,
}

impl Theirs {
    /// Its events, with nothing taking what they hand over.
    fn on(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert) -> Self {
        Self {
            events: WorkspaceEvents::start_with(
                workspaces.clone(),
                Assistant::answering(vec![]),
                std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
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

    async fn take(
        &self,
        caller: &Caller,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        take_on(
            &self.events,
            &self.workspace,
            &self.cert,
            caller,
            action,
            arguments,
        )
        .await
    }

    /// The same as a turn this server carries out for the caller's member:
    /// what an assistant's own tool call is.
    async fn take_in_turn(
        &self,
        caller: &Caller,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        take_as_turn(&self.events, &self.workspace, caller, action, arguments).await
    }

    /// Its events, as they read them on a device of their own.
    async fn listed(&self) -> Vec<EventView> {
        listed(&self.events, &self.workspace, &self.cert, &self.me()).await
    }

    /// The event their assistant sets up as [`morning_numbers`] describes.
    async fn set_up(&self) -> EventView {
        event(
            self.take_in_turn(&self.agent(), workspace_events::SET_UP, morning_numbers())
                .await,
        )
    }

    /// What `caller` is offered on the workspace's events, as a catalogue's
    /// page reads.
    async fn offered(&self, caller: &Caller) -> String {
        let page = self
            .events
            .catalogue(self.id(), caller, &Authority::of(&self.cert), &everything())
            .await;
        serde_json::to_string(&page).expect("a page is written")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_members_assistant_sets_an_event_up_and_another_member_reads_and_changes_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(&workspaces, &workspace, &cert);

    let made = theirs.set_up().await;
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
    assert_eq!(
        made.by,
        MemberId::agent("Assistant"),
        "authorship is who set it up"
    );

    // An agent is offered the event actions.
    let offered = theirs.offered(&theirs.agent()).await;
    assert!(offered.contains("\"set-up\""), "{offered}");
    assert!(
        theirs
            .events
            .provider(theirs.id(), &theirs.agent(), &Authority::of(&theirs.cert))
            .await
            .is_some()
    );

    // The person reads the same on a device of their own.
    assert_eq!(theirs.listed().await, vec![made.clone()]);

    // The member sets one up themselves too, on a device of theirs.
    let own = event(
        theirs
            .take(
                &theirs.me(),
                workspace_events::SET_UP,
                json!({
                    "name": "Weekly review",
                    "when": "every Monday at 09:00",
                    "instruction": "Sum up the week.",
                }),
            )
            .await,
    );
    assert_eq!(own.by, MemberId::person(hex::encode(cert.user_public_key)));

    // Another member of the workspace, an editor, lists both and changes the
    // one the first set up themselves: the events are the workspace's, and a
    // change keeps the authorship of whoever set one up.
    let editor = member(&workspaces, &cert, &workspace, 61, Role::Editor).await;
    let by_editor = as_person(&editor, &workspace);
    let seen = listed(&theirs.events, &workspace, &editor, &by_editor).await;
    assert_eq!(seen.len(), 2, "{seen:?}");
    for id in [&made.id, &own.id] {
        assert!(seen.iter().any(|event| &event.id == id), "{seen:?}");
    }
    let changed = event(
        take_on(
            &theirs.events,
            &workspace,
            &editor,
            &by_editor,
            workspace_events::CHANGE,
            json!({"event": own.id, "name": "Weekly numbers"}),
        )
        .await,
    );
    assert_eq!(changed.id, own.id, "it is the same event");
    assert_eq!(changed.name, "Weekly numbers");
    assert_eq!(changed.by, own.by, "authorship stays who set it up");
    let seen = theirs.listed().await;
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        seen.iter()
            .any(|event| event.id == own.id && event.name == "Weekly numbers")
    );
    assert!(
        seen.iter()
            .any(|event| event.id == made.id && event.name == "Morning numbers")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_reads_the_workspaces_events_and_changes_none() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(&workspaces, &workspace, &cert);
    let made = theirs.set_up().await;

    let viewer = member(&workspaces, &cert, &workspace, 62, Role::Viewer).await;
    let reader = as_person(&viewer, &workspace);
    assert_eq!(
        listed(&theirs.events, &workspace, &viewer, &reader).await,
        vec![made.clone()],
        "a viewer reads the workspace's events"
    );
    for (action, arguments) in [
        (
            workspace_events::CHANGE,
            json!({"event": made.id, "name": "Night numbers"}),
        ),
        (workspace_events::SET_UP, morning_numbers()),
        (workspace_events::PAUSE, json!({"event": made.id})),
        (workspace_events::REMOVE, json!({"event": made.id})),
    ] {
        let refused = take_on(
            &theirs.events,
            &workspace,
            &viewer,
            &reader,
            action,
            arguments,
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{action}: {refused:?}"
        );
    }
    assert_eq!(theirs.listed().await, vec![made], "nothing of it changed");
}

/// Older servers recorded the personal schedule's id alongside the events. The
/// actual event still records the person it was created for.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_personal_schedule_is_only_offered_to_its_creator_after_restart() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let editor = member(&workspaces, &cert, &workspace, 62, Role::Editor).await;
    let by_editor = as_person(&editor, &workspace);
    let first = Theirs::on(&workspaces, &workspace, &cert);
    let personal = first.set_up().await.id;
    let shared = first.set_up().await.id;
    drop(first);

    // The old workspace control could be turned on again by a second member.
    // Even then it is the original person's assistant that runs the event.
    let root = workspaces.workspace_root(&workspace);
    let saved = root.join("events").join(&personal).join("event.json");
    let mut description: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    description.as_object_mut().unwrap().remove("personal");
    std::fs::write(&saved, serde_json::to_vec(&description).unwrap()).unwrap();
    let old = root.join("initiative.json");
    std::fs::write(
        &old,
        serde_json::to_vec(
            &json!({"event": personal, "person": hex::encode(editor.user_public_key)}),
        )
        .unwrap(),
    )
    .unwrap();

    for _ in 0..2 {
        let restarted = Theirs::on(&workspaces, &workspace, &cert);
        assert_eq!(restarted.listed().await.len(), 2);
        let others = listed(&restarted.events, &workspace, &editor, &by_editor).await;
        assert_eq!(
            others
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [shared.as_str()]
        );
        for (action, arguments) in [
            (
                workspace_events::CHANGE,
                json!({"event":personal,"guidance":"changed"}),
            ),
            (workspace_events::PAUSE, json!({"event":personal})),
            (workspace_events::RESUME, json!({"event":personal})),
            (workspace_events::REMOVE, json!({"event":personal})),
        ] {
            let result = take_on(
                &restarted.events,
                &workspace,
                &editor,
                &by_editor,
                action,
                arguments,
            )
            .await;
            assert!(
                matches!(result, Err(ActionCallError::Refused { ref reason })
                if reason == "the workspace keeps no such event"),
                "{action}: {result:?}"
            );
        }
        let paused = event(
            restarted
                .take(
                    &restarted.me(),
                    workspace_events::PAUSE,
                    json!({"event":personal}),
                )
                .await,
        );
        assert!(!paused.on);
        // On subsequent starts the event carries its own scope.
        if old.exists() {
            std::fs::remove_file(&old).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_and_a_non_member_set_nothing_up_and_reach_no_event() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(&workspaces, &workspace, &cert);
    let made = theirs.set_up().await;
    let named = json!({"event": made.id});

    // A person the workspace does not have, and an agent acting for them,
    // set nothing up here and reach none of its events.
    for other in [
        as_another(&workspace),
        as_another(&workspace).delegated_to("scout"),
    ] {
        for action in [
            workspace_events::SET_UP,
            workspace_events::LIST,
            workspace_events::PAUSE,
            workspace_events::CHANGE,
            workspace_events::REMOVE,
        ] {
            let refused = theirs.take(&other, action, named.clone()).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{action}: {refused:?}"
            );
        }
        assert!(
            theirs
                .events
                .provider(theirs.id(), &other, &Authority::of(&theirs.cert))
                .await
                .is_none()
        );
        let offered = theirs.offered(&other).await;
        assert!(!offered.contains("\"set-up\""), "{offered}");
    }
    // Neither does a bot, whoever installed it.
    let bot =
        Caller::bot(GREETER.to_owned(), cert.user_public_key, [5; 32]).in_workspace(&workspace);
    assert!(matches!(
        theirs.take(&bot, workspace_events::LIST, json!({})).await,
        Err(ActionCallError::UnknownAction)
    ));
    assert_eq!(theirs.listed().await, vec![made], "nothing of it changed");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_is_changed_paused_and_removed_by_a_member() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let theirs = Theirs::on(&workspaces, &workspace, &cert);
    let agent = theirs.agent();
    let made = theirs.set_up().await;
    let named = json!({"event": made.id});

    // Changed: what starts it, its filter, its count, what it is told, and
    // its note. What is left out stays.
    let changed = event(
        theirs
            .take_in_turn(
                &agent,
                workspace_events::CHANGE,
                json!({
                    "event": made.id,
                    "when": "once at 2030-01-01T09:00:00Z",
                    "onlyIf": "",
                    "every": 1,
                    "instruction": "Say happy new year.",
                    "accepted": "Short and warm.",
                    "kept": {"seen": 3},
                }),
            )
            .await,
    );
    assert_eq!(changed.id, made.id, "it is the same event");
    assert_eq!(changed.when, "once at 2030-01-01T09:00:00Z");
    assert_eq!(changed.next_at_ms, Some(1_893_488_400_000));
    assert_eq!((changed.only_if.clone(), changed.every), (None, None));
    assert_eq!(changed.instruction, "Say happy new year.");
    assert_eq!(changed.note["accepted"], json!(["Short and warm."]));
    assert_eq!(changed.note["kept"], json!({"seen": 3}));
    assert_eq!(changed.note["style"], "Three lines.", "the rest stays");
    // What no host can run is refused in words, and changes nothing.
    let refused = theirs
        .take_in_turn(
            &agent,
            workspace_events::CHANGE,
            json!({"event": made.id, "when": "when someone presses \"ctrl+k\""}),
        )
        .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("every device off")),
        "{refused:?}"
    );
    assert_eq!(theirs.listed().await, vec![changed]);

    // Paused, and let go on.
    let paused = event(
        theirs
            .take_in_turn(&agent, workspace_events::PAUSE, named.clone())
            .await,
    );
    assert!(!paused.on);
    assert_eq!(paused.next_at_ms, None, "a paused event fires at no time");
    let resumed = event(
        theirs
            .take_in_turn(&agent, workspace_events::RESUME, named.clone())
            .await,
    );
    assert!(resumed.on);

    // Removed: gone with everything it kept, from the workspace's store.
    let left = theirs
        .take_in_turn(&agent, workspace_events::REMOVE, named.clone())
        .await;
    assert_eq!(left.ok(), Some(json!({"events": []})));
    assert!(
        theirs
            .take_in_turn(&agent, workspace_events::PAUSE, named)
            .await
            .is_err()
    );
    let kept = workspaces
        .workspace_root(&workspace)
        .join("events")
        .join(&made.id);
    assert!(!kept.exists(), "{}", kept.display());
}

/// A moment `in_ms` from now, as a trigger's line and in ms since the epoch.
fn in_a_moment(in_ms: i64) -> (String, i64) {
    let at_ms = tilo_clock::unix_now_ms_i64().saturating_add(in_ms);
    (format!("once at {at_ms} ms"), at_ms)
}

/// An event a turn this server carries out for `caller` sets up, saying
/// hello when it fires.
async fn set_up_saying(
    events: &WorkspaceEvents,
    workspace: &str,
    caller: &Caller,
    name: &str,
    when: String,
) -> EventView {
    event(
        take_as_turn(
            events,
            workspace,
            caller,
            workspace_events::SET_UP,
            json!({"name": name, "when": when, "instruction": "Say hello."}),
        )
        .await,
    )
}

/// Wait until `done` answers something, or fail when it never does.
async fn until<T, F, Fut>(what: &str, mut done: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(answer) = done().await {
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
    let assistant = Assistant::answering(vec![]);
    let events = WorkspaceEvents::start_with(
        workspaces.clone(),
        assistant.clone(),
        std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
    );
    let theirs = as_their_agent(&cert, &workspace);
    let me = as_person(&cert, &workspace);

    let (when, at_ms) = in_a_moment(400);
    let made = event(
        take_as_turn(
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
        .await,
    );
    assert_eq!(made.sort, Sort::Scheduled);
    assert_eq!(made.next_at_ms, Some(at_ms));
    assert!(assistant.taken().is_empty(), "nothing before its moment");

    // The moment comes: the host hands the instruction to the assistant of
    // the member who set it up, with no device of theirs asking.
    let taken = until("the reminder never fired", || async {
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
    // Its author reads that it ran, and that it has no next time.
    let ran = until("the run was never kept", || async {
        listed(&events, &workspace, &cert, &me)
            .await
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
    let events = WorkspaceEvents::start_with(
        workspaces.clone(),
        assistant.clone(),
        std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
    );
    let kept = listed(&events, &workspace, &cert, &me).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!((kept[0].id.as_str(), kept[0].runs), (made.id.as_str(), 1));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(assistant.taken().len(), 1);
    assert_eq!(listed(&events, &workspace, &cert, &me).await[0].runs, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_event_fires_nothing_and_a_refused_run_says_why() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let assistant = Assistant::answering(vec![Err(HostStepError::Refused(
        "this workspace keeps no assistant".into(),
    ))]);
    let events = WorkspaceEvents::start_with(
        workspaces.clone(),
        assistant.clone(),
        std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
    );
    let theirs = as_their_agent(&cert, &workspace);

    // Paused before its moment, it never fires, also not once it goes on. It
    // is paused an hour ahead of its moment, which is then brought near: a
    // busy machine taking its time to pause it cannot let it fire first.
    let quiet = set_up_saying(
        &events,
        &workspace,
        &theirs,
        "Quiet",
        in_a_moment(3_600_000).0,
    )
    .await;
    let named = json!({"event": quiet.id});
    take_as_turn(
        &events,
        &workspace,
        &theirs,
        workspace_events::PAUSE,
        named.clone(),
    )
    .await
    .unwrap();
    let moved = event(
        take_as_turn(
            &events,
            &workspace,
            &theirs,
            workspace_events::CHANGE,
            json!({"event": quiet.id, "when": in_a_moment(300).0}),
        )
        .await,
    );
    assert!(!moved.on, "a paused event moved stays paused");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());
    let resumed = event(
        take_as_turn(
            &events,
            &workspace,
            &theirs,
            workspace_events::RESUME,
            named,
        )
        .await,
    );
    assert!(resumed.on);
    assert_eq!(resumed.runs, 0);

    // One whose instruction nothing takes says so where a member reads it.
    let refused = set_up_saying(&events, &workspace, &theirs, "Loud", in_a_moment(200).0).await;
    let ended = until("the refused run was never kept", || async {
        listed(&events, &workspace, &cert, &theirs)
            .await
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
            .is_some_and(|why| why.contains("this workspace keeps no assistant")),
        "{ended:?}"
    );
    assert_eq!(
        assistant.taken().len(),
        1,
        "only the one that was on asked: {:?}",
        assistant.taken()
    );
}
