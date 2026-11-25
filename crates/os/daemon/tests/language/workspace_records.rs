//! The records a workspace keeps for its apps, reached the way a person, an agent and another
//! device reach them on the daemon that keeps the workspace: the device's own MCP session, a key
//! over `/v1`, and a guest admitted by invitation over the device link. A create answers the
//! record it made and it reads back; the first write into a chain the workspace keeps no part of
//! is taken; content goes in and the workspace keeps it; what a role refuses is answered
//! refused, saying why; and what an app keeps for the whole workspace is offered once the
//! workspace keeps it.

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ActionCallError;

use crate::language::guests::guest;
use crate::language::host;

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

/// `name` of a tool's answer: typed, or in the JSON its text carries.
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

/// The provider that names the first record `app` keeps in the workspace.
fn record(app: &str) -> String {
    format!(
        "{}:{app}:{}",
        tilo_actions::remote::RESOURCE_PROVIDER,
        collab_types::document::DocumentId::workspace_default().as_str()
    )
}

/// The provider that names the one record an app keeps for its whole workspace.
fn workspace_record(app: &str) -> String {
    format!("{}:{app}:", tilo_actions::remote::RESOURCE_PROVIDER)
}

/// A tracker's create, answered by the device's own agent, answers the issue it made, and it
/// reads back there; a second create answers its own.
#[test]
fn a_create_answers_the_issue_it_made_and_the_issue_reads_back() {
    host::run(|host| async move {
        let agent = host.agent();
        let mut made = Vec::new();
        for title in ["Plan the launch", "Ship it"] {
            let (answer, is_error) = said(
                &agent
                    .tool(
                        "tilo_do",
                        &json!({"path": "so.tilo.tasks#new_task", "args": {"title": title}}),
                    )
                    .await,
            );
            assert!(!is_error, "the create is made: {answer}");
            assert!(answer.contains(title), "the answer names it: {answer}");
            made.push(answer);
        }
        assert_ne!(
            made[0], made[1],
            "each create answers the issue it made: {made:?}"
        );
        let (issues, is_error) = said(
            &agent
                .tool(
                    "tilo_read",
                    &json!({"path": "so.tilo.tasks/tl:so.tilo.tasks"}),
                )
                .await,
        );
        assert!(!is_error, "{issues}");
        for title in ["Plan the launch", "Ship it"] {
            assert!(issues.contains(title), "it reads back: {issues}");
        }
    });
}

/// The first write into a chain the workspace keeps no part of is taken: a tracker that keeps no
/// issue yet takes its first.
#[test]
fn the_first_write_into_an_empty_chain_is_taken() {
    host::run(|host| async move {
        let agent = host.agent();
        let (filed, is_error) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "so.tilo.tasks#new_task", "args": {"title": "Ship the launch"}}),
                )
                .await,
        );
        assert!(
            !is_error,
            "the first issue of a tracker that keeps none is filed: {filed}"
        );
    });
}

/// A member's first write founds the part of the chain the workspace keeps none of: a guest the
/// owner admitted as an editor writes the first issue, and it is taken.
#[test]
fn a_members_first_write_founds_a_chain_the_workspace_keeps_no_part_of() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let editor = guest(&host, &owner_device, "editor", "Grace").await;
        let then = editor
            .call(
                &record("so.tilo.tasks"),
                tilo_actions::ActionAddress::new("tasks.issues", "create"),
                &json!({"title": "The first issue"}),
            )
            .await
            .expect("the workspace answers the member");
        let InvocationUpdate::Succeeded { output, .. } = &then else {
            panic!("the first write founds the chain: {then:?}");
        };
        assert!(
            output
                .value
                .as_ref()
                .and_then(|value| value.get("made"))
                .and_then(Value::as_array)
                .is_some_and(|made| made.len() == 1),
            "the create answers the issue it made: {output:?}"
        );
    });
}

