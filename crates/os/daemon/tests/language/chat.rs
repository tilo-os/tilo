//! A person's Chat in the workspace their computer daemon governs: an agent's session joined
//! before the chat was made lists it and posts into it, a chat the workspace began with is
//! posted into the same way, a batch of posts is one step a single undo takes back, and what
//! the agent posted is held in the workspace's records.
//!
//! The daemon runs its workspace's apps (a tilolang app's windowless instance per app), so a
//! test reaches the app, its records are read as either side of the app reads them, and its
//! actions run. A test that lists or acts on Chat before that piece runs stops at the app's
//! start. Chat starts empty, so the channel is one the test makes, as a person makes one.

use serde_json::{Value, json};

use crate::language::host;

const CHAT: &str = ui::apps::CHAT;
/// Chat's package under the tree's `apps/tl`, which the device installs to run it: the start a
/// test makes reaches the app's windowless instance.
const CHAT_PACKAGE: &str = "chat";
/// The channel a test makes in the workspace's chat, and posts into.
pub(crate) const CHANNEL: &str = "chat";
/// The app's own resource: where it publishes its history verbs, which is how a page reads who
/// wrote a row and takes steps back.
const CHAT_RESOURCE: &str = "tl:so.tilo.chat";
/// What the agent says in the chat.
const TEXT: &str = "Hello from the agent";

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

/// The field `name` of a tool's answer, from its typed answer or the JSON beside it.
fn field(result: &Value, name: &str) -> Value {
    if let Some(typed) = result["structuredContent"].get(name) {
        return typed.clone();
    }
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .find_map(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get(name).cloned())
        .unwrap_or(Value::Null)
}

/// Whether one of the workspace's Chat records holds `needle`: the record as the app's readers
/// lay it out, which is what a page following the chat is sent.
pub(crate) async fn chat_holds(host: &host::Host, needle: &str) -> Result<bool, String> {
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
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

/// [`chat_holds`] asked again until the record holds `needle`: a write the API answered for
/// reaches the daemon's record as the app's changes do, which may still be on their way as the
/// answer comes.
pub(crate) async fn chat_holds_soon(host: &host::Host, needle: &str) -> bool {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match chat_holds(host, needle).await {
            Ok(true) => return true,
            Ok(false) => {}
            Err(error) => panic!("{error}"),
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// [`chat_holds`] asked again until the record stops holding `needle`: an undo the API
/// answered for reaches the daemon's record as the app's changes do.
pub(crate) async fn chat_stops_holding(host: &host::Host, needle: &str) -> bool {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match chat_holds(host, needle).await {
            Ok(false) => return true,
            Ok(true) => {}
            Err(error) => panic!("{error}"),
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// The person's app started in their workspace with no window, as the daemon runs its
/// workspace's apps: its instance opens its own record there and answers its actions.
pub(crate) async fn opens(host: &host::Host) -> Result<(), String> {
    host::start_app(host, CHAT)
        .await
        .map(drop)
        .map_err(|refused| format!("{refused:?}"))
}

/// What `result`, a tool's answer to a call that made something, names it by.
fn made(result: &Value) -> String {
    let made = field(result, "resources");
    made.as_array()
        .and_then(|made| made.first())
        .and_then(|made| {
            made.as_str()
                .or_else(|| made.get("id").and_then(Value::as_str))
        })
        .map_or_else(
            || panic!("the call names what it made: {result}"),
            str::to_owned,
        )
}

/// The agent makes the channel `name` in the workspace's chat, and names it by the id a post
/// into it goes to.
pub(crate) async fn made_channel(agent: &host::Agent, name: &str) -> String {
    let answer = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": "so.tilo.chat#new_channel", "args": {"name": name}}),
        )
        .await;
    let (text, failed) = said(&answer);
    assert!(!failed, "the channel is made: {text}");
    made(&answer)
}

/// The agent posts `text` in `channel`, in the workspace's chat.
async fn post(agent: &host::Agent, channel: &str, text: &str) -> (String, bool) {
    said(
        &agent
            .tool(
                "tilo_do",
                &json!({"path": format!("so.tilo.chat/{channel}#send"),
                        "args": {"body": text}}),
            )
            .await,
    )
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

/// [`listed_by_agent`], through the key's MCP session in `workspace`: a key reaching every
/// workspace names the one it acts in, as the API's account session asks.
async fn listed_by_key(key: &host::Key, workspace: &str) -> (String, bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (text, failed) = said(
            &key.tool("tilo_ls", &json!({"workspace": workspace, "path": "chat"}))
                .await,
        );
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
/// the channel the page's chat is made with, posts into it, and the page sees what it said,
/// signed by the agent's session.
#[test]
fn an_agent_posts_to_chat_a_page_made_after_it_joined() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        let agent = host.agent();
        // A first call makes the device's actions join the workspace, before Chat exists there:
        // a listing writes nothing.
        let _ = agent.tool("tilo_ls", &json!({"path": "chat"})).await;

        // The person's page opens Chat in the workspace, and makes its channel.
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's page opens Chat: {status}"));
        let channel = made_channel(&agent, CHANNEL).await;

        let (listed, shows) = listed_by_agent(&agent).await;
        assert!(shows, "the listing shows the page's channel: {listed}");

        let (posted, failed) = post(&agent, &channel, TEXT).await;
        assert!(!failed, "the post runs: {posted}");

        // The page follows the chain and reads what the agent said. Its history names the
        // writer by its session, not by the key the session acts under.
        assert!(
            chat_holds_soon(host.as_ref(), TEXT).await,
            "the chat holds the agent's message"
        );
        let history = history_of(host.as_ref()).await;
        assert!(
            history.contains("mcp"),
            "the message's byline names the agent's session, not its key: {history}"
        );
    });
}

/// Chat's own history of its conversations root, as its `history` verb answers it: each version
/// with who made it, which is what a message's byline reads.
async fn history_of(host: &host::Host) -> String {
    let (history, failed) = said(
        &host
            .agent()
            .tool(
                "tilo_do",
                &json!({"path": format!("so.tilo.chat/{CHAT_RESOURCE}#history"), "args": {}}),
            )
            .await,
    );
    assert!(!failed, "Chat lists its history: {history}");
    history
}

/// The person's workspace began with Chat, written before any key's session entered it: the
/// key's agent lists it and posts into it as the person's own desktop does.
#[test]
fn an_agent_posts_to_chat_a_persons_own_workspace_began_with() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's workspace starts Chat: {status}"));
        let agent = host.agent();
        let channel = made_channel(&agent, CHANNEL).await;

        let key = host.key("Visa backend");
        let (listed, shows) = listed_by_key(&key, host.workspace()).await;
        assert!(shows, "the listing shows the workspace's channel: {listed}");

        let (posted, failed) = said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": host.workspace(),
                        "path": format!("so.tilo.chat/{channel}#send"),
                        "args": {"body": TEXT}}),
            )
            .await,
        );
        assert!(!failed, "the post lands in the workspace's chat: {posted}");
        assert!(
            chat_holds_soon(host.as_ref(), TEXT).await,
            "the post is in the workspace's chat"
        );
    });
}

