//! Every run of a workspace, newest first, as the host knows them
//! ([`tilo_actions::remote::workspace_invocations`]): from what the workspace's
//! records keep, joined with the calls the host remembers.
//!
//! - **The records.** Each change an action made is in its record's history
//!   with the undo step it is part of (`step`), who made it, when, and
//!   whether it was taken back. A run is its step: one call, or every call
//!   of a run its client named. Each record's history is read once per change
//!   of the record ([`Index`]), so a listing costs what changed since the
//!   last.
//! - **The calls.** The ledger remembers each invocation the host was asked
//!   for, who asked and how it stands. A call's step is the one its changes
//!   record: the gesture its caller said, under the run the host stamped
//!   ([`super::caller_context`], [`tilo_actions::context::recorded_gesture`]),
//!   so a call and its changes are one run. A call that changed nothing is a
//!   run of the ledger's alone, gone when the host restarts or lets go of it.
//!
//! A caller is given, of each run, only what lies in records they may read
//! now: a record's share of a run when the check a read of that record makes
//! passes for them (the host's own person as the owner; anyone else by the
//! record's policy ([`Worker::policy_for`]) held to their own bound, an
//! agent's grants or a device's cap included, answering Read, not Ask, at the
//! record's node), a call the ledger remembers when they may read the record
//! it addressed (or, for a call on no record, read across the workspace,
//! [`Worker::may_across`]). A run's words are its steps' titles, never what
//! a step carried ([`step_title`]): the index is read once for everyone. A run
//! with nothing left for them is not listed, nor counted, nor said to wait;
//! their own runs are no exception, as `tilo_status` reads those by id. The
//! index is the workspace's, shared; each listing asks the policy again,
//! once for each record, so a Read taken away shows on the next one. What a
//! call was given, what it answered and why it failed in words stay its
//! caller's: a run says what was asked for, how it stands, and a failure's
//! code.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use tilo_actions::remote::workspace_invocations::{
    Invocation, InvocationBy, InvocationChange, InvocationStatus, Invocations, InvocationsQuery,
};
use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, pending,
};
use tilo_actions::{ActionOutput, EffectReceiptStatus, InvocationId, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;
use ui::history_source::words::HistoryWords;

use super::ledger::Entry;
use super::{Job, Needs, Worker, WorkspaceActions, resource_of};

/// One record of a workspace: its app and its id there.
type DocumentKey = (String, String);

/// What one record's history says of one run (one step).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Share {
    app_id: String,
    record: String,
    by: InvocationBy,
    title: Option<String>,
    first_ms: i64,
    last_ms: i64,
    /// Every change it made to the record, from outside included.
    changes: usize,
    /// Of those, its edits of what the record holds: its undo steps.
    edits: usize,
    /// Of its edits, those taken back.
    undone: usize,
    /// What its newest change did, without the values it carried
    /// ([`step_title`]).
    summary: Option<String>,
    /// Who changed the record after its last change still in effect, while
    /// that change of theirs is in effect too.
    over: Option<String>,
    /// Its changes, by their short hashes as History lists them.
    made: Vec<String>,
}

/// What one record's history says of each run that changed it, by step, as
/// it stood at a revision.
type Read = ([u8; 32], Vec<(String, Share)>);

/// What every record's history says of the workspace's runs, read again for
/// a record only once it changed.
#[derive(Default)]
pub(super) struct Index {
    records: HashMap<DocumentKey, Read>,
}

impl Index {
    /// Read again each of `records` that changed since the last look:
    /// `revision` says where one stands without reading it, `document` reads
    /// it whole, both `None` for a record the replica cannot open yet.
    /// The records that could not be read.
    fn take_in(
        &mut self,
        records: &[DocumentKey],
        revision: impl Fn(&DocumentKey) -> Option<[u8; 32]>,
        document: impl Fn(&DocumentKey) -> Option<Vec<u8>>,
        words: &dyn HistoryWords,
    ) -> Vec<DocumentKey> {
        let mut unread = Vec::new();
        for key in records {
            let Some(now) = revision(key) else {
                unread.push(key.clone());
                continue;
            };
            if self.records.get(key).is_some_and(|(at, _)| *at == now) {
                continue;
            }
            let Some(doc) =
                document(key).and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            else {
                unread.push(key.clone());
                continue;
            };
            let entries = ui::history_source::verbs::entries_after(&doc, &[], words);
            self.records
                .insert(key.clone(), (now, shares(&key.0, &key.1, &entries)));
        }
        let listed: HashSet<&DocumentKey> = records.iter().collect();
        self.records.retain(|key, _| listed.contains(key));
        unread
    }

