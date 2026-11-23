//! What an agent session, or an app, asks its person, on this session's sheet.
//!
//! An agent session (an MCP server connection's, [`crate::agent_sessions`]) pauses before a step
//! its grants ask first about, and the broker puts the question to the person here
//! ([`AppActionConsentPrompter`]): the same sheet family that admitted the agent, naming it as the
//! shell names it ("Claude Code"), the step and what it would do. The person allows it
//! once, allows the agent every such step for as long as the session lasts, or denies it, with
//! Deny or Escape. An agent from another of the person's devices is asked about on the device it
//! drives, where its session is — unless the person let such agents act without asking on this
//! device (Settings > Devices & Sync, beside letting them in without asking): then the setting
//! allows the step once in the person's place, and no sheet goes up.
//!
//! An app that takes another app's action is asked about on the same sheet: Today's "Morning
//! cleanup" deleting the finished tasks in Tasks puts "Today wants to change Tasks" to the person,
//! who allows it once, always (a grant in the workspace's governance), or denies it. A step the
//! action still asks to confirm every time, the person confirms there too.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_action_policy::{ActionKind, Lifetime};
use tilo_actions::remote::PersonAsk;
use tilo_service::{Question, Questions};
use tilo_sheets::portal::{AgentAsk, AgentQuestion};
use tilo_workspace_service::apps::app_actions::{
    AGENT_SESSION_ASK, APP_ACTION_ASK, AppActionAnswer, AppActionConsentPrompter,
    AppActionConsentRequest,
};

use crate::agent_sessions::AgentSessions;

/// The kind the session's sheet reads an agent session's or an app's question as.
pub(crate) const KIND: &str = "session.step-question";

/// An agent session's or an app's question, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StepQuestion {
    /// The agent, as the shell names it; the asking app's id when an app asks.
    pub agent: String,
    /// The app whose action an app asks to take, by its id; `None` for an agent session.
    #[serde(default)]
    pub target: Option<String>,
    /// The step it asks to take.
    pub action: String,
    /// What it takes it on.
    pub resource: String,
    /// What the action does, as the catalogue says: reading or changing.
    pub kind: ActionKind,
    /// What the step would do, field by field: each label and its value.
    pub fields: Vec<(String, String)>,
    /// What the host says the step risks.
    pub risk: String,
    /// Whether the person may allow the agent, or the app, every such step from now on.
    pub may_allow_agent: bool,
}

/// The shell's sheet, as the broker asks it.
pub(crate) struct Sheet {
    questions: Questions,
    /// The MCP server's agent sessions: which of them came from the person's other devices.
    sessions: Arc<AgentSessions>,
    /// Whether the person lets such agents act without asking here, as this device's settings
    /// say when a step asks.
    unasked: fn() -> bool,
    /// The shell's workspaces, which name the workspace an agent's question is asked in.
    workspaces: Option<Arc<dyn tilo_broker_core::services::WorkspaceAuthority>>,
}

impl Sheet {
    /// The sheet over `questions`, knowing the agent sessions `sessions` and the shell's
    /// `workspaces`.
    pub(crate) fn new(
        questions: Questions,
        sessions: Arc<AgentSessions>,
        workspaces: Option<Arc<dyn tilo_broker_core::services::WorkspaceAuthority>>,
    ) -> Self {
        Self {
            questions,
            sessions,
            unasked: crate::device_agents::acts_without_asking,
            workspaces,
        }
    }

    /// The workspace `id` by the name the person gave it, for a sheet to say where it asks;
    /// empty where it is the only workspace on this device, so naming it says nothing, or where
    /// the shell does not know it.
    fn workspace_named(&self, id: &str) -> String {
        let mut named = String::new();
        if let Some(workspaces) = &self.workspaces {
            let _ = workspaces.with_tree(&mut |tree| {
                if tree.workspaces.len() > 1
                    && let Some(workspace) = tilo_broker_core::services::workspace_in(tree, id)
                {
                    named.clone_from(&workspace.name);
                }
            });
        }
        named
    }

