//! A person's agent on a computer of theirs (`tilo login`, then `tilo mcp`) is one of the
//! workspace's agents from its first call there: the server adds it to the workspace's own list
//! of its agents, kept by its person and marked as the client it connects as, so the workspace's
//! Agents tab shows it, and what its person bounds it to there holds its next call.
use super::*;
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;
use ui::tilo_resource::{Id, Replica, Resource as _};
use ui::tilo_workspace_agent::{Agent, WorkspaceAgents, workspace_agents};

const TASKS: &str = "so.tilo.tasks";
const CHAT: &str = "so.tilo.chat";
const NAME: &str = "Claude Code";

fn workspace_agents_route() -> DocumentRoute {
    DocumentRoute {
        app_id: ui::tilo_workspace_agent::OWNER.into(),
        cluster_id: WORKSPACE.into(),
        document: collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND),
    }
}

fn workspace_agents_checkout() -> ui::resource::DocumentCheckout {
    ui::resource::DocumentCheckout::new(
        ui::resource::DocumentId::parse(&workspace_agents_route().document).unwrap(),
        ui::resource::ResourceType::new(ui::tilo_workspace_agent::KIND),
    )
}

fn sdk_workspace_agents(workspace: &str) -> ui::tilo_resource::Doc {
    let mut native = ui::tilo_resource::Doc::new();
    ui::resource::genesis(
        &mut native,
        &workspace_agents_checkout(),
        workspace,
        |doc| WorkspaceAgents::initialize(doc).map_err(|error| error.to_string()),
    )
    .unwrap();
    native
}

/// The agent `NAME` on `computer`, in a session of its own: an MCP server acting as it.
pub(super) async fn computer_agent(
    hosted: &Hosted,
    computer: &Device,
    server: &Device,
) -> (Browser, McpServer<SessionActions>) {
    let agent = Browser::open(hosted, computer, server).await;
    tilo_session_client::device::session_as_agent(&agent.peer, NAME)
        .await
        .unwrap();
    let person = hex::encode(computer.cert.user_public_key);
    let id = tilo_actions::remote::device_agent_id(&computer.cert.user_public_key, NAME);
    let server = McpServer::for_agent(
        SessionActions::new(&agent.peer, agent.workspace, server.cert.device_public_key),
        WORKSPACE.into(),
        &id,
        &person,
    )
    .with_agent_session("session-laptop".to_owned());
    (agent, server)
}

/// What the agent's call to make an issue titled `title` answered.
async fn new_task(server: &McpServer<SessionActions>, title: &str) -> serde_json::Value {
    server
        .handle(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments":
                {"path": "tasks#create", "args": {"title": title}}}}),
        )
        .await
        .unwrap()
}

/// The workspace's list of its agents as its server holds it, once it holds one.
async fn agents_document(hosted: &Hosted) -> Option<automerge::AutoCommit> {
    let document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let bytes = hosted
        .server
        .workspaces()?
        .resource_document(WORKSPACE, ui::tilo_workspace_agent::OWNER, &document)
        .await
        .ok()
        .flatten()?;
    automerge::AutoCommit::load(&bytes).ok()
}

/// The workspace's agents as its server holds them, once it holds any.
async fn agents(hosted: &Hosted) -> Option<WorkspaceAgents> {
    let mut doc = agents_document(hosted).await?;
    tilo_workspace_service::agents::opened::<WorkspaceAgents>(&mut doc).ok()
}

/// The agent `id` as the workspace's list of its agents names it, once it names it as `until`
/// asks.
async fn listed(hosted: &Hosted, id: &str, until: impl Fn(&Agent) -> bool) -> Agent {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = agents(hosted)
            .await
            .and_then(|agents| agents.agents.find(&Id::from_raw(id.to_owned())).cloned())
            .filter(|agent| until(agent));
        if let Some(agent) = found {
            return agent;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace's agents never list the computer's agent as asked"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computers_agent_is_listed_once_where_it_acts() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &server).await;
    browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    let (_agent, server) = computer_agent(&hosted, &laptop, &server).await;
    let answer = new_task(&server, "From the laptop").await;
    assert_ne!(answer["result"]["isError"], true, "{answer}");

    // The workspace's agents now list it, as its person's, for the client it connects as.
    let id = tilo_actions::remote::device_agent_id(&root.verifying_key().to_bytes(), NAME);
    let agent = listed(&hosted, &id, |_| true).await;
    assert_eq!(agent.name, NAME);
    assert_eq!(agent.keeper, hex::encode(root.verifying_key().to_bytes()));
    assert_eq!(agent.mcp_client.as_deref(), Some(NAME));
    assert!(agent.is_active(), "{agent:?}");

    let mut first = agents_document(&hosted).await.unwrap();
    ui::resource::validate_identity(&first, &workspace_agents_checkout(), WORKSPACE)
        .expect("Assistant can open the roster the server founded");
    let roots: Vec<_> = first
        .get_changes(&[])
        .into_iter()
        .filter(|change| change.deps().is_empty())
        .map(|change| change.hash())
        .collect();
    assert_eq!(
        roots,
        sdk_workspace_agents(WORKSPACE).get_heads(),
        "server and SDK found one roster"
    );
    let first_heads = first.get_heads();

    // Its next call finds it listed, and lists it once.
    let again = new_task(&server, "Again from the laptop").await;
    assert_ne!(again["result"]["isError"], true, "{again}");
    let all = agents(&hosted).await.unwrap();
    assert_eq!(all.agents.len(), 1, "listed once");

    assert_eq!(
        agents_document(&hosted).await.unwrap().get_heads(),
        first_heads,
        "repeat calls reuse the roster"
    );

    deny_task_changes(&hosted, &browser, &agent.keeper, &id).await;
    let bounded = new_task(&server, "Past the bound").await;
    assert_eq!(bounded["result"]["isError"], true, "{bounded}");
}

/// The keeper narrows their agent's bound through a signed workspace edit.
async fn deny_task_changes(
    hosted: &Hosted,
    browser: &Browser,
    person: &str,
    id: &str,
) -> (Agent, Vec<automerge::ChangeHash>) {
    let doc = agents_document(hosted).await.unwrap();
    let mut agents_list = Replica::<WorkspaceAgents>::from_doc(doc)
        .as_author(person)
        .in_app(ui::tilo_workspace_agent::OWNER, "Assistant");
    let heads = agents_list.doc_mut().get_heads();
    let tasks = tilo_action_policy::agent_answers::AnswerScope::app(TASKS).node(WORKSPACE);
    agents_list
        .apply(
            WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
                agent: Id::from_raw(id.to_owned()),
                bound: vec![ui::tilo_workspace_agent::Bound {
                    node: tasks.to_string(),
                    kinds: vec!["undoable".into()],
                    answer: "deny".into(),
                }],
            }),
        )
        .unwrap();
    let changes = agents_list.doc_mut().save_after(&heads);
    let bound_heads = agents_list.doc_mut().get_heads();
    browser
        .peer
        .write(
            workspace_agents_route(),
            &collab_types::DocumentMutation::Incremental(changes),
        )
        .unwrap();
    (
        listed(hosted, id, |agent| !agent.bound.is_empty()).await,
        bound_heads,
    )
}

