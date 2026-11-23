//! The invocations of a workspace a host keeps, newest first, in the records the
//! caller may read: what a key that reaches the workspace reads to see what was
//! invoked there, what waits for the person, and what can still be taken back.
//! Of each invocation the host answers only what lies in records the caller may
//! read now, and an invocation with nothing left for them is neither listed nor
//! counted.
//!
//! A device lists a workspace's agent sessions from its own history index
//! ([`super::workspace_activity`]). A host that keeps a workspace for people
//! and their keys, with no device of theirs, answers from what it holds:
//!
//! - **The records.** Every change an action makes is kept in its record's
//!   history with the undo step it is part of, who made it and when
//!   ([`super::changes`]). An invocation is its step: one call, or every call of a
//!   group its client named. What the records say of it holds across the host's
//!   restarts.
//! - **The calls the host remembers.** The host keeps the invocations it
//!   was asked for in memory, each with its id, who asked, and how it
//!   stands: queued, waiting for the person, running, ended. An invocation that
//!   changed no record (one refused, one still waiting) is known from them
//!   alone, and only until the host restarts or lets go of it.
//!
//! [`target`] asks for one page ([`InvocationsQuery`]); the host answers it at once,
//! running nothing, with [`Invocations`].

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::api::ActionAddress;

/// The provider a host answers its workspace's invocations under.
pub const PROVIDER: &str = "tilo.invocations";
/// The resource the listing is an action of: the workspace.
pub const RESOURCE: &str = "workspace";
/// List the workspace's invocations, newest first.
pub const LIST: &str = "list";
/// How one invocation stands: an action of the invocation, by its id.
pub const GET: &str = "get";
/// Stop one invocation.
pub const CANCEL: &str = "cancel";

/// How many invocations a page holds when the query asks for no number.
pub const PAGE: usize = 50;
/// The most invocations one page holds.
pub const MOST: usize = 200;

/// What [`LIST`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, LIST),
    }
}

/// Whether `target` asks for the listing.
#[must_use]
pub fn is_listing(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && target.address.resource == RESOURCE
        && target.address.action == LIST
}

/// What the workspace root names the invocations by: `invocations` lists them, and
/// `invocations#list` reads a page of them whole.
pub const ROOT_CHILD: &str = "invocations";
/// The resource type of one invocation in the published tree.
pub const INVOCATION_TYPE: &str = "invocation";

/// The publisher a host lists the invocations of `workspace` under, beside its apps: browsed,
/// it is the workspace root's `invocations`.
#[must_use]
pub fn publisher(workspace: &str) -> super::CatalogueProvider {
    super::CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: ROOT_CHILD.to_owned(),
        workspace: workspace.to_owned(),
        browsable: true,
        ..super::CatalogueProvider::default()
    }
}

/// The published tree of a workspace's invocations, as one catalogue page lists it: the
/// workspace's invocations ([`RESOURCE`], which [`LIST`] is the action of), holding one
/// resource for each of `listed`, newest first. A listing pages through them as it pages any
/// resource's children.
#[must_use]
pub fn tree_rows(listed: &[Invocation]) -> Vec<super::CatalogueRow> {
    let row = |parent: &str, id: String, of_type: &str, label: String| super::CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: ROOT_CHILD.to_owned(),
        parent_resource: parent.to_owned(),
        resource: super::CatalogueResource {
            id,
            kind: of_type.to_owned(),
            label,
            presentation: None,
        },
        ..super::CatalogueRow::default()
    };
    let mut root = row(
        "",
        RESOURCE.to_owned(),
        "workspace.invocations",
        "Invocations".to_owned(),
    );
    root.has_children = !listed.is_empty();
    let mut list = root.clone();
    list.action = Some(list_action());
    [root, list]
        .into_iter()
        .chain(listed.iter().map(|invocation| {
            let id = invocation
                .id
                .clone()
                .or_else(|| invocation.step.clone())
                .unwrap_or_default();
            let status = serde_json::to_value(invocation.status)
                .ok()
                .and_then(|status| status.as_str().map(str::to_owned))
                .unwrap_or_default();
            let mut label = format!("{status} by {}", invocation.by.name);
            if let Some(title) = &invocation.title {
                label.push_str(": ");
                label.push_str(title);
            }
            row(RESOURCE, id, INVOCATION_TYPE, label)
        }))
        .collect()
}

