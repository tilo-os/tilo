//! What a person's assistant proposes on a server, with no device of theirs on. A piece of work
//! is filed in the workspace's tracker through the server's action service as a task of the
//! assistant's that nobody gave it, so nothing starts; its person's nod gives it to the assistant
//! and the server owes it a turn; what its person does with the task in the tracker counts as
//! their answer; and a kind of work turned down twice is refused before anything is filed. And
//! the turn itself: the model of a turn the server carries out finds `propose` among the tools
//! that turn has, takes it, and is answered the proposal.

use super::HostAgents as ServerAgents;
use super::*;
use crate::harness::hosted::serving_records_with_actions as serving_with_actions;
use tilo_actions::remote::person_initiative::{self, Proposal, Proposals, State};
use tilo_actions::remote::{ActionCallError, Caller, ReceiptStatus};
use tilo_server::{
    workspace_agents::{Ask, WorkspaceActionProvider, invoke_named},
    workspace_bots::{initiative::Initiative, workspace_resource},
    workspaces::Workspaces,
};

/// One action on what was proposed as `caller` takes it, through what the app action server and an
/// agent's turn both answer with.
async fn take(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: person_initiative::target(person_initiative::PROPOSALS, action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let answering = initiative.invoking(WorkspaceId::parse(workspace).unwrap(), caller, request);
    let receipt =
        tokio::task::spawn_blocking(move || answering.wait_timeout(Duration::from_secs(60)))
            .await
            .unwrap()
            .ok()
            .flatten()
            .expect("an action on what was proposed is answered");
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an action of the server's own ends with its answer: {other:?}"),
    }
}

/// The proposal `answer` describes.
fn proposal(answer: Result<serde_json::Value, ActionCallError>) -> Proposal {
    serde_json::from_value(answer.expect("the action is taken")).expect("a proposal is answered")
}

/// What was proposed, as `caller` reads it.
async fn proposed(initiative: &Initiative, workspace: &str, caller: &Caller) -> Proposals {
    let answer = take(
        initiative,
        workspace,
        caller,
        person_initiative::LIST,
        serde_json::json!({}),
    )
    .await;
    serde_json::from_value(answer.expect("a person lists what was proposed"))
        .expect("proposals are answered")
}

/// The assistant proposes `title`, a piece of work of the kind `tidy-notes`.
async fn tidy(
    initiative: &Initiative,
    workspace: &str,
    assistant: &Caller,
    title: &str,
) -> Result<serde_json::Value, ActionCallError> {
    take(
        initiative,
        workspace,
        assistant,
        person_initiative::PROPOSE,
        serde_json::json!({
            "kind": "tidy-notes",
            "title": title,
            "why": "Six notes from this week have no title.",
        }),
    )
    .await
}

