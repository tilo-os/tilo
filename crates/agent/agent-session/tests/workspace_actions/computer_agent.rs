//! A person's agent on a computer of theirs (`tilo login`, then `tilo mcp`) is one of the
//! workspace's agents from its first call there: the server adds it to the workspace's own list
//! of its agents, kept by its person and marked as the client it connects as, so the workspace's
//! Agents tab shows it.
use super::*;
use tilo_mcp::McpServer;
use tilo_session_client::remote::SessionActions;
use ui::tilo_resource::Id;
use ui::tilo_workspace_agent::{Agent, WorkspaceAgents};

const NOTES: &str = "so.tilo.notes";
const CHAT: &str = "so.tilo.chat";
const NAME: &str = "Claude Code";

/// The agent `NAME` on `computer`, in a session of its own: an MCP server acting as it.
async fn computer_agent(
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
    .with_agent_session("session-laptop".to_owned(), NAME);
    (agent, server)
}

/// What the agent's call to make a note titled `title` answered.
async fn new_note(server: &McpServer<SessionActions>, title: &str) -> serde_json::Value {
    server
        .handle(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments":
                {"path": "notes#new-note", "args": {"title": title}}}}),
        )
        .await
        .unwrap()
}

/// The workspace's agents as its server holds them, once it holds any.
async fn agents(hosted: &Hosted) -> Option<WorkspaceAgents> {
    let document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let bytes = hosted
        .server
        .workspaces()?
        .resource_document(WORKSPACE, CHAT, &document)
        .await
        .ok()
        .flatten()?;
    let mut doc = automerge::AutoCommit::load(&bytes).ok()?;
    tilo_server::workspace_bots::opened::<WorkspaceAgents>(&mut doc).ok()
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
    browser.create(NOTES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let (_agent, server) = computer_agent(&hosted, &laptop, &server).await;
    let answer = new_note(&server, "From the laptop").await;
    assert_ne!(answer["result"]["isError"], true, "{answer}");

    // The workspace's agents now list it, as its person's, for the client it connects as.
    let id = tilo_actions::remote::device_agent_id(&root.verifying_key().to_bytes(), NAME);
    let agent = listed(&hosted, &id, |_| true).await;
    assert_eq!(agent.name, NAME);
    assert_eq!(agent.keeper, hex::encode(root.verifying_key().to_bytes()));
    assert_eq!(agent.mcp_client.as_deref(), Some(NAME));
    assert!(agent.is_active(), "{agent:?}");

    // Its next call finds it listed, and lists it once.
    let again = new_note(&server, "Again from the laptop").await;
    assert_ne!(again["result"]["isError"], true, "{again}");
    let all = agents(&hosted).await.unwrap();
    assert_eq!(all.agents.len(), 1, "listed once");
}
