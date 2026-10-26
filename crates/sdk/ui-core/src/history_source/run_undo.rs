//! Undoing an instruction that spanned more than one application.
//!
//! [`runs`](crate::history_source::runs) records *what* the instruction did;
//! [`revert`](crate::history_source::revert) undoes one such reference inside
//! one document. This is the piece between them: coordination and reporting
//! which steps were completed.
//!
//! A run undone while one of its apps is closed leaves a report that no longer
//! mentions a photo and a photo that is still cropped — and from inside either
//! app that looks finished. So any unreachable step refuses the whole run
//! ([`unreachable_steps`], then [`refuse_run`]), nothing is written, and the
//! refusal names the affected apps.
//!
//! The one partial allowed is within a step, where slots somebody edited after
//! the run are kept rather than overwritten. It is never silent:
//! [`RunUndoReport::is_complete`] is false whenever any step carries one.

use automerge::AutoCommit;

use crate::history_source::authors::AuthorDirectory;
use crate::history_source::revert::{KeptSlot, RevertError, revert_run};
use crate::history_source::runs::{AgentRun, RunStep};

/// What happened to one application's share of a run.
#[derive(Clone, Debug, PartialEq)]
pub enum StepState {
    /// Every slot this step wrote was inverted.
    Undone { changes: usize },
    /// Inverted, except for slots somebody edited afterwards — which were left
    /// exactly as they are. A caller rendering this as "undone" is lying.
    KeptSome { changes: usize, kept: Vec<KeptSlot> },
    /// The step's changes are in the document but their effect was already
    /// gone. Not an error.
    NothingToUndo,
    /// This process could not reach the document — the app is not running here,
    /// or the record is not open. Nothing was attempted.
    Unreachable,
    /// Reachable, but deliberately left alone because the run as a whole was
    /// refused. Distinct from [`StepState::Unreachable`]: this app was fine,
    /// another one was not.
    NotAttempted,
    /// The document this step was performed against does not contain the run's
    /// changes. Usually the wrong document, or a peer that has not synced them.
    NotInDocument,
}

impl StepState {
    /// True when this step needs no caveat.
    pub fn is_clean(&self) -> bool {
        matches!(self, StepState::Undone { .. } | StepState::NothingToUndo)
    }
}

/// One application's outcome within a run undo.
#[derive(Clone, Debug, PartialEq)]
pub struct StepOutcome {
    pub app: String,
    pub record: String,
    pub state: StepState,
}

/// What a whole-run undo did, across every application it touched.
///
/// Exists so a caller cannot accidentally report success for a run where one app
/// was unreachable.
#[derive(Clone, Debug, PartialEq)]
pub struct RunUndoReport {
    pub label: String,
    pub steps: Vec<StepOutcome>,
}