/// Reproduce the old governor's first CREATE_AGENT publication, including the app attribution and
/// signed workspace channel. The caller supplies the document it starts from.
async fn publish_agents_as_host(
    hosted: &Hosted,
    person: &str,
    id: &str,
    record: Replica<WorkspaceAgents>,
) -> automerge::AutoCommit {
    let workspaces = hosted.server.workspaces().unwrap();
    workspaces
        .author_workspace_document(
            WORKSPACE,
            workspaces.governor(),
            (
                ui::tilo_workspace_agent::OWNER,
                &workspace_agents_route().document,
            ),
            |doc| {
                assert!(doc.get_heads().is_empty());
                let mut record = record
                    .as_author(person)
                    .in_app(ui::tilo_workspace_agent::OWNER, "Assistant");
                record
                    .apply(
                        WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                            name: ui::tilo_resource::NonBlank::parse(NAME).unwrap(),
                            id: Some(id.to_owned()),
                            model: None,
                            mcp_client: Some(NAME.into()),
                        }),
                    )
                    .unwrap();
                std::mem::swap(doc, record.doc_mut());
                Ok(doc.clone())
            },
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_computer_reopens_its_unstamped_agents_record_and_keeps_its_bound() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &server).await;
    browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    let person = hex::encode(root.verifying_key().to_bytes());
    let id = tilo_actions::remote::device_agent_id(&root.verifying_key().to_bytes(), NAME);
    let mut old = publish_agents_as_host(&hosted, &person, &id, Replica::new()).await;
    assert!(ui::resource::identity(&old).is_none());
    let old_heads = old.get_heads();
    let (agent, bound_heads) = deny_task_changes(&hosted, &browser, &person, &id).await;

    let (_agent, mcp) = computer_agent(&hosted, &laptop, &server).await;
    let denied = new_task(&mcp, "Past the previous keeper's bound").await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let mut recovered = agents_document(&hosted).await.unwrap();
    ui::resource::validate_identity(&recovered, &workspace_agents_checkout(), WORKSPACE).unwrap();
    assert!(
        recovered.fork_at(&old_heads).is_ok(),
        "the old governor's roster survives recovery"
    );
    assert!(
        recovered.fork_at(&bound_heads).is_ok(),
        "the keeper's signed edit survives recovery"
    );
    assert_eq!(listed(&hosted, &id, |_| true).await, agent);
    assert_eq!(agents(&hosted).await.unwrap().agents.len(), 1);
    let recovered_heads = recovered.get_heads();

    let again = new_task(&mcp, "Still past the same bound").await;
    assert_eq!(again["result"]["isError"], true, "{again}");
    assert_eq!(
        agents_document(&hosted).await.unwrap().get_heads(),
        recovered_heads
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_record_from_another_workspace_cannot_authorize_a_reconnecting_computer() {
    const FOREIGN: &str = "3f2b1c00-0000-4000-8000-00000000000b";
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &server).await;
    browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    let person = hex::encode(root.verifying_key().to_bytes());
    let id = tilo_actions::remote::device_agent_id(&root.verifying_key().to_bytes(), NAME);
    publish_agents_as_host(
        &hosted,
        &person,
        &id,
        Replica::from_doc(sdk_workspace_agents(FOREIGN)),
    )
    .await;
    let before = agents_document(&hosted).await.unwrap().get_heads();
    let (_agent, mcp) = computer_agent(&hosted, &laptop, &server).await;
    let denied = new_task(&mcp, "Using the other workspace's roster").await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let mut after = agents_document(&hosted).await.unwrap();
    assert_eq!(after.get_heads(), before);
    ui::resource::validate_identity(&after, &workspace_agents_checkout(), FOREIGN).unwrap();
    assert!(
        ui::resource::validate_identity(&after, &workspace_agents_checkout(), WORKSPACE).is_err()
    );
}