    /// Every run the records keep, by its step, each record's share of it.
    fn steps(&self) -> BTreeMap<String, Vec<Share>> {
        let mut steps: BTreeMap<String, Vec<Share>> = BTreeMap::new();
        for (_, shares) in self.records.values() {
            for (step, share) in shares {
                steps.entry(step.clone()).or_default().push(share.clone());
            }
        }
        steps
    }
}

/// A history entry's text field `key`.
fn text<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// What the change the history entry lists did, said without the values it
/// carried: an action by its published label, never through the app's
/// sentence, which may quote an argument (the words of a message posted).
/// The index reads History once for everyone, with the host's eyes; a caller
/// whose permissions withhold a field must not read it here. `None` for an
/// entry that says not what it did.
fn step_title(entry: &Value) -> Option<String> {
    let what = serde_json::from_value::<tilo_actions::Did>(entry.get("what")?.clone()).ok()?;
    ui::history_source::sentence::step_title(&tilo_actions::ActionContext {
        what: Some(what),
        ..tilo_actions::ActionContext::default()
    })
    .filter(|title| !title.trim().is_empty())
}

/// Whether the history entry is someone's edit of what the record holds: an
/// undo step, which their undo takes back. Content from outside (what a
/// command printed, recorded) is an invocation's change too, but nobody's
/// step; and housekeeping (naming a checkpoint) is no change of it at all.
fn is_content(entry: &Value) -> bool {
    entry.get("change_kind").is_none()
}

fn is_housekeeping(entry: &Value) -> bool {
    text(entry, "change_kind") == Some("Housekeeping")
}

fn in_effect(entry: &Value) -> bool {
    entry.get("undone").and_then(Value::as_bool) != Some(true)
}

/// Who made the change the history entry lists, and what its client called
/// the group, when it named it.
fn author(entry: &Value) -> (InvocationBy, Option<String>) {
    let actor = entry.get("actor");
    let acting = actor.and_then(|actor| actor.get("by"));
    let member = acting.and_then(|by| by.get("member"));
    let kind = if let Some(member) = member {
        if member.get("agent").is_some() {
            "agent"
        } else if member.get("bot").is_some() {
            "bot"
        } else {
            "person"
        }
    } else if acting.and_then(|by| by.get("automation")).is_some() {
        "automation"
    } else if text(entry, "group_kind") == Some(tilo_actions::context::AGENT_SESSION_GROUP) {
        "agent"
    } else {
        "person"
    };
    let named = actor.and_then(|actor| text(actor, "name"));
    let group_title = text(entry, "title");
    let name = named
        .or(group_title)
        .or_else(|| text(entry, "by"))
        .unwrap_or("someone");
    // The member's own id tells its invocations apart from another's,
    // whatever either is named; an automation is no member.
    let id = member
        .and_then(|member| {
            ["agent", "bot", "person"]
                .iter()
                .find_map(|kind| text(member, kind))
        })
        .map(str::to_owned);
    // A group goes by its agent's name unless its client named it.
    let title = group_title
        .filter(|title| Some(*title) != named)
        .map(str::to_owned);
    (
        InvocationBy {
            name: name.to_owned(),
            kind: kind.to_owned(),
            id,
        },
        title,
    )
}

/// What the history `entries` of the record `record` of `app_id` (oldest
/// first, each whole) say of each invocation that changed it: its share, by
/// step. A change in no step is nobody's invocation: a person's own edit on a
/// page.
fn shares(app_id: &str, record: &str, entries: &[Value]) -> Vec<(String, Share)> {
    let changes: Vec<&Value> = entries
        .iter()
        .filter(|entry| !is_housekeeping(entry))
        .collect();
    let mut shares: Vec<(String, Share)> = Vec::new();
    // Where in `shares` each step is.
    let mut places: HashMap<String, usize> = HashMap::new();
    // Where in `changes` each step's last edit still in effect is.
    let mut standing: HashMap<String, usize> = HashMap::new();
    for (at, entry) in changes.iter().enumerate() {
        let Some(step) = text(entry, "step") else {
            continue;
        };
        let time = entry.get("time_ms").and_then(Value::as_i64).unwrap_or(0);
        let held = if let Some(&held) = places.get(step) {
            held
        } else {
            let (by, title) = author(entry);
            shares.push((
                step.to_owned(),
                Share {
                    app_id: app_id.to_owned(),
                    record: record.to_owned(),
                    by,
                    title,
                    first_ms: time,
                    last_ms: time,
                    ..Share::default()
                },
            ));
            let held = shares.len().saturating_sub(1);
            places.insert(step.to_owned(), held);
            held
        };
        let Some((_, share)) = shares.get_mut(held) else {
            continue;
        };
        share.changes = share.changes.saturating_add(1);
        if let Some(change) = text(entry, "change") {
            share.made.push(change.to_owned());
        }
        share.first_ms = share.first_ms.min(time);
        share.last_ms = share.last_ms.max(time);
        if let Some(title) = step_title(entry) {
            share.summary = Some(title);
        }
        if !is_content(entry) {
            continue;
        }
        share.edits = share.edits.saturating_add(1);
        if in_effect(entry) {
            standing.insert(step.to_owned(), at);
        } else {
            share.undone = share.undone.saturating_add(1);
        }
    }
    // Someone's edit after a run's last edit in effect, in effect too, holds
    // the run's undo back, as the undo itself finds it.
    for (step, share) in &mut shares {
        let Some(&last) = standing.get(step.as_str()) else {
            continue;
        };
        share.over = changes
            .iter()
            .skip(last.saturating_add(1))
            .find(|entry| in_effect(entry) && is_content(entry))
            .map(|entry| author(entry).0.name);
    }
    shares
}

