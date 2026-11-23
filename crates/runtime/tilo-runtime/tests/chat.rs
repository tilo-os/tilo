//! A person's Chat in the workspace their computer daemon governs: an agent's session joined
//! before the chat was made lists it and posts into it, a chat the workspace began with is
//! posted into the same way, and a batch of posts is one step a single undo takes back.
//!
//! The daemon runs its workspace's apps (a tilolang app's windowless instance per app), so an
//! open reaches the app, its records are read as either side of the app reads them, and its
//! actions run. A test that lists or acts on Chat before that piece runs stops at the app's open.

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::{ActionCallError, ReceiptStatus, application_open_target};

use crate::host;

const CHAT: &str = ui::apps::CHAT;
/// What the agent says in the chat.
const TEXT: &str = "Hello from the agent";
/// The channel every chat opens with.
const CHANNEL: &str = "c_chat";

/// The text of an MCP tool's result, and whether it failed.
pub(crate) fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// Whether one of the workspace's Chat records holds `needle`: the record as the app's readers
/// lay it out, which is what a page following the chat is sent.
pub(crate) async fn chat_holds(host: &host::Host, needle: &str) -> Result<bool, String> {
    let workspaces = host.daemon().workspaces();
    let workspace = host.workspace();
    let routes = workspaces
        .document_routes(workspace)
        .await
        .map_err(|error| error.to_string())?;
    for (app, record) in routes.into_iter().filter(|(app, _)| app == CHAT) {
        let held = workspaces
            .resource_document(workspace, &app, &record)
            .await
            .map_err(|error| error.to_string())?;
        if held.is_some_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes())) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The person's app open in their workspace, as their desktop's window makes it: the daemon
/// runs the app, which makes its record with the first channel.
pub(crate) async fn opens(host: &host::Host) -> Result<(), String> {
    let target = application_open_target(host.workspace(), CHAT);
    let receipt = host
        .dispatch()
        .invoke(
            &host.caller(),
            host::call(&target.provider, target.address, &json!({})),
        )
        .await
        .expect("the dispatch answers")
        .map_err(|refused: ActionCallError| format!("{refused:?}"))?;
    match &receipt.status {
        ReceiptStatus::Completed { .. } => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

/// A listing of Chat that shows the workspace's channel: asked again until the app serves it,
/// as a listing written while another device's record is on its way is.
async fn listed_by_agent(agent: &host::Agent) -> (String, bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (text, failed) = said(&agent.tool("tilo_ls", &json!({"path": "chat"})).await);
        if !failed {
            let shows = text.contains(CHANNEL);
            return (text, shows);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the chat is never listed: {text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// [`listed_by_agent`], through the key's MCP session.
async fn listed_by_key(key: &host::Key) -> (String, bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (text, failed) = said(&key.tool("tilo_ls", &json!({"path": "chat"})).await);
        if !failed {
            let shows = text.contains(CHANNEL);
            return (text, shows);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the chat is never listed: {text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// An agent's session joined the workspace before a page opened Chat there: the agent lists
/// the channel the page's chat opened with, posts into it, and the page sees what it said,
/// signed by the agent's session.
#[test]
fn an_agent_posts_to_chat_a_page_made_after_it_joined() {
    host::run(|host| async move {
        let agent = host.agent();
        // A first call makes the device's actions join the workspace, before Chat exists there:
        // a listing writes nothing.
        let _ = agent.tool("tilo_ls", &json!({"path": "chat"})).await;

        // The person's page opens Chat in the workspace: the app makes its record with the
        // first channel every chat opens with, after the device's actions joined.
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's page opens Chat: {status}"));

        let (listed, shows) = listed_by_agent(&agent).await;
        assert!(shows, "the listing shows the page's channel: {listed}");

        // Posting into it starts the chat's first messages part.
        let (posted, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat#post",
                            "args": {"channel": CHANNEL, "text": TEXT}}),
                )
                .await,
        );
        assert!(!failed, "the post runs: {posted}");

        // The page follows the chain and reads what the agent said, signed by the session the
        // daemon handed it, as the agent's windows name it.
        assert!(
            chat_holds(host.as_ref(), TEXT)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the chat holds the agent's message"
        );
        assert!(
            chat_holds(host.as_ref(), "mcp-")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the message's byline names the agent's session, not its key"
        );
    });
}

/// The person's workspace began with Chat, written before any key's session entered it: the
/// key's agent lists it and posts into it as the person's own desktop does.
#[test]
fn an_agent_posts_to_chat_a_persons_own_workspace_began_with() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));

        let key = host.key("Visa backend");
        let (listed, shows) = listed_by_key(&key).await;
        assert!(shows, "the listing shows the workspace's channel: {listed}");

        let (posted, failed) = said(
            &key.tool(
                "tilo_do",
                &json!({"path": "chat#post", "args": {"channel": CHANNEL, "text": TEXT}}),
            )
            .await,
        );
        assert!(!failed, "the post lands in the workspace's chat: {posted}");
        assert!(
            chat_holds(host.as_ref(), TEXT)
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the post is in the workspace's chat"
        );
    });
}

/// How many posts one batch carries.
const BATCH: usize = 500;

/// A batch of posts through the chat's `group` is one step: every post is kept, and one undo
/// takes the whole batch back. The chain offers the batch at no field, so one call carries
/// posts whose verbs each take their own fields.
#[test]
fn a_batch_of_posts_is_one_step_and_one_undo_takes_it_back() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let agent = host.agent();
        let (listed, shows) = listed_by_agent(&agent).await;
        assert!(shows, "the person's chat is open: {listed}");

        let posts: Vec<Value> = (0..BATCH)
            .map(|at| {
                json!({"action": "post",
                       "args": {"channel": CHANNEL, "text": format!("bulk {at}")}})
            })
            .collect();
        let (grouped, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#group", "args": {"actions": posts}}),
                )
                .await,
        );
        assert!(!failed, "the batch is applied in one call: {grouped}");
        let mut kept = 0;
        for at in 0..BATCH {
            if chat_holds(host.as_ref(), &format!("bulk {at}"))
                .await
                .unwrap_or_else(|error| panic!("{error}"))
            {
                kept += 1;
            }
        }
        assert_eq!(kept, BATCH, "every post of the batch is kept");

        // One step: one undo takes the whole batch back.
        let (undone, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#undo", "args": {"part": 0}}),
                )
                .await,
        );
        assert!(!failed, "the undo is applied: {undone}");
        assert!(
            !chat_holds(host.as_ref(), "bulk 0")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "one undo takes the batch back"
        );
    });
}

/// The chat's posts the action API answered for are held in the workspace's records, which is
/// what a daemon started again on them reads: the restart itself waits on the fixture offering
/// one.
#[test]
fn the_chats_api_posts_are_held_in_the_workspaces_records() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let key = host.key("Lounge");
        let mut posted = Vec::new();
        for index in 0..4 {
            let text = format!("#{index} posted before the restart");
            let (answer, failed) = said(
                &key.tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#post",
                            "args": {"channel": CHANNEL, "text": text}}),
                )
                .await,
            );
            assert!(!failed, "post {index} lands: {answer}");
            posted.push(text);
        }
        for text in &posted {
            assert!(
                chat_holds(host.as_ref(), text)
                    .await
                    .unwrap_or_else(|error| panic!("{error}")),
                "the workspace holds {text:?}, which the API answered for"
            );
        }
    });
}