/// [`LIST`] as the tree publishes it: a read of one page, by `limit` and `cursor`.
fn list_action() -> crate::ActionDescriptor {
    use crate::{ActionContract, ActionKind, ActionParam, ParamType};
    crate::ActionDescriptor {
        did: None,
        state: crate::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, LIST),
        resource_kind: "workspace.invocations".to_owned(),
        resource_label: "Invocations".to_owned(),
        action_label: "List invocations".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: Some(
            "The workspace's invocations in records you may read, newest first: who made each, \
             how it stands, what it changed and whether it can still be taken back."
                .to_owned(),
        ),
        locus: crate::ExecutionLocus::ServerPreferred,
        kind: crate::OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Read,
            ..ActionContract::default()
        },
        input: crate::InputSpec::None,
        params: vec![
            ActionParam::optional("limit", ParamType::Integer, "How many, at most 200."),
            ActionParam::optional(
                "cursor",
                ParamType::Text,
                "Where the page before ended: its `next_cursor`.",
            ),
        ],
        view: None,
    }
}

/// One page of a workspace's invocations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InvocationsQuery {
    /// The newest this many after `cursor`; `0` for [`PAGE`], never more
    /// than [`MOST`].
    pub limit: usize,
    /// Where the page before ended ([`Invocations::next_cursor`]); none for the
    /// newest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Only the runs of the member with this id ([`InvocationBy::id`]), when it
    /// names one; every run otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Only the invocation this names, by one of its calls' invocation ids or by its
    /// step: what taking it back finds it by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
}

impl InvocationsQuery {
    /// How many invocations the page holds.
    #[must_use]
    pub fn page(&self) -> usize {
        if self.limit == 0 {
            PAGE
        } else {
            self.limit.min(MOST)
        }
    }
}

/// Who ran an invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InvocationBy {
    /// The name it goes by: a key's, as the person named it, an agent's, or
    /// a person's.
    pub name: String,
    /// `agent` (every key, and an agent a person's device handed its session
    /// to), `person`, `bot` or `automation`.
    pub kind: String,
    /// The member's own id inside that kind: an agent's, a bot's, a person's
    /// (the lowercase hex of their root key), as History names their changes.
    /// `None` for a run whose changes name no member (an automation's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// How an invocation stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    /// It asked the person, and waits for their answer.
    Waiting,
    /// Admitted, and not ended yet.
    Running,
    /// It ended well.
    #[default]
    Ended,
    /// It ended failing, or the person did not allow it.
    Failed,
    /// It was stopped.
    Cancelled,
    /// Everything it changed was taken back.
    Undone,
}

/// A record an invocation changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InvocationChange {
    /// The app, as a path names it (`word-processor`).
    pub app: String,
    /// The app's id (`so.tilo.word-processor`).
    pub app_id: String,
    /// The record's id there; empty for the app's own record of the
    /// workspace.
    pub record: String,
}

/// One invocation of a workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Invocation {
    /// What reading, stopping and taking it back name it by: the invocation
    /// id of its newest call (taking back one call of an invocation takes back the
    /// whole of it). `None` for one known only from the records, whose calls
    /// the host no longer remembers (it restarted since).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Every call of it the host remembers, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// The undo step its changes are kept under in every record's history:
    /// what tells it apart there, and here across the host's restarts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub by: InvocationBy,
    /// What its client called it, when it named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: InvocationStatus,
    /// Why it failed, typed (`confirmation_required`, ...), never the
    /// failure's words, which may hold what it was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// When it ended; `None` while it has not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// What its last call asked for, `<app>#<action>`, while the host
    /// remembers it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// What its newest change did, without the values it carried: the action
    /// by its published label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// How many changes it made to the workspace's records: its edits, and
    /// what it brought in from outside (what a command printed).
    pub changes: usize,
    /// How many of its edits were taken back since; what came from outside
    /// is no edit, and no undo takes it back.
    pub undone: usize,
    /// The records it changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<InvocationChange>,
    /// Its edits still stand and nobody's edit stands on them: taking it back
    /// would take back exactly its own.
    pub undoable: bool,
    /// Why it is not taken back, when someone's change stands on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_blocked: Option<String>,
    /// The records keep it: it is listed after the host restarts.
    pub recorded: bool,
    /// The host remembers its calls: `id` reads, stops and takes it back.
    pub remembered: bool,
}

/// A page of a workspace's invocations, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Invocations {
    pub items: Vec<Invocation>,
    /// Invocations the workspace holds, of which [`Self::items`] is a page.
    pub total: usize,
    /// Of those, the invocations waiting for the person.
    pub waiting: usize,
    /// Some record could not be read yet: invocations that changed only it are
    /// missing until it can.
    pub pending: bool,
    /// Passed back as [`InvocationsQuery::cursor`] for the next page; `None` on the
    /// last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The change someone else made after the last edit in effect of the step
