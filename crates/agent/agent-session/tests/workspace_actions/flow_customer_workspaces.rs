//! Flow 1 of docs/architecture/flows.md: a workspace per customer on tilo.sh.
//!
//! A company keeps one workspace for each of its customers. Its backend makes them through
//! the action API, a few files at a time, finds each again by the id it labels it with, and
//! gives each customer's agent a key that reaches that one workspace and nothing else. The
//! customer's agent, an MCP client outside Tilo, runs an action and reads the view the app
//! shows there, and is refused on its neighbour's workspace and on the account.
//!
//! Two steps are red until their fixes land: workspaces cost nothing at rest
//! (`workspaces_at_rest_cost_nothing`) and the book explains the SDK and the
//! MCP tools (`the_docs_explain_the_sdk_and_mcp`). The flow's VM-only steps (a
//! hosted Browser, a command in a VM) run in KVM and are proven in
//! os/containers/tilo-sh/check.sh.
//!
//! The tests reuse the helpers of `workspace_per_customer`, which drives the same server.
use super::api::{ISSUER, upload};
use super::api_on_server::{OnServer, api_on_server, approved_key};
use super::workspace_per_customer::{
    account_action, call, done, listed, mcp, rest, server_key, work,
};
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_api::Server;

/// How many customers this backend serves here: enough that each is made, labelled, found and
/// worked in side by side on the one server. The scale the flow names (10,000) is the service's
/// own question, asked in `workspaces_at_rest_cost_nothing` and in the throughput suite.
const CUSTOMERS: usize = 8;

/// The workspaces the idle test keeps. One account call each, so the count is paid for once;
/// 100 already fails a boot that opens every kept workspace, and the property is per workspace.
const IDLE_WORKSPACES: usize = 100;

