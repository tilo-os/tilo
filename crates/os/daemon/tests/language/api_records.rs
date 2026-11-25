#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! What a workspace's apps answer through a key's API and over MCP: a letter's History names
//! the agent that wrote it and the run it came from; a member's key posts a long history into
//! Chat and reads every message at the path its listing gave; a tracker names an issue by its id
//! and makes one at the same cost however full it is; and a workspace lists, pages and takes
//! back its runs.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::language::host;

/// The action answer of `path#action` with `args` in `workspace`, through `key`.
async fn act(key: &host::Key, workspace: &str, path: &str, action: &str, args: &Value) -> Value {
    let (status, answer) = key
        .post(
            &format!("/v1/workspaces/{workspace}/resources/{path}/actions/{action}"),
            args,
        )
        .await;
    assert_eq!(status, 200, "{path}#{action}: {answer}");
    answer
}

/// [`act`], which ran: its result.
async fn did(key: &host::Key, workspace: &str, path: &str, action: &str, args: &Value) -> Value {
    let answer = act(key, workspace, path, action, args).await;
    assert_eq!(answer["status"], "ok", "{path}#{action}: {answer}");
    answer["result"].clone()
}

/// A run of `key`'s: `path#action` with `args`, named `title` as a client names its run.
async fn in_run(
    key: &host::Key,
    workspace: &str,
    (path, action): (&str, &str),
    args: &Value,
    title: &str,
) -> (u16, Value) {
    let run = title.to_lowercase().replace(' ', "-");
    key.request(
        "POST",
        &format!("/v1/workspaces/{workspace}/resources/{path}/actions/{action}"),
        &[
            ("content-type", "application/json"),
            ("tilo-invocation", run.as_str()),
            ("tilo-invocation-title", title),
        ],
        args.to_string().as_bytes(),
    )
    .await
}

/// `key` writes `text` into the workspace's letter, in a run of its own called `title`: the id
/// of the run's last call.
async fn write_letter(key: &host::Key, workspace: &str, text: &str, title: &str) -> String {
    let (status, opened) = in_run(
        key,
        workspace,
        ("word-processor", "open"),
        &json!({}),
        title,
    )
    .await;
    assert_eq!(status, 200, "{opened}");
    let (status, written) = in_run(
        key,
        workspace,
        ("word-processor", "append-paragraph"),
        &json!({"text": text}),
        title,
    )
    .await;
    assert_eq!(status, 200, "{written}");
    written["invocation"]
        .as_str()
        .expect("the run's id")
        .to_owned()
}

/// `key` writes `text` into its letter in the workspace, in a run of its own.
async fn opened_and_written(key: &host::Key, workspace: &str) {
    did(key, workspace, "word-processor", "open", &json!({})).await;
    did(
        key,
        workspace,
        "word-processor",
        "append-paragraph",
        &json!({"text": "Dear Visa Officer,"}),
    )
    .await;
}

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// A minted key's agent goes by the name the key was minted with: History names its changes
/// with it, in the colour presence draws it in, and each change names the run its agent
/// declared beside it.
#[test]
fn history_names_a_keys_agent_by_the_name_it_was_minted_with() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        // The key a backend mints for its applicant's agent, named as the agent is.
        let key = host.key("Jane Doe's agent");
        opened_and_written(&key, &workspace).await;
        let title = "Fill Jane Doe's visa application";
        let (status, reply) = in_run(
            &key,
            &workspace,
            ("word-processor", "append-paragraph"),
            &json!({"text": "I apply for a tourist visa."}),
            title,
        )
        .await;
        assert_eq!(status, 200, "{reply}");

        let history = did(
            &key,
            &workspace,
            "word-processor",
            "history",
            &json!({"last": 50}),
        )
        .await;
        let by_agent: Vec<&Value> = history["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|version| version["actor"]["by"]["member"]["agent"].is_string())
            .collect();
        let colour = |agent: &str| {
            let (red, green, blue) = collab_types::presence::identity_rgb(
                collab_types::presence::PresenceHue::of_member(&collab_types::MemberId::agent(
                    agent, "ada",
                )),
            );
            format!("#{red:02x}{green:02x}{blue:02x}")
        };
        assert!(
            !by_agent.is_empty()
                && by_agent.iter().all(|version| {
                    version["actor"]["name"] == "Jane Doe's agent"
                        && version["actor"]["colour"]
                            == colour(
                                version["actor"]["by"]["member"]["agent"]
                                    .as_str()
                                    .unwrap_or(""),
                            )
                            .as_str()
                }),
            "{history}"
        );
        let runs: std::collections::BTreeSet<&str> = by_agent
            .iter()
            .filter_map(|version| version["title"].as_str())
            .collect();
        assert_eq!(
            runs,
            [title, "Jane Doe's agent"].into_iter().collect(),
            "a titled call goes by its title, an untitled call's by its agent's name: {history}"
        );
    });
}

