//! One instruction that changed more than one application, recorded as one
//! thing a person can undo.
//!
//! "Crop the photo and put it in the report" changes two documents, and
//! neither app's history can undo the other's. [`groups`](crate::history_source::groups)
//! names a run *within* one document; this is the record above it.
//!
//! It lives in the coordinator's collaborative document (often the room
//! document, or the primary document for a headless agent). That coordinator
//! cannot materialise every app's document, so a record holds no content,
//! only references. Undoing is
//! [`revert_run`](crate::history_source::revert::revert_run) against each
//! app's own document, performed by the app that owns it.
//!
//! The key is derived from the work and the records sit at the root: two
//! coordinators recording the same run land on one map key automerge
//! resolves identically everywhere, whereas a container would itself be a
//! thing two writers race to create — losing that race discards every
//! record inside it.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};
use tilo_automerge::ReadTyped as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc, Value};

use crate::history_source::revert::ChangeRun;
use crate::widget::history::HistoryEntryId;
use crate::widget::history_group::{from_hex16, hex16};
use tilo_automerge::read::int_at;

/// Prefix of the root keys holding this document's cross-application runs.
/// Namespaced like [`GROUPS_KEY`](crate::history_source::groups::GROUPS_KEY) so
/// it cannot collide with an app's own schema.
pub const RUNS_KEY: &str = "tilo:run:";

/// Longest label stored. A run's label is the instruction as the person phrased
/// it, and past this it stops fitting anywhere it is rendered.
pub const MAX_LABEL_LEN: usize = 200;

/// One application's share of a run.
///
/// Holds no content — only where the work is and which changes it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStep {
    /// Application that owns the document, e.g. `so.tilo.word-processor`. The
    /// same id the collaboration broker routes on, so a coordinator can find
    /// the peer to ask.
    pub app: String,
    /// The record within that app, as the app announced it (`file:<path>`, or a
    /// tab record id). Empty when the app named none.
    pub record: String,
    /// The band of changes this step produced, in that app's document.
    pub run: ChangeRun,
    /// What the step did, as history entries record it, each different
    /// thing once. Readers render it ([`RunStep::sentence`]); never the source
    /// of truth for what gets undone.
    pub what: Vec<tilo_actions::Described>,
}

impl RunStep {
    /// What the step did, in this thread's language: each thing it did, in
    /// order.
    pub fn sentence(&self) -> String {
        let entries: Vec<String> = self
            .what
            .iter()
            .map(|described| {
                crate::history_source::sentence::sentence(
                    &tilo_actions::ActionContext::for_app(&self.app, &self.app)
                        .with_described(described.clone()),
                )
            })
            .collect();
        entries.join(&crate::i18n::widget_format(
            "history-run-step-separator",
            &[],
        ))
    }
}

/// One instruction that spanned one or more applications.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRun {
    /// Document key of the record. A caller addresses an update through this.
    pub id: String,
    /// The instruction, as the person phrased it.
    pub label: String,
    /// Actor hex of whoever's work this is. `None` when the recorder did not
    /// say.
    pub by: Option<String>,
    /// Actor hex of the person whose edit most recently supplied the
    /// instruction. This is evidence from the document's causal history,
    /// rather than a display name the agent guessed from prose.
    pub issued_by: Option<String>,
    /// The request without the agent's addressing prefix. Keeping the exact
    /// wording makes a completed run reviewable after the instruction block
    /// itself has been removed from the document.
    pub instruction: Option<String>,
    /// How the agent selected the work it performed (for example, a model's
    /// short rationale or an explicit deterministic fallback reason).
    pub decision: Option<String>,
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
/// The transaction is left open — the caller commits, so the record lands
/// stamped and attributed like any other edit. A run with no steps is not
/// recorded: nothing changed, so there is nothing to offer undo on.
pub fn record_run(
    doc: &mut AutoCommit,
    label: &str,
    by: Option<&str>,
    steps: &[RunStep],
) -> Option<String> {
    record_run_with_details(doc, label, by, None, None, None, steps)
}

