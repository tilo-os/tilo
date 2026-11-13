//! What one agent may do in the workspace, app by app: Allow, Ask first or Deny for reading,
//! changes and outside effects.
//! Each answer is an ordinary workspace grant ([`agent_answers`]); without one the agent's
//! admission decides, and the view says so. Actions whose contract asks for confirmation (sending,
//! publishing, deleting for good) still ask first under an app-wide Allow; only "Always allow this"
//! on the action itself lifts that, and those exceptions are listed here to take back.

use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, Grantee, decide};
use tilo_desktop_model::{UserId, Workspace};
use tilo_sort::CompactSort as _;

use super::{AppDirectory, PolicyEdit, app_display_name, grant_subjects};
use crate::i18n::{t, t_args};

/// The three kinds of action, in column order.
pub const KINDS: [ActionKind; 3] = [ActionKind::Read, ActionKind::Undoable, ActionKind::Outside];

/// The three answers, in segment order.
pub const ANSWERS: [Answer; 3] = [Answer::Allow, Answer::Ask, Answer::Deny];

/// The key a kind of action is asked for by at the door.
#[must_use]
pub const fn kind_key(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Read => "read",
        ActionKind::Undoable => "change",
        ActionKind::Outside => "outside",
    }
}

/// The kind of action a [`kind_key`] names, if it names one.
#[must_use]
pub fn kind_from_key(key: &str) -> Option<ActionKind> {
    KINDS.into_iter().find(|kind| kind_key(*kind) == key)
}

/// The kind's name, as the access view heads its line.
#[must_use]
pub fn kind_label(kind: ActionKind) -> String {
    t(match kind {
        ActionKind::Read => "workspace-settings-agent-access-read",
        ActionKind::Undoable => "workspace-settings-agent-access-change",
        ActionKind::Outside => "workspace-settings-agent-access-outside",
    })
}

/// One answer the agent has for one kind of action at one scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnswerCell {
    pub kind: ActionKind,
    pub answer: Answer,
    /// The person set it here; otherwise it follows the agent's admission or a broader answer.
    pub explicit: bool,
}

/// One application's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAccessRow {
    /// The app whose answers these are, for its mark.
    pub app: String,
    pub scope: AnswerScope,
    pub label: String,
    pub cells: [AnswerCell; 3],
}

impl AgentAccessRow {
    /// Whether any of its answers was set here.
    #[must_use]
    pub fn customised(&self) -> bool {
        self.cells.iter().any(|cell| cell.explicit)
    }
}

/// An action the person always allows the agent, without asking first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentException {
    pub scope: AnswerScope,
    pub kind: ActionKind,
    pub label: String,
}

/// The answer `agent` holds for `kind` at `scope` : the turn's own grants (`admission`,
/// [`tilo_action_policy::roles::agent_grants`]) and the workspace's grants naming it alone, so a
/// narrower answer the person set here wins over what the admission says.
#[must_use]
pub fn answer_at(
    ws: &Workspace,
    agent: &str,
    admission: &[tilo_action_policy::Grant],
    scope: &AnswerScope,
    kind: ActionKind,
) -> AnswerCell {
    let subject = [Grantee::agent(agent)];
    let cx = tilo_desktop_model::context_at(tilo_clock::unix_now());
    let decision = decide(
        admission.iter().chain(ws.policy.grants.iter()),
        &subject,
        &scope.node(&ws.policy.workspace),
        kind,
        &cx,
    );
    let id = agent_answers::grant_id(agent, scope, kind);
    AnswerCell {
        kind,
        answer: decision.answer,
        explicit: decision.grant().is_some_and(|grant| grant.id == id),
    }
}

/// The apps an agent's answers are offered for: every app the workspace shows, holds capabilities
/// for, or that the agent already has answers for, by display name.
#[must_use]
pub fn agent_access_rows(
    ws: &Workspace,
    agent: &str,
    admission: &[tilo_action_policy::Grant],
    apps: &dyn AppDirectory,
) -> Vec<AgentAccessRow> {
    let subject = Grantee::agent(agent);
    let mut ids: Vec<String> = grant_subjects(ws)
        .into_iter()
        .filter(|candidate| candidate.kind == super::SubjectKind::App)
        .map(|candidate| candidate.id)
        .collect();
    ids.extend(
        ws.policy
            .grants
            .iter()
            .filter(|grant| grant.grantee == subject && agent_answers::is_answer(grant))
            .filter_map(|grant| match AnswerScope::of_node(&grant.node)? {
                AnswerScope::App { app }
                | AnswerScope::Kind { app, .. }
                | AnswerScope::Action { app, .. } => Some(app),
            }),
    );
    ids.compact_sort();
    ids.dedup();
    let mut rows: Vec<AgentAccessRow> = ids
        .into_iter()
        .map(|app| {
            let scope = AnswerScope::app(&app);
            AgentAccessRow {
                cells: KINDS.map(|kind| answer_at(ws, agent, admission, &scope, kind)),
                label: app_display_name(apps, &app),
                scope,
                app,
            }
        })
        .collect();
    rows.compact_sort_by_key(|row| row.label.to_lowercase());
    rows
}

