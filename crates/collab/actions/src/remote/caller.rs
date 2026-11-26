//! Who sends a request: the acting tree a transport authenticated, and what
//! it admitted them to.

use tilo_action_policy::Acting;

use super::{ExecutionLease, within};

/// The agent a person's own computer runs under the name `name`: one agent
/// on every computer of theirs and in every workspace, whichever session it
/// comes in on. A workspace's record of its agents keeps it under this id
/// (`agent:<id>`), so its name, colour, permissions and history follow it from
/// session to session; another name is another agent. The name is compared
/// without case or surrounding space.
#[must_use]
pub fn device_agent_id(person: &[u8; 32], name: &str) -> String {
    let name = name.trim().to_lowercase();
    let mut stem: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while stem.contains("--") {
        stem = stem.replace("--", "-");
    }
    let stem: String = stem.trim_matches('-').chars().take(40).collect();
    let stem = if stem.is_empty() {
        "agent".to_owned()
    } else {
        stem
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo device agent\0");
    hasher.update(person);
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    let tag = hex::encode(hash.as_bytes().get(..5).unwrap_or_default());
    format!("{stem}-{tag}")
}

/// The authenticated actor behind a request.
///
/// Built only by the transport that authenticated the connection — from the
/// certificate chain a peer presented, or from the admitted identity of a
/// local connection. It is deliberately not `Deserialize`: a payload cannot
/// assert who sent it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Caller {
    pub(super) acting: Acting,
    pub(super) user_public_key: Option<[u8; 32]>,
    pub(super) device_public_key: Option<[u8; 32]>,
    pub(super) workspace: Option<String>,
    // Transport-issued execution scope, never accepted from request context.
    // It narrows invocation ownership without changing the acting tree.
    pub(super) execution: Option<ExecutionLease>,
    // The person's own device handed this connection to the agent
    // (`Self::delegated_to`), rather than the agent being admitted as a member
    // in its own right and acting for them (`Self::acting_for`).
    pub(super) session_delegated: bool,
    // How far the invocation this caller's requests continue was admitted
    // to reach, set by the provider that matched them to it.
    pub(super) within: within::Within,
    // The turn an agent acts inside: the person's message in a conversation
    // that caused it (`Self::agent_turn`). Every op carries it, so the
    // receiving device checks it against that message and presence can show
    // what the agent would click.
    pub(super) turn: Option<tilo_action_policy::turn::Turn>,
    roster_created_at: Option<i64>,
    // The roster agent whose approval this caller works under, where that is another agent
    // than the one acting: a sub-agent its starter started (`Self::working_under`).
    approved_as: Option<(String, i64)>,
    // The person whose message the server's turn answers, as the server read
    // its cause (`Self::on_roster_turn`).
    turn_person: Option<[u8; 32]>,
    // The name an agent goes by, from the key its person certified for it
    // (`Self::named`): presentation only, what History names it by.
    pub(super) agent_name: Option<String>,
    // What the person asked the agent to do, in a line (`Self::asked`): what
    // its session is labelled by, where it was asked.
    pub(super) asked: Option<String>,
    // What the agent's own record bounds it to on its turn (`Self::on_turn`), narrowing the
    // grants a delegated agent acts with; empty follows the default.
    pub(super) bound: Vec<tilo_action_policy::Rule>,
    // What the agent's own record bounds an agent acting with its own key to, read by the
    // provider that runs it on its person's device (`Self::held_to`). `None` there holds it to
    // nothing: it carries no delegation at all.
    own_bound: Option<Vec<tilo_action_policy::Rule>>,
}

impl Caller {
    /// `acting`, as the provider admitted it (`keys`: the user and device keys a transport
    /// verified, if any), with nothing more said of it yet.
    fn admitted(acting: Acting, keys: Option<([u8; 32], [u8; 32])>) -> Self {
        Self {
            acting,
            user_public_key: keys.map(|(user, _)| user),
            device_public_key: keys.map(|(_, device)| device),
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            roster_created_at: None,
            approved_as: None,
            turn_person: None,
            agent_name: None,
            asked: None,
            bound: Vec::new(),
            own_bound: None,
        }
    }