/// One call the ledger remembers, as a run lists it.
struct Call {
    id: String,
    started_ms: i64,
    ended_ms: Option<i64>,
    update: InvocationUpdate,
    /// It asked the person and has no answer yet.
    waiting: bool,
    by: InvocationBy,
    title: Option<String>,
    action: String,
    /// The record it addressed, by its app and its id there; `None` for a
    /// call on no record (the workspace's own actions).
    addressed: Option<DocumentKey>,
    /// The changes it handed to the session: app, record, short hash.
    written: Vec<(String, String, String)>,
}

/// The step the changes of `entry`'s call record, and the call as a run
/// lists it.
fn call_of(id: String, entry: &Entry) -> (Option<String>, Call) {
    let context = super::caller_context(&entry.caller, &entry.request);
    let step = context.as_ref().and_then(|context| {
        let gesture = context.gesture?;
        let run = context
            .group
            .as_ref()
            .map(|group| (group.kind.as_str(), group.id.as_str()));
        Some(
            tilo_actions::context::recorded_gesture(gesture, run)
                .0
                .to_string(),
        )
    });
    let caller = &entry.caller;
    let acting = caller.acting();
    let kind = if acting.is_agent() {
        "agent"
    } else if acting.is_bot() {
        "bot"
    } else if acting.is_automation() {
        "automation"
    } else {
        "person"
    };
    let name = caller
        .agent_session()
        .map(|run| run.label)
        .or_else(|| {
            acting
                .person_id()
                .map(|person| person.chars().take(8).collect())
        })
        .unwrap_or_else(|| "someone".to_owned());
    let target = &entry.request.target;
    let addressed = resource_of(&target.provider, "").map(|route| (route.app_id, route.record));
    let action = match &addressed {
        Some((app_id, _)) => format!("{}#{}", app_path(app_id), target.address.action),
        None => format!("{}#{}", target.address.resource, target.address.action),
    };
    let call = Call {
        id,
        started_ms: entry.started_ms,
        ended_ms: entry.ended_ms,
        update: entry.update.clone(),
        waiting: entry.asked && entry.witness.is_none() && !entry.update.is_terminal(),
        by: InvocationBy {
            name,
            kind: kind.to_owned(),
            id: acting.member_id().map(|member| member.id().to_owned()),
        },
        title: super::said_invocation_title(&entry.request),
        action,
        addressed,
        written: entry.written.clone(),
    };
    (step, call)
}

/// Whether `entry` asks for where a record is shown (its share link): what
/// the API looks up for the link it answers after a call changed a record,
/// under that call's step. It reads, grants nothing and is nobody's run.
fn is_link_lookup(entry: &Entry) -> bool {
    entry.request.target.address.action == ui::tilo_resource::share::SHARE_LINK
}

/// The app as a path names it: the last part of its id, in lower case.
fn app_path(app_id: &str) -> String {
    app_id.rsplit('.').next().unwrap_or(app_id).to_lowercase()
}

