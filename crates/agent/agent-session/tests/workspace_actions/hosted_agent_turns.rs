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
        document: record.clone(),
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
        document: tracker,
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

/// A host's own model that must never be asked: a turn that reaches it failed the test.
fn never_asked(asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    looks_then_answers(1, asked)
}

/// The keeper linked a model of their own: the agent the host runs for them works with it, asked
/// with their own key, and the host's model is never asked. What it cost is counted on the agent's
/// record as the provider charged it, on a host that decides every change against its app's
/// surface, as tilo.sh's relay does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_works_with_its_keepers_own_model() {
    use tilo_relay::workspaces::person_model::ProviderProtocol;
    use tilo_relay::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The host's own count of what the agent spent is a change of Chat's, decided like any
    // other: the native apps name their surfaces as the host first runs them.
    let _ = crate::common::catalogue();
    tilo_relay::surfaces::decide_receipt(&state.path().join("surfaces"));
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
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    let agents_record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let tasks_record =
        file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;

    // The keeper's own provider, as their settings and their vault keep it on this host.
    let theirs = ScriptedProvider::answering([
        Reply::tool_call("call-1", "tilo_ls", serde_json::json!({})).costing(700),
        Reply::text("I looked through the workspace: the plan is ready for you.").costing(300),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &keeper,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .unwrap();

    let hosts = Arc::new(Mutex::new(Vec::new()));
    let agents = HostAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(never_asked(Arc::clone(&hosts))),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    assert!(
        hosts.lock().unwrap().is_empty(),
        "the host's own model is not asked for a keeper who linked theirs"
    );
    let asked = theirs.asked();
    assert_eq!(asked.len(), 2, "one call to look, one to answer");
    for call in &asked {
        assert_eq!(call.model, "gemini-main");
        assert_eq!(call.key.as_deref(), Some("sk-the-keepers-own"));
    }
    assert!(
        asked[0]
            .request
            .tools
            .iter()
            .any(|tool| tool.name == "tilo_ls"),
        "it acts through the agent tools"
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
        said.iter().any(|text| text.contains("the plan is ready")),
        "{said:?}"
    );

    // What the keeper's provider charged is what the agent's day counts.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = workspaces
            .resource_document(&workspace, CHAT, &agents_record)
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
            .is_some_and(|spent| spent.cost_micros == 1_000)
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

/// A keeper who linked a provider is never answered by another model: one whose key is missing is
/// told so, where they asked, and the host's own model stays unasked. A keeper who linked nothing
/// on a host that runs no model is told what to link.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keeper_whose_model_cannot_answer_is_told_and_no_other_model_is_asked() {
    use tilo_relay::workspaces::person_model::ProviderProtocol;
    use tilo_relay::workspaces::person_model::scripted::Linking;

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
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let tasks_record =
        file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;
    let said = |answered: tasks::part::IssuePart| -> Vec<String> {
        answered
            .issues
            .values()
            .flat_map(|issue| issue.comments.values().cloned())
            .filter(|message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT)))
            .map(|message| message.text)
            .collect()
    };

    // Nothing linked, and a host that runs no model: the person is told what to link.
    let agents = HostAgents::new(workspaces.clone(), actions.clone(), None);
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let told = said(tracker(&workspaces, &workspace, &tasks_record).await);
    assert!(
        told.iter()
            .any(|text| text.contains("No model is linked for this kind of work")),
        "{told:?}"
    );

    // A provider linked without its key: its keeper is told which key is missing, and the host's
    // own model does not answer in its place.
    workspaces
        .link_model_for(
            &keeper,
            &Linking {
                name: "Anthropic",
                protocol: ProviderProtocol::Anthropic,
                endpoint: "https://api.anthropic.com",
                key: None,
                models: &["claude-main"],
            },
            true,
        )
        .unwrap();
    let hosts = Arc::new(Mutex::new(Vec::new()));
    let agents = HostAgents::new(
        workspaces.clone(),
        actions,
        Some(never_asked(Arc::clone(&hosts))),
    );
    agents.answer(&workspace, &turns[0]).await.unwrap();
    assert!(hosts.lock().unwrap().is_empty(), "no other model is asked");
    let told = said(tracker(&workspaces, &workspace, &tasks_record).await);
    assert!(
        told.iter()
            .any(|text| text.contains("Anthropic has no API key here yet")),
        "{told:?}"
    );
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

/// The first part of the chat's messages, as the workspace keeps it.
async fn conversation_part(
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
) -> chat::parts::ChatPart {
    let saved = workspaces
        .resource_document(workspace, CHAT, chat::parts::part(0).as_str())
        .await
        .ok()
        .flatten();
    saved.map_or_else(chat::parts::ChatPart::default, |saved| {
        chat::messages::read_part(&automerge::AutoCommit::load(&saved).unwrap())
    })
}