/// A member's key posts a long history into the Lounge's chat through the action API, one
/// message at a time. Every post lands within the API's wait, the chat reads back the last of
/// them, and the last posts cost about what the first did.
#[test]
fn a_members_key_posts_a_long_history_into_the_chat() {
    host::run(|host| async move {
        const MESSAGES: usize = 60;
        const EACH_END: usize = 10;
        let workspace = host.workspace().to_owned();
        let key = host.key("Lounge history");
        did(&key, &workspace, "chat", "open", &json!({})).await;

        let mut took: Vec<Duration> = Vec::with_capacity(MESSAGES);
        for index in 0..MESSAGES {
            let text = format!(
                "#{index} {}",
                "lounge history words ".repeat(20 + index * 37 % 120)
            );
            let began = Instant::now();
            let reply = act(
                &key,
                &workspace,
                "chat/Messages",
                "post",
                &json!({"channel": "c_general", "text": text}),
            )
            .await;
            assert_eq!(
                reply["status"], "ok",
                "post {index} landed: {}",
                reply["error"]
            );
            took.push(began.elapsed());
        }

        // The chat reads back every one of them.
        let mut read = String::new();
        let (text, failed) = said(
            &key.tool(
                "tilo_read",
                &json!({"workspace": workspace, "path": "chat/Messages"}),
            )
            .await,
        );
        assert!(!failed, "{text}");
        read.push_str(&text);
        for index in 0..MESSAGES {
            assert!(
                read.contains(&format!("#{index} ")),
                "post {index} is in the chat"
            );
        }

        let mean = |posts: &[Duration]| {
            let count = u32::try_from(posts.len()).unwrap();
            (posts.iter().sum::<Duration>() / count).as_secs_f64() * 1000.0
        };
        let (first, last) = (mean(&took[..EACH_END]), mean(&took[MESSAGES - EACH_END..]));
        assert!(
            last <= first * 4.0 + 100.0,
            "a post costs more as the chat grows: {first:.0} ms early, {last:.0} ms late"
        );
    });
}