/// Record a run with the task evidence needed for a later review.
///
/// [`record_run`] remains the small compatibility entry point for callers
/// that only have the resulting changes. Agents should use this form: a run
/// without the instruction, issuer, and decision route is undoable but not
/// meaningfully explainable.
pub fn record_run_with_details(
    doc: &mut AutoCommit,
    label: &str,
    by: Option<&str>,
    issued_by: Option<&str>,
    instruction: Option<&str>,
    decision: Option<&str>,
    steps: &[RunStep],
) -> Option<String> {
    let first = steps.first()?;
    let key = format!("{RUNS_KEY}{}", hex16(&first.run.start));
    let label: String = label.trim().chars().take(MAX_LABEL_LEN).collect();
    let at = i64::try_from(crate::time::unix_now_ms()).unwrap_or(i64::MAX);

    // Replacing the record wholesale keeps a re-record atomic: no peer ever
    // observes the new label against the old step list.
    let rec = doc.put_object(automerge::ROOT, &key, ObjType::Map).ok()?;
    let _ = doc.put(&rec, "label", label);
    if let Some(b) = by {
        let _ = doc.put(&rec, "by", b);
    }
    put_optional(&mut *doc, &rec, "issued_by", issued_by);
    put_optional(&mut *doc, &rec, "instruction", instruction);
    put_optional(&mut *doc, &rec, "decision", decision);
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
        if let Ok(what) = serde_json::to_string(&s.what) {
            let _ = doc.put(&m, "what", what.as_str());
        }
    }
    Some(key)
}

/// Read every recorded run out of `doc`, oldest key first. A record that
/// does not parse is skipped rather than guessed at: an undo affordance
/// pointing at the wrong changes is worse than no affordance.
pub fn read_runs(doc: &AutoCommit) -> Vec<AgentRun> {
    let mut out = Vec::new();
    for key in doc.keys(automerge::ROOT) {
        if !key.starts_with(RUNS_KEY) {
            continue;
        }
        let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, &key) else {
            continue;
        };
        let Some(label) = doc.str_at(&rec, "label") else {
            continue;
        };
        // `filter` above guarantees `ms > 0` by the time the cast below runs,
        // so the conversion never loses a sign; `checked_add` only guards
        // the (unreachable for a real timestamp) case of overflowing
        // `SystemTime`'s own representation.
        let at = int_at(doc, &rec, "at")
            .filter(|ms| *ms > 0)
            .and_then(|ms| u64::try_from(ms).ok())
            .and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms)))
            .unwrap_or(UNIX_EPOCH);
        let mut steps = Vec::new();
        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&rec, "steps") {
            for i in 0..doc.length(&list) {
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                    continue;
                };
                let (Some(start), Some(end)) = (
                    doc.str_at(&m, "start").and_then(|s| from_hex16(&s)),
                    doc.str_at(&m, "end").and_then(|s| from_hex16(&s)),
                ) else {
                    continue;
                };
                steps.push(RunStep {
                    app: doc.str_at(&m, "app").unwrap_or_default(),
                    record: doc.str_at(&m, "record").unwrap_or_default(),
                    run: ChangeRun::new(start, end),
                    what: doc
                        .str_at(&m, "what")
                        .and_then(|what| serde_json::from_str(&what).ok())
                        .unwrap_or_default(),
                });
            }
        }
        if steps.is_empty() {
            continue;
        }
        out.push(AgentRun {
            id: key.clone(),
            label,
            by: doc.str_at(&rec, "by"),
            issued_by: doc.str_at(&rec, "issued_by"),
            instruction: doc.str_at(&rec, "instruction"),
            decision: doc.str_at(&rec, "decision"),
            at,
            steps,
        });
    }
    // Stable order so two peers with the same document build the same list.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn put_optional(doc: &mut AutoCommit, rec: &ObjId, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        let _ = doc.put(rec, key, value);
    }
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

/// True when every patch lands inside a run record — the change did nothing
/// but record bookkeeping, already on screen as a band, so a "1 field" row
/// under it would be noise. An empty patch set is not a marker.
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
