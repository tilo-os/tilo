//! A write is answered as the workspace decided it: one it takes is done, and
//! one it refuses is answered failed, with why, never as saved. Asked of the
//! server by a person's logged-in computer over its session, and written by a
//! device itself.
use super::resource_support::{admit_person, open_admitted_peer};
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_peer_state::join::Role;
use tilo_session_actions::SessionActions;

const TASKS: &str = "so.tilo.tasks";
const NOTES: &str = "so.tilo.notes";

/// What a tool call answers, and whether it failed.
async fn tool(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> (String, bool) {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": arguments}}))
        .await
        .expect("a call is answered");
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (
        if text.is_empty() {
            answer.to_string()
        } else {
            text
        },
        result["isError"] == true || answer.get("error").is_some(),
    )
}

/// The person's logged-in computer, over the one session its device holds,
/// asks the server to make tasks as the person: each is done once the
/// workspace took it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computers_own_tasks_are_made_and_answered_done() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let node = enrol(&root, "home-server", DeviceCapabilities::member());
    let tab_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &node, state.path()).await;
    let tab = Browser::open(&hosted, &tab_device, &node).await;
    let device = Browser::open(&hosted, &computer, &node).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let session = SessionActions::new(&device.peer, device.workspace, node.cert.device_public_key);
    let mcp = McpServer::new(session, WORKSPACE.into(), person);
    for title in ["First", "Second", "Third"] {
        let mut answer = tool(
            &mcp,
            "tilo_do",
            json!({"path": "tasks#create", "args": {"title": title}}),
        )
        .await;
        // A call answered while its change is on its way is followed to how it ended.
        let deadline = tilo_test_env::hang_deadline();
        while let Some(invocation) = answer
            .0
            .strip_prefix("pending ")
            .and_then(|rest| rest.split(':').next())
            .map(str::to_owned)
        {
            assert!(
                Instant::now() < deadline,
                "{title} never settled: {answer:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
            answer = tool(&mcp, "tilo_status", json!({"invocation_id": invocation})).await;
        }
        let (answer, failed) = answer;
        assert!(!failed, "tasks#create {title} as the person: {answer}");
    }
    drop(tab);
}

/// A viewer's own device writes to a resource: the workspace refuses it, and
/// the device's wait for its writes to be saved ends failed, with why,
/// whether the refusal came before or after it was asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_the_workspace_refuses_is_never_answered_saved() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (viewer, _) = tilo_user_identity::UserIdentity::generate("Vic", 1);
    let node = enrol(&owner, "node", DeviceCapabilities::member());
    let viewer_device = enrol(&viewer, "page", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &node, state.path()).await;
    let workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Read here, written elsewhere")
        .await
        .unwrap();
    admit_person(
        &hosted,
        &node,
        &workspace.workspace,
        &viewer_device,
        Role::Viewer,
    )
    .await;
    let mut page = open_admitted_peer(&hosted, &viewer_device, &node, &workspace.workspace).await;
    // Asked at once: the write may still be held for its keys.
    page.create(TASKS);
    let saved = page.peer.barrier().unwrap().recv().unwrap();
    let refusal = saved.expect_err("a viewer's new resource is refused, never saved");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );

    // Asked once the workspace refused it: the write left the device before
    // the wait for it began, and the wait still hears it was refused.
    page.pump();
    let route = page.create(NOTES);
    let deadline = tilo_test_env::hang_deadline();
    'refused: loop {
        for event in page.events.drain() {
            if matches!(&event, tilo_session_client::Event::Refused { route: of, .. } if *of == route)
            {
                break 'refused;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never refused the write"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let saved = page.peer.barrier().unwrap().recv().unwrap();
    let refusal = saved.expect_err("a refused write is never answered saved");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );

    // It was answered once: what the device writes after it is not refused for it.
    let after = page.peer.barrier().unwrap().recv().unwrap();
    assert_eq!(after, Ok(()), "nothing was written since");
}

