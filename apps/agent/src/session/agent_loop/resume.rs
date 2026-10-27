//! A run delivered again while one of its steps waits for its person.
//!
//! When the agent restarts while a draft waits in Task Center, the host
//! delivers the run again with the paused step ([`TaskRunPausedStep`]). The
//! model's earlier conversation is gone with the old process; planning the
//! task over would repeat what was done and ask the person about steps they
//! already saw. Instead the run takes up the paused step first, exactly as
//! drafted, waiting for the person's answer as before, and only then lets
//! the model continue, told what happened.
//!
//! The step's arguments are rebuilt from its draft and taken up only when
//! they digest to what the person was shown; otherwise the model is told
//! what waits, so that it asks for that step again with the same arguments.

use anyhow::Result;
use runtime_collab::TaskRunPausedStep;

use super::{LoopEnd, LoopState, Session, StepOutcome, TaskActionStop, task_action_from_offer};
use crate::surface;

/// Most characters of the resumed step's result the model is shown.
const RESULT_CHARS: usize = 2_000;

/// How the paused step was taken up.
pub(super) enum Resumed {
    /// Not a resumed run.
    Fresh,
    /// What the model is told before it plans the rest.
    Told(String),
    /// The run was cancelled while the step waited.
    Ended(LoopEnd),
}

impl Session {
    /// Take up the step `run` is paused at, before the model plans anything.
    pub(super) fn resume_paused_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &mut LoopState,
    ) -> Result<Resumed> {
        let Some(paused) = run.paused.as_ref() else {
            return Ok(Resumed::Fresh);
        };
        let catalogue = self.task_catalogue();
        let offer = catalogue
            .offers
            .iter()
            .find(|offer| {
                // A window publishes its resources under an alias naming
                // that window, and a restart opens a new one: the step is
                // the same resource of the same app, whichever window now
                // publishes it.
                offer.app == paused.app
                    && tilo_actions::selection::logical_resource_id(
                        &offer.descriptor.address.resource,
                    ) == tilo_actions::selection::logical_resource_id(&paused.resource)
                    && offer.descriptor.address.action == paused.action
            })
            .cloned();
        let arguments = paused.draft.arguments().filter(|arguments| {
            tilo_actions::draft::arguments_digest(arguments) == paused.arguments_digest
        });
        let (Some(offer), Some(arguments)) = (offer, arguments) else {
            tracing::info!(action = %paused.action, "a resumed run's paused step is left to the model");
            return Ok(Resumed::Told(retell(paused)));
        };
        let action = match task_action_from_offer(&offer, arguments.clone()) {
            Ok(action) => action,
            Err(error) => {
                tracing::info!(%error, "a resumed run's paused step no longer fits its action");
                return Ok(Resumed::Told(retell(paused)));
            }
        };
        tracing::info!(action = %paused.action, "resuming a run at its paused step");
        let index = state.completed;
        let outcome = self.run_task_step(run, index, &action, &arguments)?;
        state.step_index += 1;
        let label = step_label(paused);
        let told = match outcome {
            Ok(outcome) => {
                self.chooser
                    .mark(tilo_model_client::TraceEvent::ActionSettled, index);
                state.completed += 1;
                *state.done_apps.entry(offer.app).or_default() += 1;
                let told = format!(
                    "{label} ran once the person answered, and succeeded. {}",
                    result_line(&outcome)
                );
                state.previous = Some(outcome);
                told
            }
            Err(TaskActionStop::Failed(message)) => {
                format!("{label} did not run: {message}")
            }
            Err(TaskActionStop::Cancelled(message)) => {
                return Ok(Resumed::Ended(LoopEnd::Cancelled(message.unwrap_or_else(
                    || "Cancelled while waiting for confirmation".into(),
                ))));
            }
        };
        Ok(Resumed::Told(format!("{}\n{told}", interrupted())))
    }
}

/// What every resumed run is told first.
fn interrupted() -> &'static str {
    "This task was interrupted while one of its steps waited for the person's confirmation, \
     and has resumed at that step. Steps before it may already have been carried out: check \
     the current state before repeating any of them, and do not repeat the step below."
}

/// The paused step as the model reads it.
fn step_label(paused: &TaskRunPausedStep) -> String {
    let what = surface::encode_parts(paused.resource.as_str(), paused.action.as_str());
    let gist = paused
        .draft
        .gist()
        .map(|gist| format!(" ({gist})"))
        .unwrap_or_default();
    format!(
        "The step \"{}\" on {} in {}{gist} [{what}]",
        paused.draft.action_label, paused.draft.resource_label, paused.app
    )
}

/// Telling the model what waits, when the agent cannot take it up itself.
fn retell(paused: &TaskRunPausedStep) -> String {
    let fields = paused
        .draft
        .fields
        .iter()
        .map(|field| format!("{}: {}", field.path.join("."), field.value.line()))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "This task was interrupted while one of its steps waited for the person's \
         confirmation, and has resumed. {} still waits for them, drafted with {fields}. Steps \
         before it may already have been carried out: check the current state before \
         repeating any. Ask for that step again with exactly the same parameters; it will run \
         once the person answers.",
        step_label(paused)
    )
}

/// The resumed step's result, cut to [`RESULT_CHARS`].
fn result_line(outcome: &StepOutcome) -> String {
    let Some(value) = outcome.output.value.as_ref() else {
        return String::new();
    };
    let text = value.to_string();
    let shown: String = text.chars().take(RESULT_CHARS).collect();
    let more = if shown.len() < text.len() { "…" } else { "" };
    format!("It returned: {shown}{more}")
}