/// How many posts one call carries: the most a call takes.
const BATCH: usize = 32;

/// A batch of posts through one call is applied in one call, and one undo takes the whole
/// batch back. Each step of a `calls` batch is an invocation of its own
/// (`batch_invocation`, `crates/core/tilo-mcp/src/mcp_step.rs`), so the undo names how many
/// of its steps to take back: the batch's.
#[test]
fn a_batch_of_posts_is_one_step_and_one_undo_takes_it_back() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's workspace starts Chat: {status}"));
        let agent = host.agent();
        let channel = made_channel(&agent, CHANNEL).await;

        let posts: Vec<Value> = (0..BATCH)
            .map(|at| {
                json!({"path": format!("so.tilo.chat/{channel}#send"),
                       "args": {"body": format!("bulk {at}")}})
            })
            .collect();
        let batch_answer = agent.tool("tilo_do", &json!({"calls": posts})).await;
        let (batched, failed) = said(&batch_answer);
        assert!(!failed, "the batch is applied in one call: {batched}");
        let mut kept = 0;
        for at in 0..BATCH {
            if chat_holds_soon(host.as_ref(), &format!("bulk {at}")).await {
                kept += 1;
            }
        }
        assert_eq!(kept, BATCH, "every post of the batch is kept");

        // One step: the record's own history takes the whole batch back.
        let undo_answer = agent
            .tool(
                "tilo_do",
                &json!({"path": format!("so.tilo.chat/{CHAT_RESOURCE}#undo"),
                        "args": {"steps": BATCH},
                        "wait_ms": 5_000}),
            )
            .await;
        let (undone, failed) = said(&undo_answer);
        assert!(!failed, "the undo is applied: {undone}");
        assert!(
            chat_stops_holding(host.as_ref(), "bulk 0").await,
            "one undo takes the batch back"
        );
    });
}

/// The chat's posts the record answered for are held in the workspace's records, which is what
/// a daemon started again on them reads.
#[test]
fn the_chats_api_posts_are_held_in_the_workspaces_records() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's workspace starts Chat: {status}"));
        let agent = host.agent();
        let channel = made_channel(&agent, CHANNEL).await;
        let key = host.key("Lounge");
        let mut posted = Vec::new();
        for index in 0..4 {
            let text = format!("#{index} posted before the restart");
            let (answer, failed) = said(
                &key.tool(
                    "tilo_do",
                    &json!({"workspace": host.workspace(),
                            "path": format!("so.tilo.chat/{channel}#send"),
                            "args": {"body": text}}),
                )
                .await,
            );
            assert!(!failed, "post {index} lands: {answer}");
            eprintln!("KEY POST {index} = {answer}");
            posted.push(text);
        }
        for text in &posted {
            assert!(
                chat_holds_soon(host.as_ref(), text).await,
                "the workspace holds {text:?}, which the API answered for"
            );
        }

        // They are the workspace's records, not the daemon's memory: a computer that starts
        // again on them still holds every post, and its Chat runs again on them.
        drop(key);
        drop(agent);
        host.restart().await;
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's workspace starts Chat: {status}"));
        for text in &posted {
            assert!(
                crate::language::chat::chat_holds(host.as_ref(), text)
                    .await
                    .unwrap_or(false),
                "the workspace holds {text:?} after the daemon started again"
            );
        }
    });
}