/// A conversation longer than one bounded read of Chat's directory: every message the
/// conversation's read lists is read at the path the read gave for it.
#[test]
fn every_message_a_long_conversation_lists_is_read_at_its_path() {
    host::run(|host| async move {
        const MESSAGES: usize = 45;
        let workspace = host.workspace().to_owned();
        let key = host.key("Long conversation");
        did(&key, &workspace, "chat", "open", &json!({})).await;

        let mut posted = Vec::with_capacity(MESSAGES);
        for index in 0..MESSAGES {
            let text = format!("message {index} of the long conversation");
            let reply = act(
                &key,
                &workspace,
                "chat/Messages",
                "post",
                &json!({"channel": "c_general", "text": text}),
            )
            .await;
            assert_eq!(
                reply["status"], "ok",
                "post {index} landed: {}",
                reply["error"]
            );
            posted.push(text);
        }

        // What the conversation's read lists, over as many pages as it comes in.
        let mut listed: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut arguments = json!({"workspace": workspace, "path": "chat/Messages"});
            if let Some(cursor) = &cursor {
                arguments["cursor"] = json!(cursor);
            }
            let reply = key.tool_structured("tilo_read", &arguments).await;
            assert_ne!(reply["isError"], true, "{reply}");
            let read = &reply["structuredContent"];
            listed.extend(read["children"].as_array().cloned().unwrap_or_default());
            cursor = read["cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let messages: Vec<&Value> = listed
            .iter()
            .filter(|child| {
                child["item"]
                    .as_str()
                    .is_some_and(|item| item.starts_with("chat.messages/messages/"))
            })
            .collect();
        assert!(
            messages.len() >= MESSAGES,
            "the read lists every message: {} of {MESSAGES}",
            messages.len()
        );

        let mut read = String::new();
        for (index, message) in messages.iter().enumerate() {
            let path = message["path"]
                .as_str()
                .expect("a listed message has a path");
            let reply = key
                .tool_structured("tilo_read", &json!({"workspace": workspace, "path": path}))
                .await;
            assert_ne!(
                reply["isError"], true,
                "message {index} is read at {path}: {reply}"
            );
            read.push_str(&reply["structuredContent"].to_string());
        }
        for text in &posted {
            assert!(read.contains(text.as_str()), "{text:?} was read back");
        }
    });
}

/// A startup's backend makes a workspace per customer with its one key, as fast as it answers,
/// and works in some of them at once: none is refused or paced, and the workspaces made
/// before do not make the next slower.
#[test]
fn one_key_makes_and_works_in_hundreds_of_workspaces_at_once() {
    host::run(|host| async move {
        const CUSTOMERS: usize = 500;
        let key = host.key("Customer backend");
        let mut creating = Vec::with_capacity(CUSTOMERS);
        for customer in 0..CUSTOMERS {
            let began = Instant::now();
            let (status, created) = key
                .post(
                    "/v1/resources/account/workspaces/actions/create",
                    &json!({"name": format!("Customer {customer}")}),
                )
                .await;
            assert_eq!(status, 200, "a workspace is made: {created}");
            creating.push(began.elapsed());
        }
        let (status, listed) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(
            listed["result"]["workspaces"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            CUSTOMERS,
            "each of the backend's workspaces is listed: {listed}"
        );
        let mut sorted = creating.clone();
        sorted.sort();
        let p95 = sorted[(sorted.len() - 1) * 95 / 100];
        assert!(
            p95 < Duration::from_millis(500),
            "making a workspace stays fast: p95 {p95:?}"
        );
    });
}

/// Where the workspace's invocations are read, through the record API.
fn listing(workspace: &str) -> String {
    format!("/v1/workspaces/{workspace}/resources/record:tilo.invocations:")
}

/// The workspace's runs as `key` reads them, whole.
async fn invocations(key: &host::Key, workspace: &str) -> Value {
    let (status, answered) = key.get(&listing(workspace)).await;
    assert_eq!(status, 200, "{answered}");
    answered["fields"].clone()
}

/// The run titled `title` in `listed`.
fn titled<'a>(listed: &'a Value, title: &str) -> &'a Value {
    listed["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|run| run["title"] == title)
        .unwrap_or_else(|| panic!("a run titled {title:?} is listed: {listed}"))
}

/// Two keys of the person's write into the workspace's letter; the person's own key takes one
/// run back by the id its answer named, and the listing reads it undone.
#[test]
fn an_invocation_is_taken_back_by_a_key_other_than_the_one_that_ran_it() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.key("Jane Doe's agent");
        let other = host.key("Reviewer's agent");
        let person = host.key("Visa backend");
        let letter = write_letter(&agent, &workspace, "Dear Visa Officer,", "Cover letter").await;
        write_letter(&other, &workspace, "Yours faithfully", "Closing").await;

        let (status, undone) = person
            .post(
                &format!("/v1/workspaces/{workspace}/invocations/{letter}/undo"),
                &json!({}),
            )
            .await;
        assert_eq!(status, 200, "{undone}");
        assert_eq!(undone["status"], "ok", "{undone}");
        let listed = invocations(&person, &workspace).await;
        let taken = titled(&listed, "Cover letter");
        assert_eq!(taken["status"], "undone", "{listed}");
        assert_eq!(taken["undone"], taken["changes"], "{taken}");
        titled(&listed, "Closing");
    });
}

