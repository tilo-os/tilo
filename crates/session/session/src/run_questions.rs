//! What an agent's run, or an app, asks its person, on this desk's sheet.
//!
//! A run with no issue to ask on (an agent door connection's, [`crate::door_runs`]) pauses before
//! a step its grants ask first about, and the broker puts the question to the person here
//! ([`AppActionConsentPrompter`]): the same sheet family that admitted the agent, naming it as the
//! desk seats it ("Claude Code in dev #1"), the step and what it would do. The person allows it
//! once, allows the agent every such step for as long as it works here, or denies it, with Deny or
//! Escape. An agent from another of the
//! person's devices is asked about on the device it drives, where its run is.
//!
//! An app that takes another app's action is asked about on the same sheet: Today's "Morning
//! cleanup" deleting the finished tasks in Tasks puts "Today wants to change Tasks" to the person,
//! who allows it once, always (a grant in the workspace's governance), or denies it. A step the
//! action still asks to confirm every time, the person confirms there too.

use serde::{Deserialize, Serialize};
use tilo_action_broker::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use tilo_service::{Question, Questions};

/// The kind the session's sheet reads a run's question as.
pub(crate) const KIND: &str = "session.run-question";

/// What a run's question is named by, as the broker hands it over.
const RUN_QUESTION: &str = "run-question:";

/// What a question whether an app may take another app's action is named by.
const APP_ACTION: &str = "app-action:";

/// The sheet's choices, as the answer names them.
pub(crate) const ALLOW_ONCE: usize = 0;
pub(crate) const ALLOW_AGENT: usize = 1;

/// A run's or an app's question, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RunQuestion {
    /// The agent, as the desk seats it; the asking app's id when an app asks.
    pub agent: String,
    /// The app whose action an app asks to take, by its id; `None` for an agent's run.
    #[serde(default)]
    pub target: Option<String>,
    /// The step it asks to take.
    pub action: String,
    /// What it takes it on.
    pub resource: String,
    /// What the step would do, field by field: each label and its value.
    pub fields: Vec<(String, String)>,
    /// What the host says the step risks.
    pub risk: String,
    /// Whether the person may allow the agent, or the app, every such step from now on.
    pub may_allow_agent: bool,
}

/// The desk's sheet, as the broker asks it.
pub(crate) struct Sheet {
    questions: Questions,
}

impl Sheet {
    pub(crate) const fn new(questions: Questions) -> Self {
        Self { questions }
    }
}