/// One run: the calls the host remembers of it and the records' shares of it.
fn invocation(step: Option<String>, mut calls: Vec<Call>, shares: &[Share]) -> Invocation {
    calls.sort_by_key(|call| call.started_ms);
    let changes: usize = shares.iter().map(|share| share.changes).sum();
    let edits: usize = shares.iter().map(|share| share.edits).sum();
    let undone: usize = shares.iter().map(|share| share.undone).sum();
    let newest = shares.iter().max_by_key(|share| share.last_ms);
    let first_change = shares.iter().map(|share| share.first_ms).min();
    let last_change = shares.iter().map(|share| share.last_ms).max();
    let ended = calls.iter().all(|call| call.update.is_terminal());
    let last = calls.last();
    let mut status = if calls.iter().any(|call| call.waiting) {
        InvocationStatus::Waiting
    } else if !ended {
        InvocationStatus::Running
    } else {
        match last.map(|call| &call.update) {
            Some(InvocationUpdate::Failed { .. }) => InvocationStatus::Failed,
            Some(InvocationUpdate::Cancelled { .. }) => InvocationStatus::Cancelled,
            _ => InvocationStatus::Ended,
        }
    };
    if ended && edits > 0 && undone == edits {
        status = InvocationStatus::Undone;
    }
    let failure = last.and_then(
        |call| match EffectReceiptStatus::from_terminal(&call.update) {
            Some(EffectReceiptStatus::Failed { code }) => Some(code),
            _ => None,
        },
    );
    let (by, title) = match (newest, last) {
        (Some(share), _) => (share.by.clone(), share.title.clone()),
        (None, Some(call)) => (call.by.clone(), call.title.clone()),
        (None, None) => (InvocationBy::default(), None),
    };
    let started_ms = calls
        .first()
        .map(|call| call.started_ms)
        .into_iter()
        .chain(first_change)
        .min()
        .unwrap_or(0);
    let ended_ms = ended
        .then(|| {
            calls
                .iter()
                .filter_map(|call| call.ended_ms)
                .chain(last_change)
                .max()
        })
        .flatten();
    let blocked = shares.iter().find_map(|share| {
        share
            .over
            .as_ref()
            .map(|who| format!("{who} changed {} after it", app_path(&share.app_id)))
    });
    let mut records: Vec<InvocationChange> = Vec::new();
    for share in shares {
        let record = InvocationChange {
            app: app_path(&share.app_id),
            app_id: share.app_id.clone(),
            record: share.record.clone(),
        };
        if !records.contains(&record) {
            records.push(record);
        }
    }
    Invocation {
        id: last.map(|call| call.id.clone()),
        calls: calls.iter().map(|call| call.id.clone()).collect(),
        step,
        by,
        title,
        status,
        failure,
        started_ms,
        ended_ms,
        action: last.map(|call| call.action.clone()),
        summary: newest.and_then(|share| share.summary.clone()),
        changes,
        undone,
        records,
        undoable: edits > undone && blocked.is_none(),
        undo_blocked: blocked,
        recorded: changes > 0,
        remembered: !calls.is_empty(),
    }
}

/// Every run: each step the records keep, with the calls of it the host
/// remembers, and each call that changed nothing on its own (or with the
/// calls of its step), newest first.
fn listing(
    recorded: BTreeMap<String, Vec<Share>>,
    calls: Vec<(Option<String>, Call)>,
) -> Vec<Invocation> {
    let mut by_step: BTreeMap<String, Vec<Call>> = BTreeMap::new();
    let mut alone: Vec<Call> = Vec::new();
    for (step, call) in calls {
        match step {
            Some(step) => by_step.entry(step).or_default().push(call),
            None => alone.push(call),
        }
    }
    let mut items: Vec<Invocation> = Vec::new();
    for (step, shares) in recorded {
        let calls = by_step.remove(&step).unwrap_or_default();
        items.push(invocation(Some(step), calls, &shares));
    }
    for (step, calls) in by_step {
        items.push(invocation(Some(step), calls, &[]));
    }
    for call in alone {
        items.push(invocation(None, vec![call], &[]));
    }
    items.sort_by_key(|invocation| std::cmp::Reverse(key(invocation)));
    items
}

/// The runs of the member `by`, when the query asks for one member's, so an
/// agent reads its own work in a workspace where others work too; every run
/// when it asks for none.
fn of_member(invocations: Vec<Invocation>, by: Option<&str>) -> Vec<Invocation> {
    match by {
        Some(by) => invocations
            .into_iter()
            .filter(|invocation| invocation.by.id.as_deref() == Some(by))
            .collect(),
        None => invocations,
    }
}

/// Where a run sorts, newest first, and what a cursor names it by.
fn key(run: &Invocation) -> (i64, String) {
    let name = run
        .step
        .clone()
        .or_else(|| run.id.clone())
        .unwrap_or_default();
    (run.started_ms, name)
}

fn cursor_of(run: &Invocation) -> String {
    let (started, name) = key(run);
    format!("{started}:{name}")
}

