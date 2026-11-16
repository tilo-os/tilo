//! Every run of a workspace, newest first, as the host knows them
//! ([`tilo_actions::remote::workspace_runs`]): from what the workspace's
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
//! passes for them ([`Worker::policy_for`], the host's own person as the
//! owner), a call the ledger remembers when they may read the record it
//! addressed (or, for a call on no record, the workspace's content). A run
//! with nothing left for them is not listed, nor counted, nor said to wait;
//! their own runs are no exception, as `tilo_status` reads those by id. The
//! index is the workspace's, shared; each listing asks the policy again,
//! once for each record, so a Read taken away shows on the next one. What a
//! call was given, what it answered and why it failed in words stay its
//! caller's: a run says what was asked for, how it stands, and a failure's
//! code.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use tilo_actions::remote::workspace_runs::{Run, RunBy, RunRecord, RunStatus, Runs, RunsQuery};
use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, pending,
};
use tilo_actions::{ActionOutput, EffectReceiptStatus, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;
use ui::history_source::words::HistoryWords;

use super::ledger::Entry;
use super::{Job, Needs, Worker, WorkspaceActions, record_of};

/// One record of a workspace: its app and its id there.
type RecordKey = (String, String);

/// What one record's history says of one run (one step).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Share {
    app_id: String,
    record: String,
    by: RunBy,
    title: Option<String>,
    first_ms: i64,
    last_ms: i64,
    /// Every change it made to the record, from outside included.
    changes: usize,
    /// Of those, its edits of what the record holds: its undo steps.
    edits: usize,
    /// Of its edits, those taken back.
    undone: usize,
    /// What its newest change did.
    summary: Option<String>,
    /// Who changed the record after its last change still in effect, while
    /// that change of theirs is in effect too.
    over: Option<String>,
}

/// What one record's history says of each run that changed it, by step, as
/// it stood at a revision.
type Read = ([u8; 32], Vec<(String, Share)>);

/// What every record's history says of the workspace's runs, read again for
/// a record only once it changed.
#[derive(Default)]
pub(super) struct Index {
    records: HashMap<RecordKey, Read>,
}

