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

use serde_json::json;

use crate::chat::{chat_holds, opens, said};
use crate::host;

/// The channel the agent is written to in.
const CHANNEL: &str = "c_chat";
/// The workspace's assistant, as the workspace names it.
const SCOUT: &str = "Scout";
/// The workspace a daemon founds on a computer with no login.
const WORKSPACE_NAME: &str = "home";

/// The person writes `words` in the workspace's Chat, mentioning their assistant.
async fn write_to_scout(host: &host::Host, words: &str) -> (String, bool) {
    let agent = host.agent();
    said(
        &agent
            .tool(
                "tilo_do",
                &json!({"path": "chat/Messages#post",
                        "args": {"channel": CHANNEL, "text": format!("@{SCOUT} {words}")}}),
            )
            .await,
    )
}

/// Whether one of the workspace's Terminal records holds `needle`: what the workspace's machine
/// showed, as the session's screen keeps it.
async fn terminal_holds(host: &host::Host, needle: &str) -> Result<bool, String> {
    let workspaces = host.daemon().workspaces();
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
        let (wrote, failed) = write_to_scout(host.as_ref(), "Say hello in the shell").await;
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
        let (wrote, failed) = write_to_scout(host.as_ref(), "Say hello in the shell").await;
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
        let agent = host.agent();
        let (answered, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#answer-question",
                            "args": {"message": "the question",
                                     "answer": "allow",
                                     "device": "browser"}}),
                )
                .await,
        );
        assert!(!failed, "the person answers the question: {answered}");
        assert!(
            chat_holds(host.as_ref(), "first-echo-42")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the allowed command ran, and its output is in the conversation"
        );

        // A second command in the same conversation asks again, the same way.
        let (wrote, failed) = write_to_scout(host.as_ref(), "And once more").await;
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
        // The person's Agents settings allow the assistant `terminal.session#run` in the
        // workspace: the bound is written to the workspace's agents record, which their
        // device holds, before its window.
        let (wrote, failed) = write_to_scout(host.as_ref(), "Say hello in the shell").await;
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
        let (wrote, failed) = write_to_scout(host.as_ref(), "And again").await;
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
        let (wrote, failed) = write_to_scout(host.as_ref(), "Build it in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        assert!(
            chat_holds(host.as_ref(), "ask me to go on.")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the failed turn says so in its reply"
        );
        let (wrote, failed) = write_to_scout(host.as_ref(), "Go on.").await;
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
        let (wrote, failed) = write_to_scout(host.as_ref(), "Commit in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        // Her answer carries the command out, and the workspace's machine is given the agent's
        // name and the workspace's address, never the machine's loopback.
        let agent = host.agent();
        let (answered, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "chat/Messages#answer-question",
                            "args": {"message": "the question",
                                     "answer": "allow",
                                     "device": "browser"}}),
                )
                .await,
        );
        assert!(!failed, "the person answers the question: {answered}");
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
