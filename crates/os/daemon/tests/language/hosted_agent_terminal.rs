//! An agent the workspace's daemon runs works its commands in the workspace's Terminal for its
//! person, and is held to what they allow.
//!
//! A command asks the person first, in the conversation the turn is in, and the person's own
//! answer carries it out, once, with the agent told what it printed. A second command asks
//! again. Once the person allowed the agent that one action for good, from its Agents settings,
//! it is carried out unasked, and taken back it asks again.
//!
//! The daemon works the turns of the agents its workspace lists, runs a workspace's shells for
//! them, and answers a turn with the model the workspace links: an approval of the agent on the
//! roster, a model, Chat's conversation to ask in, and the workspace's shells. A test that
//! reaches the conversation before the app runs stops at the app's open.

use serde_json::{Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::workspace_invocations::{self, Invocation, InvocationStatus};
use tilo_actions::remote::{ActionProvider as _, ReceiptStatus, ResourceAddress, ResourceCopy};

use crate::language::chat::{CHANNEL, chat_holds, made_channel, opens, said};
use crate::language::host;

/// The workspace's assistant, as the workspace names it.
const SCOUT: &str = "Scout";
/// The assistant's id, as the workspace's list of its agents names it.
const SCOUT_ID: &str = "scout";
/// The workspace a daemon founds on a computer with no login.
const WORKSPACE_NAME: &str = "home";

/// The rich text a message that mentions their assistant says: its run marked as naming the
/// assistant, which is what fires its trigger.
fn mentioning(text: &str) -> Value {
    let label = format!("@{SCOUT}");
    json!({"blocks": [{
        "id": "",
        "kind": "paragraph",
        "text": format!("{label} {text}"),
        "marks": [{
            "start": 0,
            "end": label.chars().count(),
            "name": "mention",
            "value": format!("{{\"agent\":\"{SCOUT_ID}\"}}"),
        }],
        "attrs": {},
        "children": [],
    }]})
}

/// The person writes `words` in `channel`, in the workspace's Chat, mentioning their assistant.
async fn write_to_scout(host: &host::Host, channel: &str, words: &str) -> (String, bool) {
    let agent = host.agent();
    said(
        &agent
            .tool(
                "tilo_do",
                &json!({"path": format!("so.tilo.chat/{channel}#send"),
                        "args": {"body": mentioning(words)}}),
            )
            .await,
    )
}

/// The person allows what their assistant's turn asks: the newest invocation of the
/// workspace's that waits for them is answered.
async fn allow(host: &host::Host) {
    let dispatch = host.dispatch();
    let caller = host.caller();
    let reads = dispatch
        .resource_reads()
        .expect("the dispatch reads records");
    let address = ResourceAddress {
        workspace: host.workspace().to_owned(),
        app: workspace_invocations::PROVIDER.to_owned(),
        record: String::new(),
    };
    let reading = reads
        .read(&caller, &address)
        .await
        .expect("the workspace's invocations read");
    let whole = match reading {
        ResourceCopy::Reading { value, .. } => value,
        other => panic!("the invocations are a reading, not {other:?}"),
    };
    let invocations: Vec<Invocation> =
        serde_json::from_value(whole["items"].clone()).expect("the items read as invocations");
    let record = invocations
        .into_iter()
        .find(|invocation| invocation.status == InvocationStatus::Waiting)
        .and_then(|invocation| invocation.id)
        .expect("an invocation waits for the person's answer");
    let receipt = host
        .dispatch()
        .invoke(
            &caller,
            host::call(
                workspace_invocations::PROVIDER,
                ActionAddress::new(record, "resolve"),
                &json!({"resolution": "approve"}),
            ),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the person's answer is taken: {error:?}"));
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person's answer is done: {receipt:?}"
    );
}

/// Whether one of the workspace's Terminal records holds `needle`: what the workspace's machine
/// showed, as the session's screen keeps it.
async fn terminal_holds(host: &host::Host, needle: &str) -> Result<bool, String> {
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    let workspace = host.workspace();
    let routes = workspaces
        .document_routes(workspace)
        .await
        .map_err(|error| error.to_string())?;
    for (app, record) in routes
        .into_iter()
        .filter(|(app, _)| app == ui::apps::TERMINAL)
    {
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

/// The conversation holds no command's output: nothing ran.
async fn nothing_ran(host: &host::Host, output: &str) {
    assert!(
        !chat_holds(host, output)
            .await
            .unwrap_or_else(|error| panic!("{error}")),
        "nothing was run: {output}"
    );
}

/// An assistant whose approval names no Terminal runs nothing there: its command is refused in
/// words it can tell its person, and nothing is run or asked.
#[test]
fn an_agent_approved_without_the_terminal_runs_no_command() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let (wrote, failed) =
            write_to_scout(host.as_ref(), &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        // The turn works the command it was given, and the app refuses it by the agent's
        // approval, which names Chat alone.
        assert!(
            chat_holds(host.as_ref(), "this agent was not approved to change")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the agent is told its approval does not reach the Terminal"
        );
        nothing_ran(host.as_ref(), "first-echo-42").await;
        assert!(
            !chat_holds(host.as_ref(), "asked in the conversation")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "nobody was asked"
        );
    });
}

/// An agent's command asks in the conversation and is done once on its person's answer, with the
/// agent told what it printed; a second command asks again.
#[test]
fn an_agents_command_asks_in_the_conversation_and_is_done_once_on_its_persons_answer() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let (wrote, failed) =
            write_to_scout(host.as_ref(), &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "asked in the conversation")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the command is asked in the conversation"
        );
        nothing_ran(host.as_ref(), "first-echo-42").await;

        // The person answers the question the conversation holds: the command is carried out
        // once, and her assistant is told what it printed.
        allow(host.as_ref()).await;
        assert!(
            chat_holds(host.as_ref(), "first-echo-42")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the allowed command ran, and its output is in the conversation"
        );

        // A second command in the same conversation asks again, the same way.
        let (wrote, failed) = write_to_scout(host.as_ref(), &channel, "And once more").await;
        assert!(!failed, "the second message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "asked in the conversation again")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the second command is asked too"
        );
        nothing_ran(host.as_ref(), "second-echo").await;
    });
}