impl Index {
    /// Read again each of `records` that changed since the last look:
    /// `revision` says where one stands without reading it, `document` reads
    /// it whole, both `None` for a record the replica cannot open yet.
    /// The records that could not be read.
    fn take_in(
        &mut self,
        records: &[RecordKey],
        revision: impl Fn(&RecordKey) -> Option<[u8; 32]>,
        document: impl Fn(&RecordKey) -> Option<Vec<u8>>,
        words: &dyn HistoryWords,
    ) -> Vec<RecordKey> {
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
        let listed: HashSet<&RecordKey> = records.iter().collect();
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

/// Whether the history entry is someone's edit of what the record holds: an
/// undo step, which their undo takes back. Content from outside (what a
/// command printed, recorded) is a run's change too, but nobody's step; and
/// housekeeping (naming a checkpoint) is no change of the run's at all.
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
/// the run, when it named it.
fn author(entry: &Value) -> (RunBy, Option<String>) {
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
    } else if text(entry, "run_kind") == Some(tilo_actions::context::AGENT_SESSION_GROUP) {
        "agent"
    } else {
        "person"
    };
    let named = actor.and_then(|actor| text(actor, "name"));
    let run = text(entry, "run");
    let name = named
        .or(run)
        .or_else(|| text(entry, "by"))
        .unwrap_or("someone");
    // A run goes by its agent's name unless its client named it.
    let title = run.filter(|run| Some(*run) != named).map(str::to_owned);
    (
        RunBy {
            name: name.to_owned(),
            kind: kind.to_owned(),
        },
        title,
    )
}

/// What the history `entries` of the record `record` of `app_id` (oldest
/// first, each whole) say of each run that changed it: its share, by step.
/// A change in no step is nobody's run: a person's own edit on a page.
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
        share.first_ms = share.first_ms.min(time);
        share.last_ms = share.last_ms.max(time);
        if let Some(sentence) = text(entry, "sentence") {
            share.summary = Some(sentence.to_owned());
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
    by: RunBy,
    title: Option<String>,
    action: String,
    /// The record it addressed, by its app and its id there; `None` for a
    /// call on no record (the workspace's own actions).
    addressed: Option<RecordKey>,
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
    let addressed = record_of(&target.provider, "").map(|route| (route.app_id, route.record));
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
        by: RunBy {
            name,
            kind: kind.to_owned(),
        },
        title: super::said_run_title(&entry.request),
        action,
        addressed,
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
fn run(step: Option<String>, mut calls: Vec<Call>, shares: &[Share]) -> Run {
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
        RunStatus::Waiting
    } else if !ended {
        RunStatus::Running
    } else {
        match last.map(|call| &call.update) {
            Some(InvocationUpdate::Failed { .. }) => RunStatus::Failed,
            Some(InvocationUpdate::Cancelled { .. }) => RunStatus::Cancelled,
            _ => RunStatus::Ended,
        }
    };
    if ended && edits > 0 && undone == edits {
        status = RunStatus::Undone;
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
        (None, None) => (RunBy::default(), None),
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
    let mut records: Vec<RunRecord> = Vec::new();
    for share in shares {
        let record = RunRecord {
            app: app_path(&share.app_id),
            app_id: share.app_id.clone(),
            record: share.record.clone(),
        };
        if !records.contains(&record) {
            records.push(record);
        }
    }
    Run {
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
fn listing(recorded: BTreeMap<String, Vec<Share>>, calls: Vec<(Option<String>, Call)>) -> Vec<Run> {
    let mut by_step: BTreeMap<String, Vec<Call>> = BTreeMap::new();
    let mut alone: Vec<Call> = Vec::new();
    for (step, call) in calls {
        match step {
            Some(step) => by_step.entry(step).or_default().push(call),
            None => alone.push(call),
        }
    }
    let mut runs: Vec<Run> = Vec::new();
    for (step, shares) in recorded {
        let calls = by_step.remove(&step).unwrap_or_default();
        runs.push(run(Some(step), calls, &shares));
    }
    for (step, calls) in by_step {
        runs.push(run(Some(step), calls, &[]));
    }
    for call in alone {
        runs.push(run(None, vec![call], &[]));
    }
    runs.sort_by_key(|run| std::cmp::Reverse(key(run)));
    runs
}

/// Where a run sorts, newest first, and what a cursor names it by.
fn key(run: &Run) -> (i64, String) {
    let name = run
        .step
        .clone()
        .or_else(|| run.id.clone())
        .unwrap_or_default();
    (run.started_ms, name)
}

fn cursor_of(run: &Run) -> String {
    let (started, name) = key(run);
    format!("{started}:{name}")
}

/// The page of `runs` (newest first) `query` asks for.
fn page(runs: Vec<Run>, query: &RunsQuery, pending: bool) -> Runs {
    let total = runs.len();
    let waiting = runs
        .iter()
        .filter(|run| run.status == RunStatus::Waiting)
        .count();
    let after = query.cursor.as_deref().and_then(|cursor| {
        let (started, name) = cursor.split_once(':')?;
        Some((started.parse::<i64>().ok()?, name.to_owned()))
    });
    let mut rest = runs
        .into_iter()
        .filter(|run| after.as_ref().is_none_or(|after| key(run) < *after))
        .peekable();
    let mut listed = Vec::new();
    while listed.len() < query.page() {
        let Some(run) = rest.next() else {
            break;
        };
        listed.push(run);
    }
    let next_cursor = rest
        .peek()
        .is_some()
        .then(|| listed.last().map(cursor_of))
        .flatten();
    Runs {
        runs: listed,
        total,
        waiting,
        pending,
        next_cursor,
    }
}

impl WorkspaceActions {
    /// Answer `request`, a listing of `workspace_id`'s runs, on the service's
    /// thread, which checks that the workspace admits `caller`.
    pub(super) fn runs_request(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        let invocation = request.invocation;
        let receipt =
            move |status| RemoteActionMessage::Receipt(InvocationReceipt { invocation, status });
        let query = match &request.argument {
            InvokeArgument::None => Ok(RunsQuery::default()),
            InvokeArgument::Json(arguments) => {
                serde_json::from_value::<RunsQuery>(Value::Object(arguments.clone()))
            }
            InvokeArgument::Text(_) => serde_json::from_value::<RunsQuery>(Value::Null),
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
        let job = Job::Runs {
            workspace_id,
            caller: caller.clone(),
            query,
            done,
        };
        if let Err(Job::Runs { done, .. }) = self.submit(job) {
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
    /// The page of `workspace_id`'s runs `query` asks for, for `caller`, whom
    /// the workspace must admit.
    pub(super) fn runs(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &RunsQuery,
    ) -> Result<Runs, ActionHostError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionHostError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        let routes: Vec<DocumentRoute> = match state.workspace.clone() {
            Some(cluster_id) => self.records(&state, &cluster_id),
            None => Vec::new(),
        };
        let records: Vec<RecordKey> = routes
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
                    .record_revision(&cluster_id, app, record)
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
        // Of it all, what the caller may read now, each record asked once.
        let owner = self.serves_as_owner(caller);
        let who = if owner { None } else { self.bounded(caller) };
        let mut reads: HashMap<RecordKey, bool> = HashMap::new();
        let mut may_read = |key: &RecordKey| {
            *reads.entry(key.clone()).or_insert_with(|| {
                owner
                    || who.as_ref().is_some_and(|who| {
                        self.policy_for(workspace_id, who, &cluster_id, Some((&key.0, &key.1)))
                            .is_ok()
                    })
            })
        };
        recorded.retain(|_, shares| {
            shares.retain(|share| may_read(&(share.app_id.clone(), share.record.clone())));
            !shares.is_empty()
        });
        let across = owner
            || who.as_ref().is_some_and(|who| {
                self.policy_for(workspace_id, who, &cluster_id, None)
                    .is_ok()
            });
        calls.retain(|(_, call)| match &call.addressed {
            Some(record) => may_read(record),
            None => across,
        });
        let pending = unread.iter().any(&mut may_read);
        Ok(page(listing(recorded, calls), query, pending))
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
            "run": by,
            "run_kind": "agent-session",
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
            by: RunBy {
                name: "Jane Doe's agent".into(),
                kind: "agent".into(),
            },
            title: None,
            action: "terminal#run".into(),
            addressed: None,
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
            assert_eq!(first.title, None, "a run named for its agent has no title of its own");
            assert_eq!(first.over.as_deref(), Some("Ada"), "Ada's edit stands on it");
            let (_, second) = &shares[1];
            assert_eq!(second.changes, 1, "housekeeping is no change");
            assert_eq!(second.over, None, "what a command printed holds nothing back");
            let (_, ran) = &shares[2];
            assert_eq!((ran.changes, ran.edits), (1, 0), "a command's output is no edit: {ran:?}");
            let runs = listing(BTreeMap::from([("s3".to_owned(), vec![ran.clone()])]), Vec::new());
            assert_eq!(runs[0].status, RunStatus::Ended);
            assert!(runs[0].recorded && !runs[0].undoable, "{:?}", runs[0]);
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
            assert_eq!(taken.status, RunStatus::Undone);
            assert!(!taken.undoable);
            assert_eq!(taken.undo_blocked, None);
            assert!(taken.recorded && !taken.remembered);
            assert_eq!(taken.id, None, "no call of it is remembered");
            assert_eq!(runs[0].step.as_deref(), Some("s3"), "newest first");
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
            assert_eq!(runs[0].status, RunStatus::Waiting);
            assert!(!runs[0].recorded && runs[0].remembered);
            assert_eq!(runs[1].status, RunStatus::Cancelled);
            let one = &runs[2];
            assert_eq!(one.status, RunStatus::Ended);
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
            let first = page(runs.clone(), &RunsQuery { limit: 2, cursor: None }, false);
            assert_eq!((first.total, first.runs.len()), (5, 2));
            assert_eq!(first.runs[0].id.as_deref(), Some("c4"));
            let second = page(
                runs.clone(),
                &RunsQuery { limit: 2, cursor: first.next_cursor },
                false,
            );
            let ids: Vec<Option<&str>> = second.runs.iter().map(|run| run.id.as_deref()).collect();
            assert_eq!(ids, [Some("c2"), Some("c1")]);
            let last = page(runs, &RunsQuery { limit: 2, cursor: second.next_cursor }, false);
            assert_eq!(last.runs.len(), 1);
            assert_eq!(last.next_cursor, None, "the last page names no next");
        }
    }
}
