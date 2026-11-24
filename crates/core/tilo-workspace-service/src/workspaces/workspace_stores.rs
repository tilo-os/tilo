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
//! Shortcuts' and the initiative at Chat's. A member reaches them
//! through their role (an Editor uses the mail accounts and the vault's
//! items, a Viewer reads them), an agent or an API key through its grants,
//! and an owner who wants one closed says so with a grant like any other. A
//! caller the workspace does not have is told what a caller asking for
//! something that does not exist is told; one it has but whose grants do not
//! reach is refused.
//!
//! What stays a person's own (their formats and zone, what may interrupt
//! them) is kept apart, under `people/<person>/`, and no workspace reaches it.

use std::path::{Path, PathBuf};

use tilo_action_policy::{Acting, ActionKind, Answer, Context, TreePath};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::{WorkspaceId, Workspaces};

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
        kind: tilo_action_policy::roles::WORKSPACE_VAULT_KIND,
    };
    /// The workspace's model providers and their keys.
    pub const MODELS: Self = Self {
        app: tilo_actions::remote::model_providers::PROVIDER,
        kind: tilo_actions::remote::model_providers::RESOURCE,
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
    /// look-round and proposals, in Chat with the workspace's agents.
    pub const INITIATIVE: Self = Self {
        app: tilo_app_id::apps::CHAT,
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
    /// The caller as it acted: the person themselves, or the agent acting for
    /// one, which every write a store makes on their behalf names as its
    /// invoker.
    pub acting: Acting,
}

impl Reach {
    /// The name a use is remembered under: the agent's, or none for the
    /// person themselves.
    pub(super) fn by(&self) -> String {
        self.agent.clone().unwrap_or_default()
    }
}

/// Where every workspace's store stands under the host's directory `dir`.
fn workspace_store_root(dir: &Path) -> PathBuf {
    dir.join("workspaces")
}

/// Where `workspace`'s store stands under the host's directory `dir`: named by the hex of its
/// id.
pub(super) fn workspace_store_dir(dir: &Path, workspace: &str) -> PathBuf {
    workspace_store_root(dir).join(hex::encode(workspace.as_bytes()))
}

impl Workspaces {
    /// Where this host keeps what `workspace` keeps on it beside its records:
    /// its accounts, providers and connections.
    pub fn workspace_root(&self, workspace: &str) -> PathBuf {
        workspace_store_dir(&self.inner.dir, workspace)
    }

    /// Where every workspace's store stands on this host: a directory for
    /// each, named by the hex of its id.
    pub fn workspace_stores_root(&self) -> PathBuf {
        workspace_store_root(&self.inner.dir)
    }

    /// Whether `caller`, on the device `certificate` certifies, may do
    /// `effect` with `action` of `store` in the workspace `workspace_id`
    /// names, as the workspace's policy decides it now. A person acting as
    /// themselves is asked nothing: their invocation is their answer, except
    /// in the vault, where what the rules ask them first waits for their yes. An
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
                .granted(caller.granted())
        } else {
            access
        };
        let node = store.node(access.workspace(), action);
        let verdict = access.verdict(&node, effect, &Context::at_time(now));
        let answer = if acting.person_id().is_some() {
            // The person acting as themselves is the one their invocation
            // asks: nothing is put to them. Not in the vault, where a rule
            // that asks them first is answered by their yes alone, as every
            // release of a secret is decided (`Workspaces::using_vault`); a
            // step another action declares there is that action's, decided
            // when it was called.
            if verdict.answer == Answer::Ask && store != Store::VAULT {
                Answer::Allow
            } else {
                verdict.answer
            }
        } else if acting.is_agent() && effect != ActionKind::Read {
            // An agent reads freely. A change it makes is put to its person
            // first, unless they answered that very action in advance.
            let narrowed = self.agent_narrowed(&workspace.to_string(), acting).await;
            agent_change(&verdict, narrowed)
        } else if acting.is_agent() {
            // An agent reads with its person's access: what they read, unless something denies
            // it the read or its person's answers narrow it.
            let narrowed = self.agent_narrowed(&workspace.to_string(), acting).await;
            agent_read(&verdict, narrowed)
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
            acting: acting.clone(),
        })
    }

    /// [`Self::reaching`], carried through: where the policy asks the
    /// caller's person first, `request` goes on only once they approved it,
    /// shown what the action is and where, never its arguments (which may
    /// hold a secret). Without a request (a listing) it reaches only what
    /// needs nobody's yes.
    ///
    /// # Errors
    /// As [`Self::reaching`], or it waits for the person's yes.
    pub async fn reaching_allowed(
        &self,
        asked: (Option<WorkspaceId>, &Caller, Option<&DeviceCert>),
        store: Store,
        action: &str,
        effect: ActionKind,
        request: impl Into<Option<&InvokeRequest>>,
    ) -> Result<Reach, ActionCallError> {
        let request = request.into();
        let reach = self.reaching(asked, store, action, effect).await?;
        self.allowed(reach, asked.1, store, action, request).await
    }

    /// `reach`, once its person approved `request` where the policy asks them first: the
    /// second half of [`Self::reaching_allowed`], for a caller that looks at what is asked
    /// before it decides whether the question is its own.
    ///
    /// # Errors
    /// It waits for the person's yes.
    pub async fn allowed(
        &self,
        mut reach: Reach,
        caller: &Caller,
        store: Store,
        action: &str,
        request: impl Into<Option<&InvokeRequest>>,
    ) -> Result<Reach, ActionCallError> {
        if reach.answer == Answer::Ask {
            // An agent's change is put to its person; a person's own call of
            // what the rules ask them first waits for their yes.
            let acting = caller.acting();
            if acting.agent_id().is_none() && acting.person_id().is_none() {
                return Err(ActionCallError::NotPermitted);
            }
            let place = self
                .workspace_name(reach.workspace)
                .await
                .unwrap_or_else(|| store.app.to_owned());
            let facts = AskedFacts {
                destructive: false,
                facts: vec![Fact::new("In", place)],
                verb: action.replace('-', " "),
                ..AskedFacts::by(super::asking::asked_by(caller))
            };
            match request.into() {
                Some(request) => super::asking::approved_or_asks(request, || facts)?,
                None => return Err(ActionCallError::asks(facts)),
            }
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
        // The agent works on its asker's word: it reaches what they may, as its own member.
        let acting = tilo_action_policy::Acting::handed_agent(agent, person.as_str());
        let Some(access) =
            tilo_action_policy::MemberAccess::person(policy, person.as_str()).as_acting(acting)
        else {
            return false;
        };
        let access = access.granted(turn.caller.granted());
        let node = store.node(access.workspace(), action);
        let verdict = access.verdict(&node, effect, &Context::at_time(tilo_clock::wall()));
        let narrowed = policy_narrows(access.policy.as_ref(), agent);
        let answer = if effect == ActionKind::Read {
            verdict.answer
        } else {
            agent_change(&verdict, narrowed)
        };
        answer != Answer::Deny
    }
}