/// Once the person allowed the agent that one action for good, from its Agents settings, the
/// command is carried out unasked; taken back, the next command asks again.
#[test]
fn an_agent_allowed_to_execute_commands_does_unasked_until_its_person_takes_it_back() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let channel = made_channel(&host.agent(), CHANNEL).await;
        // The person's Agents settings allow the assistant `terminal.session#run` in the
        // workspace: the bound is written to the workspace's agents record, which their
        // device holds, before its window.
        let (wrote, failed) =
            write_to_scout(host.as_ref(), &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "unasked-42")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the allowed command ran unasked, and its output is in the conversation"
        );
        assert!(
            !chat_holds(host.as_ref(), "asked in the conversation")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "nobody was asked"
        );

        // Taken back, the next command asks again.
        let (wrote, failed) = write_to_scout(host.as_ref(), &channel, "And again").await;
        assert!(!failed, "the message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "asked in the conversation")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "with the bound taken back the next command is asked"
        );
        nothing_ran(host.as_ref(), "again-42").await;
    });
}

/// A turn that started a command it does not wait for, whose model call then broke off, says so
/// in its reply; the person's next message is a turn of its own and is answered.
#[test]
fn on_the_servers_model_a_turn_that_failed_with_a_command_pending_lets_the_next_message_in() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let (wrote, failed) =
            write_to_scout(host.as_ref(), &channel, "Build it in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "ask me to go on.")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the failed turn says so in its reply"
        );
        let (wrote, failed) = write_to_scout(host.as_ref(), &channel, "Go on.").await;
        assert!(!failed, "the next message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "I went on, and it is built.")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the next message is a turn of its own and is answered"
        );
    });
}

/// A command's git identity is the agent that asked, as the workspace names it, and the person's
/// own command signs as her, at her workspace's address: what the workspace's machine was given
/// of the actor who asked.
#[test]
fn a_commands_git_identity_is_the_agent_that_asked_as_the_workspace_names_it() {
    host::run(|host| async move {
        opens(host.as_ref())
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let (wrote, failed) = write_to_scout(host.as_ref(), &channel, "Commit in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        // Her answer carries the command out, and the workspace's machine is given the agent's
        // name and the workspace's address, never the machine's loopback.
        allow(host.as_ref()).await;
        let deadline = tilo_test_env::hang_deadline();
        let scout = format!("{SCOUT} <{}@{WORKSPACE_NAME}.tilo>", SCOUT.to_lowercase());
        loop {
            if terminal_holds(host.as_ref(), &scout)
                .await
                .unwrap_or_else(|error| panic!("{error}"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the command never reached the workspace's machine as {scout}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // The person herself, from the device she signed in on: the same machine, and what it
        // makes signs as her, at her workspace's address.
        let key = host.key("Ada's laptop");
        let (ran, failed) = said(
            &key.tool(
                "tilo_do",
                &json!({"path": "terminal#run",
                        "args": {"command": "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\"'"}}),
            )
            .await,
        );
        assert!(!failed, "her command is taken: {ran}");
        let hers = format!("Ada <ada@{WORKSPACE_NAME}.tilo>");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if terminal_holds(host.as_ref(), &hers)
                .await
                .unwrap_or_else(|error| panic!("{error}"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "her command never reached the workspace's machine as {hers}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
}
