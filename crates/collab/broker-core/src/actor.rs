//! Who takes an action, and what the workspace decides for them.
//!
//! Every action is admitted as an actor: a person from their own device or a
//! foreign one, an agent acting for a person, an application or a bot under
//! its own identity. The actor's grants, its delegation and the turn it acts
//! for decide one invocation, and every host decides it the same way — the
//! Linux broker over its app sockets and the page over the apps it runs.

use tilo_action_policy::{ActionKind, Answer, Authority, Grant, Subject, TreePath, roles};
use tilo_actions::remote::{Caller, CallerPrincipal};
use tilo_desktop_model::Workspace;

use crate::published::{PublishedAction, PublishedSurface, canonical_record, published_node};

/// The policy principal a user root is: the person its devices chain to.
#[must_use]
pub fn principal_id_for_root(user_public_key: [u8; 32]) -> String {
    match collab_types::Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
        user_public_key,
    )) {
        collab_types::Principal::Person(id) => id,
        _ => hex::encode(user_public_key),
    }
}

/// The task run an invocation is one of, as admission names it: the run's
/// agent, and the issue it works on.
#[derive(Clone, Copy, Debug)]
pub struct RunFacts<'a> {
    pub agent: &'a str,
    pub issue: &'a str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BrokerActor {
    pub id: String,
    pub is_agent: bool,
    /// A person acting from a foreign device in a workspace it does not govern.
    pub capped: bool,
    /// The person an agent acts for, when one delegated it: the agent then
    /// may do only what both may.
    pub for_person: Option<String>,
    /// The person's own device delegated the agent for this session: it
    /// carries the delegation a device gives its agents (read and undoable
    /// actions, outside ones asking first, no governance).
    session_delegated: bool,
    /// The turn the agent acts for: the person's message whose conversation it
    /// takes part in. Every op an agent makes names it, so presence and history
    /// say what caused it, and no agent acts outside one.
    turn: Option<tilo_action_policy::turn::Turn>,
    /// The issue whose run the agent works in, by its address: grants
    /// naming the issue decide for the run as the agent's own do.
    issue: Option<String>,
    /// How far the invocation this actor's request continues was admitted
    /// to reach: what goes further asks first
    /// ([`tilo_action_policy::Reach`]).
    pub ceiling: Option<tilo_action_policy::Reach>,
}

