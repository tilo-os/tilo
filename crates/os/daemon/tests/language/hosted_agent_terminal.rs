//! An agent the workspace's daemon runs works its commands in the workspace's Terminal for its
//! person, and is held to what they allow.
//!
//! A command asks the person first, in the conversation the turn is in, and the person's own
//! answer carries it out, once, its output kept in the conversation's Terminal. A second command
//! asks again. Once the person allowed the agent that one action for good, from its Agents
//! settings, it is carried out unasked, and taken back it asks again.
//!
//! The daemon works the turns of the agents its workspace lists, runs a workspace's shells for
//! them, and answers a turn with the model the workspace links: an approval of the agent on the
//! roster, a model, Chat's conversation to ask in, and the workspace's shells, so each test
//! installs Chat and the Terminal app first.

use std::time::{Duration, Instant};

use serde_json::json;
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{Answer, Change, GrantId, Grantee};
use tilo_action_services::workspace_invocations::{self, Invocation, InvocationStatus};
use tilo_action_types::ActionKind;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, ReceiptStatus, ResourceAddress, ResourceCopy,
};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_person_services::model_answers::scripted::Reply;

use crate::language::agent_fixture::terminal_holds;
use crate::language::chat::{CHANNEL, chat_holds, chat_holds_soon, made_channel, opens};
use crate::language::host;
use crate::language::hosted_turns::{asked, make_scout, scripted, tool_answers, write_to_scout};

/// The workspace's assistant, as the workspace names it.
const SCOUT: &str = "Scout";
/// The assistant's id, as the workspace's list of its agents names it.
const SCOUT_ID: &str = "scout";
/// The workspace's Terminal app, as its answers name it.
const TERMINAL: &str = ui::apps::TERMINAL;
/// The workspace a daemon founds on a computer with no login: its name, as a program's address
/// takes it.
const WORKSPACE_NAME: &str = "home";

/// What an agent is told an action its person's answers refuse it is: their own answer, as the
/// product words it, naming where they change it.
const DENIED_BY_ITS_PERSON: &str =
    "the person you work for set this to Deny for you in this workspace";

/// The workspace's invocations, as the person reads them.
async fn invocations(host: &host::Host) -> Vec<Invocation> {
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
    serde_json::from_value(whole["items"].clone()).expect("the items read as invocations")
}

/// The invocation `id` while it waits for the person: the command asked in the conversation.
async fn waits(host: &host::Host, id: &str) -> Invocation {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(found) = invocations(host).await.into_iter().find(|invocation| {
            invocation.id.as_deref() == Some(id) && invocation.status == InvocationStatus::Waiting
        }) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "the invocation {id} never waits for the person"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether no invocation of the workspace's waits for its person: nobody was asked.
async fn nobody_is_asked(host: &host::Host) -> bool {
    invocations(host)
        .await
        .iter()
        .all(|invocation| invocation.status != InvocationStatus::Waiting)
}

/// The person answers their assistant, in their Agents settings, for `scope` and `kind`: the
/// grant written into the workspace's governance by their device.
async fn answer(
    host: &host::Host,
    (scope, kind): (&AnswerScope, ActionKind),
    their_answer: Answer,
) -> GrantId {
    let grant = agent_answers::grant(
        host.workspace(),
        (SCOUT_ID, host.person()),
        (scope, kind),
        their_answer,
        Grantee::person(host.person().to_owned()),
    );
    let id = grant.id.clone();
    let daemon = host.daemon();
    daemon
        .workspaces()
        .commit_own_change(
            host.workspace(),
            WorkspaceMembershipCommand::Member {
                change: Change::Grant(grant),
            },
            None,
        )
        .await
        .expect("the workspace takes the person's answer for their assistant");
    id
}

/// Take back the answer `id`: the person's Agents settings no longer hold it.
async fn take_back(host: &host::Host, id: GrantId) {
    let daemon = host.daemon();
    daemon
        .workspaces()
        .commit_own_change(
            host.workspace(),
            WorkspaceMembershipCommand::Member {
                change: Change::Revoke(id),
            },
            None,
        )
        .await
        .expect("the workspace takes the person's answer back");
}

/// The person allows what their assistant's turn asks: the invocation of the workspace's that
/// waits for them is answered at `tilo`'s own prompt, on a connection the kernel says is theirs.
/// The same answer from a program on a plain connection is refused.
async fn allow(host: &host::Host) -> String {
    let dispatch = host.dispatch();
    let caller = host.caller();
    let record = invocations(host)
        .await
        .into_iter()
        .find(|invocation| invocation.status == InvocationStatus::Waiting)
        .and_then(|invocation| invocation.id)
        .expect("an invocation waits for the person's answer");
    let resolve = || {
        host::call(
            workspace_invocations::PROVIDER,
            tilo_actions::api::ActionAddress::new(record.clone(), "resolve"),
            &json!({"resolution": "approve"}),
        )
    };
    let by_a_program = dispatch.invoke(&caller, resolve()).await;
    assert!(
        matches!(
            &by_a_program,
            Some(Err(ActionCallError::Refused { reason }))
                if reason == tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM
        ),
        "a program does not answer for the person: {by_a_program:?}"
    );
    let (own, person) = host
        .daemon()
        .own_connection_in(host.workspace())
        .expect("the person's own connection reaches the workspace");
    assert!(person.is_own_connection(), "{person:?}");
    let receipt = own
        .invoke(&person, resolve())
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the person's answer is taken: {error:?}"));
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person's answer is done: {receipt:?}"
    );
    record
}