impl RunUndoReport {
    /// True only when every step came back clean. This is the one predicate a
    /// caller may put the word "undone" behind.
    pub fn is_complete(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| s.state.is_clean())
    }

    /// True when nothing at all was written — a refusal.
    pub fn is_refusal(&self) -> bool {
        !self.steps.is_empty()
            && self
                .steps
                .iter()
                .all(|s| matches!(s.state, StepState::Unreachable | StepState::NotAttempted))
    }

    /// Steps that need saying out loud — anything not clean.
    pub fn unfinished(&self) -> Vec<&StepOutcome> {
        self.steps.iter().filter(|s| !s.state.is_clean()).collect()
    }

    /// Applications whose share could not be attempted at all, in first-seen
    /// order and without repeats.
    pub fn unreachable_apps(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.steps {
            if matches!(s.state, StepState::Unreachable) && !out.contains(&s.app.as_str()) {
                out.push(&s.app);
            }
        }
        out
    }

    /// Slots kept because somebody edited them after the run, across every app.
    pub fn kept(&self) -> Vec<&KeptSlot> {
        self.steps
            .iter()
            .flat_map(|s| match &s.state {
                StepState::KeptSome { kept, .. } => kept.iter().collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }
}

/// Steps whose documents this process cannot reach.
///
/// The pre-flight a coordinator runs **before** undoing anything. An empty
/// result means every step can be attempted; a non-empty one is a refusal.
pub fn unreachable_steps(run: &AgentRun, reachable: impl Fn(&RunStep) -> bool) -> Vec<&RunStep> {
    run.steps.iter().filter(|s| !reachable(s)).collect()
}

/// Build the report for a run that was refused whole. Nothing was written.
pub fn refuse_run(run: &AgentRun, missing: &[&RunStep]) -> RunUndoReport {
    RunUndoReport {
        label: run.label.clone(),
        steps: run
            .steps
            .iter()
            .map(|s| StepOutcome {
                app: s.app.clone(),
                record: s.record.clone(),
                state: if missing
                    .iter()
                    .any(|m| m.app == s.app && m.record == s.record)
                {
                    StepState::Unreachable
                } else {
                    StepState::NotAttempted
                },
            })
            .collect(),
    }
}

/// Undo one application's share of a run against the document that app owns.
///
/// The record's own history does the undoing — [`revert_run`] over the
/// step's one change, the same revert [`revert_step`] writes for an app's
/// own Ctrl+Z — so a run undone from an agent's report and the same work
/// undone in the app land alike, with the same charge for what a later
/// writer has since changed.
///
/// The transaction is left open; the owning app commits, so the undo lands under
/// the person who asked for it and replicates like any other edit.
pub fn undo_step(
    doc: &mut AutoCommit,
    step: &RunStep,
    by: Option<&str>,
    authors: &AuthorDirectory,
) -> StepOutcome {
    // A step's run is one change, so reverting the run is reverting that
    // change: exactly what an app's own undo writes for it.
    let change = revert_run(doc, &step.run, by, authors);
    let state = match change {
        Err(RevertError::UnknownRun | RevertError::Inverted) => StepState::NotInDocument,
        Ok(o) if !o.kept.is_empty() => StepState::KeptSome {
            changes: o.covered,
            kept: o.kept,
        },
        Ok(o) if !o.wrote => StepState::NothingToUndo,
        Ok(o) => StepState::Undone { changes: o.covered },
    };
    StepOutcome {
        app: step.app.clone(),
        record: step.record.clone(),
        state,
    }
}

/// Undo every step of a run, given the documents the caller could open.
///
/// `open` pairs a step's `(app, record)` with the live document that owns it.
/// The caller assembles it, because only the caller knows which apps are running
/// here — and assembling it is also the pre-flight: any step of `run` with no
/// entry in `open` refuses the whole run, and nothing is written.
///
/// Each visited document is left with an **open** transaction. The caller
/// commits per document, so each undo is stamped and attributed where it lands.
pub fn undo_run(
    run: &AgentRun,
    by: Option<&str>,
    authors: &AuthorDirectory,
    open: &mut [(String, String, &mut AutoCommit)],
) -> RunUndoReport {
    let have = |s: &RunStep| open.iter().any(|(a, r, _)| a == &s.app && r == &s.record);
    let missing = unreachable_steps(run, have);
    if !missing.is_empty() {
        return refuse_run(run, &missing);
    }
    let mut steps = Vec::with_capacity(run.steps.len());
    for step in &run.steps {
        let slot = open
            .iter_mut()
            .find(|(a, r, _)| a == &step.app && r == &step.record);
        match slot {
            Some((_, _, doc)) => steps.push(undo_step(doc, step, by, authors)),
            // Reaching this means the pre-flight and this lookup disagreed,
            // which can only happen if `open` holds duplicates that were
            // consumed. Reported rather than skipped: a step with no outcome
            // would make the report claim less than it knows.
            None => steps.push(StepOutcome {
                app: step.app.clone(),
                record: step.record.clone(),
                state: StepState::Unreachable,
            }),
        }
    }
    RunUndoReport {
        label: run.label.clone(),
        steps,
    }
}
