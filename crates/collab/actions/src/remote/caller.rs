//! Who sends a request: the principal a transport authenticated, and what
//! it admitted them to.

use super::{CallerPrincipal, ExecutionLease, within};

/// The authenticated principal behind a request.
///
/// Built only by the transport that authenticated the connection — from the
/// certificate chain a peer presented, or from the admitted identity of a
/// local connection. It is deliberately not `Deserialize`: a payload cannot
/// assert who sent it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Caller {
    pub(super) principal: CallerPrincipal,
    pub(super) user_public_key: Option<[u8; 32]>,
    pub(super) device_public_key: Option<[u8; 32]>,
    pub(super) workspace: Option<String>,
    pub(super) foreign: bool,
    // Transport-issued execution scope, never accepted from request context.
    // It narrows invocation ownership without changing the policy principal.
    pub(super) execution: Option<ExecutionLease>,
    // The person a non-person principal acts for, as the host admitted it.
    pub(super) acting_for: Option<String>,
    // The person's own device handed this connection to the agent
    // (`Self::delegated_to`), rather than the agent being admitted as a member
    // in its own right and acting for them (`Self::acting_for`).
    pub(super) session_delegated: bool,
    // The Shortcut whose run this person's invocation is a step of.
    pub(super) shortcut: Option<String>,
    // How far the invocation this caller's requests continue was admitted
    // to reach, set by the host that matched them to it.
    pub(super) within: within::Within,
    // The turn an agent acts inside: the person's message in a conversation
    // that caused it (`Self::agent_turn`). Every op carries it, so the
    // receiving device checks it against that message and presence can show
    // what the agent would click.
    pub(super) turn: Option<tilo_action_policy::turn::Turn>,
    // The name an agent goes by, from the key its person certified for it
    // (`Self::named`): presentation only, what History names it by.
    pub(super) agent_name: Option<String>,
}

