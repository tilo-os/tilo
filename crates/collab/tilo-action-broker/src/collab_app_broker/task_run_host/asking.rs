//! A run with no issue to ask on asks its person on this host's sheet.
//!
//! An agent door connection's run is the connection's own: no Tasks issue holds it, so a step its
//! grants ask first about cannot wait for a question there. Its tracker keeps the question itself
//! ([`TrackerRuns::asks_on_host`]); the step answers its caller that it needs confirming, and the
//! caller asks through [`ActionHost::confirm`](tilo_actions::remote::ActionHost::confirm), which
//! lands here:
//!
//! - the question is kept on the run, waiting, and put to the person on the host's sheet
//!   ([`crate::app_actions::AppActionConsentPrompter`]), the same request a Tasks run's question
//!   is shown as;
//! - while the sheet is up the caller waits; the caller leaving, the run ending or the person
//!   dismissing the sheet withdraws the question, and the caller hears why;
//! - the person allowing it keeps their answer on the run, so the same request invoked again goes
//!   on with exactly the arguments it asked about, as a Tasks run's confirmed question does
//!   ([`super::super::tracker_runs::answered`]); "always" also keeps the answer grant the sheet
//!   offered, for the run's later steps like it;
//! - the person declining refuses it, in the words every declined step is refused in.
//!
//! Nothing here is the door's: a conversation turn's run could ask the same way by its tracker
//! saying so.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{ActionHostError, Confirmation, ConfirmedInvocation, InvokeRequest};

use super::{TaskRunHost, arguments_of, question_id};
use crate::app_actions::{AppActionConsent, AppActionConsentPrompter};
use crate::services::{TrackerAnswer, TrackerQuestion, TrackerRuns};

/// How often a waiting question looks whether its run still goes.
const STILL_GOES_EVERY: Duration = Duration::from_millis(500);

/// A question on the run and on the sheet, withdrawn from both unless it was answered.
struct Asking {
    tracker: Arc<dyn TrackerRuns>,
    workspace: String,
    issue: String,
    shown: Option<(Arc<dyn AppActionConsentPrompter>, String)>,
    answered: bool,
}

impl Drop for Asking {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        if let Some((prompter, handle)) = &self.shown {
            prompter.withdraw(handle);
        }
        if let Err(error) = self
            .tracker
            .keep_question(&self.workspace, &self.issue, None)
        {
            tracing::warn!(%error, issue = %self.issue, "a withdrawn question stays on its run");
        }
    }
}

impl TaskRunHost {
    /// Whether this connection's run asks its person on this host rather than on an issue.
    pub(super) fn asks_on_host(&self) -> bool {
        let Ok((run, _)) = self.pending_run() else {
            return false;
        };
        let Ok(current) = self.0.state.tracker_run_of(&run) else {
            return false;
        };
        self.0
            .state
            .tracker_runs(self.workspace())
            .is_some_and(|tracker| tracker.asks_on_host(self.workspace(), &current))
    }

