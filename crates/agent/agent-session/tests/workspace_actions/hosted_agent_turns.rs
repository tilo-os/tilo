//! An agent a workspace made to run on its server works around the clock there: a person gives it
//! an issue, and the server's agent works on it through the agent tools, as the person's agent, then
//! answers in the issue's thread. Its person's daily limit stops it before it spends past it.
use super::*;
use std::sync::Mutex;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};
use tilo_workspace_service::agents::{Model, ServerAgents, events::WorkspaceEvents};
use tilo_workspace_service::asks;
use tilo_workspace_service::tl_records::runtime::Value as TlValue;
use tilo_workspace_service::tl_records::runtime::rich::RichValue;
use ui::tilo_resource::{Id, Resource as _};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

const CHAT: &str = "so.tilo.chat";
pub(super) const TASKS: &str = "so.tilo.tasks";
/// The agent's id in the workspace's record of its agents.
pub(super) const SCOUT: &str = "scout";

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
/// set, and the workspace's member list takes the agent, acting for them. Answers once the
/// server holds both.
pub(super) async fn make_agent(
    page: &Browser,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    limit_micros: Option<u64>,
) {
    let record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let mut made = ui::tilo_resource::Replica::<WorkspaceAgents>::new().as_author(person);
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
    // The page asks its host to take the agent into the workspace's member list, acting for
    // the person.
    workspaces
        .add_agent(
            workspace,
            ui::tilo_resource::MemberId::person(person.to_owned()),
            SCOUT,
            person,
        )
        .await
        .expect("the workspace takes Scout as a member");
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
        serde_json::json!({"title": "Plan the launch", "assignee": format!("agent:{SCOUT}")});
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
    let acts_for = root.verifying_key().to_bytes();
    let person = hex::encode(acts_for);
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
                key: Some("sk-the-persons-own"),
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
        "the host's own model is not asked for a person who linked theirs"
    );
    let asked = theirs.asked();
    assert_eq!(asked.len(), 2, "one call to look, one to answer");
    for call in &asked {
        assert_eq!(call.model, "gemini-main");
        assert_eq!(call.key.as_deref(), Some("sk-the-persons-own"));
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

    // What the person's provider charged is what the agent's day counts.
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

/// A person who linked a provider is never answered by another model: one whose key is missing is
/// told so, where they asked, and the server's own model stays unasked. A person who linked nothing
/// on a server that runs no model is told what to link.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_whose_agent_model_cannot_answer_is_told_and_no_other_model_is_asked() {
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
    let acts_for = root.verifying_key().to_bytes();
    let person = hex::encode(acts_for);
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
        tilo_actions::remote::Caller::authenticated(acts_for, browser.cert.device_public_key)
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

    // What its turns cost is counted on its record, for its person.
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
async fn an_agent_on_the_host_stops_at_its_persons_daily_limit() {
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

/// Every ask of the workspace's Chat record, oldest first, as the record holds them now.
///
/// The record is opened here rather than through [`Workspaces::with_tl_record`], which runs
/// its work on a blocking thread: a model that reads the record while it decides runs on one
/// already, and would wait for a thread of that pool with none left.
pub(super) async fn asked_on(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Vec<asks::Asked> {
    let packages = asks::chat_packages().unwrap();
    let record = workspaces
        .tl_record(workspace, packages.app())
        .expect("the workspace's Chat record reads");
    let policy = workspaces
        .policy_of(workspace)
        .await
        .map(std::sync::Arc::new);
    let opened = tilo_workspace_service::tl_records::Opened::open(
        record.store(),
        &packages,
        tilo_server::tl_chat::viewer(),
        policy,
    )
    .expect("the workspace's Chat record opens");
    asks::asks(&opened).expect("the workspace's asks read")
}

/// The ask a person's message `message` made of the agent `agent`, where the record holds one:
/// the one still owed or answered, never one a message that names the agent in two ways left
/// withdrawn.
pub(super) fn ask_on<'a>(
    asked: &'a [asks::Asked],
    message: &Id,
    agent: &str,
) -> Option<&'a asks::Asked> {
    let matching = || {
        asked.iter().filter(|ask| {
            ask.agent == MemberId::agent(agent.to_owned())
                && ask
                    .from
                    .as_ref()
                    .is_some_and(|from| from.id_string() == message.as_str())
        })
    };
    matching()
        .find(|ask| ask.state != asks::AskState::Withdrawn)
        .or_else(|| matching().next_back())
}

/// The person of `root` opens their conversation with the agent `agent` in `workspace`, and
/// answers its id: their direct conversation, the one their messages to it stand in and its
/// replies stand under.
pub(super) async fn conversation_with(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    agent: &str,
) -> Id {
    let person = hex::encode(root.verifying_key().to_bytes());
    direct_of(workspaces, workspace, &person, agent).await
}

/// [`conversation_with`], for the person `person` by their id.
pub(super) async fn direct_of(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    agent: &str,
) -> Id {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let agent = MemberId::agent(agent.to_owned());
    let id = workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let direct = opened.invoke(
                &who,
                (&chat, "message_person"),
                vec![
                    TlValue::resource(&all),
                    TlValue::Actor(asks::Actor::Member(agent)),
                ],
                None,
            )?;
            let TlValue::Ref(direct) = direct else {
                return Err("message_person made nothing".to_owned());
            };
            Ok(direct.target.id_string())
        })
        .await
        .expect("the person has their conversation with the agent");
    Id::from_raw(id)
}

