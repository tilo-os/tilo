//! An in-memory replica of one resource: the harness for replay-based tests,
//! and a resource document held apart from any window — the contents of a
//! file an app saves its resource to and opens it from ([`Replica::load`],
//! [`Replica::save`]).
//!
//! A replica applies actions exactly as the host does: it publishes the
//! resource's tree, and every invocation goes through the action API —
//! resolved against what is published, checked for availability and
//! confirmation, built from the same declarations the palette and agents
//! use, decoded through the registry a peer would replay with, and committed
//! with its history entry. A test written against it proves the behaviour
//! every surface gets. Fork two replicas to test collaboration; replay one
//! replica's log into a fresh one to prove determinism.
//!
//! A replica names the app whose record it is ([`Replica::in_app`]) before it
//! applies anything: every device decides a change against that app's surface,
//! and a change that named none would be decided against an app guessed from
//! its type id. The name is part of the type, so a caller that forgets it does
//! not compile.

use automerge::ChangeHash;
use tilo_actions::api::{self, InvocationProvenance};
use tilo_actions::{
    ActionAddress, ActionEnvelope, ActionError, ActionRegistry, ApiError, LocalTransport,
};
use tilo_automerge::ApplyIncoming;
use tilo_member_types::MemberId;

use crate::action::Invoke;
use crate::history::{self, Version};
use crate::model::{Doc, ModelError};
use crate::reader::{Demand, Reader};
use crate::resource::{self, Resource};

/// The app a replica names when the record is only a model a test exercises: nothing decides
/// such a change against a surface, and no host ever receives it.
pub const MODEL_APP_ID: &str = "so.tilo.model";

/// A replica that has named no app: it reads, forks, saves and replays its log, and it cannot
/// apply an action. [`Replica::in_app`] names the app the record belongs to, and only then does
/// [`Replica::apply`] exist.
pub struct NoApp;

/// A replica whose actions name the app the record belongs to ([`Replica::in_app`]): the app
/// every device decides the change against.
pub struct AppNamed;

/// One replica of an `R` resource. See the module docs.
pub struct Replica<R: Resource, A = NoApp> {
    doc: Doc,
    registry: ActionRegistry,
    log: Vec<ActionEnvelope>,
    author: Option<MemberId>,
    /// The author as software acting for someone.
    acting: Option<tilo_actions::Acting>,
    /// Who invokes from this replica when it acts as its own device
    /// ([`Self::as_invoker`], [`Self::as_invoker_acting`]).
    invoker: Option<tilo_actions::Acting>,
    /// The app later actions are in (id, label), as a host names it.
    app: Option<(String, String)>,
    /// What a bot shows for later actions instead of its own name.
    appearance: Option<tilo_actions::Appearance>,
    /// The name the host admitted the actor of later actions under.
    name: Option<String>,
    /// The work unit later actions belong to (id, label, kind).
    group: Option<(String, String, String)>,
    /// The call later actions are made for, as its caller signed it ([`Self::serving`]).
    serving: Option<(
        tilo_actions::InvocationId,
        Box<tilo_actions::remote::SignedCall>,
    )>,
    now_ms: i64,
    /// The incrementally maintained state views would see, once asked for.
    reader: Option<Reader<R>>,
    /// The record this replica writes, as its address on the device, when its caller names it:
    /// what the kept record is keyed by, so the same record's writes start from it whichever
    /// of the device's workers runs them.
    record: Option<std::sync::Arc<str>>,
    marker: std::marker::PhantomData<fn() -> (R, A)>,
}

impl<R: Resource> Default for Replica<R> {
    fn default() -> Self {
        Self::new()
    }
}

/// A replica that names no app: what reads, forking and saving use, and what a unit test's
/// model is exercised on. It cannot apply an action until [`Replica::in_app`] names the app
/// whose record it is; a system write has no other way to reach the action API.
impl<R: Resource> Replica<R, NoApp> {
    /// The id of the record a new replica holds ([`Self::new`]), which its founding and version
    /// steps are derived from: bring a document of one up under it
    /// ([`crate::founding::bring_up_resource`]) to stand on the same chain.
    pub const ID: &str = "record";

