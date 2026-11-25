//! A write is answered as the workspace decided it: one it takes is done, and one it refuses is
//! answered failed, with why, never as saved. Asked of the daemon by the person's own computer
//! over its one session, and by a member's device over the link; and a problem report filed
//! through Chat reads back in the workspace's #feedback channel.

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ActionCallError;

use crate::language::guests::guest;
use crate::language::host;

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
        }
        let (issues, is_error) = said(
            &computer
                .tool(
                    "tilo_read",
                    &json!({"path": "so.tilo.tasks/tl:so.tilo.tasks"}),
                )
                .await,
        );
        assert!(!is_error, "{issues}");
        for title in ["First", "Second", "Third"] {
            assert!(
                issues.contains(title),
                "the workspace keeps {title}: {issues}"
            );
        }
    });
}

/// A member's device takes exactly what their role lets them, in a workspace someone else owns:
/// a viewer reads the issues and writes nothing, a commenter comments on one and creates none.
/// What the role refuses is answered refused, saying why, never as no such action.
#[test]
fn a_members_device_takes_what_its_role_lets_it() {
    host::run(|host| async move {
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

        let tasks = format!(
            "{}:so.tilo.tasks:{}",
            tilo_actions::remote::RESOURCE_PROVIDER,
            collab_types::document::DocumentId::workspace_default().as_str()
        );
        let refused = |called: &Result<InvocationUpdate, ActionCallError>| {
            matches!(called, Ok(InvocationUpdate::Failed { .. }))
                || matches!(called, Err(ActionCallError::NotPermitted))
        };

        let viewer = guest(&host, &owner_device, "viewer", "Vic").await;
        let read = viewer
            .call(
                &tasks,
                tilo_actions::ActionAddress::new("tasks.issues", "read"),
                &json!({}),
            )
            .await
            .expect("a viewer reads the issues");
        assert!(
            matches!(read, InvocationUpdate::Succeeded { .. }),
            "a viewer reads the issues: {read:?}"
        );
        let created = viewer
            .call(
                &tasks,
                tilo_actions::ActionAddress::new("tasks.issues", "create"),
                &json!({"title": "Not theirs"}),
            )
            .await;
        assert!(refused(&created), "a viewer creates no issue: {created:?}");

        let commenter = guest(&host, &owner_device, "commenter", "Cyd").await;
        let created = commenter
            .call(
                &tasks,
                tilo_actions::ActionAddress::new("tasks.issues", "create"),
                &json!({"title": "Not theirs"}),
            )
            .await;
        assert!(
            refused(&created),
            "a commenter creates no issue: {created:?}"
        );
        let comment = commenter
            .call(
                &tasks,
                tilo_actions::ActionAddress::new("tasks.issues", "comment"),
                &json!({"issue": issue, "text": "Seen by Cyd"}),
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
/// commenter who files a report is refused triage, which is the workspace's moderators'.
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

        // A commenter files a report of their own, and is refused triage: settling a report is
        // the workspace's moderators'.
        let filed_by = guest(&host, &owner_device, "commenter", "Cyd").await;
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
            .expect("a commenter files a report");
        let InvocationUpdate::Succeeded { output, .. } = &filed else {
            panic!("the commenter's report is filed: {filed:?}");
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
                || matches!(triage, Err(ActionCallError::NotPermitted)),
            "a commenter does not triage: {triage:?}"
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
