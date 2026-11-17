//! A turn of the assistant opens Browser with no device of its person doing anything: the
//! window is placed in the workspace's machine, once, and an agent whose rules keep it from
//! Browser has none placed.
//!
//! The server, the person and the assistant are those of the test this is a part of. The
//! machine is a stand-in the test holds: it keeps what the server handed the window's session
//! and starts nothing. No page is opened here.
use super::*;
use tilo_server::hosted_browser::{BrowserSessions, HostedBrowsers, Unmetered, Window};
use tilo_server_protocol::hosted::Material;
use ui::tilo_workspace_agent::Bound;

/// The provider the stand-in window publishes under.
const WINDOW: &str = "4711";

/// Where the server starts a workspace's Browser: nowhere, here. It keeps what the server
/// handed each window's session.
#[derive(Clone, Default)]
struct Machines(Arc<Mutex<Vec<Material>>>);

impl Machines {
    fn started(&self) -> Vec<Material> {
        self.0.lock().unwrap().clone()
    }
}

impl BrowserSessions for Machines {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, _: &str) {}
}

/// A server that places Browser windows, with its person, their assistant and their model.
async fn placing() -> (Hosted, Machines, Arc<HostedBrowsers>) {
    let machines = Machines::default();
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(machines.clone()),
        Box::new(Unmetered),
        "acct_ada".into(),
    ));
    let hosted = Box::pin(Hosted::start_placing(Some(Arc::clone(&browsers)))).await;
    (hosted, machines, browsers)
}

/// The model's step that opens Browser through the agent's tools.
fn opens(call: &str) -> Reply {
    does(call, "browser#open", &serde_json::json!({}))
}

/// The person narrows their assistant to `bound`, as their page does: their device writes the
/// rules into the workspace's list of its agents. Answers once the server holds them.
async fn narrowed_to(hosted: &Hosted, bound: Vec<Bound>) {
    use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents::SetAgentBound};
    let agents = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let held = hosted
        .workspaces
        .resource_document(&hosted.workspace, CHAT, &agents)
        .await
        .unwrap()
        .unwrap();
    let mut narrowed = ui::tilo_resource::Replica::<WorkspaceAgents>::load(&held)
        .unwrap()
        .as_author(&hosted.person());
    let rules = bound.len();
    narrowed
        .apply(WorkspaceAgents::SET_AGENT_BOUND.with(SetAgentBound {
            agent: Id::from_raw(SCOUT.to_owned()),
            bound,
        }))
        .unwrap();
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: hosted.workspace.clone(),
        document: agents.clone(),
    };
    let bytes = std::mem::take(narrowed.doc_mut()).save();
    hosted
        .page
        .peer
        .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = hosted
            .workspaces
            .resource_document(&hosted.workspace, CHAT, &agents)
            .await
            .unwrap()
            .unwrap();
        let listed = WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
        if listed
            .agents
            .iter()
            .any(|agent| agent.id.0 == SCOUT && agent.bound.len() == rules)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server never holds the assistant's rules"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The assistant's open of Browser, in a turn the server answers, starts the window's session
/// in the workspace the turn works in, on a device the server takes a connection from. A
/// second open, in a later turn, attaches to the window that is up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turns_open_of_browser_places_the_window_in_its_workspace_once() {
    let (hosted, machines, browsers) = Box::pin(placing()).await;
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    assert!(
        browsers.window(workspace_id).is_none(),
        "no window before anyone opened Browser"
    );
    Box::pin(hosted.turn(
        "Open the browser.",
        vec![opens("open-1"), Reply::text("Browser is open.")],
    ))
    .await;
    let shown = hosted.last_tool_result();
    let started = machines.started();
    assert_eq!(
        started.len(),
        1,
        "the turn's open started the window's session: {shown}"
    );
    assert_eq!(
        started[0].workspace, hosted.workspace,
        "in the workspace the turn works in"
    );
    assert_eq!(started[0].workspace_id, workspace_id);
    let window = browsers
        .window(workspace_id)
        .expect("the workspace has its window");
    assert_eq!(window.provider, WINDOW);
    assert_eq!(
        hex::encode(window.device),
        hex::encode(started[0].device.device_public_key),
        "held by the device the server certified for it"
    );
    // That device is one its server takes a connection from, as the window's session dials it.
    tilo_server::ServerClient::connect(
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap(),
        hosted.address.clone(),
        hosted.root.verifying_key().to_bytes(),
        started[0].device.clone(),
        started[0].key().unwrap(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .expect("the device the server placed dials its server");
    Box::pin(hosted.turn(
        "Open the browser again.",
        vec![opens("open-2"), Reply::text("It was open already.")],
    ))
    .await;
    assert_eq!(
        machines.started().len(),
        1,
        "a second open starts no second session: {}",
        hosted.last_tool_result()
    );
    hosted.page.peer.close();
}

/// An assistant its person's rules keep from Browser opens none: no session is started and the
/// workspace has no window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_rules_refuse_browser_has_no_window_placed() {
    let (hosted, machines, browsers) = Box::pin(placing()).await;
    let workspace_id = WorkspaceId::parse(&hosted.workspace).unwrap();
    narrowed_to(
        &hosted,
        vec![Bound {
            node: format!(
                "workspace:{}/app:{}",
                hosted.workspace,
                tilo_server::hosted_browser::APP
            ),
            kinds: vec!["read".into(), "undoable".into(), "outside".into()],
            answer: "deny".into(),
        }],
    )
    .await;
    Box::pin(hosted.turn(
        "Open the browser.",
        vec![opens("open-1"), Reply::text("I may not open Browser.")],
    ))
    .await;
    let shown = hosted.last_tool_result();
    assert!(
        machines.started().is_empty(),
        "no session is started for an agent its rules keep from Browser: {shown}"
    );
    assert!(
        browsers.window(workspace_id).is_none(),
        "and the workspace has no window: {shown}"
    );
    hosted.page.peer.close();
}
