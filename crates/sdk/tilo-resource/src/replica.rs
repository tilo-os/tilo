//! An in-memory replica of one resource: the harness for replay-based tests,
//! and a resource document held apart from any window — the contents of a
//! file an app saves its resource to and opens it from ([`Replica::load`],
//! [`Replica::save`]).
//!
//! A replica applies actions exactly as the host does — built from the same
//! declarations the palette and agents use, applied by the same executor with
//! the same pinned actor and time — so a test written against it proves the
//! behaviour every surface gets. Fork two replicas to test collaboration;
//! replay one replica's log into a fresh one to prove determinism.

use automerge::ChangeHash;
use tilo_actions::{ActionContext, ActionEnvelope, ActionError, ActionRegistry, EncodedAction};
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
    author: Option<Vec<u8>>,
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
        doc.commit();
        Self::from_doc(doc)
    }

    /// A replica of an existing document.
    #[must_use]
    pub fn from_doc(doc: Doc) -> Self {
        let mut registry = ActionRegistry::new();
        crate::group::register::<R>(&mut registry);
        Self {
            doc,
            registry,
            log: Vec::new(),
            author: None,
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
        self.author = Some(author.as_bytes().to_vec());
        self
    }

    /// Set the origin clock later actions are stamped with.
    pub fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    /// Invoke a declared action by name, exactly as a control, the palette,
    /// an agent or a script does.
    ///
    /// # Errors
    /// Returns a refusal, an argument error or a document error.
    pub fn apply(&mut self, invoke: Invoke<R>) -> Result<&ActionEnvelope, ActionError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(ActionError::Refused(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        if id == crate::GROUP {
            let group = crate::group::build_group::<R>(&arguments, self.now_ms)
                .map_err(ActionError::Refused)?;
            let encoded = EncodedAction::encode(crate::group::group_type::<R>(), &group)?;
            return self.apply_encoded(&group, encoded, Some(id));
        }
        let action = resource::build_action::<R>(id, &arguments, self.now_ms)
            .map_err(ActionError::Refused)?;
        let encoded = resource::encode::<R>(&action)?;
        self.apply_encoded(&action, encoded, Some(id))
    }

    /// Apply one typed action value (the path a custom surface takes).
    ///
    /// # Errors
    /// Returns a refusal or a document error.
    pub fn apply_action(
        &mut self,
        action: &R::Action,
        _label: &str,
    ) -> Result<&ActionEnvelope, ActionError> {
        let encoded = resource::encode::<R>(action)?;
        self.apply_encoded(action, encoded, None)
    }

    /// Apply `action`, invoked as the declared action `declared` when it was
    /// invoked by name.
    fn apply_encoded(
        &mut self,
        action: &dyn tilo_actions::Action,
        encoded: EncodedAction,
        declared: Option<&str>,
    ) -> Result<&ActionEnvelope, ActionError> {
        let mut context = ActionContext::for_resource(R::KIND, R::KIND, R::LABEL);
        if let Some(described) = declared.and_then(|id| {
            tilo_actions::Described::of_published(
                &encoded,
                tilo_actions::ActionKey::new("", R::KIND, id),
            )
        }) {
            context = context.with_described(described);
        }
        let envelope = tilo_actions::apply_local_with_context(
            &mut self.doc,
            action,
            encoded,
            self.author.as_deref(),
            context,
        )?;
        self.log.push(envelope);
        self.log
            .last()
            .ok_or_else(|| ActionError::Refused("no envelope".into()))
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
