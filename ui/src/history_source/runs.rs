//! One instruction that changed more than one application, recorded as one
//! thing a person can undo.
//!
//! # The problem this exists for
//!
//! Ask an agent to "crop the photo and put it in the report" and two documents
//! change: the photo editor's and the word processor's. Each app's history
//! knows its own half. Neither can undo the other's — a document's undo cannot
//! span two documents, and an app cannot reach into another app's CRDT even if
//! it wanted to.
//!
//! [`ui::history_source::groups`](crate::history_source::groups) already names a
//! contiguous run *within* one document, and that is the right unit for one
//! app. What is missing is the thing above it: a record that says *these two
//! runs, in these two documents, were one instruction*.
//!
//! # Where it lives, and what it holds
//!
//! In the room document — the compositor's, the only document that sees every
//! app in the workspace. The apps' own documents are opaque to the compositor
//! (its collaboration broker routes payloads it never materialises), so the
//! record cannot hold content, and does not:
//!
//! ```text
//! ROOT
//!   "qos:run:<hex of the first step's first change>"
//!      label : "Crop the photo and put it in the report"
//!      by    : "<actor hex of whoever's work this is>"
//!      at    : <unix millis>
//!      steps : [ { app, record, start, end, summary }, … ]
//! ```
//!
//! Each step is a **reference**: which app, which record, and the two
//! content-addressed change ids bracketing what that app did. Undoing is then
//! `revert_run` against each app's own document, performed by the app that owns
//! it — the compositor coordinates and never holds the bytes.
//!
//! Because the ids are content addresses rather than positions, a run recorded
//! on one machine names the same work on every peer that has those changes.
//!
//! # Why the key is derived from the work
//!
//! Same reason as a named range: two coordinators recording the same run land
//! on the same map key, so automerge's map-conflict rule picks one winner
//! deterministically and identically on every peer, instead of leaving two
//! records that would undo the same work twice. And, as there, the records sit
//! at the root rather than inside one container — a container would itself be
//! a thing two writers race to create, and losing that race discards every
//! record inside it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc, ScalarValue, Value};

use crate::history_source::revert::ChangeRun;
use crate::widget::history::HistoryEntryId;
use crate::widget::history_group::{from_hex16, hex16};

/// Prefix of the root keys holding this document's cross-application runs.
/// Namespaced like [`GROUPS_KEY`](crate::history_source::groups::GROUPS_KEY) so
/// it cannot collide with an app's own schema.
pub const RUNS_KEY: &str = "qos:run:";

/// Longest label stored. A run's label is the instruction as the person
/// phrased it, and past this it stops fitting anywhere it is rendered.
pub const MAX_LABEL_LEN: usize = 200;

/// One application's share of a run.
///
/// Holds no content — only where the work is and which changes it was. The
/// document itself is the app's; this is the address of a band inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStep {
    /// Application that owns the document, e.g. `dev.qos.word-processor`. The
    /// same id the collaboration broker routes on, so a coordinator can find
    /// the peer to ask.
    pub app: String,
    /// The record within that app, as the app announced it (`file:<path>`, or
    /// a tab record id). Empty when the app named none.
    pub record: String,
    /// The band of changes this step produced, in that app's document.
    pub run: ChangeRun,
    /// What the step did, in the words the history pane would use. Display
    /// only — never the source of truth for what gets undone.
    pub summary: String,
}

/// One instruction that spanned one or more applications.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRun {
    /// Document key of the record. A caller addresses an update through this.
    pub id: String,
    /// The instruction, as the person phrased it.
    pub label: String,
    /// Actor hex of whoever's work this is — the agent, usually. `None` when
    /// the recorder did not say.
    pub by: Option<String>,
    /// When the run was recorded.
    pub at: SystemTime,
    /// The per-application steps, in the order they happened.
    pub steps: Vec<RunStep>,
}

impl AgentRun {
    /// The applications this run touched, in first-seen order and without
    /// repeats — what a sentence naming the run needs.
    pub fn apps(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.steps {
            if !out.contains(&s.app.as_str()) {
                out.push(&s.app);
            }
        }
        out
    }

    /// The step addressed to `app`/`record`, when this run has one.
    pub fn step_for(&self, app: &str, record: &str) -> Option<&RunStep> {
        self.steps
            .iter()
            .find(|s| s.app == app && s.record == record)
    }
}

