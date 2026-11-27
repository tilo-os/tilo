//! Who takes an action, and what the workspace decides for them.
//!
//! Every action is admitted as an actor: a person from their own device or a
//! foreign one, an agent acting for a person, an application or a bot under
//! its own identity. The actor's grants, its delegation and the turn it acts
//! for decide one invocation, and every host decides it the same way — the
//! Linux broker over its app sockets and the page over the apps it runs.

use tilo_action_policy::{ActionKind, Answer, Authority, MemberAccess, TreePath};
use tilo_actions::remote::Caller;
use tilo_desktop_model::Workspace;
use tilo_member_types::AppId;
use tilo_member_types::{Acting, Actor, MemberId};

use crate::published::{PublishedAction, PublishedSurface, canonical_document, published_node};

/// The person a user root is, as the workspace's governance names them: the key in lowercase hex.
#[must_use]
pub fn person_id_for_root(user_public_key: [u8; 32]) -> String {
    hex::encode(user_public_key)
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BrokerActor {
    /// Who acts: a member, or an app acting by itself with only the grants that name it.
    actor: Actor,
    /// A person acting from a device the workspace does not admit.
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
    /// The bound the agent's own record narrows it to on its turn, as grants
    /// ([`Caller::granted`]): what every host adds to the delegation's preset.
    bound: Vec<tilo_action_policy::Grant>,
    /// The app the person acts through, when their session runs one: what it
    /// may reach is bounded by the kinds it declares.
    pub app: Option<AppId>,
    /// The resource kinds `app` declares, when the host holds its manifest.
    app_kinds: Option<Vec<String>>,
    /// The agent session the agent works in: the answers its person gave it decide for it as
    /// the agent's own grants do.
    session: Option<String>,
    /// How far the invocation this actor's request continues was admitted
    /// to reach: what goes further asks first
    /// ([`tilo_action_policy::Reach`]).
    pub ceiling: Option<tilo_action_policy::Reach>,
    /// The person on a plain connection to their own device's daemon, by peer credential: the
    /// connection whose own prompt (the CLI's) may confirm what the workspace asks them. Their
    /// call is never the answer.
    own_connection: bool,
}

impl BrokerActor {
    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<tilo_member_types::PersonId>) -> Self {
        Self::of_actor(MemberId::person(id).into())
    }

    /// The agent `id` of `person`.
    #[track_caller]
    #[must_use]
    pub fn agent(id: impl Into<String>, person: impl Into<tilo_member_types::PersonId>) -> Self {
        Self::of_actor(MemberId::agent(id, person).into())
    }

    /// App `app` acting by itself: no person is behind it, so it holds only the grants that
    /// name it, and history names it as the author of what it changes.
    #[must_use]
    pub fn app_itself(app: impl Into<AppId>) -> Self {
        let app = app.into();
        Self::of_actor(Actor::App(app.clone())).through_app(app)
    }

    fn of_actor(actor: Actor) -> Self {
        Self {
            actor,
            capped: false,
            for_person: None,
            session_delegated: false,
            turn: None,
            bound: Vec::new(),
            app: None,
            app_kinds: None,
            session: None,
            ceiling: None,
            own_connection: false,
        }
    }

    /// Installed bot `installation`, acting for the person who installed it:
    /// it may do only what both its grant and that person may.
    #[must_use]
    pub fn bot(installation: &str, installer: impl Into<String>) -> Self {
        Self::of_actor(MemberId::bot(installation).into()).for_person(installer)
    }

    /// The actor `caller` is, as its transport admitted it: the one admission
    /// every host starts from.
    #[must_use]
    pub fn for_caller(caller: &Caller) -> Self {
        let mut actor = Self::actor_of(caller)
            .on_turn(caller.turn().cloned())
            .within(caller.ceiling());
        actor.bound = caller.granted();
        actor.own_connection = caller.is_own_connection();
        actor
    }

    /// The actor `caller` is, before any ceiling.
    fn actor_of(caller: &Caller) -> Self {
        let acting = caller.acting();
        if let Some(agent) = acting.member_id().filter(|member| member.is_agent()) {
            let agent = Self::of_actor(agent.clone().into());
            // A person's own device handed this connection to the agent
            // (`Caller::delegated_to`); an agent member the host admitted
            // acts for the person who admitted it, with no more than the
            // workspace grants it (`Caller::acting_for`); a delegated agent
            // device acts for the person its certificate chains to.
            let agent = match acting.acted_for_person() {
                Some(person) if caller.is_session_delegated() => {
                    agent.for_person(person).session_delegated()
                }
                Some(person) => agent.for_person(person),
                None => agent,
            };
            // An agent session's changes are caused by the session.
            return match &acting.agent_session {
                Some(session) => agent.in_session(session.as_str()),
                None => agent,
            };
        }
        if let Some(app) = acting.app() {
            return match acting.person_id() {
                Some(person) => Self::person(person).through_app(app.clone()),
                // The app itself only when it is both who acts and what is acted through, in
                // the process this device's daemon launched as it: a visitor, bot or
                // automation acting through an app, another app naming it, or a caller that
                // only names it, borrows none of its grants and acts for nobody.
                None if matches!(&acting.by, Actor::App(by) if by == app)
                    && caller.launched_app() == Some(app) =>
                {
                    Self::app_itself(app.clone())
                }
                None => Self::person(String::new()).through_app(app.clone()),
            };
        }
        if let Some(bot) = acting.bot_id() {
            return match acting.acted_for_person() {
                Some(installer) => Self::bot(bot, installer),
                // A bot nobody installed has no authority to borrow: acting
                // for nobody, it gets nothing.
                None => Self::bot(bot, String::new()),
            };
        }
        Self::person(
            acting
                .person_id()
                .or_else(|| acting.acted_for_person())
                .unwrap_or_default(),
        )
    }

    /// How this actor shows where it acts when no caller says more: its kind, and for an agent
    /// the person it acts for.
    #[must_use]
    pub fn shown(&self) -> Acting {
        let shown = Acting::from(self.actor.clone());
        match &self.for_person {
            Some(person) if self.is_agent() => shown.acting_for(Acting::person(person.clone())),
            _ => shown,
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

    /// The same agent, working in the agent session `session`.
    #[must_use]
    pub fn in_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// The agent session this actor works in.
    #[must_use]
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// The member who acts, when a member does: never an app acting by itself.
    #[must_use]
    pub fn member_id(&self) -> Option<&MemberId> {
        self.actor.member_id()
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.member_id().is_some_and(MemberId::is_agent)
    }

    fn is_person(&self) -> bool {
        self.member_id().is_some_and(MemberId::is_person)
    }

    /// The app the person acts through, when their session runs one.
    #[must_use]
    pub fn app(&self) -> Option<&AppId> {
        self.app.as_ref()
    }

    /// Whether this is an app: acting through the person whose session runs it, or by itself.
    #[must_use]
    pub fn is_app(&self) -> bool {
        self.app.is_some()
    }

    /// The same actor, acting through `app`, which declares the resource
    /// `kinds` it may reach in its own node.
    #[must_use]
    pub fn in_app(
        mut self,
        app: impl Into<AppId>,
        kinds: impl IntoIterator<Item = String>,
    ) -> Self {
        self.app = Some(app.into());
        self.app_kinds = Some(kinds.into_iter().collect());
        self
    }

    /// The same actor, acting through `app`, whose declared kinds this host
    /// does not hold.
    #[must_use]
    pub fn through_app(mut self, app: impl Into<AppId>) -> Self {
        self.app = Some(app.into());
        self
    }

    /// The person this agent acts for, when one delegated it.
    #[must_use]
    pub fn acting_for(&self) -> Option<&str> {
        self.for_person.as_deref()
    }

    /// Who this actor is when it is not a person.
    #[must_use]
    pub fn delegate(&self) -> Option<tilo_member_types::Actor> {
        (!self.is_person()).then(|| self.actor.clone())
    }

    /// Who this actor is: the author history names for what it changes.
    #[must_use]
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Who this actor's actions are: a person, an agent or bot acting for the
    /// person who delegated it, an app acting as the person whose session
    /// runs it.
    #[must_use]
    pub fn acting(&self) -> Acting {
        let acting = match (self.delegate(), &self.for_person) {
            (None, Some(person)) => Acting::person(person.clone()),
            (None, None) => Acting::from(self.actor.clone()),
            (Some(tilo_member_types::Actor::Member(member)), Some(person)) => {
                let delegate = Acting::member(member);
                // The delegation a person's own device gives the agent it
                // handed its session to is the acting's own.
                let delegate = if self.session_delegated {
                    delegate.handed()
                } else {
                    delegate
                };
                delegate.acting_for(Acting::person(person.clone()))
            }
            (Some(tilo_member_types::Actor::Member(member)), None) => Acting::member(member),
            (
                Some(
                    actor @ (tilo_member_types::Actor::Automation(_)
                    | tilo_member_types::Actor::App(_)
                    | tilo_member_types::Actor::Visitor(_)),
                ),
                _,
            ) => Acting::from(actor),
        };
        let acting = match &self.app {
            Some(app) => acting.in_app(app.clone()),
            None => acting,
        };
        let acting = acting.on_foreign_device(self.capped);
        match &self.session {
            Some(session) => acting
                .in_session(session.clone())
                .caused_by(session.clone()),
            None => acting,
        }
    }

    /// Who this actor's invocation in `workspace` is decided for.
    #[must_use]
    pub fn access_in(&self, workspace: &Workspace) -> MemberAccess {
        let mut access = MemberAccess::new(workspace.policy.clone(), self.acting())
            .granted(self.bound.iter().cloned());
        if let Some(kinds) = &self.app_kinds {
            access = access.in_app(kinds.clone());
        }
        if self.capped {
            access.on_foreign_device()
        } else {
            access
        }
    }

    /// What this actor may do in `workspace` ([`Self::access_in`]).
    pub fn with_authority_in<T>(
        &self,
        workspace: &Workspace,
        decide: impl FnOnce(&Authority<'_>) -> T,
    ) -> T {
        let access = self.access_in(workspace);
        decide(&access.authority())
    }

    /// Whether this actor is a person acting from their own device: the one
    /// whose own gesture answers a grant that asks first. What an effect asks
    /// for while it continues their invocation is not their gesture.
    pub fn direct(&self) -> bool {
        self.delegate().is_none() && !self.capped && self.ceiling.is_none()
    }

    /// Whether this is the person on their own device's daemon connection
    /// ([`Caller::own_person`]).
    #[must_use]
    pub const fn own_connection(&self) -> bool {
        self.own_connection
    }

    /// The same actor, as the person on their own device's daemon connection
    /// ([`Caller::own_person`]).
    #[must_use]
    pub const fn on_own_connection(mut self) -> Self {
        self.own_connection = true;
        self
    }

    /// The person this actor is, when a person acts from their own device:
    /// the only actor a host lets administer a workspace.
    #[must_use]
    pub fn direct_person(&self) -> Option<&str> {
        self.direct()
            .then(|| self.member_id().and_then(MemberId::person_id))
            .flatten()
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
            &workspace.policy.node(),
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
    /// where it is published; and, for a person, a change of the route record
    /// it acts on, so a person restricted on one record cannot act on it
    /// through an action they may otherwise use.
    pub fn admit_published_in(
        &self,
        workspace: &Workspace,
        now: u64,
        published: (&PublishedSurface, &PublishedAction),
    ) -> Result<(bool, Vec<tilo_action_policy::GrantId>), String> {
        self.admit_in(workspace, now, published, true)
    }

    /// [`Self::admit_published_in`] for an action of a record `workspace` holds while the
    /// window publishing it is open in another (`ResourceSpec::workspace`): decided where the
    /// record is held, as that workspace's grants say, never by the roles of the window's. The
    /// route record is the window's, so its own restrictions are not asked here.
    pub fn admit_kept_elsewhere_in(
        &self,
        workspace: &Workspace,
        now: u64,
        published: (&PublishedSurface, &PublishedAction),
    ) -> Result<(bool, Vec<tilo_action_policy::GrantId>), String> {
        self.admit_in(workspace, now, published, false)
    }

    fn admit_in(
        &self,
        workspace: &Workspace,
        now: u64,
        (surface, action): (&PublishedSurface, &PublishedAction),
        route_record: bool,
    ) -> Result<(bool, Vec<tilo_action_policy::GrantId>), String> {
        let refused = || "caller no longer authorized for invocation".to_owned();
        let record = if self.is_agent() || !route_record {
            None
        } else {
            Some(canonical_document(&surface.route.document_id)?)
        };
        let need = action.contract.need();
        let node = published_node(workspace, &surface.app_id, action);
        let actor = self;
        let record = record.map(|record| {
            collab_types::validator::document_node(
                &workspace.policy.workspace,
                &surface.app_id,
                &record,
            )
        });
        let access = actor.access_in(workspace);
        // The broker reaches the person on their own device to ask them.
        let (admission, spent) = tilo_action_policy::admit_invocation(
            &access.authority(),
            access.asker(true),
            tilo_action_policy::Invocation {
                node: &node,
                kind: need.kind,
                privileged: need.privileged,
                attended: action.contract.is_attended(),
                resource: record.as_ref(),
                ceiling: self.ceiling,
            },
            &tilo_action_policy::Context::at_secs(now),
        )
        .map_err(|_| refused())?;
        let asks = admission == tilo_action_policy::Admission::AsksFirst;
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
        self.with_authority_in(workspace, |authority| {
            tilo_action_policy::answer_at(
                authority,
                (node, kind, privileged),
                self.ceiling,
                &tilo_action_policy::Context::at_secs(now),
            )
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
        self.answer_in(workspace, &workspace.policy.node(), ActionKind::Read, now) != Answer::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::BrokerActor;

    /// An app on a connection with nobody behind it is admitted as itself: what it does is
    /// decided by the grants that name it and recorded as the app's, never as a person's.
    #[test]
    fn an_app_with_nobody_behind_it_acts_as_itself() {
        let caller = tilo_actions::remote::Caller::launched("so.tilo.discord").in_workspace("w");
        let actor = BrokerActor::for_caller(&caller);
        let app = tilo_member_types::Actor::App("so.tilo.discord".into());
        assert_eq!(actor.actor(), &app);
        assert_eq!(actor.acting().by, app);
        assert_eq!(actor.shown().by, app);
        assert_eq!(actor.direct_person(), None, "an app decides nothing");

        let for_ada = BrokerActor::for_caller(
            &tilo_actions::remote::Caller::local_app("so.tilo.discord").acting_for("ada"),
        );
        assert_eq!(
            for_ada.actor(),
            &tilo_member_types::Actor::Member(tilo_member_types::MemberId::person("ada")),
            "an app its person runs acts as them"
        );
    }

    /// Only the app itself acts as the app: a visitor, a bot or another app acting through it
    /// borrows none of its grants.
    #[test]
    fn only_the_app_itself_acts_as_the_app() {
        use tilo_actions::remote::Caller;
        let discord = tilo_member_types::Actor::App("so.tilo.discord".into());
        for (who, caller) in [
            ("a visitor", Caller::visitor([7; 32])),
            ("a bot", Caller::local_bot("b1")),
            ("another app", Caller::launched("so.tilo.other")),
            (
                "a caller only naming it",
                Caller::local_app("so.tilo.discord"),
            ),
            (
                "a caller kept and restored",
                Caller::launched("so.tilo.discord").kept().restored(),
            ),
        ] {
            let actor = BrokerActor::for_caller(&caller.through_app("so.tilo.discord"));
            assert_ne!(
                actor.actor(),
                &discord,
                "{who} through an app is not the app"
            );
            assert_eq!(
                actor.actor(),
                &tilo_member_types::Actor::Member(tilo_member_types::MemberId::person("")),
                "{who} through an app acts for nobody"
            );
        }
    }

    /// A change an agent session makes records the session beside the agent and the person it
    /// acts for.
    #[test]
    fn an_agent_sessions_change_names_the_session() {
        let session = format!("{}s1", crate::services::AGENT_SESSION_PREFIX);
        let acting = BrokerActor::agent("a1", "p1")
            .for_person("p1")
            .in_session(session.clone())
            .acting();
        assert_eq!(acting.agent_id(), Some("a1"));
        assert_eq!(
            acting
                .agent_session
                .as_ref()
                .map(tilo_member_types::AgentSessionId::as_str),
            Some(session.as_str())
        );
    }
}