/// [`terminal_holds`] asked again until the workspace's Terminal holds `needle`.
async fn terminal_holds_soon(host: &host::Host, needle: &str) -> bool {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if terminal_holds(host, needle).await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The conversation holds no command's output: nothing ran, in Chat or in the Terminal.
async fn nothing_ran(host: &host::Host, output: &str) {
    assert!(
        !chat_holds(host, output)
            .await
            .unwrap_or_else(|error| panic!("{error}")),
        "nothing was said into the conversation: {output}"
    );
    assert!(
        !terminal_holds(host, output).await,
        "nothing ran in the Terminal: {output}"
    );
}

/// `tilo_do` calling the Terminal's `run` with `command`, under `invocation`: the id its asked
/// record and its invocation are known by.
fn running(invocation: &str, command: &str) -> serde_json::Value {
    json!({"path": "terminal#run", "args": {"command": command}, "invocation_id": invocation})
}

/// An assistant whose person's answers give it Chat alone runs nothing in the Terminal: its
/// command is refused, in the words its person's answer is shown by, and nothing is run or
/// asked.
#[test]
fn an_agent_approved_without_the_terminal_runs_no_command() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        // The person's answers refuse their assistant the Terminal: their one approval names
        // Chat, and the app it does not name is denied.
        answer(
            host,
            (&AnswerScope::app(TERMINAL), ActionKind::Outside),
            Answer::Deny,
        )
        .await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let scripted = scripted(
            host,
            [
                Reply::tool_call(
                    "call-1",
                    "tilo_do",
                    running("0f0e0d0c-0000-4000-8000-000000000001", "echo first-echo-42"),
                ),
                Reply::text("I could not run it in the Terminal."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 2).await;
        let answers = tool_answers(&scripted.asked()[1].request);
        assert!(
            matches!(answers.first(), Some((answer, true)) if answer.contains(DENIED_BY_ITS_PERSON)),
            "the assistant is told its person's answer refuses it: {answers:?}"
        );
        nothing_ran(host, "first-echo-42").await;
        assert!(nobody_is_asked(host).await, "nobody was asked");
    });
}

