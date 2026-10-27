//! A running task's state where the item it works on is read: a status chip
//! on the issue, in the list, on the board and in the task list alike.
//!
//! The chip is the system's: it reads the run the tracker keeps, so no view
//! has its own idea of how a task is doing. A press opens the issue, which
//! is the one task view — the chat and the steps — wherever the chip is
//! shown, so a person who follows a task from the board and one who follows
//! it from the task list land in the same place.

use ui::prelude::*;
use ui::widget::{Chip, ChipStyle, Tone};

use crate::model::{Run, RunPhase, Tracker};
use crate::{TasksEffect, tr, tr_with};

/// What the chip says: the step the run is in, and that it waits for the
/// person's answer when it does, because that is the one state where the
/// work stops until they act.
pub(crate) fn state(run: &Run) -> String {
    let step = tr(run.state_key());
    if run.asks_its_person() {
        return tr_with("tasks-run-chip-asks", &[("state", &step)]);
    }
    tr_with("tasks-run-chip", &[("state", &step)])
}

/// How the state reads: what wants the person's answer is theirs to do, a
/// run in hand is live, and a run that stopped reads quiet.
pub(crate) fn style(run: &Run) -> ChipStyle {
    if run.asks_its_person() {
        return ChipStyle::Tinted(Tone::Warning);
    }
    match run.shown_phase() {
        RunPhase::Approved | RunPhase::Accepted | RunPhase::Planning | RunPhase::Executing => {
            ChipStyle::Live
        }
        RunPhase::Waiting => ChipStyle::Tinted(Tone::Info),
        RunPhase::Done => ChipStyle::Tinted(Tone::Success),
        RunPhase::Failed => ChipStyle::Tinted(Tone::Danger),
        RunPhase::Stopped => ChipStyle::Neutral,
    }
}

/// The issue's state as a chip that opens the issue, through the effect the
/// app publishes, which is also what a script and an agent invoke: the chip
/// is a way into the one task view, not a second one only a click knows.
pub(crate) fn chip(cx: &ViewCx<Tracker>, id: &Id, run: &Run) -> Chip {
    let label = state(run);
    let chip = Chip::new(label.clone())
        .with_id(format!("tasks-run-chip-{id}"))
        .style(style(run))
        .with_accessible_name(label);
    cx.actions()
        .bind_effect_invocation(chip, &TasksEffect::OpenIssue { issue: id.clone() })
}
