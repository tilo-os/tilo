//! An agent a workspace made to run on its host works around the clock there: a person gives it
//! an issue, and the host's agent works on it through the agent tools, as the person's agent, then
//! answers in the issue's thread. Its keeper's daily limit stops it before it spends past it.
use super::*;
use std::sync::Mutex;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_relay::workspace_agents::{HostAgents, Model};
use ui::tilo_resource::{Id, Resource as _};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

const CHAT: &str = "so.tilo.chat";
const TASKS: &str = "so.tilo.tasks";
/// The agent's id in the workspace's record of its agents.
const SCOUT: &str = "scout";

/// A model whose turns are written out: each answer in order, then the last again. What each
/// request asked is kept.
struct Scripted {
    answers: Vec<ModelResponse>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Scripted {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let answer = if self.answers.len() > 1 {
            self.answers.remove(0)
        } else {
            self.answers[0].clone()
        };
        Ok(answer)
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

fn answer(content: Vec<Content>, stop_reason: StopReason, cost_micros: u64) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: Usage::default(),
        cost_micros,
    }
}

/// Look at the workspace once, then say what it did; each answer costing `cost`.
fn looks_then_answers(cost: u64, asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: "call-1".into(),
                        name: "tilo_ls".into(),
                        input: serde_json::json!({}),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    cost,
                ),
                answer(
                    vec![Content::Text {
                        text: "I looked through the workspace: the plan is ready for you.".into(),
                    }],
                    StopReason::EndTurn,
                    cost,
                ),
            ],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// The person makes an agent that runs on the host, as the Agents tab in a browser does: their
/// device writes the workspace's record of its agents, with the agent's daily limit when one is
/// set. Answers once the host holds it.
async fn make_agent(
    page: &Browser,
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    limit_micros: Option<u64>,
) {
    let record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let mut made = ui::tilo_resource::Replica::<WorkspaceAgents>::new().as_author(person);
    made.apply(
        WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
            name: ui::tilo_resource::NonBlank::parse("Scout").unwrap(),
            keeper: None,
            id: Some(SCOUT.into()),
            model: None,
            colour: None,
            mcp_client: None,
        }),
    )
    .unwrap();
    made.apply(
        WorkspaceAgents::SET_AGENT_ON_HOST.with(workspace_agents::SetAgentOnHost {
            agent: Id::from_raw(SCOUT.to_owned()),
            on_host: true,
        }),
    )
    .unwrap();
    if limit_micros.is_some() {
        made.apply(WorkspaceAgents::SET_AGENT_SPEND_LIMIT.with(
            workspace_agents::SetAgentSpendLimit {
                agent: Id::from_raw(SCOUT.to_owned()),
                limit_micros,
            },
        ))
        .unwrap();
    }
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        record: record.clone(),
    };
    let bytes = std::mem::take(made.doc_mut()).save();
    page.peer
        .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while workspaces
        .resource_document(workspace, CHAT, &record)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the host never holds the agents record"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The person files "Plan the launch" through the host's action service, as their page or their
/// MCP client does, and gives it to the agent. The part of the tracker's issues it went into.
async fn file_issue(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    (governor, browser): (&crate::support::Device, &crate::support::Device),
) -> String {
    let catalogue = crate::common::catalogue();
    let tracker = tilo_relay::workspace_bots::workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(workspace, &governor.cert, &catalogue, TASKS, &tracker)
        .await
        .unwrap();
    let id = WorkspaceId::parse(workspace).unwrap();
    let host = tilo_relay::workspace_agents::WorkspaceActionHost::new(actions.clone(), id);
    let person = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.into(),
        record: tracker,
    };
    let arguments =
        serde_json::json!({"title": "Plan the launch", "assignee": format!("agent:{SCOUT}")});
    tilo_relay::workspace_agents::invoke_named(
        &host,
        &person,
        &route,
        &ui::resource::chain_resource_id(&tasks::part::chain()),
        "create",
        arguments.as_object().unwrap().clone(),
    )
    .await
    .expect("the person files the issue");
    tasks::part::chain().part(0).as_str().to_owned()
}

/// The first part of the tracker's issues, as the workspace keeps it.
async fn tracker(
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    record: &str,
) -> tasks::part::IssuePart {
    let bytes = workspaces
        .resource_document(workspace, TASKS, record)
        .await
        .unwrap()
        .unwrap();
    tasks::part::IssuePart::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_works_on_the_issue_a_person_gives_it() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (relay, actions) = crate::harness::hosted::serving_records_with_actions(
        &root,
        &host,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // The person's page joins: the device the host admits their agent's turns on.
    let page = Browser::open_at(&relay, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());

    // The person makes an agent that runs on the host, as the Agents tab in a browser does.
    let agents_document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // They write an issue and give it to the agent.
    let tasks_record =
        file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;
    let written = tracker(&workspaces, &workspace, &tasks_record).await;
    let (issue, _) = written.issues.iter().next().unwrap();
    let issue: Id = issue.clone();

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = HostAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(looks_then_answers(1_000, Arc::clone(&asked))),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the issue given to the agent is its one turn: {turns:?}"
    );
    assert_eq!(
        turns[0].person, person,
        "it works for whoever gave it the issue"
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?}",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    // It worked through its tools, told what the person asked.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "one turn to look, one to answer");
    let first = format!("{:?}", asked[0].messages);
    assert!(
        first.contains("Plan the launch"),
        "the model is told the issue: {first}"
    );
    assert!(
        asked[0].tools.iter().any(|tool| tool.name == "tilo_ls"),
        "it acts through the agent tools"
    );
    let looked = format!("{:?}", asked[1].messages.last());
    assert!(
        !looked.contains("is_error: true"),
        "its look ran as the person's agent: {looked}"
    );

    // Its answer is its own comment in the issue's thread, and the issue asks nothing more.
    let answered = tracker(&workspaces, &workspace, &tasks_record).await;
    let thread: Vec<_> = answered
        .issues
        .get(&issue)
        .unwrap()
        .comments
        .values()
        .cloned()
        .collect();
    let reply = thread
        .iter()
        .find(|message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT)))
        .unwrap_or_else(|| panic!("the agent answers in the thread: {thread:?}"));
    assert!(reply.text.contains("the plan is ready"), "{}", reply.text);
    assert!(reply.by_agent, "the thread shows the agent wrote it");
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );

    // What its turns cost is counted on its record, for its keeper.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = workspaces
            .resource_document(&workspace, CHAT, &agents_document)
            .await
            .unwrap()
            .unwrap();
        let record = WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
        let scout = record
            .agents
            .iter()
            .find(|agent| agent.id.0 == SCOUT)
            .unwrap();
        if scout
            .spent
            .as_ref()
            .is_some_and(|spent| spent.cost_micros == 2_000)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "its spend is counted: {:?}",
            scout.spent
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_stops_at_its_keepers_daily_limit() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (relay, actions) = crate::harness::hosted::serving_records_with_actions(
        &root,
        &host,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&relay, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, Some(10_000)).await;
    let tasks_record =
        file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = HostAgents::new(
        workspaces.clone(),
        actions,
        Some(looks_then_answers(10_000, Arc::clone(&asked))),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "once a turn spent the day's limit, no more is asked of the model"
    );
    let answered = tracker(&workspaces, &workspace, &tasks_record).await;
    let said: Vec<String> = answered
        .issues
        .values()
        .flat_map(|issue| issue.comments.values().cloned())
        .filter(|message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT)))
        .map(|message| message.text)
        .collect();
    assert!(
        said.iter().any(|text| text.contains("limit")),
        "it tells the person it stopped at the limit: {said:?}"
    );
}
