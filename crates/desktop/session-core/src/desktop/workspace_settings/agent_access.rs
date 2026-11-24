//! What one agent may do in the workspace, app by app: Allow, Ask first or Deny for reading,
//! changes and outside effects.
//! Each answer is a rule of the bound the workspace's list of its agents keeps for the agent
//! ([`bound_with`]), which every device and the workspace's host read as they decide its calls;
//! without one the agent's admission decides, and the view says so. Actions whose contract asks
//! for confirmation (sending, publishing, deleting for good) still ask first under an app-wide
//! Allow; only an Allow on the action itself lifts that, and those exceptions are listed here to
//! take back.

use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::roles::{Rule, is_agent_bound};
use tilo_action_policy::{ActionKind, Answer, Grantee, KindSet, decide};
use tilo_desktop_model::Workspace;
use tilo_sort::CompactSort as _;

use super::{AppDirectory, action_name, app_display_name, grant_recipients};
use crate::desktop::i18n::{t, t_args};

/// The three kinds of action, in column order.
pub const KINDS: [ActionKind; 3] = [ActionKind::Read, ActionKind::Undoable, ActionKind::Outside];

/// The three answers, in segment order.
pub const ANSWERS: [Answer; 3] = [Answer::Allow, Answer::Ask, Answer::Deny];

/// The key a kind of action is asked for by at the API.
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
    /// The agent's bound sets it at this scope; otherwise it follows the agent's admission or a
    /// broader answer.
    pub explicit: bool,
}

/// One application's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAccessRow {
    /// The app whose answers these are, for its mark.
    pub app: String,
    pub scope: AnswerScope,
    pub label: String,
    /// What the row's outside effects are, in one line, for the person's own account rows whose
    /// name does not say what reaching outside them means: creating and deleting their
    /// workspaces, inviting people. `None` for an app's own row.
    pub effects: Option<String>,
    pub cells: [AnswerCell; 3],
}

impl AgentAccessRow {
    /// Whether any of its answers was set here.
    #[must_use]
    pub fn customised(&self) -> bool {
        self.cells.iter().any(|cell| cell.explicit)
    }