/// The actions the person always allows `agent` .
#[must_use]
pub fn agent_exceptions(
    ws: &Workspace,
    agent: &str,
    apps: &dyn AppDirectory,
) -> Vec<AgentException> {
    let subject = Grantee::agent(agent);
    let mut out: Vec<AgentException> = ws
        .policy
        .grants
        .iter()
        .filter(|grant| {
            grant.grantee == subject
                && agent_answers::is_answer(grant)
                && grant.answer == Answer::Allow
        })
        .filter_map(|grant| {
            let scope = AnswerScope::of_node(&grant.node)?;
            let AnswerScope::Action { app, action, .. } = &scope else {
                return None;
            };
            let label = t_args(
                "workspace-settings-agent-exception",
                &[
                    ("app", app_display_name(apps, app).into()),
                    ("action", humanize(action).into()),
                ],
            );
            let kind = grant.kinds.iter().next()?;
            Some(AgentException { scope, kind, label })
        })
        .collect();
    out.compact_sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Set `agent` 's answers, each for a kind at a scope (`None` returns it to what the admission and
/// broader answers say), as `actor` , in one governed edit.
pub fn set_agent_answers(
    ws: &mut Workspace,
    actor: &UserId,
    agent: &str,
    answers: &[(AnswerScope, ActionKind, Option<Answer>)],
) -> PolicyEdit {
    let root = ws.policy.workspace.clone();
    let by = Grantee::person(actor.clone());
    let changes: Vec<_> = answers
        .iter()
        .flat_map(|(scope, kind, answer)| {
            agent_answers::set(
                &root,
                agent,
                (scope, *kind),
                *answer,
                (by.clone(), &ws.policy.grants),
            )
        })
        .collect();
    if changes.is_empty() {
        return PolicyEdit::default();
    }
    ws.govern_all_at(actor, changes, tilo_clock::unix_now())
        .unwrap_or_default()
}

/// `send_message` as `Send message` .
fn humanize(id: &str) -> String {
    let spaced = id.replace(['_', '-', '.'], " ");
    let mut chars = spaced.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A model of the person's providers that an agent may plan with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentModelOption {
    pub profile: String,
    pub model: String,
    /// The model's and its provider's names, as Settings shows them.
    pub label: String,
}

impl AgentModelOption {
    /// What the model is chosen by: its profile and its model's id, which read back to this option.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.profile, self.model)
    }
}

/// The models the person may choose for one agent, from their Settings,
/// and the one it plans with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentModels {
    /// The default's label; `None` while Settings names no default.
    pub default: Option<String>,
    /// The model chosen for this agent; `None` follows the default.
    pub chosen: Option<AgentModelOption>,
    /// Every model of the person's switched-on providers.
    pub offered: Vec<AgentModelOption>,
}

/// The models of `config` the person may choose for `agent` , and the one it plans with; `None`
/// while they have no switched-on model at all.
#[must_use]
pub fn agent_models(
    config: &agent_provider_config::ProviderConfig,
    agent: &str,
) -> Option<AgentModels> {
    let option =
        |profile: &agent_provider_config::ProviderProfile,
         model: &agent_provider_config::ModelDescriptor| AgentModelOption {
            profile: profile.id.clone(),
            model: model.id.clone(),
            label: t_args(
                "workspace-settings-agent-model-option",
                &[
                    ("model", model.display_label().into()),
                    ("provider", profile.display_label().into()),
                ],
            ),
        };
    let offered: Vec<_> = config
        .profiles
        .iter()
        .filter(|profile| profile.enabled)
        .flat_map(|profile| {
            profile
                .models
                .iter()
                .map(move |model| option(profile, model))
        })
        .collect();
    if offered.is_empty() {
        return None;
    }
    let default = config.active_profile().and_then(|profile| {
        let default = profile.default_model.as_ref()?;
        let model = profile.models.iter().find(|model| &model.id == default)?;
        Some(option(profile, model).label)
    });
    let chosen = config.agent_models.get(agent).and_then(|chosen| {
        offered
            .iter()
            .find(|option| option.profile == chosen.profile && option.model == chosen.model)
            .cloned()
    });
    Some(AgentModels {
        default,
        chosen,
        offered,
    })
}

