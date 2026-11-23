//! A write is answered as the workspace decided it: one it takes is done, and
//! one it refuses is answered failed, with why, never as saved. Asked of the
//! server by a person's logged-in computer over its session, and written by a
//! device itself.
use super::agent_mcp::tool;
use super::api_on_server::OnServer;
use super::resource_support::{admit_person, open_admitted_peer};
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_peer_state::join::Role;
use tilo_session_actions::SessionActions;

const TASKS: &str = "so.tilo.tasks";
const CALENDAR: &str = "so.tilo.calendar";

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
    let saved = crate::workspace_actions::saved(&page.peer.barrier().unwrap());
    let refusal = saved.expect_err("a viewer's new resource is refused, never saved");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );

    // Asked once the workspace refused it: the write left the device before
    // the wait for it began, and the wait still hears it was refused.
    page.pump();
    let route = page.create(CALENDAR);
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
    let saved = crate::workspace_actions::saved(&page.peer.barrier().unwrap());
    let refusal = saved.expect_err("a refused write is never answered saved");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );

    // It was answered once: what the device writes after it is not refused for it.
    let after = crate::workspace_actions::saved(&page.peer.barrier().unwrap());
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
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let owner_page = enrol(&owner, "Ada's browser", DeviceCapabilities::member());
    let owner_key = approved_key(&on_server, &owner, &owner_page, "backend").await;
    let bearer = format!("Bearer {owner_key}");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, made) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            "/v1/resources/account/workspaces/actions/create",
            "",
            &headers(&pairs),
            json!({"name": "Imported"}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A person who reads the workspace joins it: a key of theirs reaches it as
    // them, and the workspace takes no write of theirs.
    let (status, invited) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            &format!("/v1/workspaces/{workspace}/resources/workspace/invitations/actions/invite"),
            "",
            &headers(&pairs),
            json!({"role": "viewer"}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert!((200..300).contains(&status), "{invited}");
    let link = invited["result"]["link"]
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
        token: owner_key,
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
    // Both writes of the grouped call are theirs, and the workspace offers a
    // person who only reads it none of them: the first step is refused, for
    // their role, and the whole call is answered so.
    let many = mcp
        .tilo_do(json!({"calls": [
            {"path": "tasks#create", "args": {"title": "Second"}},
            {"path": "tasks#create", "args": {"title": "Third"}},
        ]}))
        .await;
    let typed = &many["result"]["structuredContent"];
    assert_eq!(
        typed["status"], "failed",
        "a refused group is never done: {many}"
    );
    assert!(
        many.to_string().contains("not_permitted"),
        "the refusal says why: {many}"
    );
    // Nothing of theirs reached the tracker, while the same writes the person
    // who owns it makes are taken: what was refused is whose they were.
    let held = owner.tilo_read(json!({"path": "tasks/Issues"})).await;
    assert!(
        !held.to_string().contains("Second"),
        "the refused group wrote nothing: {held}"
    );
    for title in ["Second", "Third"] {
        let made = ended(&mut owner, "tasks#create", json!({"title": title})).await;
        assert_eq!(
            made["result"]["structuredContent"]["status"], "ok",
            "the owner's {title}: {made}"
        );
    }
}

/// Two callers write to one workspace over one session: the workspace refuses
/// one's change and takes the other's. Each is answered for its own write:
/// the one taken as saved, even while the other's refusal is on its way, and
/// the refused one as refused, even after the other was answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_caller_hears_only_its_own_refusal() {
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
    // other writes the viewer's own view of the workspace's layout, which a
    // member writes whatever their role and nobody writes for them.
    let refused_route = page.create(TASKS);
    let own = DocumentRoute {
        app_id: tilo_desktop_model::ownership::DESKTOP_APP.into(),
        cluster_id: workspace.workspace.clone(),
        document: tilo_desktop_model::person_view::resource_of(&hex::encode(
            viewer.verifying_key().to_bytes(),
        )),
    };
    let mut view = automerge::AutoCommit::new();
    tilo_desktop_model::person_view::PersonView::default()
        .write(&mut view)
        .unwrap();
    page.peer
        .write(
            own.clone(),
            &collab_types::DocumentMutation::Snapshot(view.save()),
        )
        .unwrap();

    let taken = crate::workspace_actions::saved(
        &page.peer.barrier_for(std::slice::from_ref(&own)).unwrap(),
    );
    assert_eq!(
        taken,
        Ok(()),
        "the caller whose write was taken hears it saved"
    );
    let refused = crate::workspace_actions::saved(
        &page
            .peer
            .barrier_for(std::slice::from_ref(&refused_route))
            .unwrap(),
    );
    let refusal = refused.expect_err("the caller whose write was refused hears it refused");
    assert!(
        refusal.contains("not granted"),
        "the refusal says why: {refusal}"
    );
}