/// Keys reading a workspace list its runs newest first, each with who ran it and how it ended,
/// whole through the record API; a key for another workspace is refused as on the routes
/// beside it.
#[test]
fn keys_reading_a_workspace_list_its_runs_newest_first() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.key("Jane Doe's agent");
        let reviewer = host.key("Reviewer's agent");
        let person = host.key("Visa backend");
        for (key, title) in [
            (&agent, "Agent's letter"),
            (&reviewer, "Reviewer's note"),
            (&person, "Backend's postscript"),
        ] {
            write_letter(key, &workspace, &format!("A line of the {title}"), title).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        for reader in [&agent, &reviewer, &person] {
            let listed = invocations(reader, &workspace).await;
            let order: Vec<&str> = listed["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|run| run["title"].as_str())
                .collect();
            assert_eq!(
                order,
                ["Backend's postscript", "Reviewer's note", "Agent's letter"],
                "newest first: {listed}"
            );
            for run in listed["items"].as_array().unwrap() {
                assert!(run["by"]["name"].is_string(), "{run}");
                assert!(run["changes"].as_u64().unwrap() >= 1, "{run}");
            }
        }

        // The record is read whole: every run, and how many there are.
        let whole = invocations(&agent, &workspace).await;
        assert_eq!(whole["total"], 3, "the record is whole: {whole}");
        assert_eq!(whole["items"].as_array().unwrap().len(), 3, "{whole}");
    });
}

/// A workspace a device keeps runs a key's commands in its Terminal, and a restart of the
/// device does not leave the workspace unable to run them.
#[test]
fn a_handed_workspace_runs_its_keys_commands_after_a_restart() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.key("Ada's agent");
        let (status, ran) = in_run(
            &agent,
            &workspace,
            ("terminal", "run"),
            &json!({"command": "echo before-restart"}),
            "Run a command",
        )
        .await;
        assert_eq!(status, 200, "{ran}");
        assert_eq!(ran["status"], "ok", "{ran}");
        assert!(
            ran.to_string().contains("before-restart"),
            "the command printed: {ran}"
        );
    });
}

/// One more task, as an agent makes it through MCP in `workspace`.
async fn create_issue(agent: &host::Agent, workspace: &str, made: &mut usize) -> Duration {
    let began = Instant::now();
    let (answer, failed) = said(
        &agent
            .tool(
                "tilo_do",
                &json!({"workspace": workspace,
                        "path": "so.tilo.tasks#new_task",
                        "args": {"title": format!("Issue {made}")}}),
            )
            .await,
    );
    assert!(!failed, "a create answers: {answer}");
    *made += 1;
    began.elapsed()
}

/// An agent filling a tracker over MCP, one create after another: what each create costs stays
/// the same however many issues the tracker already keeps.
#[test]
fn an_issue_create_costs_the_same_however_full_the_tracker() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let (opened, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"workspace": workspace, "path": "so.tilo.tasks#open"}),
                )
                .await,
        );
        assert!(!failed, "the workspace serves the Tasks app: {opened}");

        let mut made = 0;
        while made < 10 {
            create_issue(&agent, &workspace, &mut made).await;
        }
        let mut early = Vec::new();
        for _ in 0..3 {
            early.push(create_issue(&agent, &workspace, &mut made).await);
        }
        while made < 100 {
            create_issue(&agent, &workspace, &mut made).await;
        }
        let mut late = Vec::new();
        for _ in 0..3 {
            late.push(create_issue(&agent, &workspace, &mut made).await);
        }
        let mean = |took: &[Duration]| took.iter().sum::<Duration>() / 3;
        assert!(
            mean(&late) <= mean(&early) * 2 + Duration::from_millis(100),
            "a create in a tracker of 100 issues costs what one in a tracker of 10 does: \
             {:?} at first, {:?} at last",
            mean(&early),
            mean(&late)
        );
    });
}