/// The workspace as a person has it: their agent made to work on the server, the chat with them
/// in it, and the tracker. The document the tracker's first issues are kept in.
async fn furnished(
    page: &Browser,
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
) -> String {
    make_agent(page, workspaces, workspace, person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    let tracker_document = workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(workspace, &governor, &catalogue, TASKS, &tracker_document)
        .await
        .unwrap();
    tasks::part::chain().part(0).as_str().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_an_assistant_proposes_stands_among_its_persons_tasks_and_starts_on_their_nod() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = serving_with_actions(
        &root,
        &server_device,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = WorkspaceId::parse(&workspace).unwrap();
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;

    // What the assistant finds to do on its own, on this server, with the server's action service.
    let (initiative, _events) = Initiative::start(
        workspaces.clone(),
        state.path().join("initiative"),
        state.path().join("events"),
        Some(actions.clone()),
    );

    // The person writes to their assistant, and the server owes it a turn. The assistant acts as
    // that turn's caller: the person's device behind it, the session handed to the agent.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "What could you do for me?",
    )
    .await;
    let agents = ServerAgents::new(workspaces.clone(), actions.clone(), None);
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let me = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace.clone());
    let theirs = me
        .clone()
        .delegated_to(SCOUT)
        .named("Scout")
        .in_agent_session("agent-session:server-proposing")
        .on_turn(
            tilo_actions::policy::turn::Turn::new(
                turns[0].conversation.clone(),
                turns[0].cause.clone(),
            ),
            turns[0].agent.bound_rules().unwrap_or_default(),
        );

    // It proposes a piece of work: a task of its own that nobody gave it, so nothing starts.
    let first = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the meeting notes").await);
    assert_eq!(first.state, State::Proposed);
    let task = first.task.clone().expect("it stands as a task");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let filed: Vec<_> = part.issues.iter().collect();
    assert_eq!(filed.len(), 1, "one task is filed");
    let (first_issue, issue) = (filed[0].0.clone(), filed[0].1);
    assert_eq!(issue.title, "Tidy the meeting notes");
    assert_eq!(task.number, Some(issue.number));
    assert_eq!(
        issue.assignee.as_ref().map(|assignee| assignee.0.as_str()),
        Some("agent:scout")
    );
    assert_eq!(issue.assigned_by, None, "nobody gave it to the assistant");
    let owed = agents.turns(&workspace).await.unwrap();
    assert!(
        owed.iter()
            .all(|turn| !matches!(turn.ask, Ask::Issue { .. })),
        "a proposal starts nothing: {owed:?}"
    );

    // Their person nods: the task is given to the assistant by them, and the server owes it the
    // work.
    let accepted = proposal(
        take(
            &initiative,
            &workspace,
            &me,
            person_initiative::ACCEPT,
            serde_json::json!({"proposal": first.id}),
        )
        .await,
    );
    assert_eq!(accepted.state, State::Accepted);
    let part = tracker(&workspaces, &workspace, &issues).await;
    let given = part.issues.get(&first_issue).expect("the task is kept");
    assert!(
        given.assigned_by.as_ref().is_some_and(MemberId::is_person),
        "its person gave it: {:?}",
        given.assigned_by
    );
    let owed = agents.turns(&workspace).await.unwrap();
    assert!(
        owed.iter().any(|turn| turn.person == person
            && matches!(&turn.ask, Ask::Issue { issue, .. } if issue == first_issue.as_str())),
        "the nod starts the work: {owed:?}"
    );

    // A second one their person cancels in the tracker itself. Read from the tracker, it is
    // turned down, and its kind counts once.
    let second = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the drafts").await);
    let second_task = second.task.clone().expect("it stands as a task");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let (second_issue, _) = part
        .issues
        .iter()
        .find(|(_, issue)| Some(issue.number) == second_task.number)
        .expect("the tracker holds the second task");
    invoke_named(
        &WorkspaceActionProvider::new(actions.clone(), id),
        &me,
        &DocumentRoute {
            app_id: TASKS.into(),
            cluster_id: workspace.clone(),
            document: issues.clone(),
        },
        &second_task.issue,
        "place",
        serde_json::json!({"issue": second_issue.as_str(), "status": "cancelled"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the person cancels the task in the tracker");
    let listed = proposed(&initiative, &workspace, &me).await;
    let cancelled = listed
        .proposals
        .iter()
        .find(|proposal| proposal.id == second.id)
        .expect("the second proposal is listed");
    assert_eq!(cancelled.state, State::Declined);
    assert_eq!(
        listed
            .kinds
            .iter()
            .find(|kind| kind.kind == "tidy-notes")
            .map(|kind| (kind.declined, kind.quiet)),
        Some((1, false))
    );

    // A third their person turns down where it was proposed: its task is cancelled in the
    // tracker, and the kind is not proposed again.
    let third = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the old plans").await);
    let declined = proposal(
        take(
            &initiative,
            &workspace,
            &me,
            person_initiative::DECLINE,
            serde_json::json!({"proposal": third.id, "why": "Leave my notes as they are."}),
        )
        .await,
    );
    assert_eq!(declined.state, State::Declined);
    let part = tracker(&workspaces, &workspace, &issues).await;
    let dropped = part
        .issues
        .iter()
        .find(|(_, issue)| issue.title == "Tidy the old plans")
        .expect("the third task is kept");
    assert!(dropped.1.status.closed(), "{:?}", dropped.1.status);

    // A fourth of that kind is refused in words, before anything is filed.
    let fourth = tidy(&initiative, &workspace, &theirs, "Tidy everything").await;
    assert!(
        matches!(&fourth, Err(ActionCallError::Refused { reason }) if reason.contains("turned `tidy-notes` down 2 times")),
        "{fourth:?}"
    );
    let part = tracker(&workspaces, &workspace, &issues).await;
    assert_eq!(part.issues.iter().count(), 3, "nothing more was filed");
}

/// The whole way, as it goes with no device of the person on: the server answers a turn of the
/// assistant, the turn's model calls `propose` by the path its tools list it under, the server
/// files the task, and the model is answered the proposal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_of_a_hosted_turn_proposes_through_the_tools_its_turn_has() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = serving_with_actions(
        &root,
        &server_device,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        state.path().join("initiative"),
        state.path().join("events"),
        Some(actions.clone()),
    );

    // The person asks, and the server answers the turn with what its agents are given: the
    // workspace's resources, the person's events and what their assistant does on its own.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "What could you do for me?",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = ServerAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(does_then_answers(
            vec![serde_json::json!({
                "path": "tilo.initiative/proposals#propose",
                "args": {
                    "kind": "tidy-notes",
                    "title": "Tidy the meeting notes",
                    "why": "Six notes from this week have no title.",
                },
            })],
            "I put one piece of work in front of you: tidying the meeting notes.",
            Arc::clone(&asked),
        )),
    )
    .with_events(Some(events))
    .with_initiative(Some(initiative.clone()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?} times",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    // The model was answered its tool call with the proposal, waiting.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "one tool call, then its answer");
    let (answered, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && answered.starts_with("ok") && answered.contains("proposed"),
        "what the tool answered the model: {answered}"
    );

    // And it is there: one proposal of the person's, there as a task nobody gave.
    let me = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace.clone());
    let listed = proposed(&initiative, &workspace, &me).await;
    assert_eq!(listed.proposals.len(), 1, "{:?}", listed.proposals);
    let made = &listed.proposals[0];
    assert_eq!(
        (made.kind.as_str(), made.state),
        ("tidy-notes", State::Proposed)
    );
    assert!(made.task.is_some(), "it stands as a task: {made:?}");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let filed: Vec<_> = part.issues.iter().collect();
    assert_eq!(filed.len(), 1, "one task is filed");
    assert_eq!(filed[0].1.title, "Tidy the meeting notes");
    assert_eq!(filed[0].1.assigned_by, None, "nobody gave it");
}

