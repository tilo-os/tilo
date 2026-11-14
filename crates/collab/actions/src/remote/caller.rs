//! Who sends a request: the acting tree a transport authenticated, and what
//! it admitted them to.

use tilo_action_policy::Acting;

use super::{ExecutionLease, within};

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
            acting: Acting::person(hex::encode(user_public_key)),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// A person on a connection the host admitted itself, by the id that
    /// admission established.
    #[must_use]
    pub fn local_person(person: impl Into<String>) -> Self {
        Self {
            acting: Acting::person(person.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// A delegated agent on a connection the host admitted itself.
    #[must_use]
    pub fn local_agent(agent_id: impl Into<String>) -> Self {
        Self {
            acting: Acting::agent(agent_id.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
    }

    /// An application on a connection the host admitted itself, by the
    /// package id that admission established. It acts as the person whose
    /// session runs it ([`Self::acting_for`]), in that app; with nobody
    /// behind it, it holds nothing.
    #[must_use]
    pub fn local_app(app_id: impl Into<String>) -> Self {
        Self {
            acting: Acting::person("").in_app(app_id.into()),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            agent_name: None,
        }
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
        Self {
            acting: Acting::agent(agent_id.into())
                .acting_for(Acting::person(hex::encode(user_public_key))),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            execution: None,
            session_delegated: false,
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
            acting: Acting::agent(agent_id.into()).acting_for(Acting::person(person.into())),
            user_public_key: None,
            device_public_key: None,
            workspace: None,
            execution: None,
            session_delegated: true,
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
            acting: Acting::bot(installation.into())
                .acting_for(Acting::person(hex::encode(user_public_key))),
            user_public_key: Some(user_public_key),
            device_public_key: Some(device_public_key),
            workspace: None,
            execution: None,
            session_delegated: false,
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
        self.acting = self.acting.on_foreign_device(foreign);
        self
    }

    /// The caller acts for `person`: an agent or an app the person started.
    /// Set only by the host that admitted the delegation, never from a
    /// payload. It names who the actor works for in presence and history; it
    /// widens nothing.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<String>) -> Self {
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
    /// returned as it is. Set only by the host that admitted the person.
    #[must_use]
    pub fn delegated_to(mut self, agent_id: impl Into<String>) -> Self {
        let Some(person) = self.acting.person_id().map(str::to_owned) else {
            return self;
        };
        self.session_delegated = true;
        self.acting = Acting::agent(agent_id).acting_for(Acting::person(person));
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
    /// Set only by the host's Shortcut runner.
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

    /// The grants made for this caller's software: the delegation a person's
    /// own device gives the agent it handed its session to.
    #[must_use]
    pub fn granted(&self, workspace: &str) -> Vec<tilo_action_policy::Grant> {
        let (Some(agent), Some(person)) = (self.acting.agent_id(), self.acting.acted_for_person())
        else {
            return Vec::new();
        };
        if !self.session_delegated {
            return Vec::new();
        }
        tilo_action_policy::roles::agent_grants(workspace, agent, person, &[])
    }

    /// The run this caller's actions are part of, as the host that admitted it
    /// says: an agent (or a key it holds) in an execution the host minted for
    /// its connection works in one run, named by the name the host knows for
    /// it. History groups the run's changes, Activity lists it and undo takes
    /// it back whole, whatever the client says of itself. `None` for anyone
    /// else.
    #[must_use]
    pub fn agent_run(&self) -> Option<crate::WorkGroup> {
        let agent = self.acting.agent_id()?;
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