impl BrokerActor {
    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: false,
            capped: false,
            for_person: None,
            session_delegated: false,
            turn: None,
            issue: None,
            ceiling: None,
        }
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: true,
            capped: false,
            for_person: None,
            session_delegated: false,
            turn: None,
            issue: None,
            ceiling: None,
        }
    }

    /// The application `app`, acting under its own identity: it may do what
    /// the workspace grants it and nothing it borrows from a person. A reading it
    /// takes is decided the same way.
    #[must_use]
    pub fn app(app: &str) -> Self {
        Self {
            id: format!("app:{app}"),
            is_agent: true,
            capped: false,
            for_person: None,
            session_delegated: false,
            turn: None,
            issue: None,
            ceiling: None,
        }
    }

    /// Installed bot `installation`, acting for the person who installed it:
    /// it may do only what both its grant and that person may.
    #[must_use]
    pub fn bot(installation: &str, installer: impl Into<String>) -> Self {
        Self {
            id: format!("bot:{installation}"),
            is_agent: false,
            capped: false,
            for_person: Some(installer.into()),
            session_delegated: false,
            turn: None,
            issue: None,
            ceiling: None,
        }
    }

    /// The actor `caller`'s principal is: the one admission every door of a
    /// host starts from.
    #[must_use]
    pub fn for_caller(caller: &Caller) -> Self {
        Self::principal_of(caller)
            .on_turn(caller.turn().cloned())
            .within(caller.ceiling())
    }

    /// The actor `caller`'s principal is, before any ceiling.
    fn principal_of(caller: &Caller) -> Self {
        match caller.principal() {
            CallerPrincipal::Person(id) => Self::person(id.clone()),
            CallerPrincipal::Agent(id) => {
                let agent = Self::agent(format!("agent:{id}"));
                // A person's own device handed this connection to the agent
                // (`Caller::delegated_to`); an agent member the host admitted
                // acts for the person who admitted it, with no more than the
                // workspace grants it (`Caller::acting_for`); a delegated agent
                // device acts for the person its certificate chains to.
                match (caller.acting_for_person(), caller.user_public_key()) {
                    (Some(person), _) if caller.is_session_delegated() => {
                        agent.for_person(person).session_delegated()
                    }
                    (Some(person), _) => agent.for_person(person),
                    (None, Some(root)) => agent.for_person(principal_id_for_root(*root)),
                    (None, None) => agent,
                }
            }
            CallerPrincipal::App(id) => Self::app(id),
            CallerPrincipal::Bot(id) => match caller.user_public_key() {
                Some(root) => Self::bot(id, principal_id_for_root(*root)),
                // A bot nobody installed has no authority to borrow: acting
                // for nobody, it gets nothing.
                None => Self::bot(id, String::new()),
            },
        }
    }

    /// The same agent, acting for `person`.
    #[must_use]
    pub fn for_person(mut self, person: impl Into<String>) -> Self {
        self.for_person = Some(person.into());
        self
    }

    /// The same actor, held to the foreign-device bound when `capped`: from
    /// a device that is not its person's.
    #[must_use]
    pub fn capped(mut self, capped: bool) -> Self {
        self.capped = capped;
        self
    }

    /// The same agent, delegated by its person's own device for this
    /// session.
    #[must_use]
    pub fn session_delegated(mut self) -> Self {
        self.session_delegated = true;
        self
    }

    /// The same agent, acting inside `turn`: the person's message it answers.
    #[must_use]
    pub fn on_turn(mut self, turn: Option<tilo_action_policy::turn::Turn>) -> Self {
        self.turn = turn;
        self
    }

    /// The turn this actor acts for, when it is an agent's.
    #[must_use]
    pub fn turn(&self) -> Option<&tilo_action_policy::turn::Turn> {
        self.turn.as_ref()
    }

    /// The same actor, asking as part of an invocation admitted to reach
    /// `ceiling`, when it is one.
    #[must_use]
    pub fn within(mut self, ceiling: Option<tilo_action_policy::Reach>) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Whether an action of `kind`, privileged when `privileged`, reaches
    /// further than the invocation this actor's request continues was
    /// admitted for.
    fn exceeds(&self, kind: ActionKind, privileged: bool) -> bool {
        self.ceiling
            .is_some_and(|ceiling| !ceiling.covers(kind, privileged))
    }

    /// The same agent, working in a run of the issue at `issue`.
    #[must_use]
    pub fn working_on(mut self, issue: impl Into<String>) -> Self {
        self.issue = Some(issue.into());
        self
    }

    /// The issue whose run this actor works in, by its address.
    #[must_use]
    pub fn issue(&self) -> Option<&str> {
        self.issue.as_deref()
    }

    /// The policy principal id (`agent:`-prefixed for an agent).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.is_agent
    }

    /// The person this agent acts for, when one delegated it.
    #[must_use]
    pub fn acting_for(&self) -> Option<&str> {
        self.for_person.as_deref()
    }

    /// The grant subject this actor is when it is not a person.
    pub fn delegate(&self) -> Option<Subject> {
        if let Some(app) = self.id.strip_prefix("app:") {
            return Some(Subject::App(app.to_owned()));
        }
        // A person's policy id is lowercase hex, so it never starts with
        // `bot:` and the prefix cannot misread a person.
        if let Some(bot) = self.id.strip_prefix("bot:") {
            return Some(Subject::Bot(bot.to_owned()));
        }
        self.is_agent
            .then(|| Subject::agent(self.id.strip_prefix("agent:").unwrap_or(&self.id)))
    }

    /// What this actor may do in `workspace`: a person's own authority (held
    /// to `foreign_bound` from a device not theirs); an agent's bounded by
    /// the person it acts for, and by its own grant
    /// ([`roles::agent_turn`]); an application, or an agent nobody acts for,
    /// only what the workspace grants it.
    pub fn with_authority_in<T>(
        &self,
        workspace: &Workspace,
        foreign_bound: &[Grant],
        decide: impl FnOnce(&Authority<'_>) -> T,
    ) -> T {
        match (self.delegate(), &self.for_person) {
            (None, _) => {
                decide(&workspace.person_authority_on(&self.id, self.capped, foreign_bound))
            }
            (Some(delegate), Some(person)) => {
                let carried = self.session_grants(&workspace.id().to_string(), person);
                decide(
                    &workspace
                        .person_authority_on(person, self.capped, foreign_bound)
                        .bounded_by(self.party(&workspace.policy.grants, delegate, &carried)),
                )
            }
            (Some(delegate), None) => decide(&workspace.subject_authority(delegate)),
        }
    }

    /// The delegate `delegate` this actor is, over `grants` and what it
    /// `carried`, and the issue it works on.
    pub fn party<'g>(
        &self,
        grants: &'g [Grant],
        delegate: Subject,
        carried: &'g [Grant],
    ) -> tilo_action_policy::Party<'g> {
        let party = tilo_action_policy::Party::new(vec![delegate], grants).with_grants(carried);
        match &self.issue {
            Some(issue) => party.also_as(Subject::Task(issue.clone())),
            None => party,
        }
    }

    /// The authority an agent this connection acts as carries in `workspace`,
    /// given by `person`: the turn's, from its record's bound and the cap
    /// [`roles::agent_policy::ROLE_CAP`]. Nothing for any other actor, and
    /// nothing for an agent no person's turn carries.
    pub fn session_grants(&self, workspace: &str, person: &str) -> Vec<Grant> {
        // Only a session a person's device handed to its agent carries the
        // turn's grants; an agent never acts on its own.
        if !self.is_agent || !self.session_delegated || self.for_person.is_none() {
            return Vec::new();
        }
        let agent = self.id.strip_prefix("agent:").unwrap_or(&self.id);
        roles::agent_turn(workspace, agent, person, &[]).grants
    }

    /// Whether this actor is a person acting from their own device: the one
    /// whose own gesture answers a grant that asks first. What an effect asks
    /// for while it continues their invocation is not their gesture.
    pub fn direct(&self) -> bool {
        self.delegate().is_none() && !self.capped && self.ceiling.is_none()
    }

    /// The person this actor is, when a person acts from their own device:
    /// the only actor a host lets govern a workspace.
    #[must_use]
    pub fn direct_person(&self) -> Option<&str> {
        self.direct().then_some(self.id.as_str())
    }

    /// The answer for an action of `kind` at `node` in `workspace` at `now`.
    pub fn answer_in(
        &self,
        workspace: &Workspace,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> Answer {
        self.verdict_in(workspace, node, kind, false, now).0
    }

    /// The answer for the action `action` on `resource` (of kind
    /// `resource_kind`) the app `app` publishes, which `need` describes, in
    /// `workspace` at `now`: decided at its node, and as privileged when
    /// `need` is ([`tilo_action_policy::Verdict::privileged`]).
    pub fn action_answer_in(
        &self,
        workspace: &Workspace,
        app: &str,
        (resource_kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
        now: u64,
    ) -> Answer {
        self.action_verdict_in(workspace, app, (resource_kind, resource, action), need, now)
            .0
    }

    /// [`Self::action_answer_in`] for running the action: the answer, and the
    /// `Once` grants running it spends ([`tilo_action_policy::Verdict::spent`]).
    pub fn action_verdict_in(
        &self,
        workspace: &Workspace,
        app: &str,
        (resource_kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
        now: u64,
    ) -> (Answer, Vec<tilo_action_policy::GrantId>) {
        let node = tilo_actions::action_node(
            &workspace.node(),
            app,
            resource_kind,
            resource,
            need.field.as_deref(),
            action,
        );
        self.verdict_in(workspace, &node, need.kind, need.privileged, now)
    }

    /// Admit `action`, published on `surface`, in `workspace` at `now`:
    /// whether the workspace's grants ask first (`true`), when the invocation
    /// then needs the confirmation of the person it is made for, and the
    /// `Once` grants admitting it spends. Decided on one sample: the action
    /// where it is published, by the run's agent working on its issue when
    /// the call is a run's (`run`); and, for a person, a change of the
    /// route record it acts on, so a person restricted on one record cannot
    /// act on it through an action they may otherwise use.
    pub fn admit_published_in(
        &self,
        workspace: &Workspace,
        now: u64,
        run: Option<RunFacts<'_>>,
        (surface, action): (&PublishedSurface, &PublishedAction),
    ) -> Result<(bool, Vec<tilo_action_policy::GrantId>), String> {
        use tilo_action_policy::Refused;
        let refused = || "caller no longer authorized for invocation".to_owned();
        let record = if self.is_agent {
            None
        } else {
            Some(canonical_record(&surface.route.record_id)?)
        };
        let need = action.contract.need();
        let node = published_node(workspace, &surface.app_id, action);
        // The run's agent alone acts in it, as the agent working on the issue.
        let working;
        let actor = match run {
            Some(run)
                if self.is_agent
                    && self.id.strip_prefix("agent:").unwrap_or(&self.id) == run.agent =>
            {
                working = self.clone().working_on(run.issue);
                &working
            }
            Some(_) => return Err("only the run's agent acts in it".into()),
            None => self,
        };
        let (answer, spent) = actor.verdict_in(workspace, &node, need.kind, need.privileged, now);
        let record = record.map(|record| {
            let node = collab_types::validator::document_node(
                &workspace.policy.workspace,
                &surface.app_id,
                &record,
            );
            self.answer_in(workspace, &node, need.kind, now)
        });
        // The broker reaches the person on their own device to ask them.
        let asks = match tilo_action_policy::admit(
            tilo_action_policy::Invoked {
                answer,
                attended: action.contract.is_attended(),
                record,
            },
            tilo_action_policy::Asker {
                reachable: true,
                delegated: self.delegate().is_some(),
            },
        ) {
            Ok(admission) => admission == tilo_action_policy::Admission::AsksFirst,
            Err(Refused::Action | Refused::Record) => return Err(refused()),
        };
        Ok((asks, spent))
    }

    fn verdict_in(
        &self,
        workspace: &Workspace,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        now: u64,
    ) -> (Answer, Vec<tilo_action_policy::GrantId>) {
        let bound = roles::foreign_device_bound();
        self.with_authority_in(workspace, &bound, |authority| {
            let mut verdict = authority.decide(node, kind, &tilo_desktop_model::context_at(now));
            if privileged {
                verdict = verdict.privileged();
            }
            // A child going further than the invocation it continues was
            // admitted for is asked for as its own request.
            let answer = match verdict.answer {
                Answer::Allow if self.exceeds(kind, privileged) => Answer::Ask,
                answer => answer,
            };
            (answer, verdict.spent())
        })
    }

    /// Whether this actor may take an action of `kind` at `node` in
    /// `workspace` at `now` ([`Answer::admits`]).
    pub fn may_in(
        &self,
        workspace: &Workspace,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> bool {
        self.answer_in(workspace, node, kind, now)
            .admits(self.direct())
    }

    /// Whether this actor takes part in `workspace` at `now`: it may at
    /// least read the workspace.
    pub fn reads_in(&self, workspace: &Workspace, now: u64) -> bool {
        self.answer_in(workspace, &workspace.node(), ActionKind::Read, now) != Answer::Deny
    }
}
