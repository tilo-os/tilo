//! What a workspace keeps on the host that serves it beside its records: its
//! mail and calendar accounts, its model providers, the services it connected,
//! what its agents learned, and the events and initiative its assistant works
//! with. Each account and provider names its sign-in or key, an entry of the
//! workspace's one vault ([`super::vault`]); no secret is kept here. One set
//! per workspace, for all its members, kept under `workspaces/<workspace>/` in
//! the host's state, beside the workspace's asks and grants.
//!
//! The workspace's policy decides who reaches them, as it decides every app's
//! records ([`tilo_action_policy::decide`]). Each store stands at its app's
//! node ([`Store`]): the mail accounts at Mail's, the calendars at
//! Calendar's, the vault's items at Passwords' vault, the events at
//! Shortcuts' and the initiative at Assistant's. A member reaches them
//! through their role (an Editor uses the mail accounts and the vault's
//! items, a Viewer reads them), an agent or an API key through its grants,
//! and an owner who wants one closed says so with a grant like any other. A
//! caller the workspace does not have is told what a caller asking for
//! something that does not exist is told; one it has but whose grants do not
//! reach is refused.
//!
//! What stays a person's own (their formats and zone, what may interrupt
//! them) is kept apart, under `people/<person>/`, and no workspace reaches it.

use std::path::PathBuf;

use tilo_action_policy::{ActionKind, Answer, Context, TreePath};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::certify::PendingConfirmation;
use tilo_user_identity::DeviceCert;

use super::{WorkspaceId, Workspaces};

/// How long the person has to answer a question about a use.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

/// Where a store stands in the workspace's policy: an app's node, and the
/// kind of resource it is in that app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Store {
    app: &'static str,
    kind: &'static str,
}

impl Store {
    /// The workspace's mail accounts, in Mail.
    pub const MAIL: Self = Self {
        app: "so.tilo.mail",
        kind: tilo_actions::remote::person_accounts::PROVIDER,
    };
    /// The workspace's calendar accounts, in Calendar: only agents' hosts keep them.
    #[cfg(feature = "agents")]
    pub const CALENDARS: Self = Self {
        app: tilo_app_id::apps::CALENDAR,
        kind: tilo_actions::remote::person_accounts::PROVIDER,
    };
    /// The items of the workspace's vault, in Passwords.
    pub const VAULT: Self = Self {
        app: tilo_app_id::apps::PASSWORDS,
        kind: "passwords.workspace-vault",
    };
    /// The workspace's model providers and their keys.
    pub const MODELS: Self = Self {
        app: tilo_actions::remote::person_models::PROVIDER,
        kind: tilo_actions::remote::person_models::RESOURCE,
    };
    /// The services the workspace connected.
    pub const CONNECTIONS: Self = Self {
        app: tilo_actions::remote::person_connections::PROVIDER,
        kind: tilo_actions::remote::person_connections::KIND,
    };
    /// What the workspace's agents learned.
    pub const LEARNED: Self = Self {
        app: super::person_learned::PROVIDER,
        kind: super::person_learned::RESOURCE,
    };
    /// The workspace's scheduled work and events, in Shortcuts.
    pub const EVENTS: Self = Self {
        app: tilo_app_id::apps::SHORTCUTS,
        kind: tilo_actions::remote::workspace_events::KIND,
    };
    /// What the workspace's assistant finds to do on its own: its
    /// look-round and proposals, in Assistant.
    pub const INITIATIVE: Self = Self {
        app: tilo_app_id::apps::ASSISTANT,
        kind: tilo_actions::remote::workspace_initiative::LOOK_ROUND_KIND,
    };

    /// The node `action` of this store stands at in `workspace`.
    pub(super) fn node(self, workspace: &str, action: &str) -> TreePath {
        TreePath::workspace(workspace)
            .app(self.app)
            .kind(self.kind)
            .rest()
            .action(action)
    }
}