/// A write naming the oldest of many issues lands within a bound after the daemon restarts: the
/// tracker's issues fill more than one part of their chain, and the daemon asks for every part of
/// it as soon as it opens the tracker, so none is waited for.
#[test]
fn a_write_under_the_oldest_of_many_issues_lands() {
    host::run(|host| async move {
        let bound = std::time::Duration::from_secs(15);
        let mut oldest = Value::Null;
        {
            let agent = host.agent();
            for first in (0..2200).step_by(200) {
                let titles: Vec<Value> = (first..first + 200)
                    .map(|at| json!(format!("Issue {at}")))
                    .collect();
                let answer = agent
                    .tool(
                        "tilo_do",
                        &json!({"path": "so.tilo.tasks#new_tasks", "args": {"titles": titles}}),
                    )
                    .await;
                let (text, is_error) = said(&answer);
                assert!(!is_error, "the workspace takes new_tasks: {text}");
                let made = field(&answer, "resources");
                assert_eq!(
                    made.as_array().map(Vec::len),
                    Some(200),
                    "a create answers what it made: {text}"
                );
                if oldest.is_null() {
                    oldest = made[0].clone();
                }
            }
        }
        // The daemon restarts on the same home: its agents keep nothing of what they were sent.
        host.restart().await;
        let agent = host.agent();
        let started = std::time::Instant::now();
        loop {
            let (answer, is_error) = said(
                &agent
                    .tool(
                        "tilo_do",
                        &json!({
                            "path": format!(
                                "so.tilo.tasks/{}#add_subtask",
                                oldest.as_str().unwrap_or_default()
                            ),
                            "args": {"title": "Under the oldest"},
                        }),
                    )
                    .await,
            );
            if !is_error {
                assert!(
                    started.elapsed() < bound,
                    "the write landed within the bound"
                );
                break;
            }
            assert!(
                started.elapsed() < bound,
                "a write under the oldest issue did not land within {bound:?} of the restart: \
                 {answer}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}

/// Putting content into a workspace is not the workspace's thread's work: a member's content
/// steps answer in order, and the workspace keeps what they put.
#[test]
fn a_members_content_steps_answer_and_the_workspace_keeps_what_they_put() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let begun = key
            .tool(
                "tilo_put_begin",
                &json!({"workspace": workspace, "size": 10}),
            )
            .await;
        let (begun_said, is_error) = said(&begun);
        assert!(!is_error, "an upload begins: {begun_said}");
        let upload = field(&begun, "upload");
        assert!(upload.is_u64(), "an upload is named: {begun_said}");
        let (appended, is_error) = said(
            &key.tool(
                "tilo_put_append",
                &json!({"workspace": workspace, "upload": upload.clone(), "offset": 0,
                        "chunk": "Zmlyc3QgZmlsZQ=="}),
            )
            .await,
        );
        assert!(!is_error, "the step is answered: {appended}");
        let finished = key
            .tool(
                "tilo_put_finish",
                &json!({"workspace": workspace, "upload": upload}),
            )
            .await;
        let (finish_said, is_error) = said(&finished);
        assert!(!is_error, "the upload finishes: {finish_said}");
        let content = field(&finished, "content");
        let reference = content
            .as_str()
            .unwrap_or_else(|| panic!("the upload answers its content: {finish_said}"))
            .to_owned();
        assert!(
            reference.starts_with("tilo.content:"),
            "the put answers a content reference: {finish_said}"
        );
        let hash = reference.trim_start_matches("tilo.content:").to_owned();
        let (kept, is_error) = said(
            &key.tool(
                "tilo_put_have",
                &json!({"workspace": workspace, "hashes": [hash]}),
            )
            .await,
        );
        assert!(!is_error, "{kept}");
        assert!(kept.contains("true"), "the workspace keeps it: {kept}");
    });
}

/// A commenter comments and a moderator makes a channel, by the role the workspace's governance
/// names for them; the owner files the issue the comment is on.
#[test]
fn a_commenter_comments_and_a_moderator_makes_a_channel() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let owner = host.agent();
        let filed = owner
            .tool(
                "tilo_do",
                &json!({"path": "so.tilo.tasks#new_task", "args": {"title": "Ship the launch"}}),
            )
            .await;
        let (words, is_error) = said(&filed);
        assert!(!is_error, "{words}");
        let issue = field(&filed, "made")
            .as_array()
            .map_or(Value::Null, |made| made[0]["id"].clone());
        assert!(!issue.is_null(), "the issue is answered: {words}");

        // A commenter's comment on it, through the device link as the guest their person is.
        let commenter = guest(&host, &owner_device, "commenter", "Cyd").await;
        let comment = commenter
            .call(
                &record("so.tilo.tasks"),
                tilo_actions::ActionAddress::new("tasks.issues", "comment"),
                &json!({"issue": issue, "text": "Looks good"}),
            )
            .await
            .expect("a commenter's comment answers");
        assert!(
            matches!(comment, InvocationUpdate::Succeeded { .. }),
            "a commenter's comment is saved: {comment:?}"
        );

        // A moderator's new channel in Chat.
        let moderator = guest(&host, &owner_device, "moderator", "Mo").await;
        let made = moderator
            .call(
                &workspace_record("so.tilo.chat"),
                tilo_actions::ActionAddress::new("chat.channels", "create-channel"),
                &json!({"name": "launch"}),
            )
            .await
            .expect("a moderator's new channel answers");
        assert!(
            matches!(made, InvocationUpdate::Succeeded { .. }),
            "a moderator's new channel is saved: {made:?}"
        );
    });
}