/// `name` of `label` as a workspace id, made with one account call labelled `label`.
async fn made(api: &Arc<Server>, key: &str, name: &str, label: &str) -> String {
    let (status, made) = rest(
        api,
        key,
        hyper::Method::POST,
        &account_action("workspaces#create"),
        json!({"name": name, "labels": {"customer_id": label, "batch": "flow"}})
            .to_string()
            .as_bytes(),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{made}");
    made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A key scoped to `workspace`, minted with one account call: its token and where its agent
/// reaches it over MCP.
async fn minted(api: &Arc<Server>, key: &str, workspace: &str) -> Value {
    let (status, minted) = rest(
        api,
        key,
        hyper::Method::POST,
        &account_action("keys#mint"),
        json!({"workspace": workspace}).to_string().as_bytes(),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{minted}");
    minted["result"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_customer_workspaces_are_made_with_one_call_each() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;

    // Every customer's workspace at once, each labelled with the backend's own id for them.
    let mut making = tokio::task::JoinSet::new();
    for customer in 0..CUSTOMERS {
        let (api, key) = (Arc::clone(&api), key.clone());
        making.spawn(async move {
            let label = format!("cus_{customer}");
            let workspace = made(&api, &key, &format!("Applicant {customer}"), &label).await;
            (customer, workspace)
        });
    }
    let mut workspaces = vec![String::new(); CUSTOMERS];
    while let Some(made) = making.join_next().await {
        let (customer, workspace) = made.unwrap();
        workspaces[customer] = workspace;
    }

    // The batch lists whole, and each id finds its one workspace and no other.
    let batch = listed(&api, &key, &json!({"batch": "flow"})).await;
    assert_eq!(batch.len(), CUSTOMERS, "{batch:?}");
    for (customer, workspace) in workspaces.iter().enumerate() {
        let found = listed(
            &api,
            &key,
            &json!({ "customer_id": format!("cus_{customer}") }),
        )
        .await;
        assert_eq!(found, std::slice::from_ref(workspace));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_customers_files_are_put_and_read_with_its_workspace() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;
    let workspace = made(&api, &key, "Applicant 0", "cus_0").await;

    // A few files go in, each by the address of its own bytes, and read back through the
    // workspace: the byte routes the SDK and `tilo files put` send a folder through.
    let files = ["passport scan\n", "cover letter\n"];
    let account = mcp(&api, "/mcp", &key);
    for text in files {
        let (status, kept) = upload(&account, &workspace, text.as_bytes()).await;
        assert!((200..300).contains(&status), "{kept}");
        // The workspace names the bytes back by the content it keeps them as, sealed under its
        // own key; the read names that reference.
        let kept = kept["content"]
            .as_str()
            .unwrap_or_else(|| panic!("the put answers its content: {kept}"));
        let reference = kept
            .strip_prefix("tilo.content:")
            .unwrap_or_else(|| panic!("the put answers a content reference: {kept}"));
        let (status, read) = rest(
            &api,
            &key,
            hyper::Method::GET,
            &format!("/v1/workspaces/{workspace}/contents/{reference}"),
            b"",
            &[],
        )
        .await;
        assert_eq!(status, 200, "{read}");
        assert_eq!(read.as_str(), Some(text), "the bytes come back: {read}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_and_its_key_are_minted_with_one_call() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;
    let workspace = made(&api, &key, "Applicant 0", "cus_0").await;

    // One call gives the customer's agent its key and the MCP endpoint it reaches.
    let key = minted(&api, &key, &workspace).await;
    assert_eq!(key["workspace"], workspace.as_str(), "{key}");
    assert_eq!(
        key["mcp_url"],
        format!("{ISSUER}/w/{workspace}/mcp").as_str(),
        "{key}"
    );
    assert!(
        key["token"].as_str().is_some_and(|token| !token.is_empty()),
        "{key}"
    );
    // It reaches its own workspace and nothing else.
    let own = listed(&api, key["token"].as_str().unwrap(), &json!({})).await;
    assert_eq!(own, [workspace]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outside_agent_runs_an_action_and_reads_a_view() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;
    let workspace = made(&api, &key, "Applicant 0", "cus_0").await;
    let key = minted(&api, &key, &workspace).await;
    let token = key["token"].as_str().unwrap();
    let path = key["mcp_url"]
        .as_str()
        .unwrap()
        .strip_prefix(ISSUER)
        .unwrap();

    // The customer's agent, an MCP client outside Tilo, works in its workspace: an issue
    // written through an action, then the view the app shows of it.
    let mut agent = mcp(&api, path, token);
    assert_eq!(agent.initialize().await.0, 200);
    done(
        &mut agent,
        "tasks#create",
        json!({"title": "Checklist of cus_0", "body": "- passport\n"}),
    )
    .await;
    let (status, viewed) = call(&mut agent, "tilo_view", json!({"path": "tasks"})).await;
    assert_eq!(status, 200, "{viewed}");
    assert_eq!(viewed["result"]["isError"], false, "{viewed}");
    assert!(
        viewed.to_string().contains("Checklist of cus_0"),
        "the view shows the issue the agent wrote: {viewed}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_is_refused_on_a_neighbouring_workspace_and_the_account() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;
    let a = made(&api, &key, "Applicant A", "cus_a").await;
    let b = made(&api, &key, "Applicant B", "cus_b").await;
    let a_key = minted(&api, &key, &a).await;
    let b_key = minted(&api, &key, &b).await;
    let a_token = a_key["token"].as_str().unwrap();
    // B's agent writes a page no one else may read.
    let secret = "Checklist of cus_b, sealed";
    work(
        &api,
        b_key["mcp_url"].as_str().unwrap(),
        b_key["token"].as_str().unwrap(),
        secret,
    )
    .await;

    // A's key is turned away at B's MCP URL before any session opens.
    let mut at_b = mcp(&api, &format!("/w/{b}/mcp"), a_token);
    let (status, refused) = at_b.initialize().await;
    assert_eq!(status, 403, "{refused}");
    assert!(at_b.session.is_none());

    // At its own endpoint it reaches its own workspace and the account's read of it, and
    // nothing more of the account.
    let mut a_agent = mcp(&api, &format!("/w/{a}/mcp"), a_token);
    assert_eq!(a_agent.initialize().await.0, 200);
    let own = a_agent
        .tilo_do(json!({"path": "account/workspaces#list", "args": {}}))
        .await;
    let rows = &own["result"]["structuredContent"]["result"]["workspaces"];
    assert_eq!(rows.as_array().map(Vec::len), Some(1), "{own}");
    assert_eq!(rows[0]["id"], a.as_str(), "{own}");
    for (action, args) in [
        ("account/workspaces#create", json!({"name": "cus_c"})),
        ("account/keys#mint", json!({"workspace": b})),
        ("account/keys#revoke", json!({"grant": b_key["grant"]})),
    ] {
        let answer = a_agent.tilo_do(json!({"path": action, "args": args})).await;
        assert_ne!(
            answer["result"]["structuredContent"]["status"], "ok",
            "{action} ran for A's key: {answer}"
        );
    }

    // A `/v1` route naming B's workspace, or the account, is beyond its scope.
    let (status, beyond) = rest(
        &api,
        a_token,
        hyper::Method::POST,
        &account_action("workspaces#create"),
        json!({"name": "cus_c"}).to_string().as_bytes(),
        &[],
    )
    .await;
    assert!(
        status >= 400,
        "the account is beyond A's key: {status} {beyond}"
    );
    let (status, read) = rest(
        &api,
        a_token,
        hyper::Method::GET,
        &format!("/v1/workspaces/{b}/resources/tasks/Issues"),
        b"",
        &[],
    )
    .await;
    assert!(
        status == 403 || !read.to_string().contains(secret),
        "B's page is not read through A's key: {status} {read}"
    );
}

/// How long a workspace may sit idle before the service unloads it. The service keeps no unload
/// time yet; this test's own is what it waits, and `TILO_SERVER_IDLE_UNLOAD_SECS` shortens it.
fn idle_unload() -> Duration {
    std::env::var("TILO_SERVER_IDLE_UNLOAD_SECS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(Duration::from_secs(30), Duration::from_secs)
}

/// Workspaces at rest cost almost nothing: opening the service serves no idle workspace, and one
/// that sat past the unload time leaves memory and is opened again by the next call.
///
/// Red today: `open_kept` (crates/server/tilo-workspace-service/src/workspaces/member.rs) opens
/// every kept workspace at boot, and `evict` (`workspace_io.rs`) runs only when one is deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspaces_at_rest_cost_nothing() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    // The same person before and after the restart, so their workspaces are theirs again.
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &root, &page, "Visa backend").await;
    let mut making = tokio::task::JoinSet::new();
    for customer in 0..IDLE_WORKSPACES {
        let (api, key) = (Arc::clone(&api), key.clone());
        making.spawn(async move {
            made(
                &api,
                &key,
                &format!("Customer {customer}"),
                &format!("cus_{customer}"),
            )
            .await
        });
    }
    let mut workspaces = Vec::with_capacity(IDLE_WORKSPACES);
    while let Some(made) = making.join_next().await {
        workspaces.push(made.unwrap());
    }

    // The service is opened again on the same state, as a deployment's restart is. The resident
    // set is what it opened: it is also what the server's published status answers as its
    // `holding`, whose read is `Workspaces::list` (crates/server/tilo-server/src/operations.rs).
    // No published view exposes residency any closer than that.
    let boot = Instant::now();
    let on_server = on_server.restart(false).await;
    let boot = boot.elapsed();
    let service = on_server.hosted.server.workspaces().unwrap();
    let resident = service.list().await;
    eprintln!(
        "boot with {} kept workspaces: {boot:?}, {} resident",
        IDLE_WORKSPACES + 1,
        resident.len()
    );
    assert!(
        resident.len() <= 2,
        "a boot opens no idle workspace: {} of {} kept workspaces are resident after {boot:?} \
         (open_kept opens them all)",
        resident.len(),
        IDLE_WORKSPACES + 1
    );

    let key = approved_key(&on_server, &root, &page, "Visa backend").await;
    // One customer's workspace is read: it opens for the call, and only it.
    let touched = &workspaces[0];
    assert_eq!(
        listed(&api_of(&on_server), &key, &json!({"customer_id": "cus_0"})).await,
        std::slice::from_ref(touched)
    );

    // Idle past the unload time, it leaves memory; its files stay, and the next call opens it
    // again. Its follower tasks and agent tick stop with the runtime it held.
    tokio::time::sleep(idle_unload() + Duration::from_secs(5)).await;
    let resident = service_ids(&on_server).await;
    assert!(
        !resident.contains(touched),
        "a workspace idle past {:?} is unloaded (evict runs only on delete): {resident:?}",
        idle_unload()
    );
    assert_eq!(
        listed(&api_of(&on_server), &key, &json!({"customer_id": "cus_0"})).await,
        std::slice::from_ref(touched)
    );
    assert!(
        service_ids(&on_server).await.contains(touched),
        "the next call opens it again: {touched}"
    );
}

/// The action API of `on_server`'s server.
fn api_of(on_server: &OnServer) -> Arc<Server> {
    Arc::clone(&on_server.api)
}

/// The ids of the workspaces `on_server`'s service holds resident now.
async fn service_ids(on_server: &OnServer) -> Vec<String> {
    on_server
        .hosted
        .server
        .workspaces()
        .unwrap()
        .list()
        .await
        .into_iter()
        .map(|summary| summary.workspace)
        .collect()
}

/// The tilo.sh book's page list: the site's build (tools/docs/build-site.sh) exports what it lists,
/// and llms.txt is made from that export.
const PAGES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/tilo-sh/pages.toml"
);

/// The published book explains the SDK and the MCP tools: tilo.sh's pages carry one-workspace-per-
/// customer and the API, and every MCP tool says what it does.
///
/// Red today: neither page is in docs/tilo-sh/pages.toml.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_docs_explain_the_sdk_and_mcp() {
    let book = std::fs::read_to_string(PAGES).expect("the tilo.sh book lists its pages");
    assert!(
        book.contains("docs/developers/one-workspace-per-customer.md"),
        "the book carries a workspace per customer"
    );
    assert!(
        book.contains("docs/developers/api.md"),
        "the book carries the API page"
    );

    // Every tool the workspace's agent is offered says what it does.
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let key = server_key(&on_server, "Ada").await;
    let workspace = made(&api, &key, "Applicant 0", "cus_0").await;
    let key = minted(&api, &key, &workspace).await;
    let mut agent = mcp(
        &api,
        &format!("/w/{workspace}/mcp"),
        key["token"].as_str().unwrap(),
    );
    assert_eq!(agent.initialize().await.0, 200);
    let (status, listed) = agent
        .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    assert_eq!(status, 200, "{listed}");
    let tools = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("the endpoint lists its tools: {listed}"));
    assert!(
        tools.len() > 10,
        "the workspace's tools are listed: {listed}"
    );
    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("?");
        let description = tool["description"].as_str().unwrap_or_default();
        assert!(
            !description.trim().is_empty(),
            "{name} says what it does: {tool}"
        );
    }
}