/// Who asks of a workspace's stores, once its policy let them.
#[derive(Clone, Debug)]
pub struct Reach {
    /// The workspace whose stores they reach.
    pub workspace: WorkspaceId,
    /// The person behind the caller: the person themselves, or the person
    /// whose agent or key it is.
    pub person: [u8; 32],
    /// The agent's name, as its person knows it, when an agent asks.
    pub agent: Option<String>,
    /// What the policy answered: [`Answer::Allow`], or [`Answer::Ask`] for an
    /// agent its person is asked about first.
    pub answer: Answer,
}

impl Reach {
    /// The name a use is remembered under: the agent's, or none for the
    /// person themselves.
    pub(super) fn by(&self) -> String {
        self.agent.clone().unwrap_or_default()
    }
}

impl Workspaces {
    /// Where this host keeps what `workspace` keeps on it beside its records:
    /// its accounts, providers and connections.
    pub fn workspace_root(&self, workspace: &str) -> PathBuf {
        super::person_asking::workspace_store_dir(&self.inner.dir, workspace)
    }

    /// Where every workspace's store stands on this host: a directory for
    /// each, named by the hex of its id.
    pub fn workspace_stores_root(&self) -> PathBuf {
        super::person_asking::workspace_store_root(&self.inner.dir)
    }