    /// The workspace the record a new replica holds is in ([`Self::new`]): a record's founding
    /// and steps are derived within its workspace, so bring a document of one up in it as well.
    pub const WORKSPACE: &str = "replica";

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
    pub fn new() -> Replica<R, NoApp> {
        let mut doc = crate::founding::new_record::<R>(Self::WORKSPACE, Self::ID)
            .expect("resource initializer");
        // What a brand-new record starts with names itself as its genesis.
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
        );
        Self::from_doc(doc)
    }

    /// A replica of an existing document.
    #[must_use]
    pub fn from_doc(doc: Doc) -> Replica<R, NoApp> {
        let registry = ActionRegistry::with(crate::group::register::<R>);
        Self {
            doc,
            registry,
            log: Vec::new(),
            author: None,
            acting: None,
            invoker: None,
            app: None,
            appearance: None,
            name: None,
            group: None,
            serving: None,
            now_ms: 1_700_000_000_000,
            reader: None,
            marker: std::marker::PhantomData,
            record: None,
        }
    }

    /// A replica of a document [saved](Self::save) as bytes: a file's
    /// contents.
    ///
    /// # Errors
    /// Returns [`ModelError::Automerge`] when the bytes hold no document.
    pub fn load(bytes: &[u8]) -> Result<Replica<R, NoApp>, ModelError> {
        Ok(Self::from_doc(Doc::load(bytes)?))
    }

    /// Name `id` (labelled `label`) as the app later actions are in, as the
    /// host running the app does: every member decides them against that
    /// app's surface.
    #[must_use]
    pub fn in_app(mut self, id: &str, label: &str) -> Replica<R, AppNamed> {
        self.app = Some((id.to_owned(), label.to_owned()));
        Replica {
            doc: self.doc,
            registry: self.registry,
            log: self.log,
            author: self.author,
            acting: self.acting,
            invoker: self.invoker,
            app: self.app,
            appearance: self.appearance,
            name: self.name,
            group: self.group,
            serving: self.serving,
            now_ms: self.now_ms,
            reader: self.reader,
            marker: std::marker::PhantomData,
            record: self.record,
        }
    }
}

impl<R: Resource> Replica<R, AppNamed> {
    /// Name another app later actions are in, replacing the one named before.
    #[must_use]
    pub fn in_app(mut self, id: &str, label: &str) -> Replica<R, AppNamed> {
        self.app = Some((id.to_owned(), label.to_owned()));
        self
    }
}

impl<R: Resource, A> Replica<R, A> {
    /// The document as bytes, for a file that carries the resource whole.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Attribute later actions to `author`, as the host does for a signed-in
    /// member.
    #[must_use]
    pub fn as_author(self, person: &str) -> Self {
        self.as_member(MemberId::person(person))
    }

    /// Attribute later actions to `member`.
    #[must_use]
    pub fn as_member(mut self, member: MemberId) -> Self {
        self.author = Some(member);
        self
    }

    /// Attribute later actions to `author` invoking from this replica, as
    /// the host does for a person at one of their devices: what a verb reads
    /// as [`ActionCx::author`](crate::ActionCx::author), while the changes
    /// stay this replica's own, so the same person on two replicas makes
    /// changes that merge.
    #[must_use]
    pub fn as_invoker(mut self, person: &str) -> Self {
        self.invoker = Some(tilo_actions::Acting::person(person));
        self
    }

    /// Attribute later actions to the invoker `acting` — a person, or an agent or automation
    /// acting for one — while the changes stay this device's own: what a host
    /// writes a record for the one that asked it, so every holder decides the change as that
    /// invoker, exactly as it decides the same action invoked on the invoker's own device.
    #[must_use]
    pub fn as_invoker_acting(mut self, acting: tilo_actions::Acting) -> Self {
        self.invoker = Some(acting);
        self
    }

    /// Attribute actions to an actor already admitted by the caller.
    #[must_use]
    pub fn as_acting(mut self, acting: tilo_actions::Acting) -> Self {
        self.author = acting.member_id().cloned();
        self.acting = Some(acting);
        self
    }

    /// Attribute later actions to the agent `agent` acting for `person`, as
    /// a host does for an agent's calls.
    #[must_use]
    pub fn as_agent_for(self, agent: &str, person: &str) -> Self {
        self.as_acting(
            tilo_actions::Acting::agent(agent, person)
                .acting_for(tilo_actions::Acting::person(person)),
        )
    }

