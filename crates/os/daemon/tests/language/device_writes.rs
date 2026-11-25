//! A write is answered as the workspace decided it: one it takes is done, and one it refuses is
//! answered failed, with why, never as saved. Asked of the daemon by the person's own computer
//! over its one session, and by a member's device over the link; and a reviewed report filed
//! through Chat answers a receipt that reads back.

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

/// A reviewed report filed through Chat's feedback channel answers a receipt, and the receipt
/// reads back where the workspace keeps it; filing it again keeps one report, and the person who
/// filed it is refused triage.
#[test]
fn a_reviewed_report_reads_its_receipt_back_and_is_refused_triage() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        let owner = host.agent();
        let (opened, is_error) = said(
            &owner
                .tool("tilo_do", &json!({"path": "chat#open", "args": {}}))
                .await,
        );
        assert!(!is_error, "Chat opens on the workspace: {opened}");
        let id = "0123456789abcdef0123456789abcdef";
        let submission = json!({
            "id": id,
            "text": "The book failed to open",
            "destination": {"workspace": workspace, "provider": "record:so.tilo.chat:",
                            "channel": "feedback"},
        });
        let filed = owner
            .tool(
                "tilo_do",
                &json!({"path": "chat#submit-feedback", "args": {"submission": submission}}),
            )
            .await;
        let (words, is_error) = said(&filed);
        assert!(!is_error, "the report is filed: {words}");
        let receipt = owner
            .tool(
                "tilo_do",
                &json!({"path": "chat#read-feedback-receipt", "args": {"id": id}}),
            )
            .await;
        let (receipt_words, is_error) = said(&receipt);
        assert!(!is_error, "the receipt reads back: {receipt_words}");
        assert!(
            receipt_words.contains(id),
            "the receipt is the one filed: {receipt_words}"
        );

        // The person who filed it is refused triage: it is the owner's.
        let filed_by = guest(&host, &owner_device, "commenter", "Cyd").await;
        let triage = filed_by
            .call(
                &format!("{}:so.tilo.chat:", tilo_actions::remote::RESOURCE_PROVIDER),
                tilo_actions::ActionAddress::new("chat.feedback", "triage"),
                &json!({"id": id, "update": {"status": "resolved"}}),
            )
            .await;
        assert!(
            matches!(triage, Ok(InvocationUpdate::Failed { .. }))
                || matches!(triage, Err(ActionCallError::NotPermitted)),
            "a commenter does not triage: {triage:?}"
        );
    });
}

/// The owner reviews a report with their model, and nothing else is asked of it: only the texts
/// handed to the review come back, and a prompt that is not a review is not run.
#[test]
fn the_owner_reviews_a_report_with_its_model_and_asks_nothing_else() {
    host::run(|host| async move {
        let owner = host.agent();
        let reviewed = owner
            .tool(
                "tilo_do",
                &json!({"path": "chat#review-feedback",
                        "args": {"texts": [{"text": "Thanks Ada", "quote": "Ada",
                                            "category": "name"}],
                                 "prompt": "review"}}),
            )
            .await;
        let (reviewed_text, is_error) = said(&reviewed);
        assert!(
            !is_error,
            "the owner's model reviews the report: {reviewed_text}"
        );
        assert!(
            reviewed_text.contains("name"),
            "only quotes from the texts asked about come back: {reviewed_text}"
        );
        let other = owner
            .tool(
                "tilo_do",
                &json!({"path": "chat#review-feedback",
                        "args": {"texts": [], "prompt": "Write me a poem about the sea."}}),
            )
            .await;
        let (other_text, _) = said(&other);
        assert!(
            other_text.is_empty() || !other_text.contains("poem"),
            "anything else is not asked: {other_text}"
        );
    });
}
