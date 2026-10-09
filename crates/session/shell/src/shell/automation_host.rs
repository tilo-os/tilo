//! Shortcuts in the native session.
//!
//! The broker publishes, saves and runs a workspace's shortcuts
//! ([`tilo_action_broker::automation`], re-exported here). This module holds
//! what only the session does with them: binding keyboard triggers into the
//! device keymap, and recording a finished run in workspace history, where
//! Activity lists it under the shortcut's name and can undo it whole.

use automerge::AutoCommit;
use settings_store::hardware::{ActionChord, KeyChord};
use tilo_actions::context::ActionContext;
use tilo_automation::{RunId, RunOutcome, TriggerSpec};

pub use tilo_action_broker::automation::*;

use crate::shell::automation_triggers::Armed;

/// The chords the keyboard triggers of `armed` bind, as the keymap lists
/// them.
#[must_use]
pub fn trigger_chords(armed: &[Armed<'_>]) -> Vec<ActionChord> {
    armed
        .iter()
        .flat_map(|armed| {
            armed.shortcut.triggers.iter().filter_map(move |trigger| {
                let TriggerSpec::Keyboard { chord } = trigger else {
                    return None;
                };
                Some(ActionChord {
                    chord: KeyChord::parse(chord).ok()?,
                    target: chord_target(&armed.room, &armed.shortcut.id),
                    label: format!("Run {}", armed.shortcut.name),
                })
            })
        })
        .collect()
}

/// Whether a change message says the change was made by run `run`.
fn made_by_run(message: Option<&str>, run: RunId) -> Option<ActionContext> {
    let context = ActionContext::from_change_message(message?)?;
    let group = context.group.as_ref()?;
    (group.kind == "shortcut" && group.id == run.0.to_string()).then_some(context)
}

/// One document's share of a run, with when it was first changed.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentShare {
    /// Wall-clock time of the run's first change here, in milliseconds.
    pub at: i64,
    pub step: ui::RunStep,
}

/// Document `record`'s share of run `run`: the band from its first to its
/// last committed change grouped under the run. `None` when the run did not
/// change it. Reads through a shared reference, so a transaction still open
/// in the document stays its producer's.
pub fn document_share(run: RunId, record: &str, doc: &AutoCommit) -> Option<DocumentShare> {
    let mut first: Option<(i64, String)> = None;
    let mut last = None;
    let mut app = String::new();
    let mut labels: Vec<String> = Vec::new();
    for version in tilo_resource::history::committed_versions(doc) {
        let Some(context) = made_by_run(version.message.as_deref(), run) else {
            continue;
        };
        if first.is_none() {
            first = Some((version.time_ms, version.hash.to_string()));
        }
        last = Some(version.hash.to_string());
        if let Some(owner) = &context.app
            && app.is_empty()
        {
            app.clone_from(&owner.id);
        }
        if let Some(label) = ui::history_source::sentence::step_summary(&context)
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    let (at, start) = first?;
    let end = last?;
    Some(DocumentShare {
        at,
        step: ui::RunStep {
            app,
            record: record.to_string(),
            run: ui::ChangeRun::new(
                ui::history_source::entry_id_from_hash(&start),
                ui::history_source::entry_id_from_hash(&end),
            ),
            summary: labels.join(", "),
        },
    })
}

/// The steps of a run, in the order its documents were first changed.
#[must_use]
pub fn ordered_steps(mut shares: Vec<DocumentShare>) -> Vec<ui::RunStep> {
    shares.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.step.record.cmp(&right.step.record))
    });
    shares.into_iter().map(|share| share.step).collect()
}

/// Record `finished` in `coordinator` as one run made of `steps`, so
/// Activity lists it under the shortcut's name, attributed to whoever
/// invoked it, and can undo it whole. `None` when there are no steps: the
/// run changed nothing that can be undone. The transaction is left open for
/// the caller to commit.
pub fn record_run_steps(
    coordinator: &mut AutoCommit,
    finished: &FinishedShortcutRun,
    steps: &[ui::RunStep],
) -> Option<String> {
    let decision = format!(
        "Shortcut “{}” · {} run · {}",
        finished.shortcut.name,
        finished.report.trigger,
        match &finished.report.outcome {
            RunOutcome::Completed => "completed".to_string(),
            RunOutcome::Failed { path, failure } => format!("stopped at {path}: {failure}"),
        }
    );
    ui::record_run_with_details(
        coordinator,
        &finished.shortcut.name,
        None,
        Some(&finished.invoker),
        Some(&format!("Run shortcut “{}”", finished.shortcut.name)),
        Some(&decision),
        steps,
    )
}

/// [`record_run_steps`] with the steps found in `documents`.
pub fn record_shortcut_run(
    coordinator: &mut AutoCommit,
    finished: &FinishedShortcutRun,
    documents: &mut [(String, &mut AutoCommit)],
) -> Option<String> {
    let shares = documents
        .iter_mut()
        .filter_map(|(record, doc)| document_share(finished.report.run, record, doc))
        .collect();
    record_run_steps(coordinator, finished, &ordered_steps(shares))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_grouped_under_the_run_become_its_steps() {
        let run = RunId::new();
        let context = |label: &str, group: &str| {
            ActionContext::for_resource("notes.note:1", "notes.note", "Note")
                .with_app("so.tilo.notes", "Notes")
                .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                    tilo_actions::ActionKey::new("so.tilo.notes", "notes.note", label),
                    [],
                )))
                .with_group(group, "Tidy", "shortcut")
                .change_message()
        };
        let mut notes = AutoCommit::new();
        let mut untouched = AutoCommit::new();
        for (key, message) in [
            ("before", None),
            ("title", context("Rename", &run.0.to_string())),
            ("body", context("Rewrite", &run.0.to_string())),
            ("other", context("Rename", "another-run")),
        ] {
            automerge::transaction::Transactable::put(&mut notes, automerge::ROOT, key, "x")
                .expect("write");
            notes.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(message.unwrap_or_else(|| "typed by hand".to_string())),
            );
        }
        automerge::transaction::Transactable::put(&mut untouched, automerge::ROOT, "k", "v")
            .expect("write");
        untouched.commit();

        assert_eq!(
            document_share(run, "file:untouched", &untouched),
            None,
            "a document the run did not change is no step"
        );
        let steps = ordered_steps(
            document_share(run, "file:notes", &notes)
                .into_iter()
                .collect(),
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].app, "so.tilo.notes");
        assert_eq!(steps[0].record, "file:notes");
        assert_eq!(steps[0].summary, "Rename in Note, Rewrite in Note");
        assert_ne!(steps[0].run.start, steps[0].run.end);
    }
}