/// An agent's command asks in the conversation and is done once on its person's answer: nothing
/// runs before they answer, and after it the Terminal holds what it printed. A second command
/// asks again.
#[test]
fn an_agents_command_asks_in_the_conversation_and_is_done_once_on_its_persons_answer() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let first = "0f0e0d0c-0000-4000-8000-000000000002";
        let scripted = scripted(
            host,
            [
                Reply::tool_call("call-1", "tilo_do", running(first, "echo first-echo-42")),
                Reply::text("I asked you to let me run it."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 2).await;
        let asking = waits(host, first).await;
        assert_eq!(
            asking.by.id.as_deref(),
            Some(SCOUT_ID),
            "the command is the assistant's: {asking:?}"
        );
        nothing_ran(host, "first-echo-42").await;

        // The person answers the question the conversation holds: the command is carried out
        // once, and what it printed is in the conversation's Terminal.
        let answered = allow(host).await;
        assert_eq!(
            answered, first,
            "the person answered the assistant's own command"
        );
        assert!(
            terminal_holds_soon(host, "first-echo-42").await,
            "the allowed command ran, and its output is in the conversation's Terminal"
        );

        // A second command in the same conversation asks again, the same way.
        let second = "0f0e0d0c-0000-4000-8000-000000000003";
        scripted.then(Reply::tool_call(
            "call-2",
            "tilo_do",
            running(second, "echo second-echo-42"),
        ));
        scripted.then(Reply::text("I asked you again."));
        let (wrote, failed) = write_to_scout(host, &channel, "And once more").await;
        assert!(!failed, "the second message is written: {wrote}");
        asked(&scripted, 4).await;
        waits(host, second).await;
        nothing_ran(host, "second-echo-42").await;
    });
}

/// Once the person allowed the agent that one action for good, from its Agents settings, the
/// command is carried out unasked; taken back, the next command asks again.
#[test]
fn an_agent_allowed_to_execute_commands_does_unasked_until_its_person_takes_it_back() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        // The person's Agents settings allow the assistant
        // `tl.so.tilo.terminal.Session#run` in the workspace: the answer written to the
        // workspace's governance, before its window.
        let scope = AnswerScope::action(TERMINAL, "tl.so.tilo.terminal.Session", "run");
        let allowed = answer(host, (&scope, ActionKind::Outside), Answer::Allow).await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let scripted = scripted(
            host,
            [
                Reply::tool_call(
                    "call-1",
                    "tilo_do",
                    running("0f0e0d0c-0000-4000-8000-000000000004", "echo unasked-42"),
                ),
                Reply::text("I ran it, and it printed unasked-42."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 2).await;
        let answers = tool_answers(&scripted.asked()[1].request);
        assert!(
            matches!(answers.first(), Some((answer, false)) if answer.contains("unasked-42")),
            "the allowed command ran unasked, and the assistant is told what it printed: \
             {answers:?}"
        );
        assert!(nobody_is_asked(host).await, "nobody was asked");
        assert!(
            terminal_holds_soon(host, "unasked-42").await,
            "the allowed command ran in the Terminal"
        );

        // Taken back, the next command asks again.
        take_back(host, allowed).await;
        let second = "0f0e0d0c-0000-4000-8000-000000000005";
        scripted.then(Reply::tool_call(
            "call-2",
            "tilo_do",
            running(second, "echo again-42"),
        ));
        scripted.then(Reply::text("I asked you to let me run it again."));
        let (wrote, failed) = write_to_scout(host, &channel, "And again").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 4).await;
        waits(host, second).await;
        nothing_ran(host, "again-42").await;
    });
}

/// A turn that started a command it does not wait for, whose model call then broke off, says so
/// in its reply; the person's next message is a turn of its own and is answered.
#[test]
fn on_the_servers_model_a_turn_that_failed_with_a_command_pending_lets_the_next_message_in() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let scripted = scripted(
            host,
            [
                Reply::tool_call(
                    "call-1",
                    "tilo_do",
                    running(
                        "0f0e0d0c-0000-4000-8000-000000000006",
                        "echo building && echo built",
                    ),
                ),
                Reply::failing(
                    tilo_person_services::model_answers::protocol::ModelError::invalid(
                        "the model broke off",
                    ),
                ),
                Reply::text("I went on, and it is built."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Build it in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 2).await;
        assert!(
            chat_holds_soon(host, "ask me to go on.").await,
            "the failed turn says so in its reply"
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Go on.").await;
        assert!(!failed, "the next message is written: {wrote}");
        asked(&scripted, 3).await;
        assert!(
            chat_holds_soon(host, "I went on, and it is built.").await,
            "the next message is a turn of its own and is answered"
        );
    });
}