/// What `path` with `args` ended as, over the API's MCP: a pending answer is
/// followed with `tilo_status`, as the SDKs follow it.
async fn ended(mcp: &mut super::api::Mcp<'_>, path: &str, args: Value) -> Value {
    let mut answer = mcp.tilo_do(json!({"path": path, "args": args})).await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let typed = answer["result"]["structuredContent"].clone();
        if typed["status"] != "pending" {
            return answer;
        }
        assert!(Instant::now() < deadline, "{path} never ended: {answer}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        answer = mcp
            .post(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "tilo_status",
                           "arguments": {"invocation_id": typed["invocation_id"]},
                           "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await
            .1;
    }
}

/// A key's grouped call on a server like tilo.sh: the workspace refuses the
/// write of a person who only reads it, and the call is answered failed,
/// saying why, never done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_whose_write_is_refused_is_answered_failed() {
    use super::api::{ISSUER, Mcp};
    use super::api_on_server::{accept_invitation, api_on_server, approved_key, headers, json_of};
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &root, &page, "backend").await;
    let bearer = format!("Bearer {key}");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, made) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            "/v1/workspaces",
            "",
            &headers(&pairs),
            json!({"name": "Imported"}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    // A person who reads the workspace joins it: a key of theirs reaches it as
    // them, and the workspace takes no write of theirs.
    let (status, invited) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            &format!("/v1/workspaces/{workspace}/members/invitations"),
            "",
            &headers(&pairs),
            json!({"role": "viewer"}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert!((200..300).contains(&status), "{invited}");
    let link = invited["link"]
        .as_str()
        .unwrap_or_else(|| panic!("{invited}"))
        .to_owned();
    let (reader, _) = tilo_user_identity::UserIdentity::generate("Vic", 1);
    let (reader_page, joined) = accept_invitation(&on_server, &link, &reader, "Vic").await;
    assert_eq!(joined["role"], "viewer", "{joined}");
    let reader_key = approved_key(&on_server, &reader, &reader_page, "Vic's backend").await;
    // The tracker is open, started by the person who owns it.
    let mut owner = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: key,
        session: None,
    };
    owner.initialize().await;
    ended(&mut owner, "tasks#open", json!({})).await;
    let mut mcp = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: reader_key,
        session: None,
    };
    mcp.initialize().await;
    // The group is a write of theirs: the workspace refuses it, and the call is
    // answered so.
    let many = ended(
        &mut mcp,
        "tasks/Issues#group",
        json!({"actions": [
            {"action": "create", "args": {"title": "Second"}},
            {"action": "create", "args": {"title": "Third"}},
        ]}),
    )
    .await;
    let typed = &many["result"]["structuredContent"];
    assert_ne!(
        typed["status"], "ok",
        "a refused group is never done: {many}"
    );
    assert!(
        many.to_string().contains("permission"),
        "the refusal says why: {many}"
    );
}

/// Two callers write to one workspace over one session: the workspace refuses
/// one's change and takes the other's. Each is answered for its own write:
/// the one taken as saved, even while the other's refusal is on its way, and
/// the refused one as refused, even after the other was answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_caller_hears_only_its_own_refusal() {
    use automerge::transaction::Transactable as _;
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (viewer, _) = tilo_user_identity::UserIdentity::generate("Vic", 1);
    let node = enrol(&owner, "node", DeviceCapabilities::member());
    let viewer_device = enrol(&viewer, "page", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &node, state.path()).await;
    let workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Read here, kept own")
        .await
        .unwrap();
    admit_person(
        &hosted,
        &node,
        &workspace.workspace,
        &viewer_device,
        Role::Viewer,
    )
    .await;
    let mut page = open_admitted_peer(&hosted, &viewer_device, &node, &workspace.workspace).await;

    // One caller makes a new tasks resource, which a viewer may not; the
    // other writes the viewer's own settings, which only they write.
    let refused_route = page.create(TASKS);
    let own = DocumentRoute {
        app_id: "so.tilo.settings".into(),
        cluster_id: workspace.workspace.clone(),
        document: collab_types::OwnResource::new(
            "settings.root",
            viewer.verifying_key().to_bytes(),
        )
        .unwrap()
        .document_id(),
    };
    let mut settings = automerge::AutoCommit::new();
    settings.put(automerge::ROOT, "theme", "dark").unwrap();
    page.peer
        .write(
            own.clone(),
            &collab_types::DocumentMutation::Snapshot(settings.save()),
        )
        .unwrap();

    let taken = page
        .peer
        .barrier_for(std::slice::from_ref(&own))
        .unwrap()
        .recv()
        .unwrap();
    assert_eq!(
        taken,
        Ok(()),
        "the caller whose write was taken hears it saved"
    );
    let refused = page
        .peer
        .barrier_for(std::slice::from_ref(&refused_route))
        .unwrap()
        .recv()
        .unwrap();
    let refusal = refused.expect_err("the caller whose write was refused hears it refused");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );
}