/// `of_step` tells, still in effect: what holds the step's undo back, since
/// taking it back under their work would change what they see. `entries` are
/// a document's history entries, oldest first. Housekeeping changes nothing
/// the step wrote, and an undo or redo that only moves steps made after it
/// (`takes_back`, `brings_back`) leaves it as it was: those steps hold it back
/// themselves while they are in effect.
#[must_use]
pub fn edited_after(
    entries: &[serde_json::Value],
    of_step: impl Fn(&serde_json::Value) -> bool,
) -> Option<&serde_json::Value> {
    let in_effect = |entry: &serde_json::Value| {
        entry.get("undone").and_then(serde_json::Value::as_bool) != Some(true)
    };
    let is_edit = |entry: &serde_json::Value| entry.get("change_kind").is_none();
    let last = entries
        .iter()
        .rposition(|entry| of_step(entry) && in_effect(entry) && is_edit(entry))?;
    let after = entries.get(last.saturating_add(1)..)?;
    let later: Vec<&str> = after
        .iter()
        .filter_map(|entry| entry.get("change").and_then(serde_json::Value::as_str))
        .collect();
    let moves_only_later = |entry: &serde_json::Value| {
        ["takes_back", "brings_back"].iter().any(|key| {
            entry
                .get(*key)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|moved| {
                    moved.iter().all(|change| {
                        change
                            .as_str()
                            .is_some_and(|change| later.contains(&change))
                    })
                })
        })
    };
    after.iter().find(|entry| {
        in_effect(entry) && is_edit(entry) && !of_step(entry) && !moves_only_later(entry)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_bounded_and_the_listing_named_by_its_target() {
        assert_eq!(InvocationsQuery::default().page(), PAGE);
        assert_eq!(
            InvocationsQuery {
                limit: 3,
                ..InvocationsQuery::default()
            }
            .page(),
            3
        );
        assert_eq!(
            InvocationsQuery {
                limit: 10_000,
                ..InvocationsQuery::default()
            }
            .page(),
            MOST
        );
        assert!(is_listing(&target()));
        let mut other = target();
        other.address.action = "undo".into();
        assert!(!is_listing(&other));
        let answer = serde_json::to_value(Invocation {
            status: InvocationStatus::Waiting,
            ..Invocation::default()
        })
        .unwrap();
        assert_eq!(answer["status"], "waiting");
        assert!(answer.get("id").is_none(), "{answer}");
    }

    #[test]
    fn a_listing_asks_for_one_members_runs_and_says_who_ran_each() {
        let query = serde_json::to_value(InvocationsQuery {
            by: Some("claude".into()),
            ..InvocationsQuery::default()
        })
        .unwrap();
        assert_eq!(query["by"], "claude");
        assert!(
            serde_json::to_value(InvocationsQuery::default())
                .unwrap()
                .get("by")
                .is_none()
        );
        let run = serde_json::to_value(Invocation {
            by: InvocationBy {
                name: "Claude".into(),
                kind: "agent".into(),
                id: Some("claude".into()),
            },
            ..Invocation::default()
        })
        .unwrap();
        assert_eq!(
            run["by"],
            serde_json::json!({"name": "Claude", "kind": "agent", "id": "claude"})
        );
        let unnamed = serde_json::to_value(InvocationBy::default()).unwrap();
        assert!(unnamed.get("id").is_none(), "{unnamed}");
    }

    #[test]
    fn an_undo_of_a_later_step_holds_no_earlier_step_back_and_a_later_edit_does() {
        use serde_json::json;
        let cover = |entry: &serde_json::Value| entry["step"] == "cover";
        let mut entries = vec![
            json!({"change": "a1", "step": "cover"}),
            json!({"change": "b1", "step": "closing", "undone": true}),
            json!({"change": "c1", "step": "u1", "takes_back": ["b1"]}),
            json!({"change": "d1", "change_kind": "Housekeeping"}),
        ];
        assert_eq!(edited_after(&entries, cover), None);

        entries.push(json!({"change": "e1", "step": "u2", "takes_back": ["z9"]}));
        assert_eq!(edited_after(&entries, cover).unwrap()["change"], "e1");
        entries.pop();

        entries.push(json!({"change": "e1", "step": "bob"}));
        assert_eq!(edited_after(&entries, cover).unwrap()["change"], "e1");
    }
}
