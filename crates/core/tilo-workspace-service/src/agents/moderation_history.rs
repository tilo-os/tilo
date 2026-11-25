//! What a moderation turn reads of a reported person's recorded changes in a tl app's record,
//! and how it takes exactly those changes back.
//!
//! The workspace's history is the store's: every change a tl record took is stamped with who
//! made it, which action, in which run and, since a run's changes are one action of the
//! workspace, the invocation it belongs to. Reading a person's versions is
//! [`Workspaces::record_changes`]; taking one back is the store's own undo of its invocation
//! (the store's own undo), run within one run so it stands in history as one step.
//!
//! A turn's tools act on the workspace's action service, which knows the workspace's resources
//! and nothing of a person's history. This is the entry that answers them for the turn: it
//! lists the two actions beside the workspace's resources and takes them as the turn's caller,
//! admitted by the workspace's grants on the app's node for every person its line answers to.
//! The undo covers exactly the versions the read handed it, by their invocations, so nothing
//! written after the read is swept in.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Context, MemberAccess};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget,
};
use tilo_actions::{ActionKind, ActionOutput, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;

use super::turn_provider::{Ending, Rows};
use super::{Ask, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;

/// The provider a moderation turn reaches a person's recorded changes under.
pub(super) const PROVIDER: &str = "tilo.moderation";
/// The resource both actions are actions of.
pub(super) const RESOURCE: &str = "moderation.history";
/// Read a person's recorded changes, oldest first.
pub(super) const ACTIVITY: &str = "activity";
/// Take exactly the changes a read returned back, as one run.
pub(super) const UNDO: &str = "undo";

/// The app whose record a moderation turn reads: the workspace's tl Chat.
const CHAT: &str = "so.tilo.chat";
/// The node under an app the two actions are decided at.
const NODE_KIND: &str = "moderation.history";
/// The most versions a read hands back when a turn asks for none.
const LAST: usize = 50;

/// One turn's read of a person's recorded changes, and its undo of them.
pub(super) struct TurnHistory {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: Option<WorkspaceId>,
    /// Whether this turn is one a saved watch started: only such a turn is offered the read and
    /// the undo, never a turn a person's own message started.
    watched: bool,
}

impl TurnHistory {
    /// The history `turn` of `workspace` reaches.
    pub(super) fn for_turn(workspaces: Workspaces, workspace: &str, turn: &Turn) -> Self {
        Self {
            workspaces,
            workspace: workspace.to_owned(),
            workspace_id: WorkspaceId::parse(workspace),
            watched: matches!(&turn.ask, Ask::Triggered(_)),
        }
    }

    /// The provider's rows, for a turn a watch started.
    fn offered(&self) -> Vec<CatalogueRow> {
        if self.workspace_id.is_none() {
            return Vec::new();
        }
        [ACTIVITY, UNDO]
            .into_iter()
            .map(|action| {
                let address = tilo_actions::ActionAddress::new(RESOURCE, action);
                let (label, description) = if action == ACTIVITY {
                    (
                        "Read a person's changes",
                        "What a person changed in this app's record, oldest first, with the \
                         invocation each change was part of.",
                    )
                } else {
                    (
                        "Take those changes back",
                        "Take exactly the changes a read returned back, each by its invocation, \
                         as one run.",
                    )
                };
                let mut contract = tilo_actions::ActionContract::default();
                // A turn's own: no person picks it from a list.
                contract.set_audience(tilo_actions::contract::Audience::Agents);
                CatalogueRow::for_action(
                    PROVIDER.to_owned(),
                    PROVIDER.to_owned(),
                    tilo_actions::ActionDescriptor {
                        did: None,
                        state: tilo_actions::ActionState::default(),
                        semantic_identity: None,
                        address,
                        resource_kind: NODE_KIND.to_owned(),
                        resource_label: "History".to_owned(),
                        action_label: label.to_owned(),
                        keywords: Vec::new(),
                        icon: None,
                        description: Some(description.to_owned()),
                        locus: tilo_actions::ExecutionLocus::ClientLocal,
                        kind: tilo_actions::OperationKind::Effect,
                        contract,
                        input: tilo_actions::InputSpec::None,
                        params: Vec::new(),
                        view: None,
                    },
                )
            })
            .collect()
    }

    /// Whether `caller` may take `action` on `app`'s history: the workspace's grants on the
    /// app's node, for every person the turn answers.
    async fn admitted(
        &self,
        caller: &Caller,
        app: &str,
        action: &str,
        kind: ActionKind,
    ) -> Result<(), ActionCallError> {
        if self.workspace_id.is_none() {
            return Err(ActionCallError::UnknownAction);
        }
        let Some(policy) = self.workspaces.policy_of(&self.workspace).await else {
            return Err(unavailable("this device holds no such workspace"));
        };
        let now = tilo_clock::wall();
        let access =
            MemberAccess::new(policy.clone(), caller.acting().clone()).granted(caller.granted());
        let node = tilo_actions::action_node(&policy.node(), app, NODE_KIND, app, None, action);
        match access.answer(&node, kind, &Context::at_time(now)) {
            Answer::Allow => Ok(()),
            Answer::Ask => Err(ActionCallError::confirmation_required()),
            Answer::Deny => Err(ActionCallError::NotPermitted),
        }
    }

    /// The versions of `app`'s record, oldest first, read as the person `caller` acts for: an
    /// agent sees no more of the record's history than its person may read.
    async fn versions(&self, caller: &Caller, app: &str) -> Result<Vec<Value>, ActionCallError> {
        let reader = crate::workspaces::person_reading(caller);
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        };
        let (workspaces, workspace) = (self.workspaces.clone(), self.workspace.clone());
        tokio::task::spawn_blocking(move || workspaces.record_changes(&workspace, &route, &reader))
            .await
            .map_err(|error| unavailable(error.to_string()))?
            .ok_or_else(|| unavailable("this app's record does not read here"))
    }

    /// Read the newest `last` of `person`'s changes in `app`, oldest first.
    async fn activity(
        &self,
        caller: &Caller,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ActionCallError> {
        let app = text(arguments, "app").unwrap_or_else(|| CHAT.to_owned());
        let person = person_of(arguments)?;
        let last = arguments
            .get("last")
            .and_then(Value::as_u64)
            .and_then(|last| usize::try_from(last).ok())
            .unwrap_or(LAST);
        self.admitted(caller, &app, ACTIVITY, ActionKind::Read)
            .await?;
        let mut mine: Vec<Value> = self
            .versions(caller, &app)
            .await?
            .into_iter()
            .filter(|version| self.writes(version, &person))
            .collect();
        let skip = mine.len().saturating_sub(last);
        mine.drain(..skip);
        Ok(json!({"versions": mine}))
    }

    /// Take back each of `person`'s changes named by `changes` in `app`, by its invocation, as
    /// one run. Nothing the read did not return is taken.
    async fn undo(
        &self,
        caller: &Caller,
        arguments: &Map<String, Value>,
    ) -> Result<Value, ActionCallError> {
        let app = text(arguments, "app").unwrap_or_else(|| CHAT.to_owned());
        let person = person_of(arguments)?;
        let changes: BTreeSet<String> = arguments
            .get("changes")
            .and_then(Value::as_array)
            .map(|changes| {
                changes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if changes.is_empty() {
            return Err(ActionCallError::InvalidRequest {
                reason: "an undo names the changes a read returned".to_owned(),
            });
        }
        self.admitted(caller, &app, UNDO, ActionKind::Undoable)
            .await?;
        let agent = caller
            .acting()
            .agent_id()
            .ok_or(ActionCallError::NotPermitted)?
            .to_owned();
        // Each invocation once, at the time it last wrote.
        let mut made: Vec<(String, u64, String)> = Vec::new();
        for version in self.versions(caller, &app).await? {
            let change = version
                .get("change")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !changes.contains(change) || !self.writes(&version, &person) {
                continue;
            }
            let Some(invocation) = version.get("invocation").and_then(Value::as_str) else {
                continue;
            };
            let at = version
                .get("at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let op = version
                .get("op")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            match made.iter_mut().find(|(_, _, held)| held == invocation) {
                Some(held) => {
                    if (&at, op) > (&held.0, held.1) {
                        (held.0, held.1) = (at, op);
                    }
                }
                None => made.push((at, op, invocation.to_owned())),
            }
        }
        // The newest first: a change is taken back before the ones it was built on, so a message
        // goes before the channel it was written in.
        made.sort_by(|a, b| (&b.0, b.1).cmp(&(&a.0, a.1)));
        let invocations: Vec<String> = made
            .into_iter()
            .map(|(_, _, invocation)| invocation)
            .collect();
        if invocations.is_empty() {
            return Ok(json!({"undone": 0}));
        }
        let records = self
            .workspaces
            .tilolang()
            .ok_or_else(|| unavailable("this device runs no tilolang runtime"))?
            .records;
        let run = format!(
            "moderation:{}:{}",
            person.id(),
            tilo_clock::unix_now_ms_i64()
        );
        let viewer = tilo_actions::Actor::Member(tilo_actions::MemberId::agent(agent));
        // The agent reads what the person it works for may read, never the moderated person.
        let reader = crate::workspaces::person_reading(caller);
        // The undo writes: the record is carried on this reactor, never read-only.
        let record = self
            .workspaces
            .tl_record(&self.workspace, &app, &reader)
            .map_err(unavailable)?;
        let policy = self.workspaces.policy_now(&self.workspace).map(Arc::new);
        let undone = tokio::task::spawn_blocking(move || {
            records.undo(record, policy, &viewer, &app, (&run, ""), &invocations)
        })
        .await
        .map_err(unavailable)?
        .map_err(unavailable)?;
        Ok(json!({"undone": undone}))
    }

    /// Whether `version` is a change `person` made.
    fn writes(&self, version: &Value, person: &tilo_actions::MemberId) -> bool {
        self.workspaces
            .tilolang()
            .is_some_and(|tilolang| tilolang.records.wrote(version, person))
    }
}

impl TurnProvider for TurnHistory {
    fn providers(&self, caller: &Caller) -> Vec<CatalogueProvider> {
        if self.workspace_id.is_none() || !self.watched || !caller.acting().is_agent() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: PROVIDER.to_owned(),
            app_id: PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (self.watched && query.provider.as_deref() == Some(PROVIDER))
            .then(|| Pending::ready(Ok(self.offered())))
    }

    fn takes(&self, target: &RemoteActionTarget) -> bool {
        self.watched
            && target.provider == PROVIDER
            && target.address.resource == RESOURCE
            && matches!(target.address.action.as_str(), ACTIVITY | UNDO)
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move {
            let tilo_actions::remote::InvokeArgument::Json(arguments) = &request.argument else {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a history action takes named arguments".to_owned(),
                });
            };
            let arguments = arguments.clone();
            let value = match request.target.address.action.as_str() {
                ACTIVITY => self.activity(&caller, &arguments).await?,
                UNDO => self.undo(&caller, &arguments).await?,
                _ => return Err(ActionCallError::UnknownAction),
            };
            Ok(InvocationUpdate::Succeeded {
                invocation: request.invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            })
        })
    }
}

/// The person a read or an undo names.
fn person_of(arguments: &Map<String, Value>) -> Result<tilo_actions::MemberId, ActionCallError> {
    text(arguments, "person")
        .and_then(|person| tilo_actions::MemberId::parsed_person(&person))
        .ok_or_else(|| ActionCallError::InvalidRequest {
            reason: "a read names the person whose changes it wants".to_owned(),
        })
}

/// The text argument `name` names.
fn text(arguments: &Map<String, Value>, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Why a host read could not be answered.
fn unavailable(error: impl ToString) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: error.to_string(),
    }
}
