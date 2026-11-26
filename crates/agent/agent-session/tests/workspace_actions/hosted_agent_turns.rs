//! An agent a workspace made to run on its server works around the clock there: a person gives it
//! an issue, and the server's agent works on it through the agent tools, as the person's agent, then
//! answers in the issue's thread. Its keeper's daily limit stops it before it spends past it.
use super::*;
use std::sync::Mutex;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_workspace_service::agents::{Model, ServerAgents, events::WorkspaceEvents};
use ui::tilo_resource::{Id, Resource as _};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

const CHAT: &str = "so.tilo.chat";
pub(super) const TASKS: &str = "so.tilo.tasks";
/// The agent's id in the workspace's record of its agents.
pub(super) const SCOUT: &str = "scout";

/// The workspace the answer tests work in, and a model that calls tools before it answers.
#[path = "hosted_agent_answer.rs"]
mod answer;

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
pub(super) fn looks_then_answers(cost: u64, asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
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

/// The person makes an agent that runs on the server, as the Agents tab in a browser does: their
/// device writes the workspace's record of its agents, with the agent's daily limit when one is
/// set. Answers once the server holds it.
pub(super) async fn make_agent(
    page: &Browser,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    limit_micros: Option<u64>,
) {
    let record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let checkout = ui::resource::DocumentCheckout::new(
        ui::resource::DocumentId::parse(&record).unwrap(),
        ui::resource::ResourceType::new(ui::tilo_workspace_agent::KIND),
    );
    let mut doc = ui::tilo_resource::Doc::new();
    ui::resource::genesis(&mut doc, &checkout, workspace, |doc| {
        WorkspaceAgents::initialize(doc).map_err(|error| error.to_string())
    })
    .unwrap();
    let mut made = ui::tilo_resource::Replica::<WorkspaceAgents>::from_doc(doc).as_author(person);
    made.apply(
        WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
            name: ui::tilo_resource::NonBlank::parse("Scout").unwrap(),
            id: Some(SCOUT.into()),
            model: None,
            mcp_client: None,
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
        app_id: ui::tilo_workspace_agent::OWNER.into(),
        cluster_id: workspace.into(),
        document: record.clone(),
    };
    let bytes = std::mem::take(made.doc_mut()).save();
    page.peer
        .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
        .unwrap();
    let deadline = crate::harness::page::write_deadline();
    while workspaces
        .resource_document(workspace, ui::tilo_workspace_agent::OWNER, &record)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the host never holds the agents record the page wrote: not within {:?}",
            crate::harness::page::BARRIER_BOUND
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The person files "Plan the launch" through the server's action service, as their page or their
/// MCP client does, and gives it to the agent. The part of the tracker's issues it went into.
pub(super) async fn file_issue(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    (governor, browser): (&crate::support::Device, &crate::support::Device),
) -> String {
    file_issue_for(
        actions,
        workspaces,
        workspace,
        root,
        (governor, browser),
        SCOUT,
    )
    .await
}

async fn file_issue_for(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    (governor, browser): (&crate::support::Device, &crate::support::Device),
    assignee: &str,
) -> String {
    let catalogue = crate::common::catalogue();
    let tracker = tilo_workspace_service::agents::workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(workspace, &governor.cert, &catalogue, TASKS, &tracker)
        .await
        .unwrap();
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(actions.clone(), id);
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
        serde_json::json!({"title": "Plan the launch", "assignee": format!("agent:{assignee}")});
    tilo_workspace_service::agents::invoke_named(
        &service,
        &person,
        &route,
        &ui::resource::chain_resource_id(&tasks::issue_list::chain()),
        "create",
        arguments.as_object().unwrap().clone(),
    )
    .await
    .expect("the person files the issue");
    tasks::issue_list::chain().part(0).as_str().to_owned()
}

/// An issue worked on over many agent sessions: its place, its waits, its budget and the
/// sessions it lost.
#[path = "server_issue_work.rs"]
mod issue_work;

/// The first part of the tracker's issues, as the workspace keeps it.
pub(super) async fn tracker(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    record: &str,
) -> tasks::issue_list::IssueList {
    let bytes = workspaces
        .resource_document(workspace, TASKS, record)
        .await
        .unwrap()
        .unwrap();
    tasks::issue_list::IssueList::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap()
}

/// What the agent said in the tracker's issue threads, as the workspace keeps them now.
fn scout_said(answered: &tasks::issue_list::IssueList) -> Vec<String> {
    answered
        .issues
        .values()
        .flat_map(|issue| issue.comments.values().cloned())
        .filter(|message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT)))
        .map(|message| message.text)
        .collect()
}