    /// The setting's answer to `request` in the person's place: allowed once, with no sheet, when
    /// an agent session from the person's other devices asks and they let such agents act without
    /// asking here. `None` asks the person.
    fn allowed_unasked(&self, request: &AppActionConsentRequest) -> Option<AppActionAnswer> {
        if !asked_by_agent(request) {
            return None;
        }
        let agent = self
            .sessions
            .own_device_session(&request.workspace, request.agent_session.as_deref()?)?;
        if !(self.unasked)() {
            return None;
        }
        tracing::info!(
            %agent,
            action = %request.action_label,
            app = %request.target_app,
            "MCP server: the person's setting lets agents from their other devices act without \
             asking here, so it allowed this step"
        );
        Some(AppActionAnswer::unedited(Some(Lifetime::Once)))
    }

    /// Ask the person `ask` in the agent's own words, as `request` names the
    /// agent and its workspace: the sheet a workspace host asks an agent's
    /// question on, with its choices.
    /// `None` when nobody here is asked questions.
    fn ask_in_words(
        &self,
        request: &AppActionConsentRequest,
        ask: PersonAsk,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionAnswer>> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let agent = request
            .scope
            .clone()
            .unwrap_or_else(|| request.app_id.clone());
        let asked = self.questions.ask(Question {
            handle: request.handle.clone(),
            kind: crate::network::AGENT_QUESTION.to_owned(),
            app_id: None,
            label: agent.clone(),
            details: serde_json::to_value(AgentQuestion {
                agent,
                workspace: self.workspace_named(&request.workspace),
                ask: Some(AgentAsk {
                    question: ask.question.clone(),
                    choices: ask.choices.clone(),
                    secret: false,
                }),
                ..AgentQuestion::default()
            })
            .unwrap_or_default(),
        });
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
                    // The asker stopped waiting: dropping the question takes the sheet down.
                    () = waiting.closed() => return,
                }
            };
            // The choice they picked; Declining, Escape, or a sheet closed
            // without an answer is no answer at all, and nothing typed is one.
            let reply = chosen.and_then(|given| {
                given
                    .choice
                    .and_then(|index| ask.choices.get(index).cloned())
            });
            if let Some(answer) = answer.take() {
                let _ = answer.send(match reply {
                    Some(reply) => AppActionAnswer::answered(reply),
                    None => AppActionAnswer::DENY,
                });
            }
        });
        Some(answered)
    }
}

impl AppActionConsentPrompter for Sheet {
    fn ask(
        &self,
        request: AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionAnswer>> {
        // An agent's question in its own words: the same sheet a workspace host
        // asks it on ([`crate::network::AGENT_QUESTION`]), with the answers it offers.
        if let Some(ask) = request.ask.clone() {
            return self.ask_in_words(&request, ask);
        }
        // A Store disclosure is shown on its own sheet, which this one is not.
        if request.moderation.is_some() {
            return None;
        }
        if let Some(allowed) = self.allowed_unasked(&request) {
            let (answer, answered) = tokio::sync::oneshot::channel();
            let _ = answer.send(allowed);
            return Some(answered);
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
                    // The asker stopped waiting: dropping the question takes the sheet down.
                    () = waiting.closed() => return,
                }
            };
            // Deny, Escape, or a sheet closed without an answer: nothing is allowed.
            let allowed = chosen
                .and_then(|chosen| chosen.choice)
                .and_then(|choice| Lifetime::chosen(choice, tilo_clock::unix_now_ms()));
            if let Some(answer) = answer.take() {
                let _ = answer.send(AppActionAnswer::unedited(allowed));
            }
        });
        Some(answered)
    }
}

/// Whether an agent session asks `request`.
fn asked_by_agent(request: &AppActionConsentRequest) -> bool {
    request.handle.starts_with(AGENT_SESSION_ASK)
}

