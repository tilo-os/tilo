//! Noticing repeated work, and refining a shortcut from what the person says
//! about a run.
//!
//! The person who takes the same few actions over and over is offered the
//! shortcut that would have done it for them. Detection reads the sequence
//! the app was handed — the actions invoked in this window, in order — and
//! nothing else: there is no memory store behind it, and what was noticed
//! goes away with the window.
//!
//! A proposal is a draft like any other: ordinary invocations of the actions
//! that were repeated, the least grant those steps need
//! ([`grant::least_privilege`]), and the checker's verdict before it is
//! offered. Accepting one opens it as a draft to review; rejecting one is
//! remembered in the workbench ([`crate::model::Workbench::ignore_proposal`]),
//! so the same work is not offered twice.
//!
//! Refinement goes the same way as drafting: the person says what should be
//! different, the agent answers with the whole program, and the difference
//! from what they had is what is offered to review. A refined program is
//! checked like any other edit and refused with the checker's diagnostics.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tilo_automation::{ActionCatalog, ResourceTarget, Selector, Shortcut, Step, StepKind};
use ui::tilo_actions::ActionKey;

use crate::drafting::{self, Drafted};

/// How many kinds of action one proposal covers. A longer sequence is work a
/// person repeats less often, and its shortcut would be a program in its own
/// right rather than a name for what they just did.
pub const MAX_PROPOSED_STEPS: usize = 8;
/// The most recent invocations the detector reads: those are the ones a
/// person is repeating.
pub const MAX_OBSERVED: usize = 256;
/// How many times a sequence has to be taken in a row before it is offered.
pub const REPEATS: usize = 3;

/// One action the person took, as the catalogue names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    /// The app publishing the action.
    pub app: String,
    /// The kind of the resource it was taken on.
    pub kind: String,
    /// The action's own name.
    pub action: String,
    /// The resource it was taken on, when the host says: what "similar
    /// resources" is read from.
    pub resource: Option<String>,
    /// What the action is called, when the log says; the catalogue names it
    /// otherwise.
    pub label: Option<String>,
}

impl Occurrence {
    /// The action without its resource: what makes two invocations the same
    /// kind of step.
    #[must_use]
    pub fn key(&self) -> String {
        ActionKey::new(self.app.as_str(), self.kind.as_str(), self.action.as_str()).to_string()
    }
}

/// Repeated work the app noticed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    /// The sequence of actions one run took.
    pub steps: Vec<Occurrence>,
    /// How many times the sequence was taken.
    pub times: usize,
    /// The distinct resources the runs touched, most recent first.
    pub resources: Vec<String>,
}

impl Proposal {
    /// The name of the shortcut that would do it: the steps' labels in order
    /// ("Rename then move").
    #[must_use]
    pub fn name(&self, catalog: &ActionCatalog) -> String {
        let names: Vec<String> = self
            .steps
            .iter()
            .map(|step| label(step, catalog))
            .filter(|label| !label.is_empty())
            .collect();
        let name = match names.as_slice() {
            [] => String::from("New shortcut"),
            [only] => only.clone(),
            _ => names.join(", then "),
        };
        ui::format::capitalize_first(&name)
    }

    /// What was noticed, in the person's words.
    #[must_use]
    pub fn evidence(&self) -> String {
        let noun = if self.steps.len() == 1 {
            "step"
        } else {
            "steps"
        };
        format!(
            "You did these {} {noun} {} times in a row.",
            self.steps.len(),
            self.times
        )
    }

    /// What a rejection is remembered under: the sequence of actions, not how
    /// often or where they were taken.
    #[must_use]
    pub fn key(&self) -> String {
        self.steps
            .iter()
            .map(Occurrence::key)
            .collect::<Vec<_>>()
            .join(" > ")
    }

    /// The shortcut that does it: one invocation per step, each argument
    /// still to be bound.
    ///
    /// The runs touched resources of one kind but not one resource, so a step
    /// names the first resource of its kind that offers the action rather
    /// than one of those ids: the shortcut is for the work, not for those
    /// files.
    #[must_use]
    pub fn program(&self) -> Shortcut {
        let mut steps: Vec<Step> = Vec::new();
        for step in &self.steps {
            let kind = ActionKey::new(step.app.as_str(), step.kind.as_str(), step.action.as_str());
            let id = crate::model::unique_step_id(&steps, &step.action, "step");
            steps.push(Step::new(
                id,
                StepKind::Invoke {
                    action: kind,
                    resource: Selector::FirstOfKind(step.kind.clone()),
                    args: BTreeMap::new(),
                    appearance: None,
                },
            ));
        }
        Shortcut {
            id: String::new(),
            name: String::new(),
            inputs: Vec::new(),
            output: None,
            grant: Vec::new(),
            steps,
            triggers: Vec::new(),
            step_timeout_secs: None,
            saved_by: None,
        }
    }
}

/// What one step is called: the log's own word for it, else the catalogue's.
pub fn label(step: &Occurrence, catalog: &ActionCatalog) -> String {
    if let Some(label) = &step.label {
        return label.clone();
    }
    let key = ActionKey::new(step.app.as_str(), step.kind.as_str(), step.action.as_str());
    catalog
        .find(&key, ResourceTarget::Kind(step.kind.as_str()))
        .and_then(|row| row.action.as_ref())
        .map_or_else(|| step.action.clone(), |action| action.action_label.clone())
}