/// Wait until the agent's comment holding `text` is in the issue's thread: a turn's answer is
/// applied and saved after the turn answers, so it is read when it arrives rather than once.
pub(super) async fn told_by_scout(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    list: &str,
    text: &str,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let told = scout_said(&tracker(workspaces, workspace, list).await);
        if told.iter().any(|said| said.contains(text)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the agent never said {text:?}: {told:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A server's own model that must never be asked: a turn that reaches it failed the test.
fn never_asked(asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    looks_then_answers(1, asked)
}

/// The workspace linked a model of its own: the agent the server runs for its person works with
/// it, asked with the workspace's own key, and the server's model is never asked. What it cost is
/// counted on the agent's record as the provider charged it, on a server that decides every
/// change against its app's surface, as tilo.sh's server does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_works_with_its_workspaces_own_model() {
    use tilo_server::workspaces::person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    };

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // The server's own count of what the agent spent is a change of Chat's, decided like any
    // other: the native apps name their surfaces as the server first runs them.
    let _ = crate::common::catalogue();
    tilo_server::surfaces::decide_receipt(&state.path().join("surfaces"));
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    let agents_record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let tracker_id = file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;

    // The workspace's own provider, as its settings and its vault keep it on this server.
    let theirs = ScriptedProvider::answering([
        Reply::tool_call("call-1", "tilo_ls", serde_json::json!({})).costing(700),
        Reply::text("I looked through the workspace: the plan is ready for you.").costing(300),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();

    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::clone(&unasked)));
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
        unasked.lock().unwrap().is_empty(),
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
    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
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
            .resource_document(&workspace, ui::tilo_workspace_agent::OWNER, &agents_record)
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
/// told so, where they asked, and the server's own model stays unasked. A keeper who linked nothing
/// on a server that runs no model is told what to link.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keeper_whose_model_cannot_answer_is_told_and_no_other_model_is_asked() {
    use tilo_server::workspaces::person_model::ProviderProtocol;
    use tilo_server::workspaces::person_model::scripted::Linking;

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let tracker_id = file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;

    // Nothing linked, and a server that runs no model: the person is told what to link.
    let agents = server.approving(&workspace, &[SCOUT]).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    told_by_scout(
        &workspaces,
        &workspace,
        &tracker_id,
        "No model is linked for this kind of work",
    )
    .await;

    // A provider linked without its key: the workspace is told which key is missing, and the server's
    // own model does not answer in its place.
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Anthropic",
                protocol: ProviderProtocol::Anthropic,
                endpoint: "https://api.anthropic.com",
                key: None,
                models: &["claude-main"],
            },
            true,
        )
        .await
        .unwrap();
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::clone(&unasked)));
    // The turn that could not work stopped the work on its issue: nothing is owed until its
    // person says something new, and the turn read before that stop begins nothing again.
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let kept_in = tasks::issue_list::chain().part(0).as_str().to_owned();
    assert_eq!(
        scout_said(&tracker(&workspaces, &workspace, &kept_in).await).len(),
        1,
        "what was read before the work stopped is not begun a second time"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "work that stopped asks nothing until its person says something new"
    );
    // They write in the issue's thread, and the turn that owes now is the one answered.
    let issue = tracker(&workspaces, &workspace, &kept_in)
        .await
        .issues
        .iter()
        .next()
        .unwrap()
        .0
        .clone();
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(
        actions,
        WorkspaceId::parse(&workspace).unwrap(),
    );
    let asking =
        tilo_actions::remote::Caller::authenticated(keeper, browser.cert.device_public_key)
            .in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: kept_in.clone(),
    };
    let words = serde_json::json!({"issue": issue.as_str(), "text": "I linked one: try again."});
    tilo_workspace_service::agents::invoke_named(
        &service,
        &asking,
        &route,
        &tasks::model::issue_resource(&issue),
        "comment",
        words.as_object().unwrap().clone(),
    )
    .await
    .expect("the person writes in the issue's thread");
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    assert!(
        unasked.lock().unwrap().is_empty(),
        "no other model is asked"
    );
    told_by_scout(
        &workspaces,
        &workspace,
        &tracker_id,
        "Anthropic has no API key here yet",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_works_on_the_issue_a_person_gives_it() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // The person's page joins: the device the server admits their agent's turns on.
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());

    // The person makes an agent that runs on the server, as the Agents tab in a browser does.
    let agents_document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // They write an issue and give it to the agent.
    let tracker_id = file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;
    let written = tracker(&workspaces, &workspace, &tracker_id).await;
    let (issue, _) = written.issues.iter().next().unwrap();
    let issue: Id = issue.clone();

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
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
    assert!(
        asked[0]
            .tools
            .iter()
            .all(|tool| tool.name != "answer_with_view"),
        "an issue's answer is a comment: its turn is offered no view to answer with"
    );
    let looked = format!("{:?}", asked[1].messages.last());
    assert!(
        !looked.contains("is_error: true"),
        "its look ran as the person's agent: {looked}"
    );

    // Its answer is its own comment in the issue's thread, and the issue asks nothing more.
    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
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
            .resource_document(
                &workspace,
                ui::tilo_workspace_agent::OWNER,
                &agents_document,
            )
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
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, Some(10_000)).await;
    let tracker_id = file_issue(&actions, &workspaces, &workspace, &root, (&host, &browser)).await;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(10_000, Arc::clone(&asked)));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "once a turn spent the day's limit, no more is asked of the model"
    );
    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
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
pub(super) async fn conversation_part(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> chat::parts::ChatMessages {
    let saved = workspaces
        .resource_document(workspace, CHAT, chat::parts::part(0).as_str())
        .await
        .ok()
        .flatten();
    saved.map_or_else(chat::parts::ChatMessages::default, |saved| {
        chat::messages::read_part(&automerge::AutoCommit::load(&saved).unwrap())
    })
}

/// What Chat's `post` takes to say `text` in `conversation`.
pub(super) fn saying(conversation: &Id, text: &str) -> serde_json::Value {
    serde_json::json!({"channel": conversation.as_str(), "text": text})
}

/// A certified device sends and reads the first message in its agent's DM through Chat's
/// hosted actions. A retry of the signed invocation gets the original receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_certified_device_sends_once_to_its_agents_dm_and_reads_it() {
    let mut hosted = super::hosted_secret_use::Hosted::start().await;
    let person = hosted.person();
    let channel = chat::Chat::direct_id([MemberId::person(&person), MemberId::agent(SCOUT)]);
    let provider = "record:so.tilo.chat:";
    let messages = ui::resource::chain_resource_id(&chat::parts::chain());
    let id = "3d2c98df-46d5-4426-94d5-d8e1a7cb11f0";
    let words = "What is on my list?";
    let args = |value: serde_json::Value| InvokeArgument::Json(value.as_object().unwrap().clone());
    let post = request(
        provider,
        ActionAddress::new(&messages, "post"),
        args(serde_json::json!({ "channel": channel, "text": words, "id": id })),
    );
    let receipt = hosted.page.invoke_settled(post.clone()).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    assert_eq!(hosted.page.invoke_settled(post).await, receipt);

    let listed = hosted
        .page
        .catalogue(CatalogueQuery {
            provider: Some(provider.into()),
            resource_name: Some("chat.messages".into()),
            ..Default::default()
        })
        .await
        .expect("the device lists Chat's messages");
    let read_address = listed
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.resource_kind == "chat.messages" && action.address.action == "read")
        .expect("Chat publishes a read on its messages")
        .address
        .clone();
    assert_eq!(read_address, ActionAddress::new(&messages, "read"));
    let exact = hosted
        .page
        .catalogue(CatalogueQuery {
            provider: Some(provider.into()),
            target: Some(read_address.clone()),
            ..Default::default()
        })
        .await
        .expect("the device resolves the published address");
    let (read_provider, target) = exact
        .rows
        .into_iter()
        .filter_map(|row| Some((row.provider, row.action?.address)))
        .find(|(_, target)| target == &read_address)
        .expect("the published read resolves to a callable target");
    assert_eq!(read_provider, provider);
    let read = hosted
        .page
        .invoke_on(
            &read_provider,
            target,
            args(serde_json::json!({
                "where": { "channel": channel },
                "fields": ["channel", "author", "text"],
                "pageSize": 10
            })),
        )
        .await
        .expect("the device reads its conversation")
        .value
        .unwrap();
    let rows = read["messages"].as_array().expect("the DM's rows");
    assert_eq!(rows.len(), 1, "the device reads one copy: {read}");
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["value"]["channel"], channel.as_str());
    assert_eq!(
        rows[0]["value"]["author"],
        serde_json::json!({ "person": person })
    );
    assert_eq!(rows[0]["value"]["text"], words);
}