/// A carrier names no edit its caller is then refused: Chat's listings name a new channel to a
/// moderator and not to an editor, and an editor's call naming one anyway is refused.
#[test]
fn a_carrier_names_no_edit_its_caller_is_then_refused() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let chat = workspace_record("so.tilo.chat");
        let named = |listing: &[tilo_actions::ActionDescriptor]| {
            listing
                .iter()
                .any(|offered| offered.address.action == "create-channel")
        };

        // A new channel is a moderator's: their listing names it, and their call is taken.
        let moderator = guest(&host, &owner_device, "moderator", "Mo").await;
        assert!(
            named(&moderator.listed(&chat).await),
            "a moderator is offered a new channel"
        );
        let made = moderator
            .call(
                &chat,
                tilo_actions::ActionAddress::new("chat.channels", "create-channel"),
                &json!({"name": "launch"}),
            )
            .await
            .expect("a moderator's new channel answers");
        assert!(
            matches!(made, InvocationUpdate::Succeeded { .. }),
            "a moderator's new channel is saved: {made:?}"
        );

        // An editor's listing does not, and an editor's call naming one anyway is refused.
        let editor = guest(&host, &owner_device, "editor", "Ed").await;
        assert!(
            !named(&editor.listed(&chat).await),
            "an editor is offered no new channel"
        );
        let refused = editor
            .call(
                &chat,
                tilo_actions::ActionAddress::new("chat.channels", "create-channel"),
                &json!({"name": "launch"}),
            )
            .await;
        assert!(
            matches!(refused, Ok(InvocationUpdate::Failed { .. }))
                || matches!(refused, Err(ActionCallError::NotPermitted)),
            "an editor makes no channel: {refused:?}"
        );
    });
}

/// What Chat keeps for the whole workspace is offered once the workspace keeps it, and nobody
/// the workspace does not admit is offered it.
#[test]
fn chats_document_offers_what_chat_keeps_for_the_workspace_once_told() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let (opened, is_error) = said(
            &agent
                .tool("tilo_do", &json!({"path": "chat#open", "args": {}}))
                .await,
        );
        assert!(!is_error, "Chat opens on the workspace: {opened}");
        let (listed, is_error) = said(&agent.tool("tilo_ls", &json!({"path": "chat"})).await);
        assert!(!is_error, "{listed}");
        for kept in ["emoji", "agents"] {
            assert!(
                listed.contains(kept),
                "Chat offers what it keeps for the workspace (`{kept}`): {listed}"
            );
        }
        let visitor = host.visitor(&workspace).await;
        let refused = visitor
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "member.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "{refused:?}"
        );
    });
}

/// A sealed record is acted on for its owner and no one else: the owner's agent opens a Writer
/// record and acts on it, and a guest the owner admitted as an editor sees the change through
/// their own device and makes their own edit as theirs.
#[test]
fn a_sealed_record_is_acted_on_for_its_owner_and_no_one_else() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let owner = host.agent();
        let (opened, is_error) = said(
            &owner
                .tool(
                    "tilo_do",
                    &json!({"path": "word-processor#open", "args": {}}),
                )
                .await,
        );
        assert!(!is_error, "the owner opens a Writer record: {opened}");
        let (made, is_error) = said(
            &owner
                .tool(
                    "tilo_do",
                    &json!({"path": "word-processor#append-paragraph", "args": {"text": "Through the host"}}),
                )
                .await,
        );
        assert!(!is_error, "the owner's paragraph is made: {made}");

        let editor = guest(&host, &owner_device, "editor", "Bo").await;
        let seen = editor
            .call(
                &record("so.tilo.word-processor"),
                tilo_actions::ActionAddress::new("word-processor.document", "append-paragraph"),
                &json!({"text": "By the member"}),
            )
            .await
            .expect("an editor's edit answers");
        assert!(
            matches!(seen, InvocationUpdate::Succeeded { .. }),
            "an editor's edit runs: {seen:?}"
        );
        let (read, is_error) = said(
            &owner
                .tool("tilo_read", &json!({"path": "word-processor"}))
                .await,
        );
        assert!(!is_error, "{read}");
        for name in ["Through the host", "By the member"] {
            assert!(read.contains(name), "the document reads back: {read}");
        }
    });
}

/// A watch of the whole workspace on the person's dispatch is woken by a task a call made, and
/// the workspace's feed then tells of it, named by the app it is a record of: what `tilo watch`
/// and the SDKs' watches read.
#[test]
fn a_watch_of_the_workspace_is_woken_and_told_of_a_task_a_call_made() {
    host::run(|host| async move {
        let (dispatch, caller) = (host.dispatch(), host.caller());
        let feed = dispatch
            .change_feed()
            .expect("the daemon keeps its workspace's changes");
        let from = feed
            .since(&caller, None)
            .await
            .expect("the feed answers")
            .expect("the person reads their workspace's changes");
        let woken = dispatch
            .changed(&caller, tilo_actions::remote::WatchScope::Records)
            .expect("a watch of the workspace's records is woken by its writes");
        let title = "Told to the watch";
        let (answer, is_error) = said(
            &host
                .agent()
                .tool(
                    "tilo_do",
                    &json!({"path": "so.tilo.tasks#new_task", "args": {"title": title}}),
                )
                .await,
        );
        assert!(!is_error, "the task is made: {answer}");
        tilo_test_env::within("the watch is woken by the task", woken)
            .await
            .expect("the wake answers")
            .expect("the watch is woken");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let page = feed
                .since(&caller, Some(&from.cursor))
                .await
                .expect("the feed answers")
                .expect("the person reads their workspace's changes");
            if page
                .changes
                .iter()
                .any(|change| change.app == "so.tilo.tasks")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the feed tells of the task: {page:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
}