/// The person writes `text` to the agent in their conversation with it, as their page does: a
/// message of Chat's, which asks the agent. The message's id.
pub(super) async fn write_to_agent(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    (root, browser): (&SigningKey, &crate::support::Device),
    text: &str,
) -> Id {
    let _ = (actions, browser);
    let person = hex::encode(root.verifying_key().to_bytes());
    write_message(workspaces, workspace, &person, SCOUT, text).await
}

/// [`write_to_agent`] as `person` to the agent `agent`.
pub(super) async fn write_message(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    person: &str,
    agent: &str,
    text: &str,
) -> Id {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let agent = MemberId::agent(agent.to_owned());
    let text = text.to_owned();
    // The message, as the person writes it: what they say, then the agent named after it. The
    // mention is what asks the agent for them, made from the message, as a mention in any of
    // Chat's text does.
    let sent = {
        let (who, chat, agent, text) = (who.clone(), chat.clone(), agent.clone(), text.clone());
        workspaces
            .with_tl_record(workspace, &packages, who.clone(), move |opened| {
                let all = opened
                    .host()
                    .root_of(&chat)
                    .map_err(|error| error.to_string())?;
                let direct = opened.invoke(
                    &who,
                    (&chat, "message_person"),
                    vec![
                        TlValue::resource(&all),
                        TlValue::Actor(asks::Actor::Member(agent.clone())),
                    ],
                    None,
                )?;
                let TlValue::Ref(direct) = direct else {
                    return Err("message_person made nothing".to_owned());
                };
                let direct = (*direct.target).clone();
                let mut said = RichValue::default();
                said.push_text(&text);
                said.push_reference("@Scout", &asks::Reference::Member(agent));
                // Sent as a bridge sends one written elsewhere: a message whose text names the
                // agent, which the host asks it from. Chat's `send` would ask it too, under a
                // second key, and leave a turn nothing answers.
                let sent = opened.invoke(
                    &who,
                    (&chat, "relay"),
                    vec![
                        TlValue::resource(&direct),
                        TlValue::Rich(said),
                        TlValue::Text("Ada".into()),
                        TlValue::Text("another chat".into()),
                        TlValue::Int(0),
                        TlValue::List(Vec::new()),
                        TlValue::Opt(None),
                    ],
                    None,
                )?;
                let TlValue::Ref(sent) = sent else {
                    return Err("send made nothing".to_owned());
                };
                Ok(sent.target.id_string())
            })
            .await
            .expect("the person writes to their agent")
    };
    Id::from_raw(sent)
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
    let asked_now = asked_on(&workspaces, &workspace).await;
    let ask = ask_on(&asked_now, &first, SCOUT).expect("the message asks the agent");
    assert_eq!(turns[0].cause, ask.at.id_string());
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

    // Its answer is the reply the ask's agent wrote: the model's words, with its turn ended, so
    // the message asks nothing more.
    let asked_now = asked_on(&workspaces, &workspace).await;
    let ask = ask_on(&asked_now, &first, SCOUT).unwrap_or_else(|| {
        panic!(
            "the message asks the agent: {:?}",
            asked_now
                .iter()
                .map(|ask| (ask.agent.clone(), ask.from.clone()))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(ask.state, asks::AskState::Done, "its turn ended");
    let reply = ask.latest().expect("the agent answered");
    assert!(reply.body.contains("the plan is ready"), "{}", reply.body);
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "one turn to look, one to answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
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
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = serving
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
    assert!(
        asked_on(&workspaces, &workspace).await.is_empty(),
        "nothing was asked here yet: the first message asks the agent"
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
                    if write.workspace == workspace && write.app == CHAT {
                        break;
                    }
                }
            },
        )
        .await;
        // The look that write starts, as the server's agents make it for a write of Chat's.
        agents.look(&workspace).await.unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let asked_now = asked_on(&workspaces, &workspace).await;
            let ended = ask_on(&asked_now, &message, SCOUT)
                .is_some_and(|ask| ask.state == asks::AskState::Done);
            if ended {
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
    let events = WorkspaceEvents::start(
        workspaces.clone(),
        false,
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
mod hosted_asks;

/// A turn that answers with the thing it worked on, shown live, or with a view it wrote.
#[path = "hosted_agent_shares.rs"]
mod shares;

/// The workspace's skills, which a turn is told of and reads through its tools, and which its
/// people write through Chat's actions.
#[path = "hosted_skills.rs"]
mod skills;

#[cfg(test)]
#[path = "hosted_initiative.rs"]
mod hosted_initiative;

/// A mention of the hosted agent asks it, and it answers under the message: the reply grows
/// while its model writes, and what it learns about its keeper is kept where nobody else
/// reads.
#[path = "hosted_asked.rs"]
mod asked;
/// The member's root signs one approver command for `server`. A member's
/// page holds their root, so the commands it sends carry that signature and
/// no device certificate of its own.
fn member_approval(
    root: &SigningKey,
    request: tilo_server_protocol::certify::ApproverRequest,
) -> tilo_server_protocol::certify::MemberApproval {
    use ed25519_dalek::Signer as _;
    let user_public_key = root.verifying_key().to_bytes();
    let issued_at_ns = tilo_clock::unix_now_ns();
    let digest = tilo_server_protocol::certify::MemberApproval::signed_digest(
        &user_public_key,
        issued_at_ns,
        &request,
    )
    .unwrap();
    tilo_server_protocol::certify::MemberApproval {
        user_public_key,
        issued_at_ns,
        request,
        signature: root.sign(&digest).to_bytes(),
        device: None,
    }
}

/// A member of the host's workspace, not the host's own person, keeps an
/// agent on it: they make it from their page, approve its grant with their
/// own root from another device, and the turn that answers works for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_own_agent_on_the_host_works_for_them() {
    use tilo_peer_state::join::Role;
    use tilo_server_client::MemberApprover;
    use tilo_server_client::MemberServer as _;
    use tilo_server_protocol::agents::{
        AGENTS_PROVIDER, REQUEST_GRANT_ACTION, SUPERVISOR_RESOURCE,
    };
    use tilo_server_protocol::certify::{ApproverReply, ApproverRequest};
    use tilo_user_identity::{CertificationPolicy, issue_device_cert};
    use tilo_workspace_service::agents::ServerAgents;

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
    let acts_for = member.verifying_key().to_bytes();
    let person = hex::encode(acts_for);
    // The member makes an agent of their own, kept by them.
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    // Their page asks the host to run it, and the host queues the grant for
    // the member's root.
    let asked = page
        .invoke(request(
            AGENTS_PROVIDER,
            ActionAddress::new(SUPERVISOR_RESOURCE, REQUEST_GRANT_ACTION),
            InvokeArgument::Json(
                serde_json::json!({
                    "agentId": SCOUT,
                    "workspaces": [workspace],
                    "apps": [TASKS],
                    "expiresInSecs": 3600,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        ))
        .await;
    assert!(
        matches!(asked.status, ReceiptStatus::Completed { .. }),
        "the member asks for their own agent's grant: {asked:?}"
    );
    let approver = MemberApprover::new(ServerAddress::at(server.server.node_addr()));
    let approve = |request| member_approval(&member, request);
    let ApproverReply::DelegateProposals(proposals) = approver
        .ask(&approve(ApproverRequest::DelegateProposals))
        .await
        .unwrap()
    else {
        panic!("the member's proposals");
    };
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    let proposal = proposals[0].clone();
    let ApproverReply::Pending(pending) = approver
        .ask(&approve(ApproverRequest::ListPending))
        .await
        .unwrap()
    else {
        panic!("the member's pending requests");
    };
    let pending = pending
        .into_iter()
        .find(|entry| entry.request_digest == proposal.request_digest)
        .unwrap();
    let now = tilo_clock::unix_now_ns();
    let cert = issue_device_cert(
        &pending.request,
        &member,
        &CertificationPolicy::agent(),
        &RevocationList::new(),
        now,
    )
    .unwrap();
    let delegation = proposal
        .proposal
        .sign(&member, pending.request.device_public_key, now)
        .unwrap();
    assert_eq!(
        approver
            .ask(&approve(ApproverRequest::Decide {
                request_digest: proposal.request_digest,
                decision: tilo_server_protocol::certify::ApproverDecision::IssueDelegate {
                    cert,
                    delegation,
                },
            }))
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    let workspace_id = WorkspaceId::parse(&workspace).unwrap();
    let link = server.server.agent_link();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if link
            .roster_session_device(&server.roster.sessions, workspace_id, SCOUT, &acts_for)
            .await
            .is_some()
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
    let agents = ServerAgents::with_sessions(
        workspaces.clone(),
        actions.clone(),
        Arc::new(server.server.agent_link()),
        server.roster.sessions.clone(),
    )
    .scripted(looks_then_answers(1_000, Arc::clone(&asked)));
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
    drop(approver);
}

/// The engines' recorded result pages, served to a turn's web tools from this machine.
const BRAVE_RESULTS: &str =
    include_str!("../../../../platform/tilo-web-search/tests/fixtures/brave.html");
const BING_RESULTS: &str =
    include_str!("../../../../platform/tilo-web-search/tests/fixtures/bing.html");

/// Look `query` up with the turn's own `search_web`, then answer.
fn searches_then_answers(query: &'static str, asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: "call-1".into(),
                        name: "search_web".into(),
                        input: serde_json::json!({ "query": query }),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    0,
                ),
                answer(
                    vec![Content::Text {
                        text: format!("I looked up {query} on the web."),
                    }],
                    StopReason::EndTurn,
                    0,
                ),
            ],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// A turn's own web tools search the public web: the model calls `search_web`, the engines are
/// asked at once and their merged results are what it read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_host_looks_things_up_on_the_web() {
    use tilo_workspace_service::agents::Engine;
    use tilo_workspace_service::agents::fixtures::{FixtureServer, Route};

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

    let query = "rust borrow checker";
    let fixtures = FixtureServer::serve(vec![
        Route::new("/brave", "text/html", BRAVE_RESULTS),
        Route::new("/bing", "text/html", BING_RESULTS),
    ]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(searches_then_answers(query, Arc::clone(&asked)))
        .web_looking_at(vec![
            (Engine::Brave, fixtures.at("/brave")),
            (Engine::Bing, fixtures.at("/bing")),
        ]);
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    let (calls, results): (usize, Vec<String>) = {
        let asked = asked.lock().unwrap();
        let results = asked[1]
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                Content::ToolResult(result) => Some(result.content.clone()),
                _ => None,
            })
            .collect();
        (asked.len(), results)
    };
    assert_eq!(calls, 2, "one query, one answer");
    assert_eq!(results.len(), 1, "the search answered");
    assert!(
        results[0].contains("\"results\"") && results[0].contains("Rust Programming Language"),
        "the engines' merged results reached the model: {}",
        results[0]
    );
    let seen = fixtures.requests();
    assert!(
        seen.iter()
            .any(|request| request.parameter("q").as_deref() == Some(query)),
        "the engines were asked the query: {seen:?}"
    );

    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
    assert!(
        answered
            .issues
            .values()
            .flat_map(|issue| issue.comments.values())
            .any(
                |message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT))
                    && message.text.contains("I looked up")
            ),
        "its answer is in the issue"
    );
}

/// A turn on a model whose descriptor knows a small context keeps its conversation within it:
/// the long answer of an earlier tool gives way before the next ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_sizes_its_conversation_to_a_small_models_context() {
    use tilo_server::workspaces::person_model::ProviderProtocol;
    use tilo_server::workspaces::person_model::scripted::{Linking, Reply, ScriptedProvider};
    use tilo_workspace_service::agents::fixtures::{FixtureServer, Route};

    // A page far longer than the model's context holds, marked so the test can tell whether a
    // later ask still carries it.
    const NEEDLE: &str = "UNIQUE-PAGE-NEEDLE";

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

    let body = format!(
        "<!doctype html><html><head><title>Long</title></head><body><main><p>{NEEDLE} {}</p>\
         </main></body></html>",
        "filler ".repeat(4_000)
    );
    let fixtures = FixtureServer::serve(vec![Route::new("/long", "text/html", body)]);
    let url = fixtures.at("/long");
    let theirs = ScriptedProvider::answering([
        Reply::tool_call(
            "call-1",
            "read_page",
            serde_json::json!({"url": url, "max_chars": 20_000}),
        ),
        Reply::tool_call("call-2", "tilo_ls", serde_json::json!({})),
        Reply::text("I read the page."),
    ]);
    workspaces.answer_models_with(theirs.clone());
    let provider = workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Local",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://api.openai.com/v1",
                key: Some("sk-the-keepers-own"),
                models: &["tiny-context"],
            },
            true,
        )
        .await
        .unwrap();
    // A window of 9000 tokens, 8192 of them its answers: the conversation keeps ~2.4k characters.
    workspaces
        .set_model_context_window(&workspace, &provider, "tiny-context", 9_000)
        .unwrap();

    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::clone(&unasked)))
        .web_looking_at(Vec::new());
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
        "the linked model answered"
    );
    let calls = theirs.asked();
    assert_eq!(calls.len(), 3, "read, look, answer");
    let tool_answers = |call: &tilo_server::workspaces::person_model::scripted::Asked| {
        call.request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                Content::ToolResult(result) => Some(result.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let second = tool_answers(&calls[1]);
    assert!(
        second.iter().any(|answer| answer.contains(NEEDLE)),
        "the page was read before the next ask: {second:?}"
    );
    let third = tool_answers(&calls[2]);
    assert!(
        !third.iter().any(|answer| answer.contains(NEEDLE)),
        "a long earlier answer gave way: {third:?}"
    );
    assert!(
        third.iter().any(|answer| answer.contains("left out")),
        "what was left out says so: {third:?}"
    );
    let answered = tracker(&workspaces, &workspace, &tracker_id).await;
    assert!(
        answered
            .issues
            .values()
            .flat_map(|issue| issue.comments.values())
            .any(
                |message| message.author == Some(ui::tilo_resource::MemberId::agent(SCOUT))
                    && message.text.contains("I read the page")
            ),
        "its answer is in the issue"
    );
}