/// The person writes `text` to the agent in their conversation with it, through the host's action
/// service, as their page does. The message's id.
async fn write_to_agent(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    (root, browser): (&SigningKey, &crate::support::Device),
    text: &str,
) -> Id {
    let person = hex::encode(root.verifying_key().to_bytes());
    let id = WorkspaceId::parse(workspace).unwrap();
    let host = tilo_relay::workspace_agents::WorkspaceActionHost::new(actions.clone(), id);
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace);
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        document: String::new(),
    };
    let conversation = chat::Chat::direct_id([MemberId::person(person), MemberId::agent(SCOUT)]);
    let arguments = serde_json::json!({"channel": conversation.as_str(), "text": text});
    tilo_relay::workspace_agents::invoke_named(
        &host,
        &caller,
        &route,
        &ui::resource::chain_resource_id(&chat::parts::chain()),
        "post",
        arguments.as_object().unwrap().clone(),
    )
    .await
    .expect("the person writes to the agent");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(workspaces, workspace).await;
        if let Some((id, _)) = part.messages.iter().find(|(_, held)| held.text == text) {
            return id.clone();
        }
        assert!(Instant::now() < deadline, "the host never holds {text:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_answers_the_message_a_person_writes_it() {
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
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // The workspace's chat, and the person in it, as their own Chat window introduces them the
    // first time it opens: a conversation takes messages from the people the chat knows.
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    workspaces
        .author_application(&workspace, &governor, &catalogue, CHAT, "", |doc| {
            chat::demo::introduce(doc, &person, "Ada", "A").map_err(|error| error.to_string())
        })
        .await
        .unwrap();

    // They write to the agent, and no device of theirs has to be on for what follows: the host
    // reads the turn from its own copy of the chat.
    let written = (&root, &browser);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "what is planned?",
    )
    .await;
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
        "the message is the agent's one turn: {turns:?}"
    );
    assert_eq!(turns[0].person, person, "it works for whoever wrote to it");
    assert_eq!(turns[0].cause, first.to_string());
    assert!(
        turns[0].request.contains("what is planned?"),
        "the turn asks what the person wrote: {}",
        turns[0].request
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    // Its answer is the reply Chat derives for the message: the agent's own, in the model's
    // words, with its work ended, so the message asks nothing more.
    let part = conversation_part(&workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::record::reply_id(&first, &MemberId::agent(SCOUT)))
        .unwrap_or_else(|| {
            panic!(
                "the agent's reply is in the conversation: {:?}",
                part.messages
                    .iter()
                    .map(|(_, held)| (held.author.clone(), held.text.clone()))
                    .collect::<Vec<_>>()
            )
        });
    assert!(reply.text.contains("the plan is ready"), "{}", reply.text);
    assert_eq!(reply.author, MemberId::agent(SCOUT), "the agent wrote it");
    assert!(
        !chat::summons::unanswered(Some(reply)),
        "its work ended: {:?}",
        reply.work
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "one turn to look, one to answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );

    // Of two messages the agent has not answered, the newest is the turn: it replaces the one
    // before it.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "and who owns it?",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let newest = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "no: when is it due?",
    )
    .await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn in the conversation: {turns:?}");
    assert_eq!(
        turns[0].cause,
        newest.to_string(),
        "the newest message is the one asked"
    );
    page.peer.close();
}