/// The sheet's question for `request`.
fn question(request: &AppActionConsentRequest) -> Question {
    let agent_asks = asked_by_agent(request);
    let agent = request
        .scope
        .clone()
        .filter(|_| agent_asks)
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
    let details = StepQuestion {
        agent: agent.clone(),
        target: (!agent_asks).then(|| request.target_app.clone()),
        action: request.action_label.clone(),
        resource: request.resource_label.clone(),
        kind: request.kind,
        fields,
        risk: serde_json::to_value(request.risk)
            .ok()
            .and_then(|risk| risk.as_str().map(str::to_owned))
            .unwrap_or_default(),
        // An agent session may be allowed for as long as it lasts; an app, in the workspace, where
        // it asks for a grant rather than to confirm one call.
        may_allow_agent: if agent_asks {
            request.scope.is_some()
        } else {
            request.handle.starts_with(APP_ACTION_ASK)
        },
    };
    Question {
        handle: request.handle.clone(),
        kind: KIND.to_owned(),
        app_id: (!agent_asks).then(|| request.app_id.clone()),
        label: agent,
        details: serde_json::to_value(details).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(scope: Option<&str>) -> AppActionConsentRequest {
        AppActionConsentRequest {
            handle: format!("{AGENT_SESSION_ASK}i1"),
            workspace: "w1".into(),
            person: "p".into(),
            app_id: "claude-code".into(),
            agent_session: None,
            target_app: "so.tilo.terminal".into(),
            resource_label: "Terminal".into(),
            action_label: "Run a command line in the session".into(),
            kind: ActionKind::Outside,
            risk: tilo_actions::RiskLevel::Destructive,
            confirming: true,
            moderation: None,
            scope: scope.map(str::to_owned),
            draft: None,
            ask: None,
        }
    }

    async fn answered(choice: Option<usize>, scope: Option<&str>) -> Option<AppActionAnswer> {
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone(), AgentSessions::new(), None);
        let answer = sheet.ask(request(scope)).expect("asked");
        let pending = questions.pending();
        assert_eq!(pending.len(), 1, "the sheet is up");
        assert_eq!(pending[0].kind, KIND);
        let shown: StepQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
        assert_eq!(shown.agent, scope.unwrap_or("claude-code"));
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

    /// The shell's workspaces, as many as `names` gives, the first of them the one asked in.
    struct Shell(tilo_desktop_model::Layout);

    impl tilo_broker_core::services::WorkspaceAuthority for Shell {
        fn with_tree(
            &self,
            read: &mut dyn FnMut(&tilo_desktop_model::Layout),
        ) -> Result<(), String> {
            read(&self.0);
            Ok(())
        }
    }

    fn shell(names: &[&str]) -> Arc<dyn tilo_broker_core::services::WorkspaceAuthority> {
        let workspaces: Vec<_> = (1_u128..)
            .zip(names)
            .map(|(id, name)| {
                tilo_desktop_model::Workspace::new(
                    tilo_desktop_model::WorkspaceId::from_u128(id),
                    *name,
                )
            })
            .collect();
        Arc::new(Shell(tilo_desktop_model::Layout {
            active: workspaces[0].id(),
            workspaces,
            ..tilo_desktop_model::Layout::default()
        }))
    }

    /// The sheet names the workspace an agent asks in by the name the person gave it, never by
    /// its id, and says nothing of it where it is the only workspace on the device.
    #[tokio::test]
    async fn an_agents_question_names_its_workspace_only_among_several() {
        for (names, said) in [
            (&["Applicant: Jane Doe", "Home"][..], "Applicant: Jane Doe"),
            (&["Applicant: Jane Doe"][..], ""),
        ] {
            let questions = Questions::new();
            let sheet = Sheet::new(questions.clone(), AgentSessions::new(), Some(shell(names)));
            let mut question = request(Some("Claude Code"));
            question.handle = "agent-ask:i1".into();
            question.workspace = tilo_desktop_model::WorkspaceId::from_u128(1).to_string();
            question.ask = Some(tilo_actions::remote::PersonAsk {
                question: "Shall I file the budget under Spring?".into(),
                choices: vec!["Yes, file it".into()],
            });
            let _answered = sheet.ask(question).expect("asked");
            let pending = questions.pending();
            let shown: AgentQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
            assert_eq!(shown.workspace, said, "{names:?}");
        }
    }

    /// An agent's question in its own words goes on the sheet a workspace host
    /// asks it on: the person picks a choice, words typed are no answer, and
    /// decline answers nothing.
    #[tokio::test]
    async fn an_agents_question_is_asked_with_its_choices_and_answered_with_what_they_pick() {
        async fn asked(answer: tilo_service::Answer) -> Option<AppActionAnswer> {
            let questions = Questions::new();
            let sheet = Sheet::new(questions.clone(), AgentSessions::new(), None);
            let mut question = request(Some("Claude Code"));
            question.handle = "agent-ask:i1".into();
            question.ask = Some(tilo_actions::remote::PersonAsk {
                question: "Shall I file the budget under Spring?".into(),
                choices: vec!["Yes, file it".into(), "Leave it for now".into()],
            });
            let answered = sheet.ask(question).expect("asked");
            let pending = questions.pending();
            assert_eq!(pending.len(), 1, "the sheet is up");
            assert_eq!(pending[0].kind, crate::network::AGENT_QUESTION);
            let shown: AgentQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
            assert_eq!(shown.agent, "Claude Code");
            assert_eq!(
                shown.workspace, "",
                "a shell that names no workspace says none"
            );
            let ask = shown.ask.expect("the question in the agent's words");
            assert_eq!(ask.question, "Shall I file the budget under Spring?");
            assert_eq!(ask.choices, ["Yes, file it", "Leave it for now"]);
            assert!(!ask.secret, "an agent's own question asks for no secret");
            if questions.answer(&pending[0].handle, answer) {
                answered.await.ok()
            } else {
                assert!(questions.decline(&pending[0].handle), "the question was up");
                answered.await.ok()
            }
        }

        let picked = asked(tilo_service::Answer {
            choice: Some(1),
            ..tilo_service::Answer::default()
        })
        .await
        .expect("answered");
        assert_eq!(picked.reply.as_deref(), Some("Leave it for now"));
        assert!(picked.allowed());

        let typed = asked(tilo_service::Answer {
            values: vec![zeroize::Zeroizing::new("File it next week".to_owned())],
            ..tilo_service::Answer::default()
        })
        .await
        .expect("answered");
        assert_eq!(typed, AppActionAnswer::DENY, "words typed answer nothing");

        // Declining (or Escape) is no answer at all.
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone(), AgentSessions::new(), None);
        let mut question = request(Some("Claude Code"));
        question.handle = "agent-ask:i2".into();
        question.ask = Some(tilo_actions::remote::PersonAsk {
            question: "Shall I file the budget under Spring?".into(),
            choices: vec!["Yes, file it".into()],
        });
        let answered = sheet.ask(question).expect("asked");
        assert!(questions.decline(&questions.pending()[0].handle));
        let declined = answered.await.expect("answered");
        assert_eq!(declined, AppActionAnswer::DENY);
    }

    /// The person's choice on the sheet is the answer; Deny (the sheet declined) refuses it.
    #[tokio::test]
    async fn the_sheet_answers_as_the_person_chose() {
        let once = answered(Some(Lifetime::Once.choice()), Some("Claude Code #1")).await;
        assert_eq!(
            once.map(|answer| answer.lifetime),
            Some(Some(Lifetime::Once))
        );
        let always = answered(
            Some(Lifetime::UntilRevoked.choice()),
            Some("Claude Code #1"),
        )
        .await;
        assert_eq!(
            always.map(|answer| answer.lifetime),
            Some(Some(Lifetime::UntilRevoked))
        );
        let denied = answered(None, None).await;
        assert_eq!(denied.map(|answer| answer.lifetime), Some(None));
    }

    /// Two sessions of `agent`: one from another of the person's devices, one of a program of
    /// this login, in that order.
    fn sessions_of(agent: &str) -> (Arc<AgentSessions>, [String; 2]) {
        let sessions = AgentSessions::new();
        let consent = tilo_mcp::session_api::Consent::Admit {
            workspace: "w1".to_owned(),
            agent: agent.to_owned(),
            person: "p".to_owned(),
            name: "Claude Code #1".to_owned(),
        };
        let laptop = tilo_actions::InvokerDevice::certified([0x4c; 32], "Laptop");
        let remote = sessions.joining(None, Some(laptop));
        sessions.admit(&remote, &consent, "claude-code", "Studio");
        let local = sessions.joining(None, None);
        sessions.admit(&local, &consent, "claude-code", "Studio");
        (sessions, [remote.0, local.0])
    }

    /// With the person's setting on, the step an agent session from another of their devices
    /// asks about is allowed once with no sheet; with it off, or for a session of a program of
    /// this login (even of the same agent) or an app, they are asked.
    #[tokio::test]
    async fn the_setting_allows_an_agent_from_another_device_its_step_unasked() {
        let (sessions, [remote, local]) = sessions_of("claude-code");
        let sheet = |unasked: fn() -> bool| {
            let questions = Questions::new();
            let sheet = Sheet {
                questions: questions.clone(),
                sessions: Arc::clone(&sessions),
                unasked,
                workspaces: None,
            };
            (sheet, questions)
        };
        let asked_by = |session: &str| AppActionConsentRequest {
            agent_session: Some(session.to_owned()),
            ..request(Some("Claude Code #1"))
        };

        let (on, questions) = sheet(|| true);
        let answer = on.ask(asked_by(&remote)).expect("answered");
        assert!(questions.pending().is_empty(), "no sheet goes up");
        assert_eq!(
            answer.await.ok().map(|answer| answer.lifetime),
            Some(Some(Lifetime::Once)),
            "allowed once, in the person's place"
        );

        let (off, questions) = sheet(|| false);
        let _waiting = off.ask(asked_by(&remote)).expect("asked");
        assert_eq!(questions.pending().len(), 1, "the person is asked");

        let (on, questions) = sheet(|| true);
        let _waiting = on.ask(asked_by(&local)).expect("asked");
        assert_eq!(
            questions.pending().len(),
            1,
            "a program of this login is asked about"
        );

        let (on, questions) = sheet(|| true);
        let mut app = request(None);
        app.handle = "app-action:i1".into();
        let _waiting = on.ask(app).expect("asked");
        assert_eq!(questions.pending().len(), 1, "an app is asked about");
    }

    /// A question nobody waits for any more is taken down; anything but a step's question is not
    /// asked here at all.
    #[tokio::test]
    async fn a_question_nobody_waits_for_leaves_the_sheet() {
        let questions = Questions::new();
        let sheet = Sheet::new(questions.clone(), AgentSessions::new(), None);
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
        disclosure.moderation = Some(
            tilo_workspace_service::apps::app_actions::ModerationConsentPreview {
                purpose:
                    tilo_workspace_service::apps::app_actions::ModerationConsentPurpose::Report,
                destination: "https://registry.example".into(),
                owner_root: String::new(),
                signing_root: String::new(),
                target: "org.example.app".into(),
                category: None,
                content: "misleading".into(),
                evidence_digest: None,
            },
        );
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
        let sheet = Sheet::new(questions.clone(), AgentSessions::new(), None);
        for (handle, always) in [("app-action:i1", true), ("confirm:i2", false)] {
            let mut asked = request(None);
            asked.handle = handle.into();
            asked.app_id = "so.tilo.lang.today".into();
            asked.target_app = "so.tilo.lang.tasks".into();
            asked.action_label = "Delete finished tasks".into();
            asked.kind = ActionKind::Undoable;
            asked.scope = Some("ignored for an app".into());
            let answer = sheet.ask(asked).expect("asked");
            let pending = questions.pending();
            assert_eq!(pending.len(), 1, "the sheet is up");
            assert_eq!(pending[0].app_id.as_deref(), Some("so.tilo.lang.today"));
            let shown: StepQuestion = serde_json::from_value(pending[0].details.clone()).unwrap();
            assert_eq!(shown.agent, "so.tilo.lang.today");
            assert_eq!(shown.target.as_deref(), Some("so.tilo.lang.tasks"));
            assert_eq!(
                shown.kind,
                ActionKind::Undoable,
                "the sheet knows what it does"
            );
            assert_eq!(shown.may_allow_agent, always, "{handle}");
            let expected = if always {
                Lifetime::UntilRevoked
            } else {
                Lifetime::Once
            };
            assert!(questions.answer(
                &pending[0].handle,
                tilo_service::Answer {
                    choice: Some(expected.choice()),
                    ..tilo_service::Answer::default()
                },
            ));
            let allowed = answer.await.expect("answered").lifetime;
            assert_eq!(allowed, Some(expected), "{handle}");
        }
    }
}
