//! The invocations of a workspace, as the path `workspace/invocations` of
//! every workspace a key acts in: `tilo_read workspace/invocations` answers
//! the newest, and `tilo_ls workspace/invocations {limit, cursor}` a page of
//! them.
//!
//! Each says who made it, how it stands (`waiting` for the person, `running`,
//! `ended`, `failed`, `cancelled`, `undone`), when, what it changed and whether
//! it can still be taken back; its `id` is what `tilo_status` reads, stops and
//! takes back. The workspace's host answers it from the history its records
//! keep, which outlasts a restart of the host, and the calls it remembers
//! ([`tilo_actions::remote::workspace_invocations`]). A key reads, whoever made
//! them, the invocations in records it may read: of each only what lies in
//! those records, and none of the rest, not even as a count.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use serde_json::{Value, json};
use std::fmt::Write as _;
use tilo_actions::remote::ActionHost as _;
use tilo_actions::remote::workspace_invocations::{self, Invocations, InvocationsQuery};
use tilo_actions::remote::{ActionHostError, InvokeArgument, InvokeRequest, ReceiptStatus};

/// The path of a workspace's invocations.
pub const ROOT: &str = "workspace/invocations";

/// What every MCP client is told of it, beside `tilo_status`.
pub const INSTRUCTIONS: &str = "`tilo_read workspace/invocations` lists the workspace's invocations in \
records you may read, newest first, whoever made them (`tilo_ls workspace/invocations` with `limit` and \
`cursor` pages on): who made it, how it stands (`waiting` for the person, `running`, `ended`, `failed`, \
`cancelled`, `undone`), when, what it changed and whether it can still be taken back. Its `id` is the \
invocation `tilo_status` reads, stops (`cancel`) and takes back (`undo`).";

/// Whether `tool` with `arguments` reads the workspace's invocations.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_ls" | "tilo_read")
        && arguments.get("path").and_then(Value::as_str) == Some(ROOT)
}

