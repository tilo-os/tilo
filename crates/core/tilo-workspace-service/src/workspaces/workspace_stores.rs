//! What a workspace keeps on the host that serves it beside its records: its
//! mail and calendar accounts, its model providers, the services it connected,
//! what its agents learned, and the events its assistant works with. Each account and provider names its sign-in or key, an entry of the
//! workspace's one vault ([`super::vault`]); no secret is kept here. One set
//! per workspace, for all its members, kept under `workspaces/<workspace>/` in
//! the host's state, beside the workspace's asks and grants.
//!
//! The workspace's policy decides who reaches them, as it decides every app's
//! records ([`tilo_action_policy::decide`]). Each store stands at its app's
//! node ([`Store`]): the mail accounts at Mail's, the calendars at
//! Calendar's, the vault's items at Passwords' vault, and the events at
//! Shortcuts'. A member reaches them
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
        app: tilo_app_id::apps::MAIL,
        kind: crate::workspace_accounts::PROVIDER,
    };
    /// The workspace's calendar accounts, in Calendar: only agents' hosts keep them.
    #[cfg(feature = "agents")]
    pub const CALENDARS: Self = Self {
        app: tilo_app_id::apps::CALENDAR,
        kind: crate::workspace_accounts::PROVIDER,
    };
    /// The items of the workspace's vault, in Passwords.
    pub const VAULT: Self = Self {
        app: tilo_app_id::apps::PASSWORDS,
        kind: tilo_actions::remote::VAULT_KIND,
    };
    /// The workspace's model providers and their keys.
    pub const MODELS: Self = Self {
        app: crate::models::PROVIDER,
        kind: crate::models::RESOURCE,
    };
    /// The services the workspace connected.
    pub const CONNECTIONS: Self = Self {
        app: crate::person_connections::PROVIDER,
        kind: crate::person_connections::KIND,
    };
    /// What the workspace's agents learned.
    pub const LEARNED: Self = Self {
        app: super::actions::LEARNED,
        kind: "facts",
    };
    /// The workspace's scheduled work and events, in Shortcuts.
    pub const EVENTS: Self = Self {
        app: tilo_app_id::apps::SHORTCUTS,
        kind: tilo_actions::remote::workspace_events::KIND,
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
    /// What the policy answered: [`Answer::Allow`], or [`Answer::Ask`] for a
    /// call its person is asked about first, theirs or their agent's.
    pub answer: Answer,
    /// The caller as it acted: the person themselves, or the agent acting for
    /// one, which every write a store makes on their behalf names as its
    /// invoker.
    pub acting: Acting,
}

impl Reach {
    /// The name a use is remembered under: the agent's, or none for the
    /// person themselves.
    pub fn by(&self) -> String {
        self.agent.clone().unwrap_or_default()
    }
}

/// Where every workspace's store stands under the host's directory `dir`.
fn workspace_store_root(dir: &Path) -> PathBuf {
    dir.join("workspaces")
}

/// Where `workspace`'s store stands under the host's directory `dir`: named by the hex of its
/// id.
pub fn workspace_store_dir(dir: &Path, workspace: &str) -> PathBuf {
    workspace_store_root(dir).join(hex::encode(workspace.as_bytes()))
}