/// A member's key takes exactly what their role lets them, in a workspace
/// someone else owns: a Viewer reads the issues and writes nothing, a
/// Commenter comments on one and creates none. What the role refuses is
/// answered refused for it, never as no such action.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_key_takes_what_its_role_lets_it() {
    use super::api::{ISSUER, Mcp};
    use super::api_on_server::{api_on_server, approved_key, headers, json_of};
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let owner_page = enrol(&owner, "Ada's browser", DeviceCapabilities::member());
    let owner_key = approved_key(&on_server, &owner, &owner_page, "backend").await;
    let bearer = format!("Bearer {owner_key}");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, made) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            "/v1/resources/account/workspaces/actions/create",
            "",
            &headers(&pairs),
            json!({"name": "Shared"}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut owner_mcp = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: owner_key,
        session: None,
    };
    owner_mcp.initialize().await;
    let first = ended(&mut owner_mcp, "tasks#create", json!({"title": "First"})).await;
    assert_eq!(
        first["result"]["structuredContent"]["status"], "ok",
        "{first}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let issue = loop {
        if let Some(issue) = issues_kept(&on_server, &workspace).await.pop() {
            break issue;
        }
        assert!(Instant::now() < deadline, "the server never kept the issue");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let refused = |answer: &Value| {
        answer["result"]["structuredContent"]["status"] != "ok"
            && answer.to_string().contains("not_permitted")
    };
    for (role, name, comments) in [(Role::Viewer, "Vic", false), (Role::Commenter, "Cyd", true)] {
        let (person, _) = tilo_user_identity::UserIdentity::generate(name, 1);
        let page = enrol(
            &person,
            &format!("{name}'s browser"),
            DeviceCapabilities::member(),
        );
        admit_person(&on_server.hosted, &on_server.host, &workspace, &page, role).await;
        let key = approved_key(&on_server, &person, &page, "backend").await;
        let mut mcp = Mcp {
            api: &api,
            path: format!("/w/{workspace}/mcp"),
            token: key,
            session: None,
        };
        mcp.initialize().await;

        let read = ended(&mut mcp, "tasks#read", json!({})).await;
        assert_eq!(
            read["result"]["structuredContent"]["status"], "ok",
            "{name} reads the issues: {read}"
        );
        assert!(read.to_string().contains("First"), "{name}: {read}");

        let created = ended(&mut mcp, "tasks#create", json!({"title": "Not theirs"})).await;
        assert!(refused(&created), "{name} creates no issue: {created}");
        // A name the app has no action by is told so, and where they are listed.
        let typo = ended(&mut mcp, "tasks#reed", json!({})).await;
        assert!(
            !refused(&typo)
                && typo.to_string().contains("has no action reed")
                && typo.to_string().contains("tilo_ls tasks lists them"),
            "{name} hears that tasks has no such action: {typo}"
        );

        let said = ended(
            &mut mcp,
            "tasks#comment",
            json!({"issue": issue, "text": format!("Seen by {name}")}),
        )
        .await;
        if comments {
            assert_eq!(
                said["result"]["structuredContent"]["status"], "ok",
                "{name} comments: {said}"
            );
        } else {
            assert!(refused(&said), "{name} comments on nothing: {said}");
        }
    }
}

/// The ids of the issues of `workspace` its server keeps.
async fn issues_kept(on_server: &OnServer, workspace: &str) -> Vec<String> {
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let mut issues = Vec::new();
    for index in 0.. {
        let part_id = tasks::issue_list::chain().part(index);
        let Some(list) = workspaces
            .resource_document(workspace, "so.tilo.tasks", part_id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .and_then(|doc| {
                <tasks::issue_list::IssueList as ui::tilo_resource::Resource>::read(&doc).ok()
            })
        else {
            break;
        };
        issues.extend(list.issues.iter().map(|(id, _)| id.to_string()));
    }
    issues
}
