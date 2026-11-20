//! A person's page opens Chat after an agent's session joined the workspace:
//! the agent lists the chat, posts into it, and the page sees what it said.
//!
//! The device serving a workspace serves its actions, and is replayed the
//! workspace when it joins: a resource made after that — Chat's own, with its
//! channels — is sent to it only once it asks for it. Chat's messages are a
//! chain of parts, and a chain no device wrote in yet has no part for a replay
//! to send: the workspace is asked for the chain's head, and its answer that it
//! holds none is what lets a post start the first part rather than wait for one
//! for ever.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{api_on_server, approved_key};
use super::*;
use serde_json::json;
use std::time::{Duration, Instant};

const CHAT: &str = "so.tilo.chat";
/// What the agent says in the chat.
const TEXT: &str = "Hello from the agent";
/// The channel every chat opens with.
const CHANNEL: &str = "c_chat";

/// The document of part 0 of Chat's messages in `workspace`.
fn first_part(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.to_owned(),
        document: chat::parts::part(0).as_str().to_owned(),
    }
}

/// What `page` holds of the messages `part` keeps, oldest first.
fn held_texts(page: &mut Browser, part: &DocumentRoute) -> Vec<String> {
    page.pump();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in page.documents.get(part).cloned().unwrap_or_default() {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &bytes);
    }
    chat::messages::read_part(&doc)
        .messages
        .iter()
        .map(|(_, message)| message.text.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_posts_to_chat_a_page_made_after_it_joined() {
    let on_server = api_on_server(ISSUER).await;
    let lobby = on_server.lobby.clone();
    let operator = on_server.operator.clone();
    // A person the site's home workspace lets in, whose key the action API
    // approves as their own, as a terminal asks for one.
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let approver = enrol(&ada, "browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &approver, "Claude Code").await;
    let mut agent = Mcp {
        api: &on_server.api,
        path: format!("/w/{lobby}/mcp"),
        token: key,
        session: None,
    };
    let (status, initialized) = agent.initialize().await;
    assert_eq!(status, 200, "{initialized}");
    // A first call makes the workspace's actions join the workspace, before
    // Chat exists there: a listing writes nothing, and nothing is held of the chat.
    let _ = agent
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "tilo_ls", "arguments": {"path": "chat"},
                       "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await;

    // The person's page opens Chat in the workspace: the app's own resource with
    // its first channels, made after that device's actions joined. The page is
    // the server's own person's, whose workspace the Lounge is.
    let page_device = enrol(&operator, "the page", DeviceCapabilities::member());
    let mut page =
        Browser::open_workspace(&on_server.hosted, &page_device, &on_server.host, &lobby).await;
    let _chat = page.create(CHAT);
    page.peer.barrier().unwrap().recv().unwrap().unwrap();

    // Listing the chat reaches the resource the page made.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (status, listed) = agent
            .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "tilo_ls", "arguments": {"path": "chat"},
                           "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await;
        assert_eq!(status, 200, "{listed}");
        if !listed["result"]["isError"].as_bool().unwrap_or(false) {
            // The page made the chat's resource with the first channel every
            // chat opens with; a listing that shows it read that resource.
            assert!(
                listed["result"]["structuredContent"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("c_chat"),
                "the listing shows the channel the page's chat has: {listed}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the chat the page made is never listed: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Posting into it starts the chat's first messages part. A call answered
    // while the chain's head is on its way is asked again, as the API's
    // callers do.
    let mut posted = agent
        .tilo_do(json!({"path": "chat#post",
        "args": {"channel": CHANNEL, "text": TEXT}}))
        .await;
    while posted["result"]["structuredContent"]["status"] != "ok" {
        assert_eq!(
            posted["result"]["structuredContent"]["error"]["code"], "not_ready",
            "the post failed for another reason: {posted}"
        );
        assert!(Instant::now() < deadline, "the post never ran: {posted}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        posted = agent
            .tilo_do(json!({"path": "chat#post", "args": {"channel": CHANNEL, "text": TEXT}}))
            .await;
    }

    // The page follows the chain and reads what the agent said.
    let part = first_part(&lobby);
    page.follow_chain(CHAT, chat::parts::chain().chain_head().as_str());
    loop {
        page.pump();
        if held_texts(&mut page, &part).iter().any(|said| said == TEXT) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page never held the agent's message"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    page.peer.close();
}

/// A workspace of Ada's own on the server, its Chat written by her page before
/// the key's session exists: the agent lists it and posts into it as her page
/// does, the document having been the workspace's all along.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_posts_to_chat_a_persons_own_workspace_began_with() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page_device = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page_device, "Visa backend").await;
    let mut account = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let made = account
        .did("account/workspaces#create", json!({"name": "Team"}))
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();

    // Her page writes the workspace's Chat and leaves: the document is the
    // workspace's before the key's session enters it.
    {
        let mut page = super::resource_support::open_admitted_peer(
            &on_server.hosted,
            &page_device,
            &on_server.host,
            &workspace,
        )
        .await;
        let _chat = page.create(CHAT);
        page.peer.barrier().unwrap().recv().unwrap().unwrap();
        page.peer.close();
    }

    let minted = account
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);

    // Listing the chat reaches the document the page made.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (status, listed) = agent
            .post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "tilo_ls", "arguments": {"path": "chat"},
                           "_meta": {"so.tilo.mcp": {"structured": true}}}}))
            .await;
        assert_eq!(status, 200, "{listed}");
        if !listed["result"]["isError"].as_bool().unwrap_or(false) {
            // The page made the chat's resource with the first channel every
            // chat opens with; a listing that shows it read that resource.
            assert!(
                listed["result"]["structuredContent"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("c_chat"),
                "the listing shows the channel the page's chat has: {listed}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the chat the page made is never listed: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Posting into it lands in the workspace's chat.
    let mut posted = agent
        .tilo_do(json!({"path": "chat#post", "args": {"channel": CHANNEL, "text": TEXT}}))
        .await;
    while posted["result"]["structuredContent"]["status"] != "ok" {
        assert_eq!(
            posted["result"]["structuredContent"]["error"]["code"], "not_ready",
            "the post failed for another reason: {posted}"
        );
        assert!(Instant::now() < deadline, "the post never ran: {posted}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        posted = agent
            .tilo_do(json!({"path": "chat#post", "args": {"channel": CHANNEL, "text": TEXT}}))
            .await;
    }
}