    /// The answers offered for `cell`, by their place in [`ANSWERS`]: the one it holds, and every
    /// one its person may give it at the row's scope ([`agent_answers::may_give`]).
    #[must_use]
    pub fn choices(&self, cell: &AnswerCell) -> Vec<usize> {
        ANSWERS
            .iter()
            .enumerate()
            .filter(|(_, answer)| {
                **answer == cell.answer
                    || agent_answers::may_give(Some(&self.scope), cell.kind, **answer)
            })
            .map(|(index, _)| index)
            .collect()
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
/// [`tilo_action_policy::Acting::handed_preset`] and its bound) and the workspace's grants
/// naming it alone, so a narrower answer its bound sets wins over what the cap says.
#[must_use]
pub fn answer_at(
    ws: &Workspace,
    agent: &str,
    admission: &[tilo_action_policy::Grant],
    scope: &AnswerScope,
    kind: ActionKind,
) -> AnswerCell {
    let subject = [Grantee::agent(agent)];
    let cx = tilo_action_policy::Context::at_secs(tilo_clock::unix_now());
    let node = scope.node(&ws.policy.workspace);
    let decision = decide(
        admission.iter().chain(ws.policy.grants.iter()),
        &subject,
        &node,
        kind,
        &cx,
    );
    AnswerCell {
        kind,
        answer: decision.answer,
        explicit: decision
            .grant()
            .is_some_and(|grant| is_agent_bound(grant, agent) && grant.node == node),
    }
}

/// The apps an agent's answers are offered for: every app installed for the workspace, every app
/// it shows or holds capabilities for, and every app its bound names, by display name; then the
/// person's own account.
#[must_use]
pub fn agent_access_rows(
    ws: &Workspace,
    agent: &str,
    admission: &[tilo_action_policy::Grant],
    apps: &dyn AppDirectory,
) -> Vec<AgentAccessRow> {
    let mut ids: Vec<String> = grant_recipients(ws)
        .into_iter()
        .filter(|candidate| candidate.kind == super::RecipientKind::App)
        .map(|candidate| candidate.id)
        .collect();
    // Every app installed here is answered for, not only those with a window open: a person sets
    // what the agent may do in an app before it first reaches for it.
    ids.extend(apps.installed_apps());
    ids.extend(
        admission
            .iter()
            .filter(|grant| is_agent_bound(grant, agent))
            .filter_map(|grant| match AnswerScope::of_node(&grant.node)? {
                AnswerScope::App { app }
                | AnswerScope::Kind { app, .. }
                | AnswerScope::Action { app, .. }
                | AnswerScope::Resource { app, .. } => Some(app),
            }),
    );
    ids.compact_sort();
    ids.dedup();
    ids.retain(|app| !ACCOUNT.iter().any(|(own, ..)| own == app));
    let row = |app: String, label: String, effects: Option<String>| {
        let scope = AnswerScope::app(&app);
        AgentAccessRow {
            cells: KINDS.map(|kind| answer_at(ws, agent, admission, &scope, kind)),
            label,
            scope,
            app,
            effects,
        }
    };
    let mut rows: Vec<AgentAccessRow> = ids
        .into_iter()
        .map(|app| {
            let label = app_display_name(apps, &app);
            row(app, label, None)
        })
        .collect();
    rows.compact_sort_by_key(|row| row.label.to_lowercase());
    // What the person's own account holds follows the apps, in every workspace: the agent
    // makes and deletes their workspaces, and governs this one, as these rows answer.
    rows.extend(
        ACCOUNT
            .iter()
            .map(|(app, label, effects)| row((*app).to_owned(), t(label), Some(t(effects)))),
    );
    rows
}

/// The person's own account, as the rows after the apps name it, with a line naming the
/// outside effects the row covers.
const ACCOUNT: [(&str, &str, &str); 2] = [
    (
        agent_answers::ACCOUNT_APP,
        "workspace-settings-agent-access-workspaces",
        "workspace-settings-agent-access-workspaces-effects",
    ),
    (
        agent_answers::WORKSPACE_APP,
        "workspace-settings-agent-access-members",
        "workspace-settings-agent-access-members-effects",
    ),
];

/// The actions `bound` always allows its agent, without asking first.
#[must_use]
pub fn agent_exceptions(bound: &[Rule], apps: &dyn AppDirectory) -> Vec<AgentException> {
    let mut out: Vec<AgentException> = bound
        .iter()
        .filter(|rule| rule.answer == Answer::Allow)
        .flat_map(|rule| {
            let scope = AnswerScope::of_node(&rule.node);
            rule.kinds.iter().filter_map(move |kind| {
                let scope = scope.clone()?;
                let AnswerScope::Action { app, action, .. } = &scope else {
                    return None;
                };
                let label = t_args(
                    "workspace-settings-agent-exception",
                    &[
                        ("app", app_display_name(apps, app).into()),
                        ("action", action_name(apps, app, action).into()),
                    ],
                );
                Some(AgentException { scope, kind, label })
            })
        })
        .collect();
    out.compact_sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// `bound` with each of `answers` set: for a kind at a scope in `workspace`, the answer, or
/// (`None`) none, so the admission and broader answers decide there again. A rule is kept per
/// scope and kind, so setting one again replaces it.
#[must_use]
pub fn bound_with(
    bound: &[Rule],
    workspace: &str,
    answers: &[(AnswerScope, ActionKind, Option<Answer>)],
) -> Vec<Rule> {
    let mut rules = bound.to_vec();
    for (scope, kind, answer) in answers {
        let node = scope.node(workspace);
        for rule in rules.iter_mut().filter(|rule| rule.node == node) {
            rule.kinds = rule
                .kinds
                .iter()
                .filter(|held| held != kind)
                .fold(KindSet::NONE, KindSet::with);
        }
        rules.retain(|rule| !rule.kinds.is_empty());
        if let Some(answer) = answer {
            rules.push(Rule::new(node, KindSet::of(*kind), *answer));
        }
    }
    rules
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe";
    const MAIL: &str = "so.tilo.mail";

    /// A workspace where the agent may change it all and asks before reaching outside.
    fn ws() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from_u128(31), "access");
        ws.set_owner(OWNER);
        ws
    }

    /// The grants the agent's admission gives it: the editor cap, narrowed by `bound`.
    fn admission(ws: &Workspace, bound: &[Rule]) -> Vec<tilo_action_policy::Grant> {
        let mut grants = tilo_action_policy::Acting::handed_agent(AGENT, OWNER)
            .handed_preset(&ws.policy.workspace);
        grants.extend(tilo_action_policy::roles::agent_bound(AGENT, OWNER, bound));
        grants
    }

    tilo_test_env::scenario! {
        fn an_agents_answers_follow_its_admission_until_its_bound_sets_them;

        phase answers_follow_the_admission_until_the_bound_sets_them;
        {
            let ws = ws();
            let mail = AnswerScope::app(MAIL);
            let read = answer_at(&ws, AGENT, &admission(&ws, &[]), &mail, ActionKind::Read);
            let outside = answer_at(&ws, AGENT, &admission(&ws, &[]), &mail, ActionKind::Outside);
            assert_eq!((read.answer, read.explicit), (Answer::Allow, false));
            assert_eq!((outside.answer, outside.explicit), (Answer::Ask, false));

            let bound = bound_with(
                &[],
                &ws.policy.workspace,
                &[(mail.clone(), ActionKind::Outside, Some(Answer::Deny))],
            );
            assert_eq!(bound.len(), 1);
            let outside =
                answer_at(&ws, AGENT, &admission(&ws, &bound), &mail, ActionKind::Outside);
            assert_eq!((outside.answer, outside.explicit), (Answer::Deny, true));
            let notes = answer_at(
                &ws,
                AGENT,
                &admission(&ws, &bound),
                &AnswerScope::app("so.tilo.notes"),
                ActionKind::Outside,
            );
            assert_eq!((notes.answer, notes.explicit), (Answer::Ask, false));
        }

        phase setting_again_replaces_and_clearing_returns_to_the_admission;
        {
            let ws = ws();
            let mail = AnswerScope::app(MAIL);
            let set = |bound: &[Rule], answer| {
                bound_with(
                    bound,
                    &ws.policy.workspace,
                    &[(mail.clone(), ActionKind::Undoable, answer)],
                )
            };
            let asked = set(&[], Some(Answer::Ask));
            let denied = set(&asked, Some(Answer::Deny));
            assert_eq!(denied.len(), 1, "one rule per scope and kind");
            assert_eq!(denied[0].answer, Answer::Deny);
            let cleared = set(&denied, None);
            assert!(cleared.is_empty());
            let cell = answer_at(&ws, AGENT, &admission(&ws, &cleared), &mail, ActionKind::Undoable);
            assert_eq!((cell.answer, cell.explicit), (Answer::Allow, false));
        }

        phase rows_list_the_apps_the_bound_names_and_exceptions_only_action_allows;
        {
            let ws = ws();
            let send = AnswerScope::action(MAIL, "mail.mailbox", "send_message");
            let purge = AnswerScope::action(MAIL, "mail.mailbox", "purge");
            let bound = bound_with(
                &[],
                &ws.policy.workspace,
                &[
                    (send.clone(), ActionKind::Outside, Some(Answer::Allow)),
                    (purge, ActionKind::Undoable, Some(Answer::Deny)),
                ],
            );
            let rows = agent_access_rows(
                &ws,
                AGENT,
                &admission(&ws, &bound),
                &super::super::NoAppDirectory,
            );
            assert_eq!(rows.len(), 3, "Mail, then the person's own account");
            assert_eq!(rows[0].scope, AnswerScope::app(MAIL));
            assert_eq!(rows[0].app, MAIL);
            assert_eq!(rows[0].label, "Mail", "an app is named, not shown by id");
            assert!(!rows[0].customised(), "an action's answer is not the app's");
            // The person may let it go unasked within their own account, never elsewhere.
            let outside = |row: &AgentAccessRow| row.choices(&row.cells[2]);
            assert_eq!(outside(&rows[0]), [1, 2], "Mail's outside effects ask or are denied");
            assert_eq!(rows[0].effects, None, "an app's own name says what it is");
            assert_eq!(
                (rows[1].app.as_str(), rows[1].label.as_str()),
                (agent_answers::ACCOUNT_APP, "Your workspaces")
            );
            assert_eq!(outside(&rows[1]), [0, 1, 2], "making a workspace may be allowed");
            assert_eq!(
                rows[1].effects.as_deref(),
                Some("Creating, branching, entering and deleting your workspaces."),
                "its row says what reaching outside it is"
            );
            assert_eq!(rows[2].app, agent_answers::WORKSPACE_APP);
            assert_eq!(outside(&rows[2]), [0, 1, 2]);
            assert!(
                rows[2]
                    .effects
                    .as_deref()
                    .is_some_and(|effects| effects.contains("Inviting")),
                "its row says what reaching outside it is"
            );
            let exceptions = agent_exceptions(&bound, &super::super::NoAppDirectory);
            assert_eq!(exceptions.len(), 1, "a Deny is no exception to list");
            assert_eq!(exceptions[0].scope, send);
            assert_eq!(exceptions[0].kind, ActionKind::Outside);
            assert_eq!(exceptions[0].label, "Send message in Mail");
            let taken_back = bound_with(
                &bound,
                &ws.policy.workspace,
                &[(send, ActionKind::Outside, None)],
            );
            assert_eq!(taken_back.len(), 1);
            assert!(agent_exceptions(&taken_back, &super::super::NoAppDirectory).is_empty());
        }
    }
}
