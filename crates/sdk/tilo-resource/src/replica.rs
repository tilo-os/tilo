//! An in-memory replica of one resource: the harness for replay-based tests,
//! and a resource document held apart from any window — the contents of a
//! file an app saves its resource to and opens it from ([`Replica::load`],
//! [`Replica::save`]).
//!
//! A replica applies actions exactly as the host does: it publishes the
//! resource's tree, and every invocation goes through the action door —
//! resolved against what is published, checked for availability and
//! confirmation, built from the same declarations the palette and agents
//! use, decoded through the registry a peer would replay with, and committed
//! with its history entry. A test written against it proves the behaviour
//! every surface gets. Fork two replicas to test collaboration; replay one
//! replica's log into a fresh one to prove determinism.

use automerge::ChangeHash;
use tilo_actions::door::{self, InvocationProvenance};
use tilo_actions::{
    ActionAddress, ActionEnvelope, ActionError, ActionRegistry, DoorError, LocalTransport,
};
use tilo_automerge::ApplyIncoming;

use crate::action::Invoke;
use crate::history::{self, Version};
use crate::model::{Doc, ModelError};
use crate::reader::{Demand, Reader};
use crate::resource::{self, Resource};

/// One replica of an `R` resource. See the module docs.
pub struct Replica<R: Resource> {
    doc: Doc,
    registry: ActionRegistry,
    log: Vec<ActionEnvelope>,
    author: Option<String>,
    /// The person the author acts for, when the author is an agent.
    acting_for: Option<String>,
    /// Who invokes from this replica when it acts as its own device
    /// ([`Self::as_invoker`]).
    invoker: Option<String>,
    now_ms: i64,
    /// The incrementally maintained state views would see, once asked for.
    reader: Option<Reader<R>>,
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Resource> Default for Replica<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Resource> Replica<R> {
    /// A new resource, initialized as a new record would be.
    ///
    /// # Panics
    /// Panics when [`Resource::initialize`] fails, which is a bug in the
    /// resource type under test.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces initializer bugs loudly"
    )]
    pub fn new() -> Self {
        let mut doc = Doc::new();
        R::initialize(&mut doc).expect("resource initializer");
        // What a brand-new record starts with names itself as its genesis.
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
        );
        Self::from_doc(doc)
    }

    /// A replica of an existing document.
    #[must_use]
    pub fn from_doc(doc: Doc) -> Self {
        let registry = ActionRegistry::with(crate::group::register::<R>);
        Self {
            doc,
            registry,
            log: Vec::new(),
            author: None,
            acting_for: None,
            invoker: None,
            now_ms: 1_700_000_000_000,
            reader: None,
            marker: std::marker::PhantomData,
        }
    }

    /// A replica of a document [saved](Self::save) as bytes: a file's
    /// contents.
    ///
    /// # Errors
    /// Returns [`ModelError::Automerge`] when the bytes hold no document.
    pub fn load(bytes: &[u8]) -> Result<Self, ModelError> {
        Ok(Self::from_doc(Doc::load(bytes)?))
    }

    /// The document as bytes, for a file that carries the resource whole.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Attribute later actions to `author`, as the host does for a signed-in
    /// collaborator.
    #[must_use]
    pub fn as_author(mut self, author: &str) -> Self {
        self.author = Some(author.to_owned());
        self
    }

    /// Attribute later actions to `author` invoking from this replica, as
    /// the host does for a person at one of their devices: what a verb reads
    /// as [`ActionCx::author`](crate::ActionCx::author), while the changes
    /// stay this replica's own, so the same person on two replicas makes
    /// changes that merge.
    #[must_use]
    pub fn as_invoker(mut self, author: &str) -> Self {
        self.invoker = Some(author.to_owned());
        self
    }

    /// Attribute later actions to the agent `agent` acting for `person`, as
    /// a host does for an agent's calls.
    #[must_use]
    pub fn as_agent_for(mut self, agent: &str, person: &str) -> Self {
        self.author = Some(agent.to_owned());
        self.acting_for = Some(person.to_owned());
        self
    }

    /// Set the origin clock later actions are stamped with.
    pub fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    /// The tree the resource publishes now: what the door resolves against.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn published(&self) -> Result<tilo_actions::Resource, ModelError> {
        Ok(resource::publish::<R>(
            R::KIND,
            R::LABEL,
            &R::read(&self.doc)?,
        ))
    }

    /// Invoke a published action exactly as a control, the palette, an
    /// agent or a script does: through the door, which refuses one that
    /// asks for confirmation ([`Self::apply_confirmed`]).
    ///
    /// # Errors
    /// Returns the door's refusal: unknown, unavailable, invalid arguments,
    /// confirmation required, or the action's own refusal.
    pub fn apply(&mut self, invoke: impl Into<Invoke<R>>) -> Result<&ActionEnvelope, DoorError> {
        self.invoke(invoke.into(), false)
    }

    /// [`Self::apply`] after the person confirmed it.
    ///
    /// # Errors
    /// As [`Self::apply`], without asking for confirmation.
    pub fn apply_confirmed(
        &mut self,
        invoke: impl Into<Invoke<R>>,
    ) -> Result<&ActionEnvelope, DoorError> {
        self.invoke(invoke.into(), true)
    }

    /// Apply one typed action value, the path a custom surface takes once
    /// its person confirmed what it does: the generic
    /// [`APPLY`](crate::APPLY) action, through the door.
    ///
    /// # Errors
    /// Returns the door's refusal or the action's own.
    pub fn apply_action(&mut self, action: &R::Action) -> Result<&ActionEnvelope, DoorError> {
        let invoke = Invoke::apply(action)
            .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
        self.invoke(invoke, true)
    }

    fn invoke(&mut self, invoke: Invoke<R>, confirmed: bool) -> Result<&ActionEnvelope, DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(DoorError::InvalidArguments(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        // Published from the state kept current change by change, so an
        // action costs what it touched, not a whole read of the record.
        let tree = self
            .live()
            .map(|state| resource::publish::<R>(R::KIND, R::LABEL, state))
            .map_err(|error| DoorError::Failed(error.to_string()))?;
        let address = ActionAddress::new(tree.id.as_str(), id);
        // Without an author, the replica's own actor: every replica is a
        // device of its own.
        let actor = self.author.as_ref().map_or_else(
            || self.doc.get_actor().to_bytes().to_vec(),
            |author| author.as_bytes().to_vec(),
        );
        // An author is also the invoker a verb reads (`ActionCx::author`), as
        // the host pins the signed-in person.
        let ambient = self
            .author
            .as_ref()
            .or(self.invoker.as_ref())
            .map(|author| {
                let invoker = tilo_actions::ActionInvoker::actor(author.clone());
                let invoker = match &self.acting_for {
                    Some(person) => invoker
                        .with_kind(tilo_actions::ActorKind::Agent)
                        .acting_for(person.clone()),
                    None => invoker,
                };
                tilo_actions::ActionContext::default().with_invoker(invoker)
            });
        let mut transport = LocalTransport::new();
        let (doc, registry) = (&mut self.doc, &self.registry);
        let dispatched = resource::with_origin_clock(self.now_ms, || {
            let resources = std::slice::from_ref(&tree);
            let provenance = match &ambient {
                Some(ambient) => InvocationProvenance::caller(&actor).within(ambient),
                None => InvocationProvenance::caller(&actor),
            };
            if confirmed {
                door::invoke_json_confirmed(
                    doc,
                    resources,
                    &address,
                    &arguments,
                    provenance,
                    registry,
                    &mut transport,
                )
            } else {
                door::invoke_json(
                    doc,
                    resources,
                    &address,
                    &arguments,
                    provenance,
                    registry,
                    &mut transport,
                )
            }
        })?;
        self.log.push(dispatched.envelope().clone());
        self.log
            .last()
            .ok_or_else(|| DoorError::Failed("no envelope".into()))
    }

    /// Replay an envelope authored on another replica.
    ///
    /// # Errors
    /// Returns a decode or apply error.
    pub fn replay(&mut self, envelope: &ActionEnvelope) -> Result<(), ActionError> {
        tilo_actions::replay(&mut self.doc, envelope, &self.registry)?;
        self.log.push(envelope.clone());
        Ok(())
    }

    /// The current state.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn state(&self) -> Result<R::State, ModelError> {
        R::read(&self.doc)
    }

    /// The state as a view sees it: read once, then kept current by applying
    /// only what each change touched ([`Resource::refresh`]) and reading
    /// what is demanded ([`Resource::load`]). A test comparing it with
    /// [`Self::state`] proves the resource's incremental reads exact.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn live(&mut self) -> Result<&R::State, ModelError> {
        if let Some(reader) = &mut self.reader {
            reader.sync(&self.doc)?;
        } else {
            self.reader = Some(Reader::open(&mut self.doc)?);
        }
        match &self.reader {
            Some(reader) => Ok(reader.state()),
            None => Err(ModelError::invalid("no reader")),
        }
    }

    /// Ask the view state of [`Self::live`] for the parts `demand` names.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn demand(&mut self, demand: Demand) -> Result<&R::State, ModelError>
    where
        R::State: Clone,
    {
        self.live()?;
        let Some(reader) = self.reader.as_mut() else {
            return Err(ModelError::invalid("no reader"));
        };
        reader.demand(&self.doc, demand)?;
        Ok(reader.state())
    }

    /// A second replica sharing this one's history, as a collaborator who
    /// just joined would hold.
    #[must_use]
    pub fn fork(&mut self) -> Self {
        let mut replica = Self::from_doc(self.doc.fork());
        replica.now_ms = self.now_ms;
        replica
    }

    /// Exchange every change with `other`, as sync does.
    ///
    /// # Errors
    /// Returns a document error.
    pub fn sync_with(&mut self, other: &mut Self) -> Result<(), ModelError> {
        self.doc.merge_incoming(&mut other.doc)?;
        other.doc.merge_incoming(&mut self.doc)?;
        Ok(())
    }

    /// Every envelope applied or replayed here, in order.
    pub fn log(&self) -> &[ActionEnvelope] {
        &self.log
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The document, for a test that changes it the way something outside
    /// the resource's actions would (a restore, a raw merge).
    pub fn doc_mut(&mut self) -> &mut Doc {
        &mut self.doc
    }

    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// The version history.
    pub fn versions(&mut self) -> Vec<Version> {
        history::versions(&mut self.doc)
    }

    /// The state as of `heads`.
    ///
    /// # Errors
    /// Returns [`ModelError`] when that version does not hold the resource.
    pub fn state_at(&mut self, heads: &[ChangeHash]) -> Result<R::State, ModelError> {
        history::read_at::<R>(&mut self.doc, heads)
    }
}
