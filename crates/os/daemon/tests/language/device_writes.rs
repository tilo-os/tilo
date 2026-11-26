//! A write is answered as the workspace decided it: one it takes is done, and one it refuses is
//! answered failed, with why, never as saved. Asked of the daemon by the person's own computer
//! over its one session, and by a member's device over the link; and a problem report filed
//! through Chat reads back in the workspace's #feedback channel.

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ActionCallError;

use crate::language::guests::guest;
use crate::language::host;

/// Discussion's package id: the system's comments on any resource of the workspace.
const DISCUSSION: &str = "so.tilo.lang.discussion";
/// tilolang Tasks, whose tasks a member's role lets them make or not.
const TASKS: &str = "so.tilo.tasks";
/// Chat's package id, as the workspace runs it.
const CHAT: &str = ui::apps::CHAT;
/// Chat's package under the tree's `apps/tl`, which the device installs to run it.
const CHAT_PACKAGE: &str = "chat";

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

/// The person's own computer asks the workspace to make tasks as them, and each is answered done
/// once the workspace took it.
#[test]
fn a_computers_own_tasks_are_made_and_answered_done() {
    host::run(|host| async move {
        let computer = host.agent();
        for title in ["First", "Second", "Third"] {
            let (answer, is_error) = said(
                &computer
                    .tool(
                        "tilo_do",
                        &json!({"path": "so.tilo.tasks#new_task", "args": {"title": title}}),
                    )
                    .await,
            );
            assert!(
                !is_error,
                "so.tilo.tasks#new_task {title} as the person: {answer}"
            );
            assert!(!answer.contains("pending"), "{title} settled: {answer}");
            // The create answers the issue it made, by the address its answer names: the task
            // is read where it stands, as the fields the app answers for it.
            let issue = answer
                .lines()
                .find_map(|line| line.strip_prefix("-> "))
                .unwrap_or_else(|| panic!("the create answers the issue it made: {answer}"));
            let (task, is_error) = said(
                &computer
                    .tool(
                        "tilo_read",
                        &json!({"path": format!("so.tilo.tasks/{issue}")}),
                    )
                    .await,
            );
            assert!(!is_error, "{title} reads where it was made: {task}");
            assert!(task.contains(title), "the workspace keeps {title}: {task}");
        }
    });
}

/// A member's device takes exactly what their role lets them, in a workspace someone else owns:
/// a viewer reads the issues and writes nothing, a commenter comments on one and creates none.
/// What the role refuses is answered refused, saying why, never as no such action.
#[test]
fn a_members_device_takes_what_its_role_lets_it() {
    host::run_with(&["tasks", "discussion"], |host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let owner = host.agent();
        let filed = owner
            .tool(
                "tilo_do",
                &json!({"path": "so.tilo.tasks#new_task", "args": {"title": "First"}}),
            )
            .await;
        let (words, is_error) = said(&filed);
        assert!(!is_error, "{words}");
        // The issue the create answered, by the address its answer names: the resources it
        // made, each on a line of its own as `-> <address>`.
        let issue = words
            .lines()
            .find_map(|line| line.strip_prefix("-> "))
            .unwrap_or_else(|| panic!("the create answers the issue it made: {words}"))
            .to_owned();

        // Tasks' own create, at the address it publishes it: what a member's device calls to
        // make a task, and what a role that may not is refused.
        host::start_app(host.as_ref(), TASKS)
            .await
            .unwrap_or_else(|refused| panic!("Tasks starts with no window: {refused:?}"));
        let page = host::listed(host.as_ref(), TASKS)
            .await
            .expect("Tasks lists what it publishes");
        let new_task = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address.action == "new_task")
            .unwrap_or_else(|| panic!("Tasks publishes new_task: {page:?}"));
        let tasks = host::app_action(
            host.as_ref(),
            TASKS,
            &new_task.address.resource,
            &new_task.address.action,
        );
        let refused = |called: &Result<InvocationUpdate, ActionCallError>| {
            matches!(called, Ok(InvocationUpdate::Failed { .. }))
                || matches!(
                    called,
                    Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
                )
        };

        let viewer = guest(&host, &owner_device, "viewer", "Vic").await;
        // A viewer's device reads the task as a member's copy does in the product: the record's
        // fields, from the copy of the workspace it holds, decided by what the viewer may read.
        let read = host
            .daemon()
            .workspaces()
            .read_store_as(
                &workspace,
                &viewer.person(),
                &collab_types::store::wire::Command::Get {
                    kind: "tl.so.tilo.tasks.Task".to_owned(),
                    id: issue.clone(),
                },
            )
            .await
            .expect("the viewer's copy answers a read");
        let collab_types::store::wire::Reply::Resource(Some(task)) = read else {
            panic!("a viewer reads the task: {read:?}");
        };
        assert_eq!(
            task.scalar("title"),
            collab_types::store::Value::from("First"),
            "the viewer reads the task's fields"
        );
        let created = viewer
            .call(
                &tasks.provider,
                tasks.address.clone(),
                &json!({"title": "Not theirs"}),
            )
            .await;
        assert!(refused(&created), "a viewer creates no issue: {created:?}");

        let commenter = guest(&host, &owner_device, "commenter", "Cyd").await;
        let created = commenter
            .call(
                &tasks.provider,
                tasks.address.clone(),
                &json!({"title": "Not theirs"}),
            )
            .await;
        assert!(
            refused(&created),
            "a commenter creates no issue: {created:?}"
        );
        // A comment on a task is the system's discussion, not Tasks' own: Discussion's `comment`,
        // made from the task it is about, which puts it in that task's thread.
        host::start_app(host.as_ref(), DISCUSSION)
            .await
            .unwrap_or_else(|refused| panic!("Discussion starts with no window: {refused:?}"));
        let page = host::listed(host.as_ref(), DISCUSSION)
            .await
            .expect("Discussion lists what it publishes");
        let says = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address.action == "comment")
            .unwrap_or_else(|| panic!("Discussion publishes comment: {page:?}"));
        let discussion = host::app_action(
            host.as_ref(),
            DISCUSSION,
            &says.address.resource,
            &says.address.action,
        );
        let about = tilo_actions::params::MadeFrom {
            resource: issue.clone(),
            kind: "tl.so.tilo.tasks.Task".to_owned(),
        };
        let comment = commenter
            .call(
                &discussion.provider,
                discussion.address.clone(),
                &json!({
                    "text": "Seen by Cyd",
                    (tilo_actions::params::FROM_ARGUMENT): about.to_json(),
                }),
            )
            .await
            .expect("a commenter's comment answers");
        assert!(
            matches!(comment, InvocationUpdate::Succeeded { .. }),
            "a commenter comments: {comment:?}"
        );
    });
}