/// The person writes `text` to the agent in their conversation with it, through the server's action
/// service, as their page does. The message's id.
pub(super) async fn write_to_agent(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    (root, browser): (&SigningKey, &crate::support::Device),
    text: &str,
) -> Id {
    let person = hex::encode(root.verifying_key().to_bytes());
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(actions.clone(), id);
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
    let arguments = saying(&conversation, text);
    tilo_workspace_service::agents::invoke_named(
        &service,
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
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // The workspace's chat.
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();

    // They write to the agent, and no device of theirs has to be on for what follows: the server
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
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
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
        .get(&chat::chat::reply_id(&first, &MemberId::agent(SCOUT)))
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

/// A person's first message to an agent starts the first part of the chat's messages, and a
/// later one finds that part there. The server tells whoever follows its documents of the write
/// that starts one as of every write after it, so the look that write starts takes the first
/// message as it takes the later one, with no device of the person's on: neither waits for the
/// server's next look at every workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_message_is_taken_at_the_look_its_own_write_starts() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let serving = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = serving.roster.actions.clone();
    let workspaces = serving.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&serving, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = serving
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
    let part = chat::parts::part(0);
    assert!(
        workspaces
            .resource_document(&workspace, CHAT, part.as_str())
            .await
            .ok()
            .flatten()
            .is_none(),
        "nothing was written here yet: the first message starts the part"
    );

    for text in ["what is planned?", "and who owns it?"] {
        // What the server's agents wait on between two looks: the writes it tells of.
        let mut told = workspaces.document_writes();
        let message =
            write_to_agent(&actions, &workspaces, &workspace, (&root, &browser), text).await;
        tilo_test_env::within(
            format!("the server to tell its followers of the write of {text:?}"),
            async {
                loop {
                    let write = told.recv().await.unwrap();
                    if write.workspace == workspace
                        && write.app == CHAT
                        && write.record == part.as_str()
                    {
                        break;
                    }
                }
            },
        )
        .await;
        // The look that write starts, as the server's agents make it for a write of Chat's.
        agents.look(&workspace).await.unwrap();
        let reply = chat::chat::reply_id(&message, &MemberId::agent(SCOUT));
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let held = conversation_part(&workspaces, &workspace).await;
            if held
                .messages
                .get(&reply)
                .is_some_and(|reply| !chat::summons::unanswered(Some(reply)))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the agent never answered {text:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "each message was answered once"
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

/// The words of the person's request that `request` answers: the message after the
/// conversation's earlier ones, which come before it in their own roles, and before the turn's own
/// tool calls and what it is told after them.
fn asked_in(request: &ModelRequest) -> String {
    let own = request
        .messages
        .iter()
        .position(|message| {
            message
                .content
                .iter()
                .any(|content| !matches!(content, Content::Text { .. }))
        })
        .unwrap_or(request.messages.len());
    request.messages[..own]
        .iter()
        .rev()
        .find(|message| matches!(message.role, Role::User))
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The person writes nine long notes to the agent, each its own message, and a second passes: what
/// is written next is the newest by the clock a message carries.
async fn long_notes(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
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
/// to take into the conversation's summary; once the answer has its words the server asks a model
/// once for it and Chat keeps it, so the next turn is told what was said long ago and asks for
/// nothing more. When the conversation outgrows it again the summary is extended from where it
/// stood. The question goes to the workspace's own model once one is linked, and is counted on
/// the agent's day as its turns are.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_server_keeps_the_summary_of_a_long_conversation() {
    use tilo_server::workspaces::person_model::ProviderProtocol;
    use tilo_server::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
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

    // The keeper linked nothing yet, so the server's model answers the turn, and the summary's
    // question is asked as the turn was.
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::clone(&unasked);
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(Some::<Model>(Arc::new(move || {
            Some(Box::new(Summarising {
                summary: "The boat is called Aurelia. The crossings went as planned.",
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the newest message is the one turn");
    assert_eq!(turns[0].cause, first.to_string());
    let instruction = chat::recall::instruction_with_history(&turns[0].request, &turns[0].history);
    assert!(
        instruction.len() <= ui::collaboration::recall::INSTRUCTION_BYTES,
        "a turn stays inside its budget: {} bytes",
        instruction.len()
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
        .get(&chat::chat::reply_id(&first, &MemberId::agent(SCOUT)))
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
        let calls = unasked.lock().unwrap();
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
    let recalled = chat::recall::instruction_with_history(&turns[0].request, &turns[0].history);
    assert!(
        recalled.contains("What came before, as you summarised it"),
        "the next turn is told the summary: {recalled}"
    );
    assert!(
        recalled.contains("The boat is called Aurelia."),
        "{recalled}"
    );
    assert!(
        !recalled.contains("Remember this"),
        "what the summary took in is not told again as written"
    );
    assert_eq!(
        turns[0].fold, None,
        "what is left outside the summary is carried as written"
    );

    // The workspace links a model of its own, and the conversation outgrows its summary again:
    // their model is asked, once, for what the new messages add to it.
    let theirs = ScriptedProvider::answering([
        Reply::text("Still nothing.").costing(300),
        Reply::text("The later crossings were quiet too.").costing(40),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
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
    let messages = &calls[0].request.messages;
    let has_text = |message: &Message, words: &str| {
        message.content.iter().any(|content| {
            matches!(
                content,
                Content::Text { text } if text.contains(words)
            )
        })
    };
    assert!(messages.iter().any(|message| {
        message.role == Role::Assistant && has_text(message, "Nothing needs doing today.")
    }));
    assert!(
        messages
            .iter()
            .any(|message| { message.role == Role::User && has_text(message, "Later note 8:") })
    );
    assert!(messages.last().is_some_and(|message| {
        message.role == Role::User
            && has_text(message, "and now?")
            && !has_text(message, "Nothing needs doing today.")
            && !has_text(message, "Later note 8:")
    }));
    assert!(
        calls[1].request.tools.is_empty(),
        "a summary is words, no tools"
    );
    assert_eq!(
        unasked.lock().unwrap().len(),
        2,
        "the host's model is not asked once the keeper linked their own"
    );

    // The summary holds both: what it held, and what the new messages added.
    write_to_agent(&actions, &workspaces, &workspace, written, "thanks").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn in the conversation: {turns:?}");
    let recalled = chat::recall::instruction_with_history(&turns[0].request, &turns[0].history);
    for kept in [
        "The boat is called Aurelia.",
        "The later crossings were quiet too.",
    ] {
        assert!(
            recalled.contains(kept),
            "{kept:?} is in the summary the turn is told: {recalled}"
        );
    }

    // Every call is counted on the agent's day: two of the server's model, two of the keeper's.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = workspaces
            .resource_document(&workspace, ui::tilo_workspace_agent::OWNER, &agents_record)
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

/// A model that takes each of `calls` in turn through `tilo_do`, the tool its turn has for an
/// action, and then says `said`.
pub(super) fn does_then_answers(
    calls: Vec<serde_json::Value>,
    said: &'static str,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    Arc::new(move || {
        let mut answers: Vec<ModelResponse> = calls
            .iter()
            .enumerate()
            .map(|(at, call)| {
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: format!("call-{}", at + 1),
                        name: "tilo_do".into(),
                        input: call.clone(),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    1_000,
                )
            })
            .collect();
        answers.push(answer(
            vec![Content::Text { text: said.into() }],
            StopReason::EndTurn,
            1_000,
        ));
        Some(Box::new(Scripted {
            answers,
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What the turn's tools answered the model's call `call`, as the request after it carries it:
/// the words, and whether they were told as a failure.
pub(super) fn answered_to(request: &ModelRequest, call: &str) -> (String, bool) {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            Content::ToolResult(result) if result.call_id == call => {
                Some((result.content.clone(), result.is_error))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("the model was answered nothing for {call}: {request:?}"))
}

/// What a person's events answer reaches the model of a turn. An action of theirs is answered
/// with its receipt alone, while the tools a turn has read how an invocation ended by following
/// it: the assistant sets an event up and then lists its person's events, both through
/// `tilo_do`, and is told what each answered, not that it is still pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_persons_events_answer_reaches_the_model_of_a_turn() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = served.roster.actions.clone();
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    // The workspace as a person has it: their agent made to work here, and the chat with them
    // in it.
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    let events = WorkspaceEvents::start(
        workspaces.clone(),
        None,
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );

    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Remind me every morning to call home.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(does_then_answers(
            vec![
                serde_json::json!({
                    "path": "tilo.events/events#set-up",
                    "args": {
                        "name": "Call home",
                        "when": "every day at 09:00",
                        "instruction": "Say that it is time to call home.",
                    },
                }),
                serde_json::json!({"path": "tilo.events/events#list"}),
            ],
            "You will be reminded every morning.",
            Arc::clone(&asked),
        ))
        .with_events(Some(events));
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
    let (set_up, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && set_up.starts_with("ok"),
        "what setting the event up answered the model: {set_up}"
    );
    // Taken after: the event is one of its person's now.
    let (listed, failed) = answered_to(&asked[2], "call-2");
    assert!(
        !failed && listed.starts_with("ok") && listed.contains("Call home"),
        "what the person's events answered the model: {listed}"
    );
}

/// What a turn finds by words in what its workspace holds.
#[path = "hosted_search.rs"]
mod search;

/// What wakes a turn's watch of its workspace, and what it is told of it.
#[path = "hosted_agent_watch.rs"]
mod watch;

/// What starts a turn besides its person's typing, and what reaches a turn that is going.
#[path = "hosted_agent_asks.rs"]
mod asks;

/// What a turn answered on the workspace's server knows about its agent's keeper, and keeps
/// for them.
#[path = "hosted_learning.rs"]
mod learning;
/// A turn that answers with the thing it worked on, shown live.
#[path = "hosted_agent_shares.rs"]
mod shares;
/// A turn that answers with a view it wrote.
#[path = "hosted_agent_views.rs"]
mod views;

#[cfg(test)]
#[path = "hosted_initiative.rs"]
mod hosted_initiative;

/// Invoke the member's native assistant grant action from their certified page.
async fn member_grant_action(
    page: &mut Browser,
    action: tilo_server_protocol::agents::AgentGrantAction,
) {
    let target = action.target(SCOUT);
    let receipt = page
        .invoke(request(
            &target.provider,
            target.address,
            InvokeArgument::None,
        ))
        .await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the keeper's grant action {action:?} failed: {receipt:?}"
    );
}

/// The keeper approves exactly the proposal shown on their root-bearing device,
/// then waits for the workspace host to admit that signing session.
async fn approve_member_grant(
    server: &crate::harness::hosted::Hosted,
    member: &SigningKey,
    workspace_id: WorkspaceId,
    apps: &[&str],
) {
    use tilo_server_client::MemberApprover;
    use tilo_server_client::pairing::{answer_agent_grant, pending_agent_grants};
    let keeper = member.verifying_key().to_bytes();
    let approver = MemberApprover::new(ServerAddress::at(server.server.node_addr()));
    let proposals = pending_agent_grants(&approver, member, workspace_id)
        .await
        .unwrap();
    let [shown] = proposals.as_slice() else {
        panic!("one pending agent")
    };
    assert_eq!(shown.proposal.scope.apps, apps);
    let device = tilo_server_client::pending_pairings(&approver, member)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.request_digest == shown.request_digest)
        .unwrap()
        .request
        .device_public_key;
    let elsewhere = WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    assert!(
        pending_agent_grants(&approver, member, elsewhere)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        answer_agent_grant(&approver, member, elsewhere, shown, true)
            .await
            .is_err()
    );
    let mut changed = shown.clone();
    changed.proposal.scope.apps.clear();
    assert!(
        answer_agent_grant(&approver, member, workspace_id, &changed, true)
            .await
            .is_err()
    );
    answer_agent_grant(&approver, member, workspace_id, shown, true)
        .await
        .unwrap();
    let link = server.server.agent_link();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if link
            .roster_session_device(&server.roster.sessions, workspace_id, SCOUT, &keeper)
            .await
            == Some(device)
        {
            break;
        }
        let grant = server
            .agents
            .supervisor()
            .snapshot()
            .grants
            .into_iter()
            .find(|grant| grant.proposal.delegate.agent_id() == Some(SCOUT))
            .map(|grant| (grant.state, grant.reason));
        assert!(
            Instant::now() < deadline,
            "the member's agent did not join under their approval: {grant:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A member of the host's workspace, not the host's own person, keeps an
/// agent on it: they make it from their page, approve its grant with their
/// own root from another device, and the turn that answers works for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_own_agent_on_the_host_works_for_them() {
    use tilo_peer_state::join::Role;
    use tilo_server_protocol::agents::AgentGrantAction;
    use tilo_workspace_service::agents::{Ask, ServerAgents};

    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (member, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let host = enrol(&owner, "tilo-sh", DeviceCapabilities::member());
    let member_page = enrol(&member, "bobs-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &owner,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    super::resource_support::admit_person(&server, &host, &workspace, &member_page, Role::Editor)
        .await;
    let mut page =
        super::resource_support::open_admitted_peer(&server, &member_page, &host, &workspace).await;
    let keeper = member.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    // The member makes an agent of their own, kept by them.
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    let workspace_id = WorkspaceId::parse(&workspace).unwrap();
    let work_apps = [
        CHAT,
        TASKS,
        ui::tilo_workspace_agent::OWNER,
        "so.tilo.shortcuts",
    ];
    member_grant_action(&mut page, AgentGrantAction::RequestWork).await;
    approve_member_grant(&server, &member, workspace_id, &work_apps).await;

    // The member gives it an issue, and the turn works for them, not for the
    // host's own person.
    let tracker_id = file_issue(
        &actions,
        &workspaces,
        &workspace,
        &member,
        (&host, &member_page),
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let runner = || {
        ServerAgents::with_sessions(
            workspaces.clone(),
            actions.clone(),
            Arc::new(server.server.agent_link()),
            server.roster.sessions.clone(),
        )
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)))
    };
    let agents = runner();
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(
        turns.len(),
        1,
        "the issue is the member's agent's turn: {turns:?}"
    );
    assert_eq!(
        turns[0].person, person,
        "it works for the member who kept it"
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
    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
    assert!(
        answered
            .issues
            .values()
            .flat_map(|issue| issue.comments.values())
            .any(|message| {
                message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT))
                    && message.text.contains("the plan is ready")
            }),
        "the member's agent answers in their issue"
    );
    workspaces
        .prepare_application(
            &workspace,
            workspaces.governor(),
            &crate::common::catalogue(),
            CHAT,
            "",
        )
        .await
        .unwrap();
    let caller =
        tilo_actions::remote::Caller::authenticated(keeper, member_page.cert.device_public_key)
            .in_workspace(&workspace);
    asks::sub_agents::child_writes_as_itself(
        agents.clone(),
        &workspaces,
        &workspace,
        &actions,
        &caller,
        &person,
    )
    .await;
    // The child Chat can wake a final turn of its lead. Stop its worker before filing
    // the next two tasks, which the test will hand to the host one at a time.
    assert!(agents.draining().finish(Duration::from_secs(30)).await);
    let agents = runner();
    let agents_document_id = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let roster = workspaces
        .read_resource::<WorkspaceAgents>(
            &workspace,
            (ui::tilo_workspace_agent::OWNER, &agents_document_id),
        )
        .await
        .unwrap()
        .unwrap();
    let children: Vec<_> = roster.sub_agents_of(&person).collect();
    let [child] = children.as_slice() else {
        panic!("the member's agent started one child")
    };
    for assignee in [SCOUT, child.id.0.as_str()] {
        file_issue_for(
            &actions,
            &workspaces,
            &workspace,
            &member,
            (&host, &member_page),
            assignee,
        )
        .await;
    }
    let mut tasks: Vec<_> = agents
        .turns(&workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|turn| matches!(turn.ask, Ask::Issue { .. }))
        .collect();
    tasks.sort_by(|a, b| a.agent.id.0.cmp(&b.agent.id.0));
    assert_eq!(
        tasks
            .iter()
            .map(|turn| turn.agent.id.0.as_str())
            .collect::<Vec<_>>(),
        [SCOUT, child.id.0.as_str()]
    );

    // Both assigned tasks were already owed when their keeper narrowed the grant.
    // A turn read under the earlier approval cannot spend or claim the workspace's work.
    member_grant_action(&mut page, AgentGrantAction::Revoke).await;
    member_grant_action(&mut page, AgentGrantAction::RequestChat).await;
    approve_member_grant(&server, &member, workspace_id, &[CHAT]).await;
    let calls = asked.lock().unwrap().len();
    let before = tracker(&workspaces, &workspace, &tracker_id).await;
    for task in &tasks {
        agents.answer(&workspace, task).await.unwrap();
        let Ask::Issue {
            record,
            issue,
            cause,
        } = &task.ask
        else {
            unreachable!()
        };
        let lease = format!("turn/{}/{record}/{issue}/{cause}", task.agent.id.0);
        assert!(
            workspaces
                .lease_holder(&workspace, &lease)
                .await
                .unwrap()
                .is_none(),
            "the Chat-only session took a task lease for {}",
            task.agent.id.0
        );
    }
    assert_eq!(
        asked.lock().unwrap().len(),
        calls,
        "neither task consumed a model call"
    );
    assert_eq!(
        tracker(&workspaces, &workspace, &tracker_id).await,
        before,
        "neither task acquired work or an answer"
    );

    member_grant_action(&mut page, AgentGrantAction::Revoke).await;
    member_grant_action(&mut page, AgentGrantAction::RequestWork).await;
    approve_member_grant(&server, &member, workspace_id, &work_apps).await;
    for task in &tasks {
        agents.answer(&workspace, task).await.unwrap();
        let Ask::Issue { issue, .. } = &task.ask else {
            unreachable!()
        };
        let tracker = tracker(&workspaces, &workspace, &tracker_id).await;
        let written = &tracker.issues[&Id::from_raw(issue.clone())].comments;
        assert!(
            written.values().any(|comment| comment.author
                == Some(MemberId::agent(&task.agent.id.0))
                && comment.text.contains("the plan is ready")),
            "{} answered its assigned task after approval",
            task.agent.id.0
        );
    }
}