    /// A remote device whose certificate the transport verified to chain to
    /// `user_public_key`.
    #[must_use]
    pub fn authenticated(user_public_key: [u8; 32], device_public_key: [u8; 32]) -> Self {
        Self::admitted(
            Acting::person(hex::encode(user_public_key)),
            Some((user_public_key, device_public_key)),
        )
    }

    /// A visitor: a key no certificate in the workspace names, known by the key its
    /// connection proved alone. A page at a workspace's public entry is one, and so is a
    /// computer being added to a person's devices while it asks to be certified. It acts for
    /// nobody, so it holds only what the workspace opens to any key
    /// ([`tilo_action_policy::Grantee::AnyKey`]).
    #[must_use]
    pub fn visitor(device_public_key: [u8; 32]) -> Self {
        let mut caller = Self::admitted(Acting::visitor(&device_public_key), None);
        caller.device_public_key = Some(device_public_key);
        caller
    }

    /// A person on a connection the provider admitted itself, by the id that
    /// admission established.
    #[must_use]
    pub fn local_person(person: impl Into<tilo_action_policy::PersonId>) -> Self {
        Self::admitted(Acting::person(person.into()), None)
    }

    /// A delegated agent on a connection the provider admitted itself.
    #[must_use]
    pub fn local_agent(agent_id: impl Into<String>) -> Self {
        Self::admitted(Acting::agent(agent_id.into()), None)
    }

    /// An installed bot on a connection the provider admitted itself, by its
    /// installation: software, never a person, whatever its id spells.
    #[must_use]
    pub fn local_bot(installation: impl Into<String>) -> Self {
        Self::admitted(Acting::bot(installation.into()), None)
    }

    /// An application on a connection the provider admitted itself, by the
    /// package id that admission established. It acts as the person whose
    /// session runs it ([`Self::acting_for`]), in that app; with nobody
    /// behind it, it holds nothing.
    #[must_use]
    pub fn local_app(app_id: impl Into<String>) -> Self {
        Self::admitted(Acting::person("").in_app(app_id.into()), None)
    }

    /// A remote agent device whose certificate the transport verified to
    /// chain to `user_public_key` and whose root-signed delegation names it
    /// agent `agent_id`. It is its own member, never the person who
    /// delegated it.
    #[must_use]
    pub fn delegated_agent(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self::admitted(
            Acting::agent(agent_id.into()).acting_for(Acting::person(hex::encode(user_public_key))),
            Some((user_public_key, device_public_key)),
        )
    }

    /// An agent acting inside a turn: `person`'s message in `turn`'s
    /// conversation caused it, and the agent takes part there. It may do
    /// exactly what that person may, held to the editor preset and to the
    /// bound its own record carries, and nothing outside a turn. Every op it
    /// makes carries the turn, so the receiving device checks the cause it
    /// arrived with. Set only by the provider that admitted the turn.
    #[must_use]
    pub fn agent_turn(
        agent_id: impl Into<String>,
        person: impl Into<tilo_action_policy::PersonId>,
        turn: tilo_action_policy::turn::Turn,
    ) -> Self {
        let acting = Acting::agent(agent_id.into()).acting_for(Acting::person(person.into()));
        Self {
            session_delegated: true,
            turn: Some(turn),
            ..Self::admitted(acting, None)
        }
    }

    /// A bot device whose certificate chains to `user_public_key` (its
    /// installer) and whose root-signed delegation names it installation
    /// `installation`. It acts for its installer and is never that person.
    #[must_use]
    pub fn bot(
        installation: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self::admitted(
            Acting::bot(installation.into())
                .acting_for(Acting::person(hex::encode(user_public_key))),
            Some((user_public_key, device_public_key)),
        )
    }