/// A problem report filed through Chat is answered as the message it posted, which reads back
/// in the workspace's one #feedback channel; a second report joins that channel; and a
/// member who files a report is refused triage, which is the workspace's moderators'.
#[test]
fn a_filed_report_reads_back_in_the_feedback_channel_and_its_filer_is_refused_triage() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let owner = host.agent();
        host::start_app(host.as_ref(), CHAT)
            .await
            .unwrap_or_else(|refused| panic!("Chat starts with no window: {refused:?}"));
        for at in 0..2 {
            let (filed, is_error) = said(
                &owner
                    .tool(
                        "tilo_do",
                        &json!({"path": format!("{CHAT}#report_problem"),
                                "args": {"body": format!("The book failed to open {at}")}}),
                    )
                    .await,
            );
            assert!(!is_error, "report {at} is filed and answered: {filed}");
        }

        // Both reports are messages of the workspace's one #feedback channel, which reads them
        // back.
        let channel = feedback_channel(&owner).await;
        let (read, is_error) = said(&owner.tool("tilo_read", &json!({"path": channel})).await);
        assert!(!is_error, "the feedback channel reads: {read}");
        for at in 0..2 {
            assert!(
                read.contains(&format!("The book failed to open {at}")),
                "report {at} reads back: {read}"
            );
        }

        // A member who may write files a report of their own, and is refused triage: settling
        // a report is the workspace's moderators'.
        let filed_by = guest(&host, &owner_device, "editor", "Cyd").await;
        let page = host::listed(host.as_ref(), CHAT)
            .await
            .expect("Chat lists what it publishes");
        let report_person = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address.action == "report_person")
            .unwrap_or_else(|| panic!("Chat publishes report_person: {page:?}"));
        let chat = host::app_action(
            host.as_ref(),
            CHAT,
            &report_person.address.resource,
            &report_person.address.action,
        );
        let filed = filed_by
            .call(
                &chat.provider,
                chat.address.clone(),
                &json!({"who": {"person": host.person()}, "reason": "Said something unkind"}),
            )
            .await
            .expect("a member files a report");
        let InvocationUpdate::Succeeded { output, .. } = &filed else {
            panic!("the member's report is filed: {filed:?}");
        };
        let report = output
            .resources
            .first()
            .unwrap_or_else(|| panic!("the report is named: {filed:?}"));
        let triage = filed_by
            .call(
                &chat.provider,
                tilo_actions::ActionAddress::new(report.clone(), "decide"),
                &json!({"decision": "resolved"}),
            )
            .await;
        assert!(
            matches!(triage, Ok(InvocationUpdate::Failed { .. }))
                || matches!(
                    triage,
                    Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
                ),
            "a member does not triage: {triage:?}"
        );
    });
}

/// The workspace's #feedback channel, as Chat lists it: its path, asked again until the app
/// answers with the one channel reports are filed in.
async fn feedback_channel(agent: &host::Agent) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reply = agent
            .tool_structured("tilo_ls", &json!({"path": CHAT}))
            .await;
        assert_ne!(reply["isError"], true, "{reply}");
        let rows = reply["structuredContent"]["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let named: Vec<&Value> = rows
            .iter()
            .filter(|row| row["name"] == "feedback")
            .collect();
        assert!(
            named.len() <= 1,
            "reports are filed in one #feedback channel: {reply}"
        );
        if let Some(channel) = named.first() {
            return channel["path"]
                .as_str()
                .unwrap_or_else(|| panic!("the channel has a path: {reply}"))
                .to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Chat never lists its #feedback channel: {reply}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