/// Have `agent` plan with `model`, or the default with `None`, in the
/// person's Settings, unless Settings changed meanwhile. Answers the
/// settings as saved.
///
/// # Errors
///
/// The message to show when the settings could not be written, or changed
/// while the choice was being made.
pub fn choose_agent_model(
    agent: &str,
    model: Option<AgentModelOption>,
) -> Result<agent_provider_config::ProviderConfig, String> {
    let read = agent_provider_config::ProviderConfig::load();
    let mut config = read.clone();
    config.set_agent_model(
        agent,
        model.map(|model| agent_provider_config::AgentModel {
            profile: model.profile,
            model: model.model,
        }),
    )?;
    if !config
        .compare_save(&read)
        .map_err(|error| error.to_string())?
    {
        return Err("Settings changed meanwhile; choose again".into());
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::WorkspaceSlot;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe";
    const MAIL: &str = "so.tilo.mail";

    /// A workspace where the agent may change it all and asks before reaching outside.
    fn ws() -> Workspace {
        let mut ws = Workspace::new(WorkspaceSlot::from(31), "access");
        ws.set_owner(OWNER);
        ws
    }

    /// The grants the agent's admission gives it: the editor cap, unbounded by its record.
    fn admission(ws: &Workspace) -> Vec<tilo_action_policy::Grant> {
        tilo_action_policy::roles::agent_grants(&ws.policy.workspace, AGENT, OWNER, &[])
    }

    #[test]
    fn answers_follow_the_admission_until_set_here() {
        let mut ws = ws();
        let mail = AnswerScope::app(MAIL);
        let read = answer_at(&ws, AGENT, &admission(&ws), &mail, ActionKind::Read);
        let outside = answer_at(&ws, AGENT, &admission(&ws), &mail, ActionKind::Outside);
        assert_eq!((read.answer, read.explicit), (Answer::Allow, false));
        assert_eq!((outside.answer, outside.explicit), (Answer::Ask, false));

        let edit = set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(mail.clone(), ActionKind::Outside, Some(Answer::Deny))],
        );
        assert_eq!(edit.granted.len(), 1);
        let outside = answer_at(&ws, AGENT, &admission(&ws), &mail, ActionKind::Outside);
        assert_eq!((outside.answer, outside.explicit), (Answer::Deny, true));
        let notes = answer_at(
            &ws,
            AGENT,
            &admission(&ws),
            &AnswerScope::app("so.tilo.notes"),
            ActionKind::Outside,
        );
        assert_eq!((notes.answer, notes.explicit), (Answer::Ask, false));
    }

    #[test]
    fn setting_again_replaces_and_clearing_returns_to_the_admission() {
        let mut ws = ws();
        let mail = AnswerScope::app(MAIL);
        let set = |ws: &mut Workspace, answer| {
            set_agent_answers(
                ws,
                &OWNER.to_string(),
                AGENT,
                &[(mail.clone(), ActionKind::Undoable, answer)],
            )
        };
        set(&mut ws, Some(Answer::Ask));
        let edit = set(&mut ws, Some(Answer::Deny));
        assert_eq!(edit.granted.len(), 1);
        let held: Vec<_> = ws
            .policy
            .grants
            .iter()
            .filter(|grant| agent_answers::is_answer(grant))
            .collect();
        assert_eq!(held.len(), 1, "one answer per scope and kind");
        assert_eq!(held[0].answer, Answer::Deny);
        let cleared = set(&mut ws, None);
        assert_eq!((cleared.revoked.len(), cleared.granted.len()), (1, 0));
        let cell = answer_at(&ws, AGENT, &admission(&ws), &mail, ActionKind::Undoable);
        assert_eq!((cell.answer, cell.explicit), (Answer::Allow, false));
        // Nothing to clear: no edit.
        assert!(set(&mut ws, None).is_empty());
    }

    #[test]
    fn rows_list_the_apps_with_answers_and_exceptions_only_action_allows() {
        let mut ws = ws();
        let send = AnswerScope::action(MAIL, "mail.mailbox", "send_message");
        let purge = AnswerScope::action(MAIL, "mail.mailbox", "purge");
        set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[
                (send.clone(), ActionKind::Outside, Some(Answer::Allow)),
                (purge, ActionKind::Undoable, Some(Answer::Deny)),
            ],
        );
        let rows = agent_access_rows(&ws, AGENT, &admission(&ws), &super::super::NoAppDirectory);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scope, AnswerScope::app(MAIL));
        assert_eq!(rows[0].app, MAIL);
        assert_eq!(rows[0].label, "Mail", "an app is named, not shown by id");
        assert!(!rows[0].customised(), "an action's answer is not the app's");
        let exceptions = agent_exceptions(&ws, AGENT, &super::super::NoAppDirectory);
        assert_eq!(exceptions.len(), 1, "a Deny is no exception to list");
        assert_eq!(exceptions[0].scope, send);
        assert_eq!(exceptions[0].kind, ActionKind::Outside);
        assert_eq!(exceptions[0].label, "Send message in Mail");
        let edit = set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(send, ActionKind::Outside, None)],
        );
        assert_eq!(edit.revoked.len(), 1);
        assert!(agent_exceptions(&ws, AGENT, &super::super::NoAppDirectory).is_empty());
    }

    #[test]
    fn someone_who_may_not_govern_changes_nothing() {
        let mut ws = ws();
        let edit = set_agent_answers(
            &mut ws,
            &"stranger".to_string(),
            AGENT,
            &[(AnswerScope::app(MAIL), ActionKind::Read, Some(Answer::Deny))],
        );
        assert!(edit.is_empty());
        assert!(!ws.policy.grants.iter().any(agent_answers::is_answer));
    }
}
