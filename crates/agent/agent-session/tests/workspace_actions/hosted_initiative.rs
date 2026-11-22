//! What a person's assistant proposes on a server, with no device of theirs on. A piece of work
//! is filed in the workspace's tracker through the server's action service as a task of the
//! assistant's that nobody gave it, so nothing starts; its person's nod gives it to the assistant
//! and the server owes it a turn; what its person does with the task in the tracker counts as
//! their answer; and a kind of work turned down twice is refused before anything is filed. And
//! the turn itself: the model of a turn the server carries out finds `propose` among the tools
//! that turn has, takes it, and is answered the proposal.

use super::*;
use tilo_actions::remote::workspace_initiative::{
    self, LookEnded, LookRound, Proposal, Proposals, State,
};
use tilo_actions::remote::{ActionCallError, Caller, ReceiptStatus};
use tilo_agent_session::bot::host_steps::{
    HostStepError as StepError, HostSteps as Steps, Instruction, Judgement, StepCause,
};
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_workspace_service::agents::{
    Ask, WorkspaceActionProvider, events::Authority, initiative::Initiative, invoke_named,
    workspace_resource,
};

/// One `action` of `resource` as `caller` takes it, through what the app action server and an
/// agent's turn both answer with.
async fn take_of(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    (resource, action): (&str, &str),
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: workspace_initiative::target(resource, action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let answering = initiative.invoking(
        WorkspaceId::parse(workspace).unwrap(),
        caller,
        Authority::Turn,
        request,
    );
    let receipt =
        tokio::task::spawn_blocking(move || answering.wait_timeout(Duration::from_secs(60)))
            .await
            .unwrap()
            .ok()
            .flatten()
            .expect("an action of the server's own is answered");
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an action of the server's own ends with its answer: {other:?}"),
    }
}

/// One action on what was proposed, as `caller` takes it.
async fn take(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let proposals = (workspace_initiative::PROPOSALS, action);
    take_of(initiative, workspace, caller, proposals, arguments).await
}