    /// Give an already admitted connection its own execution scope. The provider
    /// mints the identifier; no payload, display name or history run selects it.
    /// This changes invocation ownership, never the caller's permissions.
    #[must_use]
    pub fn in_new_execution(mut self) -> Self {
        self.execution = Some(ExecutionLease::mint());
        self
    }

    /// Retain a provider-issued execution when projecting the same transport into
    /// another verified action API. A lease conveys ownership and lifetime,
    /// never certificate ownership or additional permissions.
    #[must_use]
    pub fn in_execution(mut self, execution: ExecutionLease) -> Self {
        self.execution = Some(execution);
        self
    }

    /// Provider-issued execution identity. An identifier alone conveys no authority.
    #[must_use]
    pub fn execution_id(&self) -> Option<uuid::Uuid> {
        self.execution.as_ref().map(|lease| lease.id)
    }

    #[must_use]
    pub fn execution_lease(&self) -> Option<ExecutionLease> {
        self.execution.clone()
    }

    #[must_use]
    pub fn execution_is_live(&self) -> bool {
        self.execution.as_ref().is_some_and(ExecutionLease::is_live)
    }

    /// Whether this caller may follow, ask again or cancel an invocation `owner` started: it is
    /// `owner`, or `owner`'s connection ended and this is the same caller on a connection admitted
    /// after that. A device or key whose connection dropped in the middle of a long import (one does,
    /// now and then) keeps the invocations it started; a second live connection of the same device still
    /// borrows none of the first's.
    #[must_use]
    pub fn continues(&self, owner: &Self) -> bool {
        // The name an agent goes by is presentation only ([`Self::named`]): a client that
        // names its session names the calls it makes, and the caller following them names
        // nothing, so ownership reads the callers without their names. The name is display
        // only and grants nothing either way.
        let (mine, theirs) = (self.unnamed(), owner.unnamed());
        if mine == theirs {
            return true;
        }
        let replaced = match (&self.execution, &owner.execution) {
            (Some(own), Some(ended)) => own.replaced(ended),
            _ => false,
        };
        replaced && {
            let mut same = mine;
            same.execution.clone_from(&owner.execution);
            same == theirs
        }
    }

    /// This caller without the name it goes by, which is presentation only
    /// ([`Self::named`]): what ownership compares.
    #[must_use]
    fn unnamed(&self) -> Self {
        let mut bare = self.clone();
        bare.agent_name = None;
        bare
    }

    /// A transport ends its own run; retained caller clones cannot reopen it.
    pub fn retire_execution(&self) {
        if let Some(lease) = &self.execution {
            lease.close();
        }
    }

    /// Mark whether the caller acts from a foreign device: a Tilo runtime on
    /// an operating system Tilo does not control, as its certificate says.
    /// Workspaces it does not govern cap such a caller's role.
    #[must_use]
    pub fn from_foreign_device(mut self, foreign: bool) -> Self {
        self.acting = self.acting.on_foreign_device(foreign);
        self
    }

