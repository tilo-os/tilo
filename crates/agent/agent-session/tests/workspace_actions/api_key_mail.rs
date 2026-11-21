//! A key a person approved is their agent in the workspace: it lists the workspace's accounts
//! at `workspace/mail-accounts`, as what an agent of theirs may. A key the workspace holds no grant
//! of is refused, and told nothing of its mail.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{OnServer, api_on_server, approved_key};
use super::api_v1::own_workspace;
use super::*;
use serde_json::{Value, json};

/// The account the workspace keeps.
const ACCOUNT: &str = "ada@example.test";

/// A key of `token`'s, at `path` in the workspace `workspace`, as the API's MCP session reads
/// it: the status of its `tilo_read` and what the API answered.
async fn read(on_server: &OnServer, token: &str, workspace: &str, path: &str) -> (u16, Value) {
    let mut session = Mcp {
        api: &on_server.api,
        path: "/mcp".into(),
        token: token.to_owned(),
        session: None,
    };
    let (status, init) = session.initialize().await;
    assert_eq!(status, 200, "{init}");
    let reply = session
        .tilo_read(json!({"path": path, "workspace": workspace}))
        .await;
    (status, reply)
}

/// What `tilo_ls` of `path` in `workspace` lists for a key of `token`'s, as text.
async fn listed(on_server: &OnServer, token: &str, workspace: &str, path: &str) -> String {
    let mut session = Mcp {
        api: &on_server.api,
        path: "/mcp".into(),
        token: token.to_owned(),
        session: None,
    };
    let (status, init) = session.initialize().await;
    assert_eq!(status, 200, "{init}");
    session
        .tilo_ls(json!({"path": path, "workspace": workspace}))
        .await
        .to_string()
}

/// Whether `reply` is the API's `tilo_read` answer, as opposed to its refusal.
fn read_ok(reply: &Value) -> bool {
    reply.get("error").is_none()
        && reply["result"]["isError"].as_bool() == Some(false)
        && reply["result"]["structuredContent"]["fields"].is_object()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_mail_grant_lets_it_list_the_workspaces_accounts() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    // The workspace keeps an account.
    workspaces
        .keep_mail_account_as_added(&workspace, ACCOUNT)
        .await
        .unwrap();

    // A key Ada approved for her backend: her agent in the workspace, and what an agent of hers
    // may there.
    let mail = approved_key(&on_server, &ada, &page, "Mail reader").await;

    // The workspace's published tree holds them beside its apps: `workspace` at the root, and
    // in it the accounts.
    let root = listed(&on_server, &mail, &workspace, "").await;
    assert!(
        root.contains("\\nworkspace "),
        "the workspace beside its apps: {root}"
    );
    let governance = listed(&on_server, &mail, &workspace, "workspace").await;
    assert!(
        governance.contains("Mail accounts"),
        "its mail accounts in the workspace's tree: {governance}"
    );

    // It lists the workspace's accounts at `workspace/mail-accounts`.
    let (status, listed) = read(&on_server, &mail, &workspace, "workspace/mail-accounts").await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        read_ok(&listed),
        "the key reaches the workspace's mail: {listed}"
    );
    assert!(
        listed.to_string().contains(ACCOUNT),
        "the account the workspace keeps: {listed}"
    );

    // A key of another person's, which this workspace holds no grant of, is refused, and told
    // nothing of the workspace's mail.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let bo_page = enrol(&bo, "Bo's browser", DeviceCapabilities::member());
    let theirs = approved_key(&on_server, &bo, &bo_page, "Bo's backend").await;
    let (status, refused) = read(&on_server, &theirs, &workspace, "workspace/mail-accounts").await;
    assert_eq!(status, 200, "{refused}");
    assert!(
        !read_ok(&refused),
        "a key the workspace holds no grant of is refused: {refused}"
    );
    assert!(
        !refused.to_string().contains(ACCOUNT),
        "nothing of the workspace's mail: {refused}"
    );
}