impl Caller {
    /// A remote device whose certificate the transport verified to chain to
    /// `user_public_key`.
    #[must_use]
    pub fn authenticated(user_public_key: [u8; 32], device_public_key: [u8; 32]) -> Self {
        Self {
            principal: CallerPrincipal::Person(hex::encode(user_public_key)),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: None,
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// A person on a connection the host admitted itself, by the policy
    /// principal id that admission established.
    #[must_use]
    pub fn local_person(principal: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::Person(principal.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: None,
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// A delegated agent on a connection the host admitted itself.
    #[must_use]
    pub fn local_agent(agent_id: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::Agent(agent_id.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: None,
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// An application on a connection the host admitted itself, by the
    /// package id that admission established. It holds no role of its own:
    /// it may use only what it owns and what a workspace grants it.
    #[must_use]
    pub fn local_app(app_id: impl Into<String>) -> Self {
        Self {
            principal: CallerPrincipal::App(app_id.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: None,
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// A remote agent device whose certificate the transport verified to
    /// chain to `user_public_key` and whose root-signed delegation names it
    /// agent `agent_id`. It is its own principal, never the person who
    /// delegated it.
    #[must_use]
    pub fn delegated_agent(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self {
            principal: CallerPrincipal::Agent(agent_id.into()),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: None,
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// An agent acting inside a turn: `person`'s message in `turn`'s
    /// conversation caused it, and the agent takes part there. It may do
    /// exactly what that person may, held to the editor preset and to the
    /// bound its own record carries, and nothing outside a turn. Every op it
    /// makes carries the turn, so the receiving device checks the cause it
    /// arrived with. Set only by the host that admitted the turn.
    #[must_use]
    pub fn agent_turn(
        agent_id: impl Into<String>,
        person: impl Into<String>,
        turn: tilo_action_policy::turn::Turn,
    ) -> Self {
        Self {
            principal: CallerPrincipal::Agent(agent_id.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: Some(person.into()),
            session_delegated: true,
            shortcut: None,
            within: within::Within::NONE,
            turn: Some(turn),
            agent_name: None,
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
        Self {
            principal: CallerPrincipal::Bot(installation.into()),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            foreign: false,
            execution: None,
            acting_for: Some(hex::encode(user_public_key)),
            session_delegated: false,
            shortcut: None,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// Give an already admitted connection its own execution scope. The host
    /// mints the identifier; no payload, display name or history run selects it.
    /// This changes invocation ownership, never the caller's permissions.
    #[must_use]
    pub fn in_new_execution(mut self) -> Self {
        self.execution = Some(ExecutionLease::mint());
        self
    }

    /// Retain a host-issued execution when projecting the same transport into
    /// another verified action door. A lease conveys ownership and lifetime,
    /// never certificate custody or additional permissions.
    #[must_use]
    pub fn in_execution(mut self, execution: ExecutionLease) -> Self {
        self.execution = Some(execution);
        self
    }

    /// Host-issued execution identity. An identifier alone conveys no authority.
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
        self.foreign = foreign;
        self
    }

    /// The caller acts for `person`, by that person's policy principal id:
    /// an agent, a Shortcut or an app the person started. Set only by the
    /// host that admitted the delegation, never from a payload. It names who
    /// the actor works for in presence and history; it widens nothing.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<String>) -> Self {
        self.acting_for = Some(person.into());
        self
    }

    /// This person's own device hands the connection to agent `agent_id`,
    /// acting for them: an MCP client, a local assistant. The agent is the
    /// principal (its edits, presence and history are the agent's, for the
    /// person) and it may do only what both the person and the delegation a
    /// device gives its agents allow. A caller that is not a person is
    /// returned as it is. Set only by the host that admitted the person.
    #[must_use]
    pub fn delegated_to(mut self, agent_id: impl Into<String>) -> Self {
        let CallerPrincipal::Person(person) = &self.principal else {
            return self;
        };
        self.acting_for = Some(person.clone());
        self.session_delegated = true;
        self.principal = CallerPrincipal::Agent(agent_id.into());
        self
    }

    /// An agent caller named `name`, as the certificate the transport
    /// verified for its key says (`device_label`): what History names its
    /// changes by. Names nothing for a caller that is not an agent.
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        let name = name.trim();
        if matches!(self.principal, CallerPrincipal::Agent(_)) && !name.is_empty() {
            self.agent_name = Some(name.to_owned());
        }
        self
    }

    /// The name an agent caller goes by ([`Self::named`]).
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    /// The person whose own device handed this connection to its agent
    /// ([`Self::delegated_to`]), as that device is admitted: the person the
    /// agent acts for, on the same device, whose current standing bounds
    /// everything the agent may do. `None` for any other caller. Use it to
    /// read the person's standing, never to act as them.
    #[must_use]
    pub fn delegating_person(&self) -> Option<Self> {
        let CallerPrincipal::Agent(_) = &self.principal else {
            return None;
        };
        let person = self.acting_for.clone().filter(|_| self.session_delegated)?;
        self.device_public_key?;
        Some(Self {
            principal: CallerPrincipal::Person(person),
            acting_for: None,
            session_delegated: false,
            turn: None,
            agent_name: None,
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
    /// every door it reaches decides it against.
    #[must_use]
    pub fn turn(&self) -> Option<&tilo_action_policy::turn::Turn> {
        self.turn.as_ref()
    }

    /// The person this caller acts for, when it is not that person.
    #[must_use]
    pub fn acting_for_person(&self) -> Option<&str> {
        self.acting_for.as_deref()
    }

    /// The caller's invocations are the steps of Shortcut `id`'s run: still
    /// the person's authority (bounded by the Shortcut's own grants, which
    /// the runner applies), but the Shortcut is who acts, for the person.
    /// Set only by the host's Shortcut runner.
    #[must_use]
    pub fn running_shortcut(mut self, id: impl Into<String>) -> Self {
        self.shortcut = Some(id.into());
        self
    }

    /// The Shortcut whose run this invocation is a step of.
    #[must_use]
    pub fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }

    /// Who acts, as presence and history name them: the kind of actor, its
    /// id, and the person it acts for when that is someone else.
    #[must_use]
    pub fn actor(&self) -> crate::InteractionActor {
        match (&self.principal, &self.shortcut) {
            (CallerPrincipal::Person(person), Some(shortcut)) => crate::InteractionActor::new(
                format!("shortcut:{shortcut}"),
                crate::ActorKind::Shortcut,
            )
            .acting_for(person.clone()),
            (CallerPrincipal::Person(person), None) => {
                crate::InteractionActor::new(person.clone(), crate::ActorKind::Person)
            }
            (CallerPrincipal::Agent(agent), _) => {
                let actor =
                    crate::InteractionActor::new(format!("agent:{agent}"), crate::ActorKind::Agent);
                // An agent device acts for the person who certified it, as
                // `Self::invoker` reads it.
                match self
                    .acting_for
                    .clone()
                    .or_else(|| self.user_public_key.map(hex::encode))
                {
                    Some(person) => actor.acting_for(person),
                    None => actor,
                }
            }
            (CallerPrincipal::App(app), _) => {
                let actor =
                    crate::InteractionActor::new(format!("app:{app}"), crate::ActorKind::App);
                match &self.acting_for {
                    Some(person) => actor.acting_for(person.clone()),
                    None => actor,
                }
            }
            (CallerPrincipal::Bot(bot), _) => {
                let actor =
                    crate::InteractionActor::new(format!("bot:{bot}"), crate::ActorKind::Bot);
                match &self.acting_for {
                    Some(person) => actor.acting_for(person.clone()),
                    None => actor,
                }
            }
        }
    }

    /// Who an action this caller invokes in `workspace` is decided for: the
    /// one reading of a caller every route that runs actions takes
    /// ([`tilo_action_policy::Acting`]).
    ///
    /// - A person acts as themselves.
    /// - An agent a person's own device handed its session to
    ///   ([`Self::delegated_to`]) acts for them, carrying the delegation a
    ///   device gives its agents ([`Self::granted`]).
    /// - An agent admitted as acting for a person ([`Self::acting_for`]),
    ///   or a delegated agent device acting for the person its certificate
    ///   chains to, does what both the person and the workspace's grants
    ///   for it allow.
    /// - A bot acts for its installer within the workspace's grants for it.
    /// - An application, or an agent acting for nobody, does only what the
    ///   workspace grants it.
    ///
    /// Whether the person works from a device that is not theirs is the
    /// host's to say ([`tilo_action_policy::Acting::on_foreign_device`]).
    #[must_use]
    pub fn acting(&self) -> tilo_action_policy::Acting {
        use tilo_action_policy::Acting;
        let root = || self.user_public_key.map(hex::encode);
        let acting = match &self.principal {
            CallerPrincipal::Person(person) => Acting::person(person.clone()),
            CallerPrincipal::Agent(agent) => match self.acting_for.clone().or_else(root) {
                Some(person) => Acting::agent(agent.clone()).acting_for(Acting::person(person)),
                None => Acting::agent(agent.clone()),
            },
            CallerPrincipal::App(app) => Acting::app(app.clone()),
            // A bot nobody installed has no authority to borrow: acting for
            // nobody's standing, it gets nothing.
            CallerPrincipal::Bot(bot) => match self.acting_for.clone().or_else(root) {
                Some(person) => Acting::bot(bot.clone()).acting_for(Acting::person(person)),
                None => Acting::bot(bot.clone()),
            },
        };
        acting.on_foreign_device(self.foreign)
    }

    /// The grants made for this caller's software: the delegation a person's
    /// own device gives the agent it handed its session to.
    #[must_use]
    pub fn granted(&self, workspace: &str) -> Vec<tilo_action_policy::Grant> {
        let mut granted = match (&self.principal, self.acting_for.clone()) {
            (CallerPrincipal::Agent(agent), Some(person)) if self.session_delegated => {
                tilo_action_policy::roles::agent_grants(workspace, agent, &person, &[])
            }
            _ => Vec::new(),
        };
        if self.foreign {
            granted.extend(
                tilo_action_policy::roles::foreign_device_bound()
                    .iter()
                    .cloned(),
            );
        }
        granted
    }

    /// The run this caller's actions are part of, as the host that admitted it
    /// says: an agent (or a key it holds) in an execution the host minted for
    /// its connection works in one run, named by the name the host knows for
    /// it. History groups the run's changes, Activity lists it and undo takes
    /// it back whole, whatever the client says of itself. `None` for anyone
    /// else.
    #[must_use]
    pub fn agent_run(&self) -> Option<crate::WorkGroup> {
        let CallerPrincipal::Agent(agent) = &self.principal else {
            return None;
        };
        let execution = self.execution_id()?;
        Some(crate::WorkGroup {
            id: format!("{}:{execution}", crate::context::AGENT_RUN_GROUP),
            label: self.agent_name().unwrap_or(agent).to_owned(),
            kind: crate::context::AGENT_RUN_GROUP.to_owned(),
            title: None,
        })
    }

    /// Confine the caller to one workspace: the connection was admitted to that
    /// workspace only, so nothing outside it is offered or reachable.
    #[must_use]
    pub fn in_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    #[must_use]
    pub const fn principal(&self) -> &CallerPrincipal {
        &self.principal
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
        self.foreign
    }

    /// The remote device that sent the request.
    #[must_use]
    pub const fn device_public_key(&self) -> Option<&[u8; 32]> {
        self.device_public_key.as_ref()
    }
}