/// What the server's own services answer reaches the model of a turn. They answer an action with
/// its receipt alone, while the tools a turn has read how an invocation ended by following it,
/// so the server keeps the one for the other: the assistant turns its person's look-round on and
/// then reads their events, both through `tilo_do`, and is told what each answered, not that it
/// is still pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_servers_own_services_answer_reaches_the_model_of_a_turn() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = serving_with_actions(
        &root,
        &server_device,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        state.path().join("initiative"),
        state.path().join("events"),
        Some(actions.clone()),
    );

    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Look round for me every hour.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = ServerAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(does_then_answers(
            vec![
                serde_json::json!({
                    "path": "tilo.initiative/look-round#turn-on",
                    "args": {"when": "every 3600 seconds"},
                }),
                serde_json::json!({"path": "tilo.events/events#list"}),
            ],
            "I look round every hour now.",
            Arc::clone(&asked),
        )),
    )
    .with_events(Some(events))
    .with_initiative(Some(initiative));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?} times",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "two tool calls, then what each answered");
    // The look-round's own action: it is on, at the rhythm asked for.
    let (turned_on, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && turned_on.starts_with("ok") && turned_on.contains("every 3600 seconds"),
        "what turning it on answered the model: {turned_on}"
    );
    // The events' own action, taken after: the look-round is one of its person's events now.
    let (listed, failed) = answered_to(&asked[2], "call-2");
    assert!(
        !failed && listed.starts_with("ok") && listed.contains("Look-round"),
        "what the person's events answered the model: {listed}"
    );
}