impl Workspaces {
    /// Whether the agent working a turn as `caller` in `workspace` may do `effect` with `action` of
    /// `store` in its workspace now: the workspace's policy decides it for that
    /// agent acting on its asker's word, as it decides every change the agent
    /// makes to the workspace's records. The agent's own grants and the rules
    /// its entry bounds it to hold whoever asked; the asker's role bounds what
    /// their ask may reach, and a person the workspace no longer admits asks
    /// nothing. A policy that asks first has not refused: the turn waits on
    /// that answer for the one change, and this is where the change is weighed
    /// again after it.
    pub async fn turn_reaches(
        &self,
        (workspace, caller): (WorkspaceId, &Caller),
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> bool {
        let (Some(agent), Some(person)) = (
            caller
                .acting()
                .member_id()
                .filter(|member| member.is_agent()),
            caller.turn_person().map(hex::encode),
        ) else {
            return false;
        };
        let Some(policy) = self.policy_of(&workspace.to_string()).await else {
            return false;
        };
        if policy.active_role(&person).is_none() {
            return false;
        }
        // The agent works on its asker's word: it reaches what they may, as its own member.
        let acting = tilo_action_policy::Acting::member(agent.clone())
            .handed()
            .acting_for(tilo_action_policy::Acting::person(person.as_str()));
        // What its delegation hands it: the preset of a handed agent and its caller's grants.
        let mut handed = acting.handed_preset(policy.workspace.as_str());
        handed.extend(caller.granted());
        let Some(access) =
            tilo_action_policy::MemberAccess::person(policy, person.as_str()).as_acting(acting)
        else {
            return false;
        };
        let access = access.granted(caller.granted());
        let node = store.node(access.workspace(), action);
        let cx = Context::at_time(tilo_clock::wall());
        let mut verdict = access.verdict(&node, effect, &cx);
        let narrowed = narrowed(access.policy.as_ref(), caller);
        // A narrowed agent takes only the actions its person's answers name, which each caller
        // checks where the action is taken. The store is what such an action reaches, so the
        // agent's own part reaches it as its grants and its delegation let it.
        if narrowed
            && let Some((id, own_person)) = agent.as_agent()
            && let Some(own) = verdict.parts.last_mut()
        {
            *own = access
                .policy
                .party_of_agent(id, own_person.as_str())
                .with_grants(&handed)
                .decide(&node, effect, &cx);
        }
        let answer = if effect == ActionKind::Read {
            // The agent's own part, decided again above for a narrowed agent, joins its people's.
            if narrowed {
                agent_read(&verdict, narrowed)
            } else {
                verdict.answer
            }
        } else {
            agent_change(&verdict, narrowed)
        };
        answer != Answer::Deny
    }

    /// Where this host keeps what `workspace` keeps on it beside its records:
    /// its accounts, providers and connections.
    pub fn workspace_root(&self, workspace: &str) -> PathBuf {
        workspace_store_dir(&self.inner.dir, workspace)
    }

    /// Whether `caller`, on the device `certificate` certifies, may do
    /// `effect` with `action` of `store` in the workspace `workspace_id`
    /// names, as the workspace's policy decides it now. What the rules ask a
    /// person first is asked of them in every store ([`Answer::Ask`]): making
    /// the call is not their answer, only the confirmation it carries is
    /// ([`Self::allowed`]). An agent its person's session was handed to is
    /// bound by its delegation too: it reads, and a change asks its person
    /// unless they answered that very action in advance.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a caller the workspace does not
    /// have, on a device it does not certify, or a workspace not hosted
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
        // An app acting by itself holds the grants naming it, never a person's: those the
        // workspace recorded (what installing it gave it), and those its device hands it from
        // its signed manifest. With nobody at it to ask, what none of them allows is refused.
        if let Some(app) = acting.app()
            && acting.person_id().is_none()
        {
            let policy = self
                .policy_of(&workspace.to_string())
                .await
                .ok_or(ActionCallError::UnknownAction)?;
            let node = store.node(&policy.workspace, action);
            let answer = app_answer(
                app.as_str(),
                (&policy, &caller.granted()),
                (&node, effect),
                &Context::at_time(tilo_clock::wall()),
            );
            if answer != Answer::Allow {
                return Err(ActionCallError::NotPermitted);
            }
            return Ok(Reach {
                workspace,
                person: self.device_cert().user_public_key,
                agent: None,
                answer,
                acting: acting.clone(),
            });
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        let hosted = self
            .held(workspace)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let now = tilo_clock::wall();
        let access = hosted
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
        let answer = if acting.is_agent() && effect != ActionKind::Read {
            // An agent reads freely. A change it makes is put to its person
            // first, unless they answered that very action in advance.
            agent_change(&verdict, narrowed(access.policy.as_ref(), caller))
        } else if acting.is_agent() {
            // An agent reads with its person's access: what they read, unless something denies
            // it the read or its person's answers narrow it.
            agent_read(&verdict, narrowed(access.policy.as_ref(), caller))
        } else {
            verdict.answer
        };
        // A person working through an app reaches only what the app's grants reach too: what
        // installing it gave it, or what its person answers when asked.
        let answer = match acting.app() {
            Some(app) => answer.min(app_answer(
                app.as_str(),
                (access.policy.as_ref(), &caller.granted()),
                (&node, effect),
                &Context::at_time(now),
            )),
            None => answer,
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
            // An agent's change is put to its person, and so is a person's own
            // call of what the rules ask them first: only the confirmation the
            // call carries answers it, the person's approve of the asked record
            // it was kept as ([`InvokeRequest::approval`]). The system's confirm
            // view or the CLI's own prompt gives it, and a frame carrying one
            // its record did not hand back is refused where it enters
            // ([`tilo_remote_actions::asked::admit_frame`]), so an app or an
            // agent cannot set it, and it answers this one call only.
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

/// Whether its own person narrowed the agent `caller` acts as in `policy`: another member's
/// answers for an agent of that id are for their own agent and narrow nothing here
/// ([`tilo_action_policy::WorkspacePolicy::answers_narrow`]).
fn narrowed(policy: &tilo_action_policy::WorkspacePolicy, caller: &Caller) -> bool {
    agent_of(caller).is_some_and(|(agent, person)| {
        policy.answers_narrow(agent, person, &Context::at_time(tilo_clock::wall()))
    })
}

/// What the grants naming `app` answer for `effect` at `node`: the workspace's (what installing
/// the app gave it, or a member gave it since) and those its device hands it from its signed
/// manifest (`granted`). A node none of them reaches is asked of the person, never allowed by
/// the app's name; a grant that denies it refuses it.
fn app_answer(
    app: &str,
    (policy, granted): (
        &tilo_action_policy::WorkspacePolicy,
        &[tilo_action_policy::Grant],
    ),
    (node, effect): (&TreePath, ActionKind),
    cx: &Context<'_>,
) -> Answer {
    let decided = policy
        .party_of(tilo_action_policy::Grantee::app(app))
        .with_grants(granted)
        .decide(node, effect, cx);
    match (decided.answer, decided.basis) {
        (Answer::Deny, tilo_action_policy::Basis::NoGrant) => Answer::Ask,
        (answer, _) => answer,
    }
}

/// The agent `caller` acts as and the person it is the agent of, as its member names them: the
/// one person whose answers narrow it. `None` for a caller that is no agent.
#[must_use]
pub fn agent_of(caller: &Caller) -> Option<(&str, &str)> {
    caller
        .acting()
        .member_id()?
        .as_agent()
        .map(|(agent, person)| (agent, person.as_str()))
}
