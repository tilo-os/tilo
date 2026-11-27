//! The invocations of a workspace a host keeps, newest first, in the records the
//! caller may read: what a key that reaches the workspace reads to see what was
//! invoked there, what waits for the person, and what can still be taken back.
//! Of each invocation the host answers only what lies in records the caller may
//! read now, and an invocation with nothing left for them is neither listed nor
//! counted.
//!
//! A device lists a workspace's agent sessions from its own history index
//! ([`tilo_actions::remote::workspace_activity`]). A host that keeps a workspace for people
//! and their keys, with no device of theirs, answers from what it holds:
//!
//! - **The records.** Every change an action makes is kept in its record's
//!   history with the undo step it is part of, who made it and when
//!   ([`tilo_actions::remote::ChangeFeed`]). An invocation is its step: one call, or every call of a
//!   group its client named. What the records say of it holds across the host's
//!   restarts.
//! - **The calls the host remembers.** The host keeps the invocations it
//!   was asked for in memory, each with its id, who asked, and how it
//!   stands: queued, waiting for the person, running, ended. An invocation that
//!   changed no record (one refused, one still waiting) is known from them
//!   alone, and only until the host restarts or lets go of it.
//!
//! The workspace's invocations are a record, read through the record API
//! ([`tilo_actions::remote::ResourceReads`]): whole, or one by its id, as the caller may read each
//! call. [`InvocationsQuery`] is the page a host builds that record's reading from.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;

/// The provider a host answers its workspace's invocations under.
pub const PROVIDER: &str = tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER;
/// The resource the invocations are published as: the workspace's own record of them.
pub const RESOURCE: &str = "workspace";
/// Stop one invocation.
pub const CANCEL: &str = "cancel";
/// Run one invocation again: the same request, by the same caller, under a new id.
pub const RETRY: &str = "retry";
/// Take back what one invocation changed, through its record's own history; addressed to a work
/// group's id instead, what every call of that group changed.
pub const UNDO: &str = "undo";

/// Why an invocation is not taken back at a provider: the record there offers no history, so the
/// call changed nothing there to take back.
pub const NO_HISTORY: &str = "the record it changed offers no history to take back";

/// How many invocations a page holds when the query asks for no number.
pub const PAGE: usize = 50;
/// The most invocations one page holds.
pub const MOST: usize = 200;

/// What the workspace root names the invocations by: `invocations` reads them.
pub const ROOT_CHILD: &str = "invocations";
/// The resource type of one invocation in the published tree.
pub const INVOCATION_TYPE: &str = "invocation";

/// The publisher a host lists the invocations of `workspace` under, beside its apps: browsed,
/// it is the workspace root's `invocations`.
#[must_use]
pub fn publisher(workspace: &str) -> tilo_actions::remote::CatalogueProvider {
    tilo_actions::remote::CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: ROOT_CHILD.to_owned(),
        workspace: workspace.to_owned(),
        browsable: true,
        ..tilo_actions::remote::CatalogueProvider::default()
    }
}

/// The published tree of a workspace's invocations, as one catalogue page lists it: the
/// workspace's invocations ([`RESOURCE`]), holding one resource for each of `listed`, newest
/// first. A listing pages through them as it pages any resource's children.
#[must_use]
pub fn tree_rows(listed: &[Invocation]) -> Vec<tilo_actions::remote::CatalogueRow> {
    let row = |parent: &str, id: String, of_type: &str, label: String| {
        tilo_actions::remote::CatalogueRow {
            provider: PROVIDER.to_owned(),
            app_id: ROOT_CHILD.to_owned(),
            parent_resource: parent.to_owned(),
            resource: tilo_actions::remote::CatalogueResource {
                id,
                kind: of_type.to_owned(),
                label,
                presentation: None,
            },
            ..tilo_actions::remote::CatalogueRow::default()
        }
    };
    let mut root = row(
        "",
        RESOURCE.to_owned(),
        "workspace.invocations",
        "Invocations".to_owned(),
    );
    root.has_children = !listed.is_empty();
    std::iter::once(root)
        .chain(listed.iter().flat_map(|invocation| {
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
            let waits = invocation.status == InvocationStatus::Waiting && !id.is_empty();
            let own = row(RESOURCE, id.clone(), INVOCATION_TYPE, label);
            // One that waits for its person is answered by its own `resolve`.
            let resolve = waits.then(|| tilo_actions::remote::CatalogueRow {
                action: Some(resolve_action(&id)),
                ..own.clone()
            });
            std::iter::once(own).chain(resolve)
        }))
        .collect()
}