/// The page of `runs` (newest first) `query` asks for.
fn page(items: Vec<Invocation>, query: &InvocationsQuery, pending: bool) -> Invocations {
    let total = items.len();
    let waiting = items
        .iter()
        .filter(|invocation| invocation.status == InvocationStatus::Waiting)
        .count();
    let after = query.cursor.as_deref().and_then(|cursor| {
        let (started, name) = cursor.split_once(':')?;
        Some((started.parse::<i64>().ok()?, name.to_owned()))
    });
    let mut rest = items
        .into_iter()
        .filter(|invocation| after.as_ref().is_none_or(|after| key(invocation) < *after))
        .peekable();
    let mut listed = Vec::new();
    while listed.len() < query.page() {
        let Some(invocation) = rest.next() else {
            break;
        };
        listed.push(invocation);
    }
    let next_cursor = rest
        .peek()
        .is_some()
        .then(|| listed.last().map(cursor_of))
        .flatten();
    Invocations {
        items: listed,
        total,
        waiting,
        pending,
        next_cursor,
    }
}

/// Join each call to the step its own changes record, where the record it
/// addressed holds a change it handed to the session: what the change
/// records is the truth of which run it is part of. A call whose caller said
/// no step (a person acting as themselves, whose changes the app stamps with
/// a step of its own) has no other way to its changes, and would be listed
/// beside them as a run of its own. A call that wrote nothing there keeps
/// the step it computed.
fn join_written(recorded: &BTreeMap<String, Vec<Share>>, calls: &mut [(Option<String>, Call)]) {
    let mut by_change: HashMap<(&str, &str, &str), &str> = HashMap::new();
    for (step, shares) in recorded {
        for share in shares {
            for change in &share.made {
                by_change.insert((&share.app_id, &share.record, change), step);
            }
        }
    }
    for (step, call) in calls {
        let Some((app, record)) = &call.addressed else {
            continue;
        };
        let found = call
            .written
            .iter()
            .filter(|(written_app, written_document, _)| {
                written_app == app && written_document == record
            })
            .find_map(|(_, _, change)| {
                by_change.get(&(app.as_str(), record.as_str(), change.as_str()))
            });
        if let Some(found) = found {
            *step = Some((*found).to_owned());
        }
    }
}

impl WorkspaceActions {
    /// Answer `request`, a listing of `workspace_id`'s runs, on the service's
    /// thread, which checks that the workspace admits `caller`.
    pub(super) fn invocations_request(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        let invocation = request.invocation;
        let receipt =
            move |status| RemoteActionMessage::Receipt(InvocationReceipt { invocation, status });
        let query = match &request.argument {
            InvokeArgument::None => Ok(InvocationsQuery::default()),
            InvokeArgument::Json(arguments) => {
                serde_json::from_value::<InvocationsQuery>(Value::Object(arguments.clone()))
            }
            InvokeArgument::Text(_) => serde_json::from_value::<InvocationsQuery>(Value::Null),
        };
        let query = match query {
            Ok(query) => query,
            Err(error) => {
                return Pending::ready(receipt(ReceiptStatus::Rejected {
                    error: ActionHostError::InvalidRequest {
                        reason: format!("a listing of runs takes {{limit, cursor}}: {error}"),
                    },
                }));
            }
        };
        let (done, answer) = pending();
        let job = Job::Invocations {
            workspace_id,
            caller: caller.clone(),
            query,
            done,
        };
        if let Err(Job::Invocations { done, .. }) = self.submit(job) {
            done.complete(Err(ActionHostError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            }));
        }
        answer.map(move |listed| {
            receipt(match listed {
                Ok(runs) => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: serde_json::to_value(runs).ok(),
                        resources: Vec::new(),
                    },
                },
                Err(error) => ReceiptStatus::Rejected { error },
            })
        })
    }
}

impl Worker {
    /// Note in the ledger the changes `invocation` handed to the session, as
    /// it ran, before they are saved and let go of ([`join_written`]).
    pub(super) fn note_written(&self, invocation: InvocationId) {
        let written: Vec<(String, String, String)> =
            self.handed
                .values()
                .flatten()
                .flat_map(|write| {
                    let route = write.route();
                    write.changes().iter().map(move |change| {
                        (route.app_id.clone(), route.record.clone(), change.clone())
                    })
                })
                .collect();
        if written.is_empty() {
            return;
        }
        if let Some(entry) = self.ledger().entries.get_mut(&invocation) {
            entry.written = written;
        }
    }