/// A model that answers a turn in words, and a question asked without tools with `summary`.
struct Summarising {
    summary: &'static str,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Summarising {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let text = if request.tools.is_empty() {
            self.summary
        } else {
            "Nothing needs doing today."
        };
        Ok(answer(
            vec![Content::Text { text: text.into() }],
            StopReason::EndTurn,
            5,
        ))
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// Everything `request` says, in the order it says it.
fn said_in(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The person writes nine long notes to the agent, each its own message, and a second passes: what
/// is written next is the newest by the clock a message carries.
async fn long_notes(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    written: (&SigningKey, &crate::support::Device),
    about: &str,
) {
    for index in 0..9 {
        let note = format!(
            "{about} {index}: {}",
            "the crossing went as planned and nothing needs doing. ".repeat(22)
        );
        write_to_agent(actions, workspaces, workspace, written, &note).await;
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

/// A conversation that outgrew what a turn is told as written. The turn carries what its agent is
/// to take into the conversation's summary; once the answer has its words the host asks a model
/// once for it and Chat keeps it, so the next turn is told what was said long ago and asks for
/// nothing more. When the conversation outgrows it again the summary is extended from where it
/// stood. The question goes to the keeper's own model once they linked one, and is counted on
/// the agent's day as its turns are.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_keeps_the_summary_of_a_long_conversation() {
    use tilo_relay::workspaces::person_model::ProviderProtocol;
    use tilo_relay::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};

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
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    let agents_record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    workspaces
        .author_application(&workspace, &governor, &catalogue, CHAT, "", |doc| {
            chat::demo::introduce(doc, &person, "Ada", "A").map_err(|error| error.to_string())
        })
        .await
        .unwrap();

    // Something said at the start, then much more than a turn is told as written.
    let written = (&root, &browser);
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "Remember this: the boat is called Aurelia.",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    long_notes(&actions, &workspaces, &workspace, written, "Note").await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "anything to do today?",
    )
    .await;

    // The keeper linked nothing yet, so the host's model answers the turn, and the summary's
    // question is asked as the turn was.
    let hosts = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::clone(&hosts);
    let agents = HostAgents::new(
        workspaces.clone(),
        actions.clone(),
        Some(Arc::new(move || {
            Some(Box::new(Summarising {
                summary: "The boat is called Aurelia. The crossings went as planned.",
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })),
    );
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the newest message is the one turn");
    assert_eq!(turns[0].cause, first.to_string());
    assert!(
        turns[0].request.len() <= ui::collaboration::recall::INSTRUCTION_BYTES,
        "a turn stays inside its budget: {} bytes",
        turns[0].request.len()
    );
    let fold = turns[0]
        .fold
        .clone()
        .expect("more stands outside the summary than a turn carries as written");
    assert!(fold.summary.is_empty(), "nothing is summarised yet");
    assert!(
        fold.lines.contains("the boat is called Aurelia"),
        "the oldest is what the summary takes in: {}",
        fold.lines
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    // The answer is the person's as any turn's is, with its work ended.
    let part = conversation_part(&workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::record::reply_id(&first, &MemberId::agent(SCOUT)))
        .expect("the agent's reply is in the conversation");
    assert!(
        reply.text.contains("Nothing needs doing today"),
        "{}",
        reply.text
    );
    assert!(
        !chat::summons::unanswered(Some(reply)),
        "its work ended: {:?}",
        reply.work
    );
    {
        let calls = hosts.lock().unwrap();
        assert_eq!(calls.len(), 2, "one call answers, one summarises");
        assert!(calls[1].tools.is_empty(), "a summary is words, no tools");
        assert!(
            said_in(&calls[1]).contains("the boat is called Aurelia"),
            "the question holds the oldest messages: {}",
            said_in(&calls[1])
        );
    }

    // What was said long ago reaches the next turn as the summary, no longer as it was written,
    // and the turn asks for nothing more.
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "what is the boat called?",
    )
    .await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn in the conversation: {turns:?}");
    assert_eq!(turns[0].cause, second.to_string());
    assert!(
        turns[0]
            .request
            .contains("What came before, as you summarised it"),
        "the next turn is told the summary: {}",
        turns[0].request
    );
    assert!(
        turns[0].request.contains("The boat is called Aurelia."),
        "{}",
        turns[0].request
    );
    assert!(
        !turns[0].request.contains("Remember this"),
        "what the summary took in is not told again as written"
    );
    assert_eq!(
        turns[0].fold, None,
        "what is left outside the summary is carried as written"
    );

    // The keeper links a model of their own, and the conversation outgrows its summary again:
    // their model is asked, once, for what the new messages add to it.
    let theirs = ScriptedProvider::answering([
        Reply::text("Still nothing.").costing(300),
        Reply::text("The later crossings were quiet too.").costing(40),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &keeper,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    long_notes(&actions, &workspaces, &workspace, written, "Later note").await;
    let third = write_to_agent(&actions, &workspaces, &workspace, written, "and now?").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn in the conversation: {turns:?}");
    assert_eq!(turns[0].cause, third.to_string());
    let fold = turns[0]
        .fold
        .clone()
        .expect("the conversation outgrew its summary again");
    assert!(
        fold.summary.contains("The boat is called Aurelia."),
        "the summary is extended from where it stood: {}",
        fold.summary
    );
    assert!(
        !fold.lines.contains("Remember this"),
        "nothing is read from the conversation's start again: {}",
        fold.lines
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    let calls = theirs.asked();
    assert_eq!(
        calls.len(),
        2,
        "their model answers and summarises, once each"
    );
    assert!(
        calls[1].request.tools.is_empty(),
        "a summary is words, no tools"
    );
    assert_eq!(
        hosts.lock().unwrap().len(),
        2,
        "the host's model is not asked once the keeper linked their own"
    );

    // The summary holds both: what it held, and what the new messages added.
    write_to_agent(&actions, &workspaces, &workspace, written, "thanks").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn in the conversation: {turns:?}");
    for kept in [
        "The boat is called Aurelia.",
        "The later crossings were quiet too.",
    ] {
        assert!(
            turns[0].request.contains(kept),
            "{kept:?} is in the summary the turn is told: {}",
            turns[0].request
        );
    }

    // Every call is counted on the agent's day: two of the host's model, two of the keeper's.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = workspaces
            .resource_document(&workspace, CHAT, &agents_record)
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
            .is_some_and(|spent| spent.cost_micros == 350)
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
    page.peer.close();
}

/// What starts a turn besides its person's typing, and what reaches a turn that is going.
#[path = "hosted_agent_asks.rs"]
mod asks;