/// The look-round after its `action`, as its person takes it.
async fn look_round(
    initiative: &Initiative,
    workspace: &str,
    person: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> LookRound {
    let look_round = (workspace_initiative::LOOK_ROUND, action);
    let answer = take_of(initiative, workspace, person, look_round, arguments).await;
    serde_json::from_value(answer.expect("the look-round answers its person"))
        .expect("a look-round is answered")
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
        workspace_initiative::LIST,
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
        workspace_initiative::PROPOSE,
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
    tasks::issue_list::chain().part(0).as_str().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_an_assistant_proposes_stands_among_its_persons_tasks_and_starts_on_their_nod() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = WorkspaceId::parse(&workspace).unwrap();
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;

    // What the assistant finds to do on its own, on this server, with the server's action service.
    let (initiative, _events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
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
    let agents = server.approving(&workspace, &[SCOUT]).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let me = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace.clone());
    let person_key = root.verifying_key().to_bytes();
    let theirs = Caller::delegated_agent(SCOUT, person_key, browser.cert.device_public_key)
        .in_workspace(workspace.clone())
        .named("Scout")
        .in_agent_session("agent-session:server-proposing")
        .on_roster(turns[0].agent.created_at, person_key)
        .on_roster_turn(tilo_actions::policy::turn::Turn::new(
            turns[0].conversation.clone(),
            turns[0].cause.clone(),
        ))
        .held_to(turns[0].agent.bound_rules().unwrap_or_default());

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
            workspace_initiative::ACCEPT,
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

    // A third their person turns down where it was proposed: its task is cancelled in the
    // tracker.
    let third = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the old plans").await);
    let declined = proposal(
        take(
            &initiative,
            &workspace,
            &me,
            workspace_initiative::DECLINE,
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
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
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
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(does_then_answers(
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
        ))
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
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
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
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(does_then_answers(
            vec![
                serde_json::json!({
                    "path": "tilo.initiative/look-round#turn-on",
                    "args": {"when": "every 3600 seconds"},
                }),
                serde_json::json!({"path": "tilo.events/events#list"}),
            ],
            "I look round every hour now.",
            Arc::clone(&asked),
        ))
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

/// What takes a look's instruction, as the server's events hand one over: one ask of Chat's
/// `ask-agent` in the conversation of the person and their assistant, made as the person's
/// event, under the step's own invocation as the cause's key.
struct HandsOver {
    actions: WorkspaceActions,
    /// The person, at their page's device.
    person: Caller,
    conversation: String,
    /// Every instruction handed over.
    handed: Mutex<Vec<String>>,
}

impl Steps for HandsOver {
    fn instruct(&self, instruction: Instruction) -> Result<String, StepError> {
        let workspace = WorkspaceId::parse(&instruction.workspace).unwrap();
        let event = self
            .person
            .clone()
            .delegated_to(instruction.cause.installation.clone())
            .named("Look-round");
        let route = DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: instruction.workspace.clone(),
            document: String::new(),
        };
        let arguments = serde_json::json!({
            "channel": self.conversation,
            "text": instruction.instruction,
            "cause": {
                "kind": "event",
                "label": "Look-round",
                "key": instruction.cause.invocation.0.to_string(),
            },
        });
        tokio::runtime::Handle::current()
            .block_on(invoke_named(
                &WorkspaceActionProvider::new(self.actions.clone(), workspace),
                &event,
                &route,
                &ui::resource::chain_resource_id(&chat::parts::chain()),
                "ask-agent",
                arguments.as_object().unwrap().clone(),
            ))
            .map_err(StepError::Unavailable)?;
        self.handed.lock().unwrap().push(instruction.instruction);
        Ok(String::new())
    }

    fn judge(&self, _: Judgement) -> Result<serde_json::Value, StepError> {
        Err(StepError::Unavailable("nothing is judged here".into()))
    }
}

/// The look-round `event` of `person` fires in `workspace`: what the events' engine hands the
/// server's steps at its time, on a thread that drives nothing else.
async fn fires(
    steps: &Arc<dyn Steps>,
    person: &str,
    workspace: &str,
    event: &str,
) -> Result<String, StepError> {
    let instruction = Instruction {
        person: person.to_owned(),
        workspace: WorkspaceId::parse(workspace).unwrap().to_string(),
        instruction: "Look round.".into(),
        allow: Vec::new(),
        permits: Vec::new(),
        note: serde_json::json!({}),
        cause: StepCause {
            installation: event.to_owned(),
            invocation: InvocationId::new(),
            event: None,
        },
    };
    let steps = Arc::clone(steps);
    tokio::task::spawn_blocking(move || steps.instruct(instruction))
        .await
        .unwrap()
}

/// The person files `title` in the workspace's tracker, a task of nobody's yet.
async fn files(actions: &WorkspaceActions, workspace: &str, person: &Caller, title: &str) {
    let catalogue = crate::common::catalogue();
    invoke_named(
        &WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(workspace).unwrap()),
        person,
        &DocumentRoute {
            app_id: TASKS.into(),
            cluster_id: workspace.to_owned(),
            document: workspace_resource(&catalogue, TASKS),
        },
        &ui::resource::chain_resource_id(&tasks::issue_list::chain()),
        "create",
        serde_json::json!({"title": title})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the person files a task");
}

/// The look-round's own day in a turn the server really carries out. A look hands over what it
/// found; the turn that starts is answered by the workspace's own model, and every call of it counts
/// on the look-round's day beside the assistant's. At the limit the turn is asked nothing more,
/// the look-round says what it spent, and the next look, with news again, starts no turn and
/// says what the one before it cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_a_look_started_spends_the_look_rounds_own_day_and_stops_at_its_limit() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let acts_for = root.verifying_key().to_bytes();
    let person = hex::encode(acts_for);
    furnished(&page, &workspaces, &workspace, &person).await;

    // The person's own provider. The first call of a turn costs the look-round's whole day.
    let theirs = ScriptedProvider::answering([
        Reply::tool_call("call-1", "tilo_ls", serde_json::json!({})).costing(2_000),
        Reply::text("One task is new: call the bank.").costing(300),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-persons-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();

    // What the server's events hand a look's instruction to, and the look in front of it.
    let me = Caller::authenticated(acts_for, browser.cert.device_public_key)
        .in_workspace(workspace.clone());
    let hands = Arc::new(HandsOver {
        actions: actions.clone(),
        person: me.clone(),
        conversation: chat::Chat::direct_id([
            MemberId::person(person.clone()),
            MemberId::agent(SCOUT),
        ])
        .as_str()
        .to_owned(),
        handed: Mutex::default(),
    });
    let (initiative, events) = Initiative::start_with(
        workspaces.clone(),
        hands.clone(),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );
    let steps = (initiative.before())(hands.clone());

    // Its person turns the look-round on, with a day of its own.
    let on = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        serde_json::json!({"when": "every 3600 seconds", "dayLimitMicros": 2_000}),
    )
    .await;
    assert_eq!((on.day_limit_micros, on.spent_today_micros), (2_000, 0));
    let event = on.event.clone().expect("it is an event now");

    // The first look notes where the workspace stands and hands nothing over.
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert!(hands.handed.lock().unwrap().is_empty());

    // Its person files a task: news at the next look, which hands it over.
    files(&actions, &workspace, &me, "Call the bank").await;
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(
        hands.handed.lock().unwrap().len(),
        1,
        "what the look found is handed over"
    );

    // The server owes the assistant the turn the look started and carries it out with the
    // workspace's own model, whose first call spends the look-round's day: it is asked nothing more.
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::clone(&unasked)))
        .with_events(Some(events))
        .with_initiative(Some(initiative.clone()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0].person, person, "it works for the look's person");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    assert_eq!(
        theirs.asked().len(),
        1,
        "no call past the look-round's limit"
    );
    assert!(unasked.lock().unwrap().is_empty());
    let spent = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::READ,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        (spent.day_limit_micros, spent.spent_today_micros),
        (2_000, 2_000)
    );

    // News again. The look finds it, starts no turn on a spent day, and the look before it
    // now says what its turn cost.
    files(&actions, &workspace, &me, "Renew the lease").await;
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(
        hands.handed.lock().unwrap().len(),
        1,
        "a spent day starts no turn"
    );
    assert_eq!(theirs.asked().len(), 1);
    let after = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::READ,
        serde_json::json!({}),
    )
    .await;
    let looks: Vec<_> = after
        .looks
        .iter()
        .map(|look| (look.ended, look.cost_micros))
        .collect();
    assert_eq!(
        looks,
        [
            (LookEnded::OverLimit, 0),
            (LookEnded::Told, 2_000),
            (LookEnded::Quiet, 0),
        ],
        "its looks, newest first"
    );
}