/// Record a run. Returns the document key, which is also [`AgentRun::id`].
///
/// The transaction is left open — the caller commits, so the record lands
/// stamped and attributed like any other edit.
///
/// A run with no steps is not recorded: an instruction that changed nothing has
/// nothing to undo, and a record claiming otherwise would put an undo
/// affordance on screen that could only ever fail.
pub fn record_run(
    doc: &mut AutoCommit,
    label: &str,
    by: Option<&str>,
    steps: &[RunStep],
) -> Option<String> {
    let first = steps.first()?;
    let key = format!("{RUNS_KEY}{}", hex16(&first.run.start));
    let label: String = label.trim().chars().take(MAX_LABEL_LEN).collect();
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // Replacing the record wholesale keeps a re-record atomic: no peer ever
    // observes the new label against the old step list.
    let rec = doc.put_object(automerge::ROOT, &key, ObjType::Map).ok()?;
    let _ = doc.put(&rec, "label", label);
    if let Some(b) = by {
        let _ = doc.put(&rec, "by", b);
    }
    let _ = doc.put(&rec, "at", at);
    let list = doc.put_object(&rec, "steps", ObjType::List).ok()?;
    for (i, s) in steps.iter().enumerate() {
        let Ok(m) = doc.insert_object(&list, i, ObjType::Map) else {
            continue;
        };
        let _ = doc.put(&m, "app", s.app.as_str());
        let _ = doc.put(&m, "record", s.record.as_str());
        let _ = doc.put(&m, "start", hex16(&s.run.start));
        let _ = doc.put(&m, "end", hex16(&s.run.end));
        let _ = doc.put(&m, "summary", s.summary.as_str());
    }
    Some(key)
}

/// Read every recorded run out of `doc`, oldest key first.
///
/// A record that does not parse is skipped rather than guessed at: an undo
/// affordance pointing at the wrong changes is worse than no affordance.
pub fn read_runs(doc: &AutoCommit) -> Vec<AgentRun> {
    let mut out = Vec::new();
    for key in doc.keys(automerge::ROOT) {
        if !key.starts_with(RUNS_KEY) {
            continue;
        }
        let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, &key) else {
            continue;
        };
        let Some(label) = str_at(doc, &rec, "label") else {
            continue;
        };
        let at = int_at(doc, &rec, "at")
            .filter(|ms| *ms > 0)
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms as u64))
            .unwrap_or(UNIX_EPOCH);
        let mut steps = Vec::new();
        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&rec, "steps") {
            for i in 0..doc.length(&list) {
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                    continue;
                };
                let (Some(start), Some(end)) = (
                    str_at(doc, &m, "start").and_then(|s| from_hex16(&s)),
                    str_at(doc, &m, "end").and_then(|s| from_hex16(&s)),
                ) else {
                    continue;
                };
                steps.push(RunStep {
                    app: str_at(doc, &m, "app").unwrap_or_default(),
                    record: str_at(doc, &m, "record").unwrap_or_default(),
                    run: ChangeRun::new(start, end),
                    summary: str_at(doc, &m, "summary").unwrap_or_default(),
                });
            }
        }
        if steps.is_empty() {
            continue;
        }
        out.push(AgentRun {
            id: key.clone(),
            label,
            by: str_at(doc, &rec, "by"),
            at,
            steps,
        });
    }
    // Stable order so two peers with the same document build the same list.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Remove a run record. The changes it referenced stay untouched in their own
/// documents; only the grouping goes.
pub fn forget_run(doc: &mut AutoCommit, id: &str) -> bool {
    if !id.starts_with(RUNS_KEY) {
        return false;
    }
    if doc.get(automerge::ROOT, id).ok().flatten().is_none() {
        return false;
    }
    doc.delete(automerge::ROOT, id).is_ok()
}

/// True when every patch lands inside a run record — the change did nothing but
/// record bookkeeping.
///
/// The history producer filters on this for the same reason it filters group
/// markers: the run is already on screen as a band, and a "1 field" row under
/// it is noise. An empty patch set is not a marker.
pub fn is_run_marker(patches: &[Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(RUNS_KEY),
            Some(_) => false,
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(RUNS_KEY)
                }
                _ => false,
            },
        })
}

/// The run whose first step starts at `start`, if one is recorded.
pub fn run_starting_at(doc: &AutoCommit, start: &HistoryEntryId) -> Option<AgentRun> {
    let key = format!("{RUNS_KEY}{}", hex16(start));
    read_runs(doc).into_iter().find(|r| r.id == key)
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(t) => Some(t.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn int_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(n) => Some(*n),
            ScalarValue::Uint(n) => Some(*n as i64),
            _ => None,
        },
        _ => None,
    }
}