    /// Put `request` to the run's person on this host's sheet and wait for their answer: the
    /// confirmation the caller invokes it again with, or why not.
    pub(super) async fn ask_person(
        &self,
        request: &InvokeRequest,
    ) -> Result<ConfirmedInvocation, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let state = Arc::clone(&self.0.state);
        let workspace = self.workspace().to_owned();
        let issue = self.issue().to_owned();
        let (run, _) = self.pending_run().map_err(refused)?;
        let current = state.tracker_run_of(&run).map_err(refused)?;
        let tracker = state
            .tracker_runs(&workspace)
            .cloned()
            .ok_or_else(|| refused("the run's tracker is unavailable".into()))?;
        if !tracker.asks_on_host(&workspace, &current) {
            // Its questions go on its issue: nobody here is asked.
            return Err(ActionHostError::ConfirmationRequired);
        }
        let question = self.question_about(request)?;
        tracker
            .keep_question(&workspace, &issue, Some(question.clone()))
            .map_err(refused)?;
        let mut asking = Asking {
            tracker: Arc::clone(&tracker),
            workspace: workspace.clone(),
            issue: issue.clone(),
            shown: None,
            answered: false,
        };
        let (consent, grant) = state
            .run_question_for(&workspace, &current.starter, &issue)
            .ok_or_else(|| refused("the run's question could not be put to its person".into()))?;
        let nobody = || ActionHostError::Unavailable {
            reason: "nobody is at this desktop to answer the agent's question".into(),
        };
        let prompter = state.consent_prompter().ok_or_else(nobody)?;
        let label = consent.action_label.clone();
        let handle = consent.handle.clone();
        let answer = prompter.ask(consent).ok_or_else(nobody)?;
        asking.shown = Some((prompter, handle));
        let answer = self.answer_to(answer, &label).await?;
        if matches!(
            answer.consent,
            AppActionConsent::Session | AppActionConsent::Always
        ) && let Some(grant) = grant
        {
            tracker
                .keep_answer(&workspace, &issue, grant)
                .map_err(refused)?;
        }
        let edits = serde_json::to_value(answer.allowed_edits())
            .map_err(|error| refused(error.to_string()))?;
        let id = question.id.clone();
        tracker
            .keep_question(
                &workspace,
                &issue,
                Some(TrackerQuestion {
                    answer: TrackerAnswer::Confirmed { edits },
                    ..question
                }),
            )
            .map_err(refused)?;
        asking.answered = true;
        // The person's answer is kept on the run, which decides the step invoked again: the
        // token only names it, and the arguments stay the ones they were asked about.
        Ok(ConfirmedInvocation::unchanged(Confirmation::Witnessed(id)))
    }

    /// The question `request` asks, as the run keeps it: the action, its arguments and the draft
    /// the person is shown.
    fn question_about(&self, request: &InvokeRequest) -> Result<TrackerQuestion, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let argument = request.argument.to_slot();
        let arguments = arguments_of(argument.as_deref()).map_err(refused)?;
        let peer_id = request.target.provider.parse::<u64>().map_err(|_| {
            refused("only a running application's action is asked about here".into())
        })?;
        let (app, offered) = self
            .0
            .state
            .surfaces
            .lock()
            .map_err(|_| refused("task surface unavailable".into()))?
            .get(&peer_id)
            .and_then(|surface| {
                surface
                    .action_projection(
                        &request.target.address.resource,
                        &request.target.address.action,
                    )
                    .map(|(surface, offered)| (surface.app_id, offered))
            })
            .ok_or_else(|| refused("the action asked about is no longer published".into()))?;
        let draft = tilo_actions::Draft::render(
            &offered.descriptor(tilo_actions::ExecutionLocus::ClientLocal),
            &arguments,
        );
        Ok(TrackerQuestion {
            id: question_id(request, &arguments),
            app,
            resource: tilo_actions::selection::logical_resource_id(
                &request.target.address.resource,
            )
            .into_owned(),
            action: request.target.address.action.clone(),
            arguments_digest: hex::encode(tilo_actions::draft::arguments_digest(&arguments)),
            draft: serde_json::to_value(&draft).map_err(|error| refused(error.to_string()))?,
            before: None,
            answer: TrackerAnswer::Waiting,
        })
    }

    /// The person's answer on the sheet to the question about `label`, waited for while the run
    /// goes: why not, where the run ended or the person dismissed it.
    async fn answer_to(
        &self,
        mut answer: tokio::sync::oneshot::Receiver<crate::app_actions::AppActionAnswer>,
        label: &str,
    ) -> Result<crate::app_actions::AppActionAnswer, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let answer = loop {
            tokio::select! {
                answer = &mut answer => break answer,
                () = tilo_rt::sleep(STILL_GOES_EVERY) => {
                    if let Err(reason) = self.still_goes() {
                        return Err(refused(format!(
                            "the question about {label} was withdrawn: {reason}"
                        )));
                    }
                }
            }
        };
        let Ok(answer) = answer else {
            return Err(refused(format!(
                "the question about {label} was withdrawn: the person dismissed it"
            )));
        };
        if !answer.allowed() {
            return Err(refused(format!(
                "{}: the person declined {label}",
                tilo_actions::draft::DECLINED
            )));
        }
        Ok(answer)
    }
}