/// The listing as a person reads it, one line per invocation.
fn said(invocations: &Invocations) -> String {
    let mut text = format!(
        "{} {}, newest first{}",
        invocations.total,
        if invocations.total == 1 {
            "invocation"
        } else {
            "invocations"
        },
        if invocations.waiting > 0 {
            format!("; {} waiting for the person", invocations.waiting)
        } else {
            String::new()
        }
    );
    for listed in &invocations.items {
        let id = listed
            .id
            .as_deref()
            .or(listed.step.as_deref())
            .unwrap_or_default();
        let status = serde_json::to_value(listed.status)
            .ok()
            .and_then(|status| status.as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ = write!(
            text,
            "\n{id} {status} by {} ({})",
            listed.by.name, listed.by.kind
        );
        if let Some(title) = &listed.title {
            let _ = write!(text, " \"{title}\"");
        }
        let _ = write!(
            text,
            ": {} {}",
            listed.changes,
            if listed.changes == 1 {
                "change"
            } else {
                "changes"
            }
        );
        if listed.undone > 0 {
            let _ = write!(text, ", {} undone", listed.undone);
        }
        if let Some(summary) = &listed.summary {
            let _ = write!(text, "; {summary}");
        } else if let Some(action) = &listed.action {
            let _ = write!(text, "; {action}");
        }
    }
    if let Some(cursor) = &invocations.next_cursor {
        let _ = write!(
            text,
            "\nmore: tilo_ls {{path: \"{ROOT}\", cursor: \"{cursor}\"}}"
        );
    }
    text
}

/// What the host's refusal `error` tells the caller.
fn host_refused(error: &ActionHostError) -> Outcome {
    match error {
        ActionHostError::NotPermitted => failed(
            "not_permitted",
            "the workspace does not let this key read its invocations",
            "a key reaching the workspace reads them",
        ),
        ActionHostError::InvalidRequest { reason } => failed(
            "invalid_arguments",
            reason.clone(),
            "pass `limit` (at most 200) and the `cursor` a page answered",
        ),
        other => failed("unavailable", other.to_string(), "try again shortly"),
    }
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s invocations, typed when
    /// `structured`.
    pub(super) async fn invocations(
        &self,
        workspace: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let query = InvocationsQuery {
            limit: arguments
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|limit| usize::try_from(limit).ok())
                .unwrap_or(0),
            cursor: arguments
                .get("cursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned),
            ..InvocationsQuery::default()
        };
        match self.list_invocations(workspace, &query).await {
            Ok(listed) => {
                // The lines a person reads, beside the typed listing when it
                // was asked for, as a listing of resources answers.
                let typed = serde_json::to_value(&listed).unwrap_or_default();
                let mut answered = json!(
                    {"content": [{"type": "text", "text": said(&listed)}], "isError": false}
                );
                if structured && let Some(fields) = answered.as_object_mut() {
                    fields.insert("structuredContent".into(), typed);
                }
                answered
            }
            Err(outcome) => answer(structured, ROOT, outcome),
        }
    }

    /// The page of `workspace`'s invocations `query` asks for, as its host
    /// answers it.
    async fn list_invocations(
        &self,
        workspace: &str,
        query: &InvocationsQuery,
    ) -> Result<Invocations, Outcome> {
        let connection = self.connection().map_err(|error| refused(&error))?;
        let id = connection
            .server()
            .enter(connection, workspace)
            .await
            .map_err(|error| refused(&error))?;
        let argument = match serde_json::to_value(query) {
            Ok(Value::Object(fields)) => InvokeArgument::Json(fields),
            _ => InvokeArgument::None,
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: workspace_invocations::target(),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let unanswered = || failed("unavailable", "the workspace did not answer", "try again");
        let receipt = connection
            .device
            .actions(id)
            .invoke(&self.caller(), request)
            .await
            .ok_or_else(unanswered)?
            .map_err(|error| failed("unavailable", error.to_string(), "try again"))?;
        match receipt.status {
            ReceiptStatus::Completed { output } => output
                .value
                .and_then(|value| serde_json::from_value::<Invocations>(value).ok())
                .ok_or_else(|| {
                    failed(
                        "unavailable",
                        "the workspace's host answered no listing of its invocations",
                        "a workspace one of your devices hosts lists its invocations in its Activity there",
                    )
                }),
            ReceiptStatus::Rejected { error } => Err(match error {
                // A host that knows no listing of items: older, or not one that
                // keeps invocations.
                ActionHostError::UnknownAction => failed(
                    "not_found",
                    "this workspace's host lists no invocations",
                    "tilo_status {invocation_id} reads one invocation",
                ),
                other => host_refused(&other),
            }),
            ReceiptStatus::Failed { .. }
            | ReceiptStatus::Accepted
            | ReceiptStatus::Cancelled { .. } => Err(unanswered()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::workspace_invocations::{Invocation, InvocationBy, InvocationStatus};

    #[test]
    fn a_listing_reads_one_line_per_invocation_and_where_the_next_page_starts() {
        let listed = Invocations {
            items: vec![
                Invocation {
                    id: Some("8f1c".into()),
                    by: InvocationBy {
                        name: "Jane Doe's agent".into(),
                        kind: "agent".into(),
                        ..InvocationBy::default()
                    },
                    status: InvocationStatus::Waiting,
                    action: Some("terminal#run".into()),
                    ..Invocation::default()
                },
                Invocation {
                    step: Some("5d2a".into()),
                    by: InvocationBy {
                        name: "Visa backend".into(),
                        kind: "agent".into(),
                        ..InvocationBy::default()
                    },
                    title: Some("Write the letter".into()),
                    status: InvocationStatus::Undone,
                    changes: 2,
                    undone: 2,
                    summary: Some("Added a paragraph".into()),
                    ..Invocation::default()
                },
            ],
            total: 3,
            waiting: 1,
            pending: false,
            next_cursor: Some("17:5d2a".into()),
        };
        assert_eq!(
            said(&listed),
            "3 invocations, newest first; 1 waiting for the person\n\
             8f1c waiting by Jane Doe's agent (agent): 0 changes; terminal#run\n\
             5d2a undone by Visa backend (agent) \"Write the letter\": 2 changes, 2 undone; \
             Added a paragraph\n\
             more: tilo_ls {path: \"workspace/invocations\", cursor: \"17:5d2a\"}"
        );
        assert!(addresses("tilo_read", &json!({"path": ROOT})));
        assert!(addresses("tilo_ls", &json!({"path": ROOT, "limit": 2})));
        assert!(!addresses("tilo_do", &json!({"path": ROOT})));
        assert!(!addresses(
            "tilo_read",
            &json!({"path": "workspace/invocations/x"})
        ));
    }
}