    /// Whether `caller`, on the device `certificate` certifies, may do
    /// `effect` with `action` of `store` in the workspace `workspace_id`
    /// names, as the workspace's policy decides it now. A person acting as
    /// themselves is asked nothing: their invocation is their answer. An
    /// agent its person's session was handed to is held to its delegation
    /// too: it reads, and a change asks its person ([`Answer::Ask`]) unless
    /// they answered that very action in advance.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a caller the workspace does not
    /// have, on a device it does not certify, or a workspace not governed
    /// here; [`ActionCallError::NotPermitted`] for one whose grants do not
    /// reach.
    pub async fn reaching(
        &self,
        (workspace_id, caller, certificate): (Option<WorkspaceId>, &Caller, Option<&DeviceCert>),
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        let workspace = workspace_id.ok_or(ActionCallError::UnknownAction)?;
        let acting = caller.acting();
        if acting.app().is_some() {
            return Err(ActionCallError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        let governed = self
            .held(workspace)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let now = tilo_clock::wall();
        let access = governed
            .lock()
            .await
            .peer()
            .permissions(certificate.device_public_key, now)
            .map_err(|_| ActionCallError::UnknownAction)?;
        // An agent a device of its person's runs acts as itself, with what its person's grant
        // gives it, within what its person may.
        let access = if caller.is_device_agent() {
            access
                .as_acting(acting.clone())
                .ok_or(ActionCallError::UnknownAction)?
                .granted(caller.granted(&[]))
        } else {
            access
        };
        let node = store.node(access.workspace(), action);
        let verdict = access.verdict(&node, effect, &Context::at_time(now));
        let answer = if acting.person_id().is_some() {
            // The person acting as themselves is the one their invocation
            // asks: nothing is put to them.
            if verdict.answer == Answer::Ask {
                Answer::Allow
            } else {
                verdict.answer
            }
        } else if acting.is_agent() && effect != ActionKind::Read {
            // An agent reads freely. A change it makes is put to its person
            // first, unless they answered that very action in advance.
            verdict.parts.last().map_or(verdict.answer, |agent| {
                verdict
                    .answer
                    .min(tilo_action_policy::agent_answers::held_to_contract(
                        agent, true,
                    ))
            })
        } else {
            verdict.answer
        };
        if answer == Answer::Deny {
            return Err(ActionCallError::NotPermitted);
        }
        let agent = acting.is_agent().then(|| {
            caller
                .agent_name()
                .or_else(|| acting.agent_id())
                .unwrap_or_default()
                .to_owned()
        });
        Ok(Reach {
            workspace,
            person: certificate.user_public_key,
            agent,
            answer,
        })
    }

    /// [`Self::reaching`], carried through: where the policy asks the
    /// caller's person first, they are asked on their approving devices,
    /// shown what the action is and where, never its arguments (which may
    /// hold a secret), and only their allowing it lets it go on.
    ///
    /// # Errors
    /// As [`Self::reaching`], or the person did not allow it.
    pub async fn reaching_allowed(
        &self,
        asked: (Option<WorkspaceId>, &Caller, Option<&DeviceCert>),
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        let reach = self.reaching(asked, store, action, effect).await?;
        self.allowed(reach, asked.1, store, action, effect).await
    }

    /// `reach`, once its person allowed it where the policy asks them first:
    /// the second half of [`Self::reaching_allowed`], for a caller that looks
    /// at what is asked before it decides whether the question is its own.
    ///
    /// # Errors
    /// The person did not allow it.
    pub async fn allowed(
        &self,
        mut reach: Reach,
        caller: &Caller,
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        if reach.answer == Answer::Ask {
            let agent = caller
                .acting()
                .agent_id()
                .ok_or(ActionCallError::NotPermitted)?;
            let place = self.workspace_name(reach.workspace).await;
            let question = PendingConfirmation {
                id: super::actions::agents::question_id(),
                agent_id: agent.to_owned(),
                agent_label: reach.by(),
                title: String::new(),
                action_label: action.replace('-', " "),
                resource_label: place.unwrap_or_else(|| store.app.to_owned()),
                arguments: Vec::new(),
                risk: "sensitive".into(),
                kind: match effect {
                    ActionKind::Read => "read",
                    ActionKind::Undoable => "undoable",
                    ActionKind::Outside => "outside",
                }
                .into(),
                undoable: effect == ActionKind::Undoable,
                workspace: reach.workspace.to_string(),
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
                page: None,
                ask: None,
                allow_always: false,
            };
            self.ask_person(reach.person, question).await?;
            reach.answer = Answer::Allow;
        }
        Ok(reach)
    }

    /// Whether the person `person` may do `effect` with `action` of `store`
    /// in `workspace` now, as its policy decides for them: what a turn this
    /// server carries out for them reaches, with no device of theirs behind
    /// it. A policy that asks them has not refused them: the turn waits on
    /// their answer for that one change, and this is where the change is
    /// weighed again after it.
    pub async fn person_reaches(
        &self,
        workspace: WorkspaceId,
        person: &[u8; 32],
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> bool {
        let Some(policy) = self.policy_of(&workspace.to_string()).await else {
            return false;
        };
        let person = hex::encode(person);
        if policy.active_role(&person).is_none() {
            return false;
        }
        let node = store.node(&policy.workspace, action);
        let access = tilo_action_policy::MemberAccess::person(policy, person.as_str());
        access.answer(&node, effect, &Context::at_time(tilo_clock::wall())) != Answer::Deny
    }

    /// Whether the agent `turn` carries out may do `effect` with `action` of
    /// `store` in its workspace now: the workspace's policy decides it for that
    /// agent acting on its asker's word, as it decides every change the agent
    /// makes to the workspace's records. The agent's own grants and the rules
    /// its entry bounds it to hold whoever asked; the asker's role bounds what
    /// their ask may reach, and a person the workspace no longer admits asks
    /// nothing. A policy that asks first has not refused: the turn waits on
    /// that answer for the one change, and this is where the change is weighed
    /// again after it.
    pub(super) async fn turn_reaches(
        &self,
        turn: &super::hosted_use::HostedTurn<'_>,
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> bool {
        let (Some(agent), Some(person)) = (
            turn.caller.acting().agent_id(),
            turn.caller.turn_person().map(hex::encode),
        ) else {
            return false;
        };
        let Some(policy) = self.policy_of(&turn.workspace.to_string()).await else {
            return false;
        };
        if policy.active_role(&person).is_none() {
            return false;
        }
        let bound = turn
            .bound
            .map(|bound| tilo_action_policy::roles::agent_bound(agent, &person, bound))
            .unwrap_or_default();
        // The agent works on its asker's word: it reaches what they may, as its own member.
        let acting = tilo_action_policy::Acting::handed_agent(agent, person.as_str());
        let Some(access) =
            tilo_action_policy::MemberAccess::person(policy, person.as_str()).as_acting(acting)
        else {
            return false;
        };
        let access = access.granted(bound);
        let node = store.node(access.workspace(), action);
        access.answer(&node, effect, &Context::at_time(tilo_clock::wall())) != Answer::Deny
    }
}