/// Whether the `length` invocations ending at `end` repeat the ones ending
/// at `end` before them: one more run of the same work.
fn same_run(observed: &[Occurrence], end: usize, length: usize) -> bool {
    if length == 0 {
        return false;
    }
    let Some(before) = end
        .checked_sub(2 * length)
        .and_then(|from| observed.get(from..end - length))
    else {
        return false;
    };
    let Some(now) = observed.get(end - length..end) else {
        return false;
    };
    now.iter()
        .zip(before)
        .all(|(now, before)| same_step(now, before))
}

/// Whether `now` repeats `before`: the same action, taken on the same
/// resource or — when both named one — on another of its kind, since tidying
/// two folders one after the other is the same work, and tidying one folder
/// twice over is not. A resource the host does not name never matches one it
/// does.
fn same_step(now: &Occurrence, before: &Occurrence) -> bool {
    if now.key() != before.key() {
        return false;
    }
    match (&now.resource, &before.resource) {
        (None, None) => true,
        (Some(now), Some(before)) => now == before || same_kind(now, before),
        _ => false,
    }
}

/// Whether two resource ids name resources of one kind: `notes.folder:inbox`
/// and `notes.folder:archive` are both folders.
fn same_kind(now: &str, before: &str) -> bool {
    kind_of(now).is_some_and(|kind| kind_of(before) == Some(kind))
}

/// The kind of a resource id, as the host writes it: the part before the
/// colon.
fn kind_of(id: &str) -> Option<&str> {
    id.split_once(':')
        .map(|(kind, _)| kind)
        .filter(|kind| !kind.is_empty())
}

/// How many times the run of `length` invocations ending at `end` repeats,
/// counting backwards.
fn times_run(observed: &[Occurrence], end: usize, length: usize) -> usize {
    let mut times = 1;
    let mut at = end;
    while same_run(observed, at, length) {
        times += 1;
        at -= length;
    }
    times
}

/// The most recently repeated work in `observed`: the sequence taken the
/// most times ([`REPEATS`] at least), and among equally often taken ones the
/// longest.
///
/// Only a sequence repeated back to back counts, which is what redoing work
/// looks like; a step in between means a different task started.
#[must_use]
pub fn detect(observed: &[Occurrence]) -> Option<Proposal> {
    let observed = observed
        .get(observed.len().saturating_sub(MAX_OBSERVED)..)
        .unwrap_or_default();
    let mut best: Option<(usize, usize)> = None;
    for length in 1..=MAX_PROPOSED_STEPS.min(observed.len() / REPEATS) {
        let times = times_run(observed, observed.len(), length);
        if times < REPEATS {
            continue;
        }
        let better = match best {
            None => true,
            Some((best_length, best_times)) => (times, length) > (best_times, best_length),
        };
        if better {
            best = Some((length, times));
        }
    }
    let (length, times) = best?;
    let touched = observed.get(observed.len() - length * times..)?;
    let mut resources: Vec<String> = Vec::new();
    for occurrence in touched {
        if let Some(resource) = &occurrence.resource
            && !resources.contains(resource)
        {
            resources.push(resource.clone());
        }
    }
    Some(Proposal {
        steps: observed.get(observed.len() - length..)?.to_vec(),
        times,
        resources,
    })
}

/// The arguments of a refinement request: the program as it is, and what the
/// person said about the run.
#[must_use]
pub fn arguments(before: &Shortcut, comment: &str, catalog: &ActionCatalog) -> Map<String, Value> {
    let mut arguments = drafting::arguments(comment, catalog);
    let current = json!({
        "name": before.name,
        "inputs": before.inputs,
        "steps": before.steps,
        "output": before.output,
    });
    let asked = format!(
        "The shortcut as it ran:\n{current}\n\nWhat the person wants different about it:\n{}",
        comment.trim()
    );
    let head = arguments
        .get("prompt")
        .and_then(Value::as_str)
        .and_then(|prompt| {
            prompt
                .split_once("The person asked for:\n")
                .map(|(head, _)| head.to_owned())
        })
        .unwrap_or_default();
    arguments.insert("prompt".to_owned(), Value::String(format!("{head}{asked}")));
    arguments
}

/// Adopt the agent's answer to a refinement as the program to review. It
/// keeps the shortcut's identity and is checked like any other edit, so an
/// answer that does not check comes back with the checker's diagnostics.
///
/// # Errors
///
/// Why the answer is not a shortcut at all.
pub fn refine(
    answer: Value,
    before: &Drafted,
    saved: &[Shortcut],
    catalog: &ActionCatalog,
) -> Result<Drafted, String> {
    let mut refined = drafting::adopt(answer, saved, catalog)?;
    refined.shortcut.id.clone_from(&before.shortcut.id);
    Ok(refined)
}

/// How `after` differs from `before`, one line per step that changed, in the
/// words the editor already uses for a step.
#[must_use]
pub fn diff(before: &[Step], after: &[Step], catalog: &ActionCatalog) -> Vec<String> {
    let old: Vec<String> = before
        .iter()
        .flat_map(|step| crate::editor::outline(step, catalog, before))
        .collect();
    let new: Vec<String> = after
        .iter()
        .flat_map(|step| crate::editor::outline(step, catalog, after))
        .collect();
    let mut lines = Vec::new();
    let common = old.len().min(new.len());
    for (index, (was, is)) in old.iter().zip(&new).enumerate() {
        if was != is {
            lines.push(format!("Step {}: {is}, in place of {was}", index + 1));
        }
    }
    for (index, is) in new.iter().enumerate().skip(common) {
        lines.push(format!("Step {}: {is} is new", index + 1));
    }
    for (index, _) in old.iter().enumerate().skip(common) {
        lines.push(format!("Step {} is removed", index + 1));
    }
    lines
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