/// [`tilo_actions::remote::asked::RESOLVE`] as an invocation that waits for its person publishes it: the
/// person's answer to it.
fn resolve_action(invocation: &str) -> tilo_actions::ActionDescriptor {
    use tilo_actions::{ActionContract, ActionKind, ActionParam, ParamType};
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(invocation, tilo_actions::remote::asked::RESOLVE),
        resource_kind: INVOCATION_TYPE.to_owned(),
        resource_label: "Invocation".to_owned(),
        action_label: "Answer".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: Some(
            "Answer what this invocation asks you: approve it, approve it and let its asker do \
             the same from now on, or decline it."
                .to_owned(),
        ),
        locus: tilo_actions::ExecutionLocus::ServerPreferred,
        kind: tilo_actions::OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Undoable,
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: vec![
            ActionParam::required(
                "resolution",
                ParamType::Text,
                "`approve`, `approve_and_grant` or `decline`.",
            ),
            ActionParam::optional(
                "choice",
                ParamType::Integer,
                "For a question with choices: the one picked, by its place.",
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
    /// Only the invocations in this status, when it names one: the questions that wait for the
    /// person are [`InvocationStatus::Waiting`]. Every status otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<InvocationStatus>,
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
    /// The member's own id inside that kind: an agent's, a bot's, a key's
    /// (`grant-` and its device key, hex), a person's (the lowercase hex of
    /// their root key), as History names their changes. `None` for a run whose
    /// changes name no member (an automation's).
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

/// The resource a call was made from: what an invocation runs for, so whoever may write it may
/// too stop the invocation or run it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Origin {
    /// Its kind, as the app that publishes it names it (`chat.workspace.channels`).
    pub kind: String,
    /// Its id in that kind (`c1`).
    pub id: String,
}

impl Origin {
    /// The resource `address` names, with `kind` when its caller says what the resource is:
    /// its stable address (`chat.workspace.channels:c1`), or the item form an item's link
    /// carries (`chat.workspace/channels/c1`). `None` for an address naming no resource.
    #[must_use]
    pub fn parse(address: &str, kind: Option<&str>) -> Option<Self> {
        let address = address.trim();
        if address.is_empty() {
            return None;
        }
        if let Some(stable) = tilo_actions::stable_item_address(address) {
            let (kind, id) = stable.split_once(':')?;
            return Some(Self {
                kind: kind.to_owned(),
                id: id.to_owned(),
            });
        }
        if let Some((kind, id)) = address.split_once(':')
            && !kind.is_empty()
            && !id.is_empty()
        {
            return Some(Self {
                kind: kind.to_owned(),
                id: id.to_owned(),
            });
        }
        let kind = kind.map(str::trim).filter(|kind| !kind.is_empty())?;
        Some(Self {
            kind: kind.to_owned(),
            id: address.to_owned(),
        })
    }

    /// The resource's stable address (`chat.workspace.channels:c1`).
    #[must_use]
    pub fn address(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
}

/// What a model call an invocation made cost: what the model endpoint writes as it answers, so
/// a listing says what work a run paid for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Spend {
    /// The provider that answered, as its person named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model's id, as its provider names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The prompt's tokens, as the provider counted them.
    pub input_tokens: u64,
    /// The answer's tokens, as the provider counted them.
    pub output_tokens: u64,
    /// What the call cost, in millionths of the currency.
    pub cost_micros: u64,
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
    /// The resource the call was made from, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The invocation this one runs again, when it is a retry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<String>,
    /// What a model call it made cost, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
    /// The undo step its changes are kept under in every record's history:
    /// what tells it apart there, and here across the host's restarts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub by: InvocationBy,
    /// The work it is a step of, as its caller's context named it: an agent's session (every
    /// call of one connection), an issue, a task. What a listing of one session's calls reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<tilo_actions::WorkGroup>,
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
    /// What the action answered, when it ended well and answered a value: a
    /// read's result, a record the caller then reads. Set when it settles;
    /// a failed or refused invocation answers none and says why in
    /// [`Self::failure`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
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
    /// The person it waits for, the hex of their root key, while it asks them
    /// ([`InvocationStatus::Waiting`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// What its person is asked while it waits for them: the words, facts, what they type
    /// (by label: what they typed never reaches a record), choose and pick, where it was
    /// pressed, and the answers it offers. They answer it with `<record>#resolve`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<Box<tilo_actions::remote::asked::Asked>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_bounded() {
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
}