impl AppActionConsentPrompter for Sheet {
    fn ask(
        &self,
        request: AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionAnswer>> {
        // A Store disclosure is shown on its own sheet, which this one is not.
        if request.moderation.is_some() {
            return None;
        }
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let asked = self.questions.ask(question(&request));
        let (answer, answered) = tokio::sync::oneshot::channel();
        let mut answer = Some(answer);
        runtime.spawn(async move {
            let mut asked = asked;
            let chosen = {
                let Some(waiting) = answer.as_mut() else {
                    return;
                };
                tokio::select! {
                    chosen = &mut asked => chosen,
                    // The run stopped waiting: dropping the question takes the sheet down.
                    () = waiting.closed() => return,
                }
            };
            // Deny, Escape, or a sheet closed without an answer: nothing is allowed.
            let consent = match chosen.and_then(|chosen| chosen.choice) {
                Some(ALLOW_ONCE) => AppActionConsent::Once,
                Some(ALLOW_AGENT) => AppActionConsent::Always,
                _ => AppActionConsent::Deny,
            };
            if let Some(answer) = answer.take() {
                let _ = answer.send(AppActionAnswer::unedited(consent));
            }
        });
        Some(answered)
    }
}

/// The sheet's question for `request`.
fn question(request: &AppActionConsentRequest) -> Question {
    let run = request.handle.starts_with(RUN_QUESTION);
    let agent = request
        .scope
        .clone()
        .filter(|_| run)
        .unwrap_or_else(|| request.app_id.clone());
    let fields = request
        .draft
        .as_ref()
        .map(|draft| {
            draft
                .fields
                .iter()
                .map(|field| (field.label.clone(), field.value.line()))
                .collect()
        })
        .unwrap_or_default();
    let details = RunQuestion {
        agent: agent.clone(),
        target: (!run).then(|| request.target_app.clone()),
        action: request.action_label.clone(),
        resource: request.resource_label.clone(),
        fields,
        risk: serde_json::to_value(request.risk)
            .ok()
            .and_then(|risk| risk.as_str().map(str::to_owned))
            .unwrap_or_default(),
        // A run may be allowed for its issue; an app, in the workspace, where it asks for a
        // grant rather than to confirm one call.
        may_allow_agent: if run {
            request.scope.is_some()
        } else {
            request.handle.starts_with(APP_ACTION)
        },
    };
    Question {
        handle: request.handle.clone(),
        kind: KIND.to_owned(),
        app_id: (!run).then(|| request.app_id.clone()),
        label: agent,
        details: serde_json::to_value(details).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(scope: Option<&str>) -> AppActionConsentRequest {
        AppActionConsentRequest {
            handle: "run-question:q1".into(),
            workspace: "w1".into(),
            person: "p".into(),
            app_id: "task-0123456789ab".into(),
            target_app: "so.tilo.terminal".into(),
            resource: "terminal".into(),
            action: "run".into(),
            resource_label: "Terminal".into(),
            action_label: "Run a command line in the session".into(),
            risk: tilo_actions::RiskLevel::Destructive,
            needs_confirmation: true,
            confirming: true,
            moderation: None,
            preview: None,
            scope: scope.map(str::to_owned),
            draft: None,
        }
    }

    async fn answered(choice: Option<usize>, scope: Option<&str>) -> Option<AppActionAnswer> {
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone());
        let answer = sheet.ask(request(scope)).expect("asked");
        let pending = questions.pending();
        assert_eq!(pending.len(), 1, "the sheet is up");
        assert_eq!(pending[0].kind, KIND);
        let shown: RunQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
        assert_eq!(shown.agent, scope.unwrap_or("task-0123456789ab"));
        assert_eq!(shown.may_allow_agent, scope.is_some());
        let given = match choice {
            Some(choice) => questions.answer(
                &pending[0].handle,
                tilo_service::Answer {
                    choice: Some(choice),
                    ..tilo_service::Answer::default()
                },
            ),
            None => questions.decline(&pending[0].handle),
        };
        assert!(given, "the question was up to answer");
        answer.await.ok()
    }

    /// The person's choice on the sheet is the run's answer; Deny (the sheet declined) refuses it.
    #[tokio::test]
    async fn the_sheet_answers_the_runs_question_as_the_person_chose() {
        let once = answered(Some(ALLOW_ONCE), Some("Claude Code #1")).await;
        assert_eq!(
            once.map(|answer| answer.consent),
            Some(AppActionConsent::Once)
        );
        let always = answered(Some(ALLOW_AGENT), Some("Claude Code #1")).await;
        assert_eq!(
            always.map(|answer| answer.consent),
            Some(AppActionConsent::Always)
        );
        let denied = answered(None, None).await;
        assert_eq!(
            denied.map(|answer| answer.consent),
            Some(AppActionConsent::Deny)
        );
    }

    /// A run that stops waiting takes its question down; anything but a run's question is not
    /// asked here at all.
    #[tokio::test]
    async fn a_question_nobody_waits_for_leaves_the_sheet() {
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone());
        let answer = sheet.ask(request(None)).expect("asked");
        assert_eq!(questions.pending().len(), 1);
        drop(answer);
        for _ in 0..100 {
            if questions.pending().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(questions.pending().is_empty(), "withdrawn");
        let mut disclosure = request(None);
        disclosure.handle = "confirm:x".into();
        disclosure.moderation = Some(tilo_action_broker::app_actions::ModerationConsentPreview {
            purpose: tilo_action_broker::app_actions::ModerationConsentPurpose::Report,
            destination: "https://registry.example".into(),
            owner_root: String::new(),
            signing_root: String::new(),
            target: "org.example.app".into(),
            category: None,
            content: "misleading".into(),
            evidence_digest: None,
        });
        assert!(
            sheet.ask(disclosure).is_none(),
            "a Store disclosure is not shown on this sheet"
        );
    }

    /// An app taking another app's action is asked about on the same sheet: as the app it is,
    /// changing the app whose action it is, with "always" for a grant and only once for a step
    /// confirmed each time.
    #[tokio::test]
    async fn an_apps_change_of_another_app_is_asked_on_the_sheet() {
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone());
        for (handle, always) in [("app-action:i1", true), ("confirm:i2", false)] {
            let mut asked = request(None);
            asked.handle = handle.into();
            asked.app_id = "so.tilo.lang.today".into();
            asked.target_app = "so.tilo.lang.tasks".into();
            asked.action_label = "Delete finished tasks".into();
            asked.scope = Some("ignored for an app".into());
            let answer = sheet.ask(asked).expect("asked");
            let pending = questions.pending();
            assert_eq!(pending.len(), 1, "the sheet is up");
            assert_eq!(pending[0].app_id.as_deref(), Some("so.tilo.lang.today"));
            let shown: RunQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
            assert_eq!(shown.agent, "so.tilo.lang.today");
            assert_eq!(shown.target.as_deref(), Some("so.tilo.lang.tasks"));
            assert_eq!(shown.may_allow_agent, always, "{handle}");
            let choice = if always { ALLOW_AGENT } else { ALLOW_ONCE };
            assert!(questions.answer(
                &pending[0].handle,
                tilo_service::Answer {
                    choice: Some(choice),
                    ..tilo_service::Answer::default()
                },
            ));
            let consent = answer.await.expect("answered").consent;
            let expected = if always {
                AppActionConsent::Always
            } else {
                AppActionConsent::Once
            };
            assert_eq!(consent, expected, "{handle}");
        }
    }
}
