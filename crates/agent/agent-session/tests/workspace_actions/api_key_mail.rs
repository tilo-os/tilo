//! A key a person approved is their agent in the workspace: it lists the workspace's accounts
//! through `tilo.accounts` and reads its messages through the API, as what an agent of theirs
//! may. A key the workspace holds no grant of is refused, and told nothing of its mail.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{OnServer, api_on_server, approved_key};
use super::api_v1::{Holder, own_workspace};
use super::*;
use serde_json::{Value, json};

/// The account the workspace keeps.
const ACCOUNT: &str = "ada@example.test";
/// Where `tilo migrate` delivers a message (thunderbird.rs `DELIVER`), as resource and action.
const MESSAGES: &str = "mail/mail.messages";
const DELIVER: &str = "deliver-message";

/// A client reaching the API with the key `token` carries, as `tilo keys export` writes it.
fn keyed(on_server: &OnServer, token: &str, name: &str) -> tilo_api::client::Client {
    let now_ms = tilo_clock::unix_now_ms();
    let key = tilo_api::store::Store::in_memory()
        .access(token, now_ms)
        .expect("the token carries its grant")
        .key()
        .expect("and the grant its key");
    let file = tilo_api::direct::KeyFile::new(
        &on_server.server,
        Some(&on_server.web_entry),
        name,
        key.key(),
        key.cert(),
    );
    let keys = tempfile::tempdir().unwrap();
    let path = keys.path().join("key.json");
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    tilo_api::client::Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&path),
        json: false,
    })
    .unwrap()
}

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

/// Whether `reply` is the API's `tilo_read` answer, as opposed to its refusal.
fn read_ok(reply: &Value) -> bool {
    reply.get("error").is_none()
        && reply["result"]["isError"].as_bool() != Some(true)
        && reply["result"]["structuredContent"]["status"] == "ok"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_mail_grant_lets_it_list_the_workspaces_accounts_and_read_its_messages() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    // The workspace keeps an account, and holds a message Mail brought in.
    workspaces
        .keep_mail_account_as_added(&workspace, ACCOUNT)
        .await
        .unwrap();
    let holder = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;
    let delivered = holder
        .command()
        .act_invoked(
            &workspace,
            MESSAGES,
            DELIVER,
            &json!({
                "from": "Dana Ruiz <dana@example.test>",
                "to": ACCOUNT,
                "subject": "Offsite venue",
                "body": "Please book the harbour venue for 14 people.",
                "date": "2026-10-12T09:00:00Z",
                "folder": "inbox",
                "read": false,
            }),
            "migrate-thunderbird",
            "Bring Thunderbird in",
            "migrate-thunderbird-0",
        )
        .await
        .unwrap_or_else(|error| panic!("{MESSAGES}#{DELIVER}: {error:#}"));
    assert_eq!(delivered["status"], "ok", "{delivered}");

    // A key Ada approved for her backend: her agent in the workspace, and what an agent of hers
    // may there.
    let mail = approved_key(&on_server, &ada, &page, "Mail reader").await;

    // It lists the workspace's accounts through `tilo.accounts`, and reads the workspace's mail.
    let (status, listed) = read(
        &on_server,
        &mail,
        &workspace,
        tilo_api::session::mail_accounts::ROOT,
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        read_ok(&listed),
        "the key reaches the workspace's mail: {listed}"
    );
    assert!(
        listed.to_string().contains(ACCOUNT),
        "the account the workspace keeps: {listed}"
    );
    let messages = keyed(&on_server, &mail, "Mail reader")
        .get(&format!(
            "/v1/workspaces/{workspace}/resources/{}",
            tilo_api::client::encode_path("mail/Messages")
        ))
        .await
        .unwrap_or_else(|error| panic!("read mail/Messages: {error:#}"));
    assert!(
        messages.to_string().contains("Offsite venue"),
        "the message the workspace holds: {messages}"
    );

    // A key of another person's, which this workspace holds no grant of, is refused, and told
    // nothing of the workspace's mail.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let bo_page = enrol(&bo, "Bo's browser", DeviceCapabilities::member());
    let theirs = approved_key(&on_server, &bo, &bo_page, "Bo's backend").await;
    let (status, refused) = read(
        &on_server,
        &theirs,
        &workspace,
        tilo_api::session::mail_accounts::ROOT,
    )
    .await;
    assert_eq!(status, 200, "{refused}");
    assert!(
        !read_ok(&refused),
        "a key the workspace holds no grant of is refused: {refused}"
    );
    assert!(
        !refused.to_string().contains(ACCOUNT),
        "nothing of the workspace's mail: {refused}"
    );
    let refused = keyed(&on_server, &theirs, "Bo's backend")
        .get(&format!(
            "/v1/workspaces/{workspace}/resources/{}",
            tilo_api::client::encode_path("mail/Messages")
        ))
        .await;
    assert!(
        match &refused {
            Err(_) => true,
            Ok(read) => !read.to_string().contains("Offsite venue"),
        },
        "a key the workspace holds no grant of reads no mail: {refused:?}"
    );
}