    /// The caller acts for `person`: an agent or an app the person started.
    /// Set only by the provider that admitted the delegation, never from a
    /// payload. It names who the actor works for in presence and history; it
    /// widens nothing.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<tilo_action_policy::PersonId>) -> Self {
        let person = person.into();
        self.acting = if let Some(app) = self.acting.app.clone() {
            Acting::person(person).in_app(app)
        } else {
            let mut acting = self.acting;
            acting.acted_for = vec![Acting::person(person)];
            acting
        };
        self
    }

    /// This person's own device hands the connection to agent `agent_id`,
    /// acting for them: an MCP client, a local assistant. The agent is the
    /// actor (its edits, presence and history are the agent's, for the
    /// person) and it may do only what both the person and the delegation a
    /// device gives its agents allow. A caller that is not a person is
    /// returned as it is. Set only by the provider that admitted the person.
    #[must_use]
    pub fn delegated_to(mut self, agent_id: impl Into<String>) -> Self {
        let Some(person) = self.acting.person_id().map(str::to_owned) else {
            return self;
        };
        self.session_delegated = true;
        self.acting = Acting::agent(agent_id)
            .handed()
            .acting_for(Acting::person(person));
        self
    }

    /// An agent caller named `name`, as the certificate the transport
    /// verified for its key says (`device_label`): what History names its
    /// changes by. Names nothing for a caller that is not an agent.
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        let name = name.trim();
        if self.acting.is_agent() && !name.is_empty() {
            self.agent_name = Some(name.to_owned());
        }
        self
    }

    /// An agent caller as `message` says the session it works in is called: by that name
    /// ([`super::InvokeRequest::said_session_name`]), where its client named one, and by the name it
    /// went by else. One name for everything the agent does here: the window it opens, the
    /// message it writes, its History rows and the questions it puts to its person. Display
    /// only, like the session's name itself; it grants nothing and changes no member id.
    #[must_use]
    pub fn in_named_session(self, message: &super::RemoteActionMessage) -> Self {
        let super::RemoteActionMessage::Invoke(request) = message else {
            return self;
        };
        self.in_session_of(request)
    }

    /// [`Self::in_named_session`] for the call `request`.
    #[must_use]
    pub fn in_session_of(self, request: &super::InvokeRequest) -> Self {
        match request.said_session_name() {
            Some(session) => self.named(&session),
            None => self,
        }
    }

    /// An agent caller working in the agent session `session`: what its
    /// changes are grouped and taken back by. Set only by the provider that
    /// admitted the session; names nothing for a caller that is not an agent.
    #[must_use]
    pub fn in_agent_session(mut self, session: impl Into<String>) -> Self {
        if self.acting.is_agent() {
            self.acting = self.acting.in_session(session);
        }
        self
    }

    /// An agent caller working on `request`, the words of the person that
    /// started its session (their message, what they sent it): its first line,
    /// trimmed and bounded, labels the session ([`Self::agent_session`]). A
    /// blank request leaves the session labelled by the agent's name.
    #[must_use]
    pub fn asked(mut self, request: &str) -> Self {
        self.asked = crate::context::one_line(request, crate::context::MAX_LABEL_CHARS);
        self
    }

    /// What an agent caller is called: its name ([`Self::named`]), else its id.
    #[must_use]
    pub fn agent_called(&self) -> Option<&str> {
        self.agent_name().or_else(|| self.acting.agent_id())
    }

    /// The name an agent caller goes by ([`Self::named`]).
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    /// The agent this person's own device handed its session to ([`Self::delegated_to`]) acts
    /// inside `turn`, held to `bound`, the rules its own record narrows it to: the provider that runs
    /// the agent for the person and admitted the turn's cause sets it, never a payload. Changes
    /// nothing for a caller that is not such an agent.
    #[must_use]
    pub fn on_turn(
        mut self,
        turn: tilo_action_policy::turn::Turn,
        bound: Vec<tilo_action_policy::Rule>,
    ) -> Self {
        if self.session_delegated && self.acting.is_agent() {
            self.turn = Some(turn);
            self.bound = bound;
        }
        self
    }

    /// Bind a certified agent caller to the hosted roster entry it was
    /// approved as, answering `person`: the author of the message whose turn
    /// it answers, as its provider read it. The agent's own key still signs:
    /// every call is decided for `person`, bounded by the agent and by the
    /// person it belongs to ([`Acting::agent_of`],
    /// [`tilo_action_policy::Acting::signed_by`]), so the turn does only what
    /// both the agent and `person` may, whoever the agent belongs to. The
    /// workspace service must verify this binding.
    #[must_use]
    pub fn on_roster(mut self, created_at: i64, person: [u8; 32]) -> Self {
        if !self.session_delegated && self.acting.is_agent() && self.device_public_key.is_some() {
            self.roster_created_at = Some(created_at);
            self.turn_person = Some(person);
            self.acting.acted_for = vec![Acting::person(hex::encode(person))];
            if let Some(owner) = self.user_public_key.map(hex::encode) {
                self.acting = self.acting.clone().agent_of(owner);
            }
        }
        self
    }

    /// This agent caller as the agent of `owner`, the person it belongs to, answering whoever
    /// it acts for ([`Acting::agent_of`]): where that is someone else, `owner` bounds it too.
    /// Set only by the provider that runs the agent, from the agent's own record. Changes
    /// nothing for a caller that is not an agent.
    #[must_use]
    pub fn agent_of(mut self, owner: impl Into<tilo_action_policy::PersonId>) -> Self {
        if self.acting.is_agent() {
            self.acting = self.acting.agent_of(owner);
        }
        self
    }

    /// A caller bound to its roster entry ([`Self::on_roster`]), inside `turn`. Changes nothing
    /// for a caller that is not so bound.
    #[must_use]
    pub fn on_roster_turn(mut self, turn: tilo_action_policy::turn::Turn) -> Self {
        if self.roster_created_at.is_some() {
            self.turn = Some(turn);
        }
        self
    }

    /// The agent `agent_id`, named `name`, working under this roster agent's approval: a
    /// sub-agent its starter started. It acts as itself, so what it writes and says is its
    /// own, for the same person, as one more actor in its starter's chain
    /// ([`Acting::handed_on`]): the starter, the person its turn answers and the person the
    /// starter belongs to all bound it. Every change it makes is signed by the starter's key,
    /// whose approval bounds that tree too ([`Acting::signed_by`]). It holds no approval of
    /// its own. Changes nothing for a caller not bound to its roster entry
    /// ([`Self::on_roster`]).
    #[must_use]
    pub fn working_under(
        mut self,
        agent_id: impl Into<String>,
        name: &str,
        created_at: i64,
    ) -> Self {
        let Some(approved) = self.approved_agent().map(str::to_owned) else {
            return self;
        };
        if self.acting.acted_for_person().is_none() {
            return self;
        }
        self.acting = self.acting.handed_on(agent_id);
        self.approved_as = Some((approved, created_at));
        self.agent_name = None;
        self.named(name)
    }

    /// The roster agent whose approval a caller bound to its roster entry acts under
    /// ([`Self::on_roster`]): the acting agent itself, or the starter of a sub-agent
    /// ([`Self::working_under`]). `None` for any other caller.
    #[must_use]
    pub fn approved_agent(&self) -> Option<&str> {
        self.roster_created_at?;
        self.approved_as
            .as_ref()
            .map(|(signer, _)| signer.as_str())
            .or_else(|| self.acting.agent_id())
    }

    /// The incarnation of the child that acts under the approved agent, when there is one.
    #[must_use]
    pub fn child_created_at(&self) -> Option<i64> {
        self.approved_as.as_ref().map(|(_, created_at)| *created_at)
    }

    /// The person a roster agent works for ([`Self::on_roster`]).
    #[must_use]
    pub const fn turn_person(&self) -> Option<&[u8; 32]> {
        self.turn_person.as_ref()
    }

    /// The creation time of the roster entry the provider bound this turn to.
    #[must_use]
    pub const fn roster_created_at(&self) -> Option<i64> {
        self.roster_created_at
    }

    /// The person whose own device handed this connection to its agent
    /// ([`Self::delegated_to`]), as that device is admitted: the person the
    /// agent acts for, on the same device, whose current access bounds
    /// everything the agent may do. `None` for any other caller. Use it to
    /// read the person's access, never to act as them.
    #[must_use]
    pub fn delegating_person(&self) -> Option<Self> {
        if !self.acting.is_agent() {
            return None;
        }
        let person = self
            .acting
            .acted_for_person()
            .filter(|_| self.session_delegated)?
            .to_owned();
        self.device_public_key?;
        Some(Self {
            acting: Acting::person(person),
            session_delegated: false,
            turn: None,
            agent_name: None,
            asked: None,
            bound: Vec::new(),
            own_bound: None,
            ..self.clone()
        })
    }

    /// Whether this agent is one the person's own device handed its session to
    /// ([`Self::delegated_to`]): it carries the delegation a device gives its
    /// agents. An agent admitted as a member that acts for a person
    /// ([`Self::acting_for`]) carries only what the workspace grants it.
    #[must_use]
    pub fn is_session_delegated(&self) -> bool {
        self.session_delegated
    }

    /// The turn this agent acts inside ([`Self::agent_turn`]), when it was
    /// admitted as one: the message whose conversation caused its ops, which
    /// every API it reaches decides it against.
    #[must_use]
    pub fn turn(&self) -> Option<&tilo_action_policy::turn::Turn> {
        self.turn.as_ref()
    }

    /// The person this caller acts for, when it is not that person.
    #[must_use]
    pub fn acting_for_person(&self) -> Option<&str> {
        self.acting.acted_for_person().or_else(|| {
            self.acting
                .app()
                .and_then(|_| self.acting.person_id())
                .filter(|person| !person.is_empty())
        })
    }

    /// The caller's invocations are the steps of Shortcut `id`'s run: still
    /// the person's authority (bounded by the Shortcut's own grants, which
    /// the runner applies), but the Shortcut is who acts, for the person.
    /// Set only by the provider's Shortcut runner.
    #[must_use]
    pub fn running_shortcut(mut self, id: impl Into<String>) -> Self {
        let person = self
            .acting
            .person_id()
            .or_else(|| self.acting.acted_for_person())
            .unwrap_or_default()
            .to_owned();
        let mut acting = Acting::automation(id).acting_for(Acting::person(person));
        acting.app.clone_from(&self.acting.app);
        acting.foreign_device = self.acting.foreign_device;
        self.acting = acting;
        self
    }

    /// Who an action this caller invokes is decided for, as the transport
    /// admitted it ([`tilo_action_policy::Acting`]).
    #[must_use]
    pub const fn acting(&self) -> &Acting {
        &self.acting
    }

    /// An agent acting with its own key on its person's device, held to `bound`, the rules its
    /// own record narrows it to: it carries the delegation its person's grant gives an agent,
    /// narrowed by them ([`Self::granted`]). The provider that runs the agent and read its record
    /// sets it, never a payload. Changes nothing for any other caller.
    #[must_use]
    pub fn held_to(mut self, bound: Vec<tilo_action_policy::Rule>) -> Self {
        if !self.session_delegated && self.acting.is_agent() && self.device_public_key.is_some() {
            self.own_bound = Some(bound);
        }
        self
    }

    /// The grants made for this caller's software, the one place an agent's bound becomes
    /// grants, so every host and every receiver narrows an agent alike. An agent a person's own
    /// device handed its session to is held to the bound its turn carries, else `kept`, the one
    /// the workspace's list of its agents keeps for it; its delegation is its acting's own
    /// ([`Acting::handed_preset`]). An agent acting with its own key carries, where its person's
    /// device read its record ([`Self::held_to`]), its person's delegation narrowed by that
    /// bound; elsewhere it carries nothing of its own, and the host that admits it bounds what
    /// the workspace gives its approved key. An agent whose bound nobody read is never more than
    /// its person allows.
    #[must_use]
    pub fn granted(&self, kept: &[tilo_action_policy::Rule]) -> Vec<tilo_action_policy::Grant> {
        let (Some(agent), Some(person)) = (self.acting.agent_id(), self.acting.acted_for_person())
        else {
            return Vec::new();
        };
        if self.session_delegated {
            let bound = if self.turn.is_some() {
                &self.bound[..]
            } else {
                kept
            };
            return tilo_action_policy::roles::agent_bound(agent, person, bound);
        }
        match (&self.own_bound, &self.workspace) {
            (Some(bound), Some(workspace)) => {
                let mut grants = Acting::handed_agent(agent, person).handed_preset(workspace);
                grants.extend(tilo_action_policy::roles::agent_bound(agent, person, bound));
                grants
            }
            _ => Vec::new(),
        }
    }

    /// The group a session named `id` is, as this caller's provider admitted
    /// it: called by what the session was asked ([`Self::asked`]), else by the
    /// name the provider knows the agent by ([`Self::named`]). History groups
    /// its changes, Activity lists it and undo takes it back whole.
    #[must_use]
    pub fn session_group(&self, id: impl Into<String>) -> crate::WorkGroup {
        crate::WorkGroup {
            id: id.into(),
            label: self
                .asked
                .clone()
                .or_else(|| self.agent_name().map(str::to_owned)),
            kind: crate::context::AGENT_SESSION_GROUP.to_owned(),
            declared_id: None,
            session: None,
        }
    }

    /// The session this caller's actions are part of, as the provider that
    /// admitted it says: an agent (or a key it holds) in an execution the
    /// provider minted for its connection works in one session
    /// ([`Self::session_group`]), whatever the client says of itself. `None`
    /// for anyone else.
    #[must_use]
    pub fn agent_session(&self) -> Option<crate::WorkGroup> {
        self.acting.agent_id()?;
        let id = match &self.acting.agent_session {
            Some(session) => session.as_str().to_owned(),
            None => format!(
                "{}:{}",
                crate::context::AGENT_SESSION_GROUP,
                self.execution_id()?
            ),
        };
        Some(self.session_group(id))
    }

    /// Confine the caller to one workspace: the connection was admitted to that
    /// workspace only, so nothing outside it is offered or reachable.
    #[must_use]
    pub fn in_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// The workspace this caller is confined to, if any.
    #[must_use]
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// The user root a remote device chains to.
    #[must_use]
    pub const fn user_public_key(&self) -> Option<&[u8; 32]> {
        self.user_public_key.as_ref()
    }

    #[must_use]
    pub const fn is_foreign_device(&self) -> bool {
        self.acting.foreign_device
    }

    /// The remote device that sent the request.
    #[must_use]
    pub const fn device_public_key(&self) -> Option<&[u8; 32]> {
        self.device_public_key.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::{Caller, device_agent_id};
    use crate::context::{AGENT_SESSION_GROUP, ActionContext, WorkGroup};
    use crate::remote::{ActionAddress, InvokeArgument, InvokeRequest, RemoteActionTarget};

    /// A call made in the session `session` names, in a work group of `kind`.
    fn call_in(kind: &str, session: Option<&str>) -> InvokeRequest {
        InvokeRequest {
            invocation: crate::InvocationId::new(),
            target: RemoteActionTarget {
                provider: "chat".into(),
                address: ActionAddress::new("chat.messages", "post"),
            },
            argument: InvokeArgument::Text(String::new()),
            context: Some(ActionContext {
                group: Some(WorkGroup {
                    id: "visit-1".into(),
                    label: None,
                    kind: kind.into(),
                    declared_id: None,
                    session: session.map(str::to_owned),
                }),
                ..ActionContext::default()
            }),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// Ada's approved agent answers Bo: it acts for Bo, bounded by Ada. A sub-agent it starts
    /// on that turn works for Bo under Ada's agent's approval, and records the whole chain, so
    /// whatever decides its changes is bounded by Ada's agent and Ada as well.
    #[test]
    fn a_sub_agent_on_another_members_turn_carries_its_starters_whole_chain() {
        let (ada, bo) = ([1; 32], [2; 32]);
        let (ada_id, bo_id) = (hex::encode(ada), hex::encode(bo));
        let scout = Caller::delegated_agent("scout", ada, [3; 32]).on_roster(7, bo);
        assert_eq!(scout.acting().acted_for_person(), Some(bo_id.as_str()));
        assert_eq!(
            scout.acting().people(),
            vec![bo_id.as_str(), ada_id.as_str()]
        );
        assert_eq!(scout.turn_person(), Some(&bo));

        let helper = scout.clone().working_under("helper", "Helper", 11);
        let acting = helper.acting();
        assert_eq!(acting.agent_id(), Some("helper"));
        assert_eq!(
            acting.acted_for_person(),
            Some(bo_id.as_str()),
            "it works for Bo"
        );
        assert!(acting.people().contains(&ada_id.as_str()), "{acting:?}");
        assert!(
            acting.nodes().iter().any(|node| *node == scout.acting()),
            "its starter's whole tree: {acting:?}"
        );
        assert_eq!(helper.approved_agent(), Some("scout"));
        assert_eq!(helper.child_created_at(), Some(11));

        // Ada's own turn records her once.
        let hers = Caller::delegated_agent("scout", ada, [3; 32]).on_roster(7, ada);
        assert_eq!(hers.acting().people(), vec![ada_id.as_str()]);
    }

    /// An agent goes by the name of the session its client named for the call, and by its own
    /// name where none was named; a person is named by no session.
    #[test]
    fn an_agent_goes_by_its_sessions_name() {
        let claude = || Caller::local_agent("claude-code").named("Claude Code");
        let named = |caller: Caller, request| {
            caller
                .in_session_of(&request)
                .agent_called()
                .map(str::to_owned)
        };
        assert_eq!(
            named(claude(), call_in(AGENT_SESSION_GROUP, Some("Release prep"))).as_deref(),
            Some("Release prep")
        );
        assert_eq!(
            named(claude(), call_in(AGENT_SESSION_GROUP, Some("   "))).as_deref(),
            Some("Claude Code")
        );
        assert_eq!(
            named(claude(), call_in("issue", Some("Release prep"))).as_deref(),
            Some("Claude Code")
        );
        let ada = Caller::local_person("ada")
            .in_session_of(&call_in(AGENT_SESSION_GROUP, Some("Release prep")));
        assert_eq!(ada.agent_called(), None);
    }

    /// A call that names its session names the caller, and the connection following the
    /// invocation it started names nothing of it: what an agent is called is presentation
    /// only, so both continue the same invocations. Another session borrows none of them.
    #[test]
    fn an_agents_session_name_does_not_change_who_continues_its_invocations() {
        let caller = || Caller::local_agent("claude-code").named("Claude Code");
        let named = caller().in_session_of(&call_in(AGENT_SESSION_GROUP, Some("Release prep")));
        assert_eq!(named.agent_called(), Some("Release prep"));
        assert!(
            caller().continues(&named),
            "the connection follows what it started"
        );
        assert!(named.continues(&caller()));
        let another = Caller::local_agent("claude-code")
            .in_new_execution()
            .named("Claude Code");
        assert!(
            !another.continues(&named),
            "another live connection of the same agent borrows nothing"
        );
    }

    /// An agent session names itself by what it was asked, and by the agent's name where it was
    /// asked nothing.
    #[test]
    fn an_agent_sessions_label_is_what_it_was_asked_else_the_agents_name() {
        let session = Caller::local_agent("claude-code")
            .in_new_execution()
            .named("Claude Code")
            .in_agent_session("visit-1");
        let label = |caller: &Caller| caller.agent_session().and_then(|session| session.label);
        assert_eq!(label(&session).as_deref(), Some("Claude Code"));
        let asked = session.asked("Fill Jane's application");
        assert_eq!(label(&asked).as_deref(), Some("Fill Jane's application"));
    }

    /// One person's agent under one name is one agent, however the name is typed; another name
    /// or another person's agent is another.
    #[test]
    fn a_persons_agent_is_one_agent_by_its_name() {
        let ada = [1; 32];
        let claude = device_agent_id(&ada, "Claude Code");
        assert!(claude.starts_with("claude-code-"), "{claude}");
        assert_eq!(claude, device_agent_id(&ada, "  claude code "));
        assert_ne!(claude, device_agent_id(&ada, "Reviewer"));
        assert_ne!(claude, device_agent_id(&[2; 32], "Claude Code"));
        assert!(device_agent_id(&ada, "…").starts_with("agent-"));
    }
}