/// The agent that asked does not answer its own question: the model's own call to resolve the
/// asked record is refused by the system, and the command runs only on the person's answer.
#[test]
fn an_agents_own_answer_does_not_carry_its_command() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        // The command's record is named by the invocation the assistant ran it under, so the
        // model can name that same record back.
        let record = "0f0e0d0c-0000-4000-8000-000000000007";
        let scripted = scripted(
            host,
            [
                Reply::tool_call("call-1", "tilo_do", running(record, "echo own-answer-42")),
                Reply::tool_call(
                    "call-2",
                    "tilo_do",
                    json!({"path": format!("invocations/{record}#resolve"),
                           "args": {"resolution": "approve"}}),
                ),
                Reply::text("I cannot answer my own question."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Say hello in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 3).await;
        let answers = tool_answers(&scripted.asked()[2].request);
        assert!(
            answers.iter().any(|(answer, failed)| *failed
                && answer.contains(tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM)),
            "the system refuses the assistant's own answer: {answers:?}"
        );
        nothing_ran(host, "own-answer-42").await;

        // It still waits for the person, and their own answer carries it out once.
        waits(host, record).await;
        allow(host).await;
        assert!(
            terminal_holds_soon(host, "own-answer-42").await,
            "the person's answer ran the command"
        );
    });
}

/// A command's git identity is the agent that asked, as the workspace names it, and the person's
/// own command signs as her, at her workspace's address: what the workspace's machine was given
/// of the actor who asked.
#[test]
fn a_commands_git_identity_is_the_agent_that_asked_as_the_workspace_names_it() {
    host::run_with(&["chat", "terminal"], |host| async move {
        let host = host.as_ref();
        opens(host)
            .await
            .unwrap_or_else(|status| panic!("the person's desktop opens Chat: {status}"));
        make_scout(host).await;
        let channel = made_channel(&host.agent(), CHANNEL).await;
        let who = "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\"'";
        let record = "0f0e0d0c-0000-4000-8000-000000000008";
        let scripted = scripted(
            host,
            [
                Reply::tool_call("call-1", "tilo_do", running(record, who)),
                Reply::text("I asked you to let me run it."),
            ],
        );
        let (wrote, failed) = write_to_scout(host, &channel, "Commit in the shell").await;
        assert!(!failed, "the message is written: {wrote}");
        asked(&scripted, 2).await;
        waits(host, record).await;
        allow(host).await;
        // The workspace's machine is given the agent's name and the workspace's address, never
        // the machine's loopback.
        let scout = format!("{SCOUT} <{}@{WORKSPACE_NAME}.tilo>", SCOUT.to_lowercase());
        assert!(
            terminal_holds_soon(host, &scout).await,
            "the command reached the workspace's machine as {scout}"
        );

        // The person herself, from the device she signed in on: the same machine, and what it
        // makes signs as her, at her workspace's address.
        let key = host.key("Ada's laptop");
        let (ran, failed) = crate::language::chat::said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": host.workspace(),
                        "path": "terminal#run", "args": {"command": who}}),
            )
            .await,
        );
        assert!(!failed, "her command is taken: {ran}");
        // The person this computer founded is unnamed there, so their programs sign as a
        // member of it, at the workspace's address.
        let hers = format!("Tilo member <tilo-member@{WORKSPACE_NAME}.tilo>");
        assert!(
            terminal_holds_soon(host, &hers).await,
            "her command reached the workspace's machine as {hers}"
        );
    });
}
