//! What a run asks its person, as the host's own sheet shows it.
//!
//! A step an agent's grants ask first about waits for the person's answer
//! kept on the run ([`super::task_run_host::asking`]). The run's tracker
//! keeps the question and this puts it on the person's sheet: the action as
//! its window publishes it, the draft the arguments render, and the grant
//! "Allow for the issue" records where the person may give it
//! ([`tilo_action_policy::agent_answers::answer_grant`]), so the answer shows
//! in the workspace's history and settings and is revoked like any grant.

use std::sync::Arc;

use tilo_action_policy::{Answer, Grant, Grantee};

use super::AppBrokerState;
use crate::app_actions::{AppActionConsentPrompter, AppActionConsentRequest};
use crate::services::{TrackerAnswer, issue_answer_id, issue_answer_scope};

impl AppBrokerState {
    pub(super) fn consent_prompter(&self) -> Option<Arc<dyn AppActionConsentPrompter>> {
        self.app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())
    }

    /// What `issue`'s going run, started by `person` in `workspace`, asks them,
    /// as the sheet puts it, and the grant "Allow for the issue" records
    /// where they may give it.
    pub(super) fn run_question_for(
        &self,
        workspace: &str,
        person: &str,
        issue: &str,
    ) -> Option<(AppActionConsentRequest, Option<Grant>)> {
        let run = self
            .tracker_runs(workspace)?
            .runs(workspace)
            .ok()?
            .into_iter()
            .find(|run| run.issue == issue && run.active && run.starter == person)?;
        // A run that keeps its own answers (an agent door connection's) takes its person's
        // "allow always" itself, for as long as it goes; any other is granted in the workspace's
        // record, where its person may grant.
        let keeps_answers = self
            .tracker_runs(workspace)
            .is_some_and(|tracker| tracker.asks_on_host(workspace, &run));
        let question = run
            .question
            .filter(|question| question.answer == TrackerAnswer::Waiting)?;
        let (draft, preview) = super::super::services::confirmation_preview(&question);
        let draft = (!draft.fields.is_empty()).then_some(draft);
        let asked = self.published_surfaces().into_iter().find_map(|surface| {
            (surface.app_id == question.app && surface.route.cluster_id == workspace)
                .then(|| {
                    surface.actions.into_iter().find(|action| {
                        action.action == question.action
                            && tilo_actions::selection::logical_resource_id(&action.resource)
                                == question.resource
                    })
                })
                .flatten()
        });
        let may_grant = keeps_answers
            || self
                .services
                .workspace_grants
                .as_ref()
                .is_some_and(|grants| grants.may_record(workspace, person));
        let grant = asked
            .as_ref()
            .filter(|action| may_grant && !action.contract.need().privileged)
            .map(|action| {
                let (scope, kind) = (
                    issue_answer_scope(
                        &question.app,
                        (&action.resource_kind, &action.action),
                        action.contract.asks_for_argument(None),
                    ),
                    action.contract.need().kind,
                );
                Grant::new(
                    issue_answer_id(issue, (&scope, kind)),
                    Grantee::agent(run.agent.clone()),
                    scope.node(workspace),
                    tilo_action_policy::KindSet::of(kind),
                    Answer::Allow,
                    Grantee::person(person),
                )
            });
        let labels = |of: fn(&tilo_actions::Draft) -> &String, published: Option<&String>| {
            draft
                .as_ref()
                .map(of)
                .or(published)
                .cloned()
                .unwrap_or_else(|| question.action.clone())
        };
        let request = AppActionConsentRequest {
            handle: run_question_handle(&question.id),
            workspace: workspace.to_owned(),
            person: person.to_owned(),
            app_id: run.agent,
            run: Some(run.issue.clone()),
            target_app: question.app.clone(),
            resource: question.resource.clone(),
            action: question.action.clone(),
            resource_label: labels(
                |draft| &draft.resource_label,
                asked.as_ref().map(|action| &action.resource_label),
            ),
            action_label: labels(
                |draft| &draft.action_label,
                asked.as_ref().map(|action| &action.action_label),
            ),
            // An action no longer published is read as the gravest it may be.
            risk: asked
                .as_ref()
                .map_or(tilo_actions::RiskLevel::Destructive, |action| {
                    action.contract.risk
                }),
            needs_confirmation: true,
            confirming: true,
            moderation: None,
            preview,
            scope: grant.is_some().then_some(run.title),
            draft,
        };
        Some((request, grant))
    }
}

/// The sheet's and the notice's handle of the run's question `id`.
fn run_question_handle(id: &str) -> String {
    format!("run-question:{id}")
}