    /// Attribute later actions to the agent `agent` a device runs on `person`'s turn, handed
    /// the delegation a device gives the agents it runs ([`tilo_actions::Acting::handed_agent`]):
    /// whichever member it acts for, it does what their grants and its preset allow, as every
    /// receiver decides it.
    #[must_use]
    pub fn as_handed_agent_for(self, agent: &str, person: &str) -> Self {
        self.as_acting(tilo_actions::Acting::handed_agent(agent, person))
    }

    /// Attribute later actions to the bot installation `installation`
    /// acting for `installer`, as a bot host does for a bot's calls.
    #[must_use]
    pub fn as_bot_for(mut self, installation: &str, installer: &str) -> Self {
        self.author = Some(MemberId::bot(installation));
        self.acting = Some(
            tilo_actions::Acting::bot(installation)
                .acting_for(tilo_actions::Acting::person(installer)),
        );
        self
    }

    /// Show `appearance` for the bot's later actions, as a bot host admits
    /// a bridge's post under another name.
    #[must_use]
    pub fn showing(mut self, appearance: tilo_actions::Appearance) -> Self {
        self.appearance = Some(appearance);
        self
    }

    /// Admit the actor of later actions under the name `name`, as a host
    /// names an agent it admits (`Company mover`).
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }

    /// Stamp later actions as part of the work unit `id` (labelled `label`,
    /// of kind `kind`), as a shell does for every action one command line
    /// asks for: History attributes and undoes them as one.
    #[must_use]
    pub fn within_group(mut self, id: &str, label: &str, kind: &str) -> Self {
        self.group = Some((id.to_owned(), label.to_owned(), kind.to_owned()));
        self
    }

    /// Keep `call`, the invocation `invocation` as its caller signed it, with later actions: what
    /// a host writes for a caller who asked under their own key, so every device reading them checks that
    /// they asked, and for what.
    #[must_use]
    pub fn serving(
        mut self,
        invocation: tilo_actions::InvocationId,
        call: tilo_actions::remote::SignedCall,
    ) -> Self {
        self.serving = Some((invocation, Box::new(call)));
        self
    }

    /// Set the origin clock later actions are stamped with.
    pub fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    /// The tree the resource publishes now: what the API resolves against.
    pub fn published(&self) -> Result<tilo_actions::ResourceSpec, ModelError> {
        Ok(resource::publish::<R>(
            R::KIND,
            R::LABEL,
            &R::read(&self.doc)?,
        ))
    }

    /// Who later actions are invoked by, as a verb reads it: the author, or
    /// the invoker, acting as attributed. `None` when neither is set.
    #[must_use]
    pub fn invoker(&self) -> Option<tilo_actions::ActionInvoker> {
        let acting = match (&self.acting, &self.invoker) {
            (Some(acting), _) | (None, Some(acting)) => acting.clone(),
            (None, None) => tilo_actions::Acting::member(self.author.clone()?),
        };
        Some(
            tilo_actions::ActionInvoker::admitted(&acting)
                .with_appearance(self.appearance.clone())
                .named(self.name.clone()),
        )
    }

    /// The record this replica writes, as its address on the device (`workspace/app/record`): the
    /// key the record an action leaves is kept under, so its next write starts from it rather
    /// than reading the whole record again, whichever worker runs it.
    #[must_use]
    pub fn at_record(mut self, record: impl Into<std::sync::Arc<str>>) -> Self {
        self.record = Some(record.into());
        self
    }

    fn invoke(&mut self, invoke: Invoke<R>, confirmed: bool) -> Result<&ActionEnvelope, ApiError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(ApiError::InvalidArguments(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        // The action's declaration is what the write is dispatched against, and a type declares
        // its actions: the tree is built from them alone, so a write costs what it changed, not
        // a read of the whole record (which is what publishing its state would take).
        let tree = resource::publish_actions::<R>(R::KIND, R::LABEL);
        let address = ActionAddress::new(tree.id.as_str(), id);
        // Without an author, the replica's own actor: every replica is a
        // device of its own.
        let actor = self.author.as_ref().map_or_else(
            || self.doc.get_actor().to_bytes().to_vec(),
            MemberId::actor_bytes,
        );
        // An author is also the invoker a verb reads (`ActionCx::author`), as
        // the host pins the signed-in person.
        let ambient = self
            .invoker()
            .map(|invoker| tilo_actions::ActionContext::default().with_invoker(invoker));
        let ambient = match &self.app {
            Some((id, label)) => Some(ambient.unwrap_or_default().with_app(id, label)),
            None => ambient,
        };
        let ambient = match &self.group {
            Some((id, label, kind)) => {
                Some(ambient.unwrap_or_default().with_group(id, label, kind))
            }
            None => ambient,
        };
        let ambient = match &self.serving {
            Some((invocation, call)) => {
                let mut ambient = ambient.unwrap_or_default();
                ambient.source_invocation = Some(*invocation);
                ambient.signed = Some(call.clone());
                Some(ambient)
            }
            None => ambient,
        };
        let mut transport = LocalTransport::new();
        let record = self.record.clone();
        let (doc, registry) = (&mut self.doc, &self.registry);
        let dispatched = crate::apply_cache::with_record(record.as_ref(), || {
            resource::with_origin_clock(self.now_ms, || {
                let resources = std::slice::from_ref(&tree);
                let provenance = match &ambient {
                    Some(ambient) => InvocationProvenance::caller(&actor).within(ambient),
                    None => InvocationProvenance::caller(&actor),
                };
                api::invoke_json(
                    doc,
                    resources,
                    &address,
                    &arguments,
                    (provenance, confirmed),
                    registry,
                    &mut transport,
                )
            })
        })?;
        self.log.push(dispatched.envelope().clone());
        self.log
            .last()
            .ok_or_else(|| ApiError::Failed("no envelope".into()))
    }

    /// Replay an envelope authored on another replica.
    pub fn replay(&mut self, envelope: &ActionEnvelope) -> Result<(), ActionError> {
        tilo_actions::replay(&mut self.doc, envelope, &self.registry)?;
        self.log.push(envelope.clone());
        Ok(())
    }

    /// The current state.
    pub fn state(&self) -> Result<R::State, ModelError> {
        R::read(&self.doc)
    }

    /// The state as a view sees it: read once, then kept current by applying
    /// only what each change touched ([`Resource::refresh`]) and reading
    /// what is demanded ([`Resource::load`]). A test comparing it with
    /// [`Self::state`] proves the resource's incremental reads exact.
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

    /// A second replica sharing this one's history, as a member who
    /// just joined would hold.
    #[must_use]
    pub fn fork(&mut self) -> Self {
        Self {
            doc: self.doc.fork(),
            registry: ActionRegistry::with(crate::group::register::<R>),
            log: Vec::new(),
            author: None,
            acting: None,
            invoker: None,
            // The fork acts in the same app's record as the copy it is taken from.
            app: self.app.clone(),
            appearance: None,
            name: None,
            group: None,
            serving: None,
            now_ms: self.now_ms,
            reader: None,
            marker: std::marker::PhantomData,
            record: self.record.clone(),
        }
    }

    /// Exchange every change with `other`, as sync does.
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

    pub fn state_at(&mut self, heads: &[ChangeHash]) -> Result<R::State, ModelError> {
        history::read_at::<R>(&mut self.doc, heads)
    }
}

/// A replica whose actions name the app its record belongs to.
impl<R: Resource> Replica<R, AppNamed> {
    /// Invoke a published action exactly as a control, the palette, an
    /// agent or a script does: through the API, which refuses one that
    /// asks for confirmation ([`Self::apply_confirmed`]).
    pub fn apply(&mut self, invoke: impl Into<Invoke<R>>) -> Result<&ActionEnvelope, ApiError> {
        self.invoke(invoke.into(), false)
    }

    /// [`Self::apply`] after the person confirmed it.
    ///
    /// # Errors
    /// As [`Self::apply`], without asking for confirmation.
    pub fn apply_confirmed(
        &mut self,
        invoke: impl Into<Invoke<R>>,
    ) -> Result<&ActionEnvelope, ApiError> {
        self.invoke(invoke.into(), true)
    }

    /// Apply one typed action value, the path a custom surface takes once
    /// its person confirmed what it does: the generic
    /// [`APPLY`](crate::APPLY) action, through the API.
    pub fn apply_action(&mut self, action: &R::Action) -> Result<&ActionEnvelope, ApiError> {
        let invoke =
            Invoke::apply(action).map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
        self.invoke(invoke, true)
    }
}