    /// The page of `workspace_id`'s runs `query` asks for, for `caller`, whom
    /// the workspace must admit.
    pub(super) fn invocations(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &InvocationsQuery,
    ) -> Result<Invocations, ActionHostError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionHostError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        let routes: Vec<DocumentRoute> = match state.workspace.clone() {
            Some(cluster_id) => self.records(&state, &cluster_id),
            None => Vec::new(),
        };
        let records: Vec<DocumentKey> = routes
            .iter()
            .map(|route| (route.app_id.clone(), route.record.clone()))
            .collect();
        let cluster_id = state.workspace.clone().unwrap_or_default();
        let engine = std::sync::Arc::clone(&state.engine);
        let index = &mut self.membership(workspace_id)?.run_index;
        let unread = index.take_in(
            &records,
            |(app, record)| {
                engine
                    .document_revision(&cluster_id, app, record)
                    .ok()
                    .flatten()
            },
            |(app, record)| engine.record(&cluster_id, app, record).ok().flatten(),
            &ui::history_source::sentence::ThreadWords,
        );
        let mut recorded = index.steps();
        let mut calls = {
            let mut ledger = self.ledger();
            ledger.stamp_ended();
            ledger
                .order
                .iter()
                .filter_map(|id| {
                    let entry = ledger.entries.get(id)?;
                    (entry.workspace == Some(workspace_id) && !is_link_lookup(entry))
                        .then(|| call_of(id.0.to_string(), entry))
                })
                .collect::<Vec<_>>()
        };
        join_written(&recorded, &mut calls);
        // Of it all, what the caller may read now, each record asked once.
        let owner = self.serves_as_owner(caller);
        let who = if owner { None } else { self.bounded(caller) };
        let mut reads: HashMap<DocumentKey, bool> = HashMap::new();
        // A bounded caller reads a record as a read decides it: the record's
        // policy for their person, held to their own bound (an agent's
        // grants, a device's cap, the workspace's sandbox), answering Read at
        // the record's node; Ask is no read.
        let mut may_read = |key: &DocumentKey| {
            *reads.entry(key.clone()).or_insert_with(|| {
                owner
                    || who.as_ref().is_some_and(|who| {
                        self.policy_for(workspace_id, who, &cluster_id, Some((&key.0, &key.1)))
                            .and_then(|policy| super::bounded::bounded_permissions(&policy, who))
                            .is_ok_and(|permissions| {
                                permissions.answer(
                                    &super::TreePath::root().app(&key.0),
                                    super::ActionKind::Read,
                                ) == super::Answer::Allow
                            })
                    })
            })
        };
        recorded.retain(|_, shares| {
            shares.retain(|share| may_read(&(share.app_id.clone(), share.record.clone())));
            !shares.is_empty()
        });
        let across = owner || self.may_across(workspace_id, caller, super::ActionKind::Read);
        calls.retain(|(_, call)| match &call.addressed {
            Some(record) => may_read(record),
            None => across,
        });
        let pending = unread.iter().any(&mut may_read);
        let runs = of_member(listing(recorded, calls), query.by.as_deref());
        Ok(page(runs, query, pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(change: &str, step: Option<&str>, by: &str, time_ms: i64) -> Value {
        let mut entry = json!({
            "change": change,
            "sentence": format!("{by} wrote {change}"),
            "time_ms": time_ms,
            "title": by,
            "group_kind": "agent-session",
            "actor": {"by": {"member": {"agent": "a"}}, "name": by},
        });
        if let Some(step) = step {
            entry["step"] = json!(step);
        }
        entry
    }

    fn undone(mut entry: Value) -> Value {
        entry["undone"] = json!(true);
        entry
    }

    fn call(id: &str, started_ms: i64, update: InvocationUpdate, waiting: bool) -> Call {
        Call {
            id: id.to_owned(),
            started_ms,
            ended_ms: update.is_terminal().then_some(started_ms.saturating_add(5)),
            update,
            waiting,
            by: InvocationBy {
                name: "Jane Doe's agent".into(),
                kind: "agent".into(),
                id: Some("a".into()),
            },
            title: None,
            action: "terminal#run".into(),
            addressed: None,
            written: Vec::new(),
        }
    }

    fn invocation() -> tilo_actions::InvocationId {
        tilo_actions::InvocationId::new()
    }

    fn succeeded() -> InvocationUpdate {
        InvocationUpdate::Succeeded {
            invocation: invocation(),
            output: ActionOutput::default(),
        }
    }

    tilo_test_env::scenario! {
        fn a_workspaces_runs_are_its_records_steps_joined_with_the_calls_it_remembers;

        phase each_step_is_a_run_with_who_made_it_and_what_stands_on_it;
        {
            let entries = [
                change("a1", Some("s1"), "Jane Doe's agent", 10),
                change("a2", Some("s1"), "Jane Doe's agent", 11),
                json!({"change": "p1", "sentence": "Ada typed", "time_ms": 12, "by": "Ada"}),
                change("b1", Some("s2"), "Reviewer", 13),
                json!({"change": "c1", "step": "s2", "change_kind": "Housekeeping", "time_ms": 14}),
                json!({"change": "d1", "step": "s3", "change_kind": "External", "time_ms": 15,
                       "sentence": "Ran echo hello", "actor": {"by": {"member": {"agent": "a"}}, "name": "Jane Doe's agent"}}),
            ];
            let shares = shares("so.tilo.word-processor", "r", &entries);
            assert_eq!(shares.len(), 3, "a person's own edit is no run: {shares:?}");
            let (step, first) = &shares[0];
            assert_eq!(step, "s1");
            assert_eq!((first.changes, first.undone), (2, 0));
            assert_eq!((first.first_ms, first.last_ms), (10, 11));
            assert_eq!(first.by.name, "Jane Doe's agent");
            assert_eq!(first.by.kind, "agent");
            assert_eq!(
                first.by.id.as_deref(),
                Some("a"),
                "the agent's own id tells its runs from another's"
            );
            assert_eq!(first.title, None, "a run named for its agent has no title of its own");
            assert_eq!(first.over.as_deref(), Some("Ada"), "Ada's edit stands on it");
            let (_, second) = &shares[1];
            assert_eq!(second.changes, 1, "housekeeping is no change");
            assert_eq!(second.over, None, "what a command printed holds nothing back");
            let (_, ran) = &shares[2];
            assert_eq!((ran.changes, ran.edits), (1, 0), "a command's output is no edit: {ran:?}");
            let runs = listing(BTreeMap::from([("s3".to_owned(), vec![ran.clone()])]), Vec::new());
            assert_eq!(runs[0].status, InvocationStatus::Ended);
            assert!(runs[0].recorded && !runs[0].undoable, "{:?}", runs[0]);
        }

        phase a_runs_words_never_quote_what_it_carried;
        {
            let mut posted = change("p1", Some("s1"), "Jane Doe's agent", 10);
            posted["what"] = json!({"did": "verb",
                "action": {"app": "so.tilo.chat", "kind": "chat.channel", "action": "post"},
                "args": {"text": "the secret words"}});
            posted["sentence"] = json!("Posted \"the secret words\"");
            let said = shares("so.tilo.chat", "", &[posted]);
            let summary = said[0].1.summary.clone().unwrap_or_default();
            assert!(!summary.is_empty(), "the step says what it did");
            assert!(!summary.contains("secret"), "never what it carried: {summary}");
            let bare = shares("so.tilo.chat", "", &[change("p2", Some("s2"), "Jane Doe's agent", 11)]);
            assert_eq!(bare[0].1.summary, None, "an entry that says not what it did says nothing");
        }

        phase a_call_with_no_step_of_its_own_joins_the_step_its_change_records;
        {
            let recorded: BTreeMap<String, Vec<Share>> =
                shares("so.tilo.notes", "", &[change("c1", Some("s1"), "Ada", 10)])
                    .into_iter()
                    .map(|(step, share)| (step, vec![share]))
                    .collect();
            let mut person = call("theirs", 9, succeeded(), false);
            person.addressed = Some(("so.tilo.notes".into(), String::new()));
            person.written = vec![("so.tilo.notes".into(), String::new(), "c1".into())];
            let mut elsewhere = call("other", 12, succeeded(), false);
            elsewhere.addressed = Some(("so.tilo.notes".into(), String::new()));
            elsewhere.written = vec![("so.tilo.tasks".into(), String::new(), "c1".into())];
            let mut calls = vec![(None, person), (None, elsewhere)];
            join_written(&recorded, &mut calls);
            let runs = listing(recorded, calls);
            assert_eq!(runs.len(), 2, "one act is one run: {runs:?}");
            let joined = runs.iter().find(|run| run.id.as_deref() == Some("theirs")).unwrap();
            assert_eq!(joined.step.as_deref(), Some("s1"));
            assert_eq!(joined.changes, 1);
            assert!(joined.recorded && joined.remembered);
            let apart = runs.iter().find(|run| run.id.as_deref() == Some("other")).unwrap();
            assert_eq!(apart.step, None, "a change in another record joins nothing: {apart:?}");
        }

                phase a_run_taken_back_is_undone_and_stands_under_nothing;
        {
            let entries = [
                undone(change("a1", Some("s1"), "Jane Doe's agent", 10)),
                change("u1", Some("s3"), "Jane Doe's agent", 20),
            ];
            let shares = shares("so.tilo.notes", "", &entries);
            let runs = listing(
                shares.into_iter().map(|(step, share)| (step, vec![share])).collect(),
                Vec::new(),
            );
            assert_eq!(runs.len(), 2);
            let taken = runs.iter().find(|run| run.step.as_deref() == Some("s1")).unwrap();
            assert_eq!(taken.status, InvocationStatus::Undone);
            assert!(!taken.undoable);
            assert_eq!(taken.undo_blocked, None);
            assert!(taken.recorded && !taken.remembered);
            assert_eq!(taken.id, None, "no call of it is remembered");
            assert_eq!(runs[0].step.as_deref(), Some("s3"), "newest first");
        }

        phase a_listing_of_one_member_leaves_anothers_runs_out;
        {
            let mine = shares(
                "so.tilo.notes",
                "",
                &[change("a1", Some("s1"), "Claude", 10)],
            );
            let theirs = shares(
                "so.tilo.notes",
                "",
                &[json!({"change": "b1", "step": "s2", "time_ms": 20,
                         "actor": {"by": {"member": {"agent": "b"}}, "name": "Another agent"}})],
            );
            let runs = listing(
                BTreeMap::from([
                    (mine[0].0.clone(), vec![mine[0].1.clone()]),
                    (theirs[0].0.clone(), vec![theirs[0].1.clone()]),
                ]),
                Vec::new(),
            );
            assert_eq!(of_member(runs.clone(), None).len(), 2, "every run when none is asked for");
            let only_mine = of_member(runs.clone(), Some("a"));
            assert_eq!(only_mine.len(), 1, "{only_mine:?}");
            assert_eq!(only_mine[0].by.id.as_deref(), Some("a"));
            assert!(of_member(runs, Some("nobody")).is_empty());
        }

        phase a_call_joins_its_steps_changes_and_says_how_it_stands;
        {
            let shares = shares(
                "so.tilo.word-processor",
                "r",
                &[change("a1", Some("s1"), "Jane Doe's agent", 100)],
            );
            let recorded = shares.into_iter().map(|(step, share)| (step, vec![share])).collect();
            let runs = listing(
                recorded,
                vec![
                    (Some("s1".into()), call("one", 99, succeeded(), false)),
                    (Some("s9".into()), call("asked", 200, InvocationUpdate::Accepted { invocation: invocation() }, true)),
                    (None, call("alone", 150, InvocationUpdate::Cancelled { invocation: invocation(), message: None }, false)),
                ],
            );
            let ids: Vec<Option<&str>> = runs.iter().map(|run| run.id.as_deref()).collect();
            assert_eq!(ids, [Some("asked"), Some("alone"), Some("one")], "newest first");
            assert_eq!(runs[0].status, InvocationStatus::Waiting);
            assert!(!runs[0].recorded && runs[0].remembered);
            assert_eq!(runs[1].status, InvocationStatus::Cancelled);
            let one = &runs[2];
            assert_eq!(one.status, InvocationStatus::Ended);
            assert_eq!((one.changes, one.started_ms, one.ended_ms), (1, 99, Some(104)));
            assert!(one.undoable && one.recorded && one.remembered);
            assert_eq!(one.records[0].app, "word-processor");
            assert_eq!(one.action.as_deref(), Some("terminal#run"));
        }

        phase a_page_reads_on_from_its_cursor;
        {
            let calls = (0..5)
                .map(|at| (None, call(&format!("c{at}"), at, succeeded(), false)))
                .collect();
            let runs = listing(BTreeMap::new(), calls);
            let first = page(
                runs.clone(),
                &InvocationsQuery {
                    limit: 2,
                    ..InvocationsQuery::default()
                },
                false,
            );
            assert_eq!((first.total, first.items.len()), (5, 2));
            assert_eq!(first.items[0].id.as_deref(), Some("c4"));
            let second = page(
                runs.clone(),
                &InvocationsQuery {
                    limit: 2,
                    cursor: first.next_cursor,
                    ..InvocationsQuery::default()
                },
                false,
            );
            let ids: Vec<Option<&str>> = second.items.iter().map(|run| run.id.as_deref()).collect();
            assert_eq!(ids, [Some("c2"), Some("c1")]);
            let last = page(
                runs,
                &InvocationsQuery {
                    limit: 2,
                    cursor: second.next_cursor,
                    ..InvocationsQuery::default()
                },
                false,
            );
            assert_eq!(last.items.len(), 1);
            assert_eq!(last.next_cursor, None, "the last page names no next");
        }
    }
}