/// What `verdict` answers a change an agent makes: what the people it acts for answer, and its
/// own part held to the change's contract, so a change it makes is put to its person unless
/// they answered that very action in advance
/// ([`tilo_action_policy::agent_answers::held_to_contract`]). A part of the agent's that no grant
/// covers is its person's to answer too: their yes admits it for that call, and their "Always"
/// for its scope, which is then the grant that covers it. A part that something denies (its
/// person's answer, a workspace's grant naming it, what the workspace keeps from agents) stays
/// refused, and so does every part no grant covers of an agent its person's answers narrow
/// (`narrowed`): it takes only what those answers name.
pub(super) fn agent_change(verdict: &tilo_action_policy::Verdict<'_>, narrowed: bool) -> Answer {
    let Some((agent, people)) = verdict.parts.split_last() else {
        return verdict.answer;
    };
    let theirs = people
        .iter()
        .map(|part| part.answer)
        .min()
        .unwrap_or(verdict.answer);
    let own = if !narrowed
        && agent.answer == Answer::Deny
        && matches!(agent.basis, tilo_action_policy::Basis::NoGrant)
    {
        Answer::Ask
    } else {
        tilo_action_policy::agent_answers::held_to_contract(agent, true)
    };
    theirs.min(own)
}

/// What `verdict` answers a read an agent makes: what the people it acts for answer, where its
/// own part allows it or no grant covers it and its person's answers do not narrow it
/// (`narrowed`). A part that something denies stays refused.
fn agent_read(verdict: &tilo_action_policy::Verdict<'_>, narrowed: bool) -> Answer {
    let Some((agent, people)) = verdict.parts.split_last() else {
        return verdict.answer;
    };
    let theirs = people
        .iter()
        .map(|part| part.answer)
        .min()
        .unwrap_or(verdict.answer);
    let own = if !narrowed && matches!(agent.basis, tilo_action_policy::Basis::NoGrant) {
        Answer::Allow
    } else {
        agent.answer
    };
    theirs.min(own)
}

/// Whether its person narrowed the agent `agent` in `policy`: an answer they gave it stands
/// there ([`tilo_action_policy::agent_answers`]). It then takes only what those answers name,
/// and is never asked for what they do not.
fn policy_narrows(policy: &tilo_action_policy::WorkspacePolicy, agent: &str) -> bool {
    let named = tilo_action_policy::Grantee::agent(agent.to_owned());
    let now = Context::at_time(tilo_clock::wall());
    policy.grants.iter().any(|grant| {
        grant.grantee == named
            && grant.is_live(&now)
            && tilo_action_policy::agent_answers::is_answer(grant)
    })
}

impl Workspaces {
    /// Whether the agent `acting` names is narrowed by its person's answers in `workspace`
    /// ([`policy_narrows`]).
    async fn agent_narrowed(&self, workspace: &str, acting: &Acting) -> bool {
        let Some(agent) = acting.agent_id() else {
            return false;
        };
        self.policy_of(workspace)
            .await
            .is_some_and(|policy| policy_narrows(&policy, agent))
    }
}
