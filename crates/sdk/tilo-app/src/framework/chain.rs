//! Records an app keeps beside its own as a chain of parts: what grows
//! without end — a chat's messages, a log — kept as numbered records of
//! another resource type ([`DocumentId::part`]) in the workspace the app's record
//! lives in.
//!
//! One record cannot hold everything such an app ever wrote: a record's
//! snapshot has a size limit, and a newcomer would be sent all of it. A
//! chain keeps it in parts. The part with the highest number a device holds
//! is the chain's head; a workspace's host replays a joining device the head
//! only, and an earlier part reaches a device when it opens it
//! ([`Chain::hold`]). Two writers that start the same new part start the
//! same record (each part's genesis is the same everywhere), so their
//! changes meet there.
//!
//! The resource of the parts says how they are used ([`Chained`]): which
//! part an action changes (the one holding the item it names, or for a new
//! item the head or the part after it), and what an action reads of the
//! app's own record where it is invoked. Everything else is the framework's:
//!
//! - **One API.** The chain is published beside the app's resource as one
//!   resource ([`Chain::id`]): the head's actions, and every held part's
//!   items with theirs, each carried out on the part [`Chained::part_for`]
//!   names when it is invoked. The palette, agents, scripts and the app's
//!   own surface reach the parts through it, checked against the
//!   member's roles in the workspace like the app's own actions (the parts
//!   carry the app record's grants), attributed to whoever invokes them and
//!   written into the part's history.
//! - **Held where the app is.** The parts are records of the app's own
//!   store, so whatever syncs the app's records — the browser's workspace, an
//!   attached view — syncs them on their own routes.
//! - **Read as they change.** [`Chain::parts`] is every part the window
//!   holds, oldest first, followed change by change; [`Chain::revision`]
//!   moves whenever any of them changed, or a new head arrived.
//!
//! An app declares its chain in [`ResourceApp::records`](super::ResourceApp::records)
//! (`records.chain::<Part>()`), so it is opened and published with the
//! window, and reaches it from a view or surface with
//! [`ResourceEnv::chain`](super::ResourceEnv::chain). Edits of a part are not
//! undone with the app's own: they are recorded in the part's history, and
//! the chain publishes the history verbs (`undo`, `redo`, `restore`,
//! `checkpoint`) naming the part they change, so a person, an agent or the
//! workspace's moderation takes back steps in a part through the same API.
//!
//! A part opened in the app's store by anyone — a peer's merged into an
//! attached view, one this device asked the workspace for — is held from the
//! next read on.
//!
//! Where the store's records come from the workspace's hosts — a page's, and a
//! native app's, which asks its session for them — the chain asks them for its
//! head as it opens ([`DocumentId::chain_head`]): a public workspace sends a
//! newcomer no part of it until then, and the head they send is held like any
//! part that arrives. Until they answered, a window that holds no part does
//! not know which part is the newest, and writes nothing
//! ([`Chain::head_arriving`]).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tilo_actions::{ActionContext, ActionError, ActionInvoker, OperationKind};
use tilo_resource::{Invoke, Reader, Resource};
use ui_core::automerge::transaction::Transactable as _;
use ui_core::automerge::{ChangeHash, ObjType, ROOT, ReadDoc as _, Value};
use ui_core::permissions::{LivePermissions, PermissionScope, TreePath};
use ui_core::resource::{
    DocumentCheckout, DocumentId, ResourceOpenError, ResourceStore, ResourceType,
};

use super::dispatch::Dispatcher;
use super::history::{PART_EDIT_TYPE, PartEdit};
use super::live::Live;
use super::record::ResourceApi;

/// The operation type a chain's published actions are carried out under:
/// the host applies the action to the part of the chain it names.
pub(crate) const CHAIN_ACTION_TYPE: &str = "tilo.app.chain-action";

/// The operation type the read a chain publishes is answered under: what the
/// chain holds, from the parts this device holds, rather than the record the
/// parts are kept beside.
pub(crate) const CHAIN_READ_TYPE: &str = "tilo.app.chain-read";

/// The items one page of a chain's read answers with, when the caller names
/// no size.
const READ_PAGE: usize = 50;

/// The most one page of a chain's read answers with.
const READ_PAGE_MAX: usize = 200;

/// Where an earlier part a window asked for ([`Chain::hold`]) stands until
/// what the workspace has of it is read here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartArrival {
    /// Asked for; what the workspace has of it is on its way.
    Arriving,
    /// The workspace's hosts sent everything they hold, and none of it was this
    /// part, or no host is connected to ask: nothing more comes until that
    /// changes (a host connects, someone who has it shares it).
    Unavailable,
}

/// A resource whose records are the parts of a chain kept beside the app's
/// own record ([`DocumentId::part`]). See the module docs.
pub trait Chained: Resource {
    /// The app's own resource: what [`Self::prepare`] reads where an action
    /// is invoked.
    type Base: Resource;

    /// The chain the parts belong to: part `n` is `chain().part(n)`.
    fn chain() -> DocumentId;

    /// The chain that belongs beside `base` rather than beside the app's own
    /// record of the workspace: the messages of one conversation, kept beside
    /// its record ([`DocumentId::of_direct`]), and the parts of an app that
    /// keeps a record per member, kept beside the member's own
    /// ([`DocumentId::of_member`]). The default is [`Self::chain`], for every
    /// chain there is only one of.
    fn chain_beside(base: &DocumentId) -> DocumentId {
        let _ = base;
        Self::chain()
    }

    /// The places the app derives from everything the chain holds — a book's
    /// groups, its favorites, its likely duplicates — computed where the
    /// chain is published from the app's record and the parts held, and
    /// published as the chain's own children beside its items.
    fn places(
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Vec<tilo_resource::Derived<Self>> {
        let _ = (base, parts);
        Vec::new()
    }

    /// The part `action` changes, given the parts this device holds, oldest
    /// first: the one holding the item it names, or for a new item the head
    /// (or the next part, once the head is full).
    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self::State>)]) -> Result<u64, String>;

    /// Fill in, where the action is invoked, what it records from the app's
    /// own record (who a mention names), and refuse what that record does
    /// not allow (a post to a channel it has not got). A part's action
    /// itself reads only its part, so it replays the same on every replica.
    ///
    /// `parts` are the parts this device holds, oldest first: what an
    /// action checks across items of other parts reads them here (a task
    /// placed before one kept in another part, a parent that must exist),
    /// so the verb itself still reads only its own part.
    fn prepare(
        action: &mut Self::Action,
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Result<(), String> {
        let _ = (action, base, parts);
        Ok(())
    }

    /// Whether a window holds every part of the chain rather than its head
    /// and what a reader scrolled back to: for a chain whose views list all
    /// of it (a tracker's issues), where a part is a bound on what one
    /// change costs, not on what a device holds.
    const HELD_WHOLE: bool = false;

    /// What a part started after part `previous` (whose state is `part`)
    /// carries of the parts before it, written into it as it starts, before
    /// the action that started it: a summary a reader needs of parts it
    /// does not hold (a chat's message counts for unread badges). Whoever
    /// starts a part holds the one before. `None` carries nothing.
    fn carry(previous: u64, part: &Self::State) -> Option<Invoke<Self>> {
        let _ = (previous, part);
        None
    }

    /// Where in the app's own record `action` happens, named as everyone
    /// the record is shared with sees that place (a post's channel,
    /// `#general`): history keeps it on the step
    /// ([`tilo_actions::PLACE_FACET`]), so what someone is doing can say
    /// where. `None` for an action of no such place, and for a place not
    /// everyone sees (a direct conversation).
    fn place(action: &Self::Action, base: &<Self::Base as Resource>::State) -> Option<String> {
        let _ = (action, base);
        None
    }
}

/// How a part was opened ([`ChainState::open_or_start`]).
enum Opened {
    /// Held already, or read from this device.
    Held,
    /// Started from its genesis here: nothing of it was on this device.
    Started,
    /// Asked of the workspace, through which the store reads records; not open
    /// until it arrives.
    Asked,
}

/// Why a part is not held here, and whether that is only for this moment.
struct Unopened {
    reason: String,
    /// Another process held the part's store for a write while it was looked
    /// for (a save, the workspace adopting the part): the part opens at the
    /// next refresh, so a view keeps waiting for it rather than being told
    /// it is unavailable.
    retry: bool,
}

impl From<ResourceOpenError> for Unopened {
    fn from(error: ResourceOpenError) -> Self {
        Self {
            retry: error.writer_busy(),
            reason: error.to_string(),
        }
    }
}

impl From<String> for Unopened {
    fn from(reason: String) -> Self {
        Self {
            reason,
            retry: false,
        }
    }
}

/// What a window holds a chain with: the store of the app's records, who
/// edits, and what they may do in the workspace.
#[derive(Clone)]
pub(crate) struct WorkspaceResources {
    pub(crate) store: ResourceStore,
    pub(crate) me: tilo_resource::MemberId,
    pub(crate) permissions: Option<LivePermissions>,
    pub(crate) app_id: &'static str,
    pub(crate) app_title: &'static str,
}

/// The parts a window holds, oldest first, by their numbers.
pub type Parts<P> = Rc<[(u64, Rc<<P as Resource>::State>)]>;

/// What each held part published, by its number: the state and language it
/// was built from, and the publication.
type PartsPublished<P> =
    BTreeMap<u64, (Rc<<P as Resource>::State>, u64, tilo_actions::ResourceSpec)>;

/// What search found in each held part, by its number, with the state it was
/// read from.
type PartsSearched<P> = BTreeMap<
    u64,
    (
        Rc<<P as Resource>::State>,
        Rc<Vec<tilo_resource::SearchEntry>>,
    ),
>;

/// The app's record as a part's [`Chained::prepare`] reads it.
type Base<P> = Rc<dyn Fn() -> Rc<<<P as Chained>::Base as Resource>::State>>;

/// A chain held for a window: its parts, read as they change. See the
/// [module docs](self).
pub(crate) struct ChainState<P: Chained> {
    workspace: WorkspaceResources,
    base: Base<P>,
    /// The chain the parts belong to.
    chain: DocumentId,
    id: String,
    kind: ResourceType,
    parts: RefCell<BTreeMap<u64, Live<P>>>,
    /// Earlier parts asked for ([`Self::hold`]) that are not read here
    /// yet: held from their genesis, or not open at all while the workspace
    /// sends them.
    waiting: RefCell<BTreeMap<u64, PartArrival>>,
    /// What a part holds before anything was written to it: what the chain
    /// publishes before it has a head.
    empty: Rc<P::State>,
    revision: Cell<u64>,
    /// The store's [`ResourceStore::kept_revision`] when the window last
    /// looked for a newer head and at the parts it waits for.
    looked: Cell<Option<u64>>,
    published: RefCell<Option<((u64, u64), tilo_actions::ResourceSpec)>>,
    /// What each held part published, with the state and language it was
    /// built from ([`Self::part_published`]).
    part_published: RefCell<PartsPublished<P>>,
    /// The parts held, as view state that follows every change
    /// ([`Chain::state`]), and the revision it was last written at.
    state: ui_core::LocalState<Parts<P>>,
    state_at: Cell<Option<u64>>,
    /// The parts this window's person wrote into here, newest last: what
    /// the window's Undo takes back first ([`Chain::undo_last`]), and the
    /// parts it took back from, for Redo.
    written: RefCell<Vec<u64>>,
    undone: RefCell<Vec<u64>>,
    /// What search found in each held part, with the state it was read from.
    part_search: RefCell<PartsSearched<P>>,
    /// The person's record of the actions asked for while the part they
    /// change was on its way from the workspace ([`ChainState::apply_as`]),
    /// kept with the app's records: every window of the app shows them and
    /// applies them in order once the part is here, across a closed window
    /// and a restart.
    waiting_resource: DocumentCheckout,
    /// Whether [`Self::waiting_resource`] was looked for on this device.
    unsent_looked: Cell<bool>,
    /// What [`Self::waiting_resource`] writes into this chain, as a part holding only
    /// that reads ([`ChainState::unwritten`]), at the record's heads.
    unwritten: RefCell<Unwritten<P>>,
    /// Whether the actions that waited are being applied now.
    draining: Cell<bool>,
}

/// What to do once an action that waited for its part was applied (the
/// part's number) or refused.
pub(crate) type Later = Box<dyn FnOnce(Result<u64, ActionError>)>;

/// What the actions waiting for their part write, read as a part, at the
/// heads of the record that keeps them.
type Unwritten<P> = (Vec<ChangeHash>, Option<Rc<<P as Resource>::State>>);

/// The kind of the record of a person's actions waiting for their part.
const UNSENT_KIND: &str = tilo_actions::surface::CHAIN_UNSENT_KIND;

thread_local! {
    /// Who waits to be told what became of an action that waited for its
    /// part, by its entry in the record of them: told by whichever window
    /// applies it.
    static ANSWERS: RefCell<HashMap<String, Later>> = RefCell::default();
}

/// An action that waits for the part it changes to arrive, as the record
/// of them keeps it: `id` names its entry there.
struct Unsent<P: Chained> {
    id: String,
    action: P::Action,
    invoker: ActionInvoker,
}

/// What became of an action applied to the part it changes.
enum Write {
    /// Written into the part numbered first, creating the item resources
    /// listed second.
    Done(u64, Vec<String>),
    /// The part is on its way from the workspace; nothing was written.
    Arriving,
}

/// Why a part still on its way cannot be opened for what needs it now.
const PART_ARRIVING: &str = "the part is on its way from the workspace";

impl<P: Chained> ChainState<P> {
    /// Hold the chain in `workspace`, with its head when this device has one.
    pub(crate) fn open(workspace: WorkspaceResources, base: Base<P>) -> Option<Self> {
        Self::open_at(workspace, base, P::chain())
    }

    /// [`Self::open`] of the chain `chain` rather than the app's own: the
    /// messages of one direct conversation, which lives beside the record
    /// the two people share ([`Chained::chain_beside`]).
    pub(crate) fn open_at(
        workspace: WorkspaceResources,
        base: Base<P>,
        chain: DocumentId,
    ) -> Option<Self> {
        let mut doc = ui_core::automerge::AutoCommit::new();
        P::initialize(&mut doc)
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "chain part has no genesis"))
            .ok()?;
        let empty = Reader::<P>::read(&doc)
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "chain part unreadable"))
            .ok()?
            .state()
            .clone();
        let id = ui_core::resource::chain_resource_id(&chain);
        let state = Self {
            base,
            chain,
            id,
            kind: ResourceType::new(P::KIND),
            parts: RefCell::default(),
            waiting: RefCell::default(),
            empty,
            revision: Cell::new(0),
            looked: Cell::new(None),
            published: RefCell::new(None),
            part_published: RefCell::default(),
            state: ui_core::LocalState::new(Rc::default()),
            state_at: Cell::new(None),
            written: RefCell::default(),
            undone: RefCell::default(),
            part_search: RefCell::default(),
            waiting_resource: unsent_checkout(&workspace.me),
            unsent_looked: Cell::new(false),
            unwritten: RefCell::default(),
            draining: Cell::new(false),
            workspace,
        };
        // Asked for as the chain opens, with the window's other records: the
        // newest part arrives in the same round trip. A chain with a part
        // here already knows its newest one, and asks for nothing.
        if state.head().is_none() {
            let _ = state.head_on_its_way();
        }
        state.look();
        Some(state)
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    fn checkout(&self, index: u64) -> DocumentCheckout {
        DocumentCheckout::new(self.chain.part(index), self.kind.clone())
    }

    /// The parts held, oldest first.
    pub(crate) fn parts(&self) -> Vec<(u64, Rc<P::State>)> {
        self.refresh();
        self.parts
            .borrow()
            .iter()
            .map(|(index, live)| (*index, live.get()))
            .collect()
    }

    /// The parts held, oldest first, as they were last followed.
    fn held(&self) -> Vec<(u64, Rc<P::State>)> {
        self.parts
            .borrow()
            .iter()
            .map(|(index, live)| (*index, live.get()))
            .collect()
    }

    pub(crate) fn head(&self) -> Option<u64> {
        self.refresh();
        self.parts.borrow().keys().next_back().copied()
    }

    /// Whether no part is held here and the workspace has not yet said which
    /// one is the newest ([`Self::head_on_its_way`]).
    pub(crate) fn head_arriving(&self) -> bool {
        self.head().is_none() && self.head_on_its_way()
    }

    /// Whether the workspace may still send this chain's newest part: it has
    /// not said it has no part at all, and opening the chain's head
    /// ([`DocumentId::chain_head`]) found none of it here. That open is what
    /// asks the store's workspace for the head — a page's hosts, or a native
    /// app's — and it answers `ResourceOpenError::Arriving` until the part
    /// arrives, or until the workspace answers it has none
    /// ([`ResourceStore::workspace_answered`]).
    fn head_on_its_way(&self) -> bool {
        let head = self.chain.chain_head();
        if self.workspace.store.workspace_answered(&head) {
            return false;
        }
        // A store written at this moment cannot say which part is the newest: the head is
        // waited for rather than taken as the workspace's word that there is none.
        matches!(
            self.workspace
                .store
                .try_open_resource(&DocumentCheckout::new(head, self.kind.clone())),
            Err(ResourceOpenError::Arriving | ResourceOpenError::Busy)
        )
    }

    pub(crate) fn revision(&self) -> u64 {
        self.refresh();
        self.revision.get()
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Follow every held part that changed, and take up a newer head that
    /// arrived on this device. Whether anything moved.
    pub(crate) fn refresh(&self) -> bool {
        let mut moved = false;
        for (index, live) in self.parts.borrow().iter() {
            match self
                .workspace
                .store
                .read(&self.chain.part(*index), |doc| live.sync(doc))
            {
                Some(Ok(true)) => {
                    // Its history arrived, or a first change was made here.
                    self.waiting.borrow_mut().remove(index);
                    moved = true;
                }
                Some(Err(error)) => {
                    tracing::warn!(kind = P::KIND, part = index, %error, "chain part unreadable");
                }
                _ => {}
            }
            let _ = live.take_change();
        }
        // A part opened in the store since (a peer's merged in, one this
        // device asked for and received) is held at once.
        let opened: Vec<u64> = {
            let parts = self.parts.borrow();
            self.workspace
                .store
                .open_parts(&self.chain)
                .into_iter()
                .filter(|index| !parts.contains_key(index))
                .collect()
        };
        for index in opened {
            moved |= self.follow(index).is_ok();
        }
        if P::HELD_WHOLE {
            self.hold_earlier();
        }
        if self.due() {
            moved |= self.look();
            moved |= self.follow_waiting();
        }
        moved |= self.follow_unsent();
        if self.workspace.store.takes_writes() {
            self.apply_pending();
        }
        if moved {
            self.moved();
        }
        let revision = self.revision.get();
        if self.state_at.replace(Some(revision)) != Some(revision) {
            self.state.set(self.held().into());
        }
        moved
    }

    /// Apply the actions that wait for their part, in order, while their part
    /// is here: what is still on its way waits on, and the ones after it too.
    /// Each leaves the record of them once written or refused.
    fn apply_pending(&self) {
        // Writing one refreshes the chain, which comes back here.
        if self.draining.replace(true) {
            return;
        }
        for entry in self.unsent() {
            let written = match self.try_apply(&entry.action, &entry.invoker) {
                Ok(Write::Arriving) => break,
                Ok(Write::Done(index, _)) => Ok(index),
                Err(error) => Err(error),
            };
            self.forget_unsent(&entry.id);
            answer::<P>(&entry.id, written);
        }
        self.draining.set(false);
        if self.follow_unsent() {
            self.moved();
        }
    }

    /// Apply `action`, invoked by `invoker`, to the part it changes: read
    /// against the app's record ([`Chained::prepare`]), checked against
    /// what they may do in the workspace, attributed to them and written into
    /// the part's history. The part it was written to, with the item
    /// resources it created.
    ///
    /// While that part is on its way from the workspace (a device back from
    /// away, before its head arrived) the action waits for it: `Ok(None)`.
    /// It is kept in the person's record of such actions with the app's
    /// records, shown in [`Self::unwritten`] by every window of the app, and
    /// applied by whichever holds the chain once the part is here, after
    /// those that waited before it — after the window closed or the app
    /// restarted too. `later` is taken then: told the part or why it was
    /// refused if this process still runs; a refusal nobody waits for is
    /// logged. Otherwise it is left for the caller to answer now.
    ///
    /// # Errors
    /// As the API refuses it now: the person may not, the app's record or
    /// the part refused it, or the record of waiting actions cannot be
    /// written.
    pub(crate) fn apply_as(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
        later: &mut Option<Later>,
    ) -> Result<Option<(u64, Vec<String>)>, ActionError> {
        // Behind the ones already waiting: actions are applied in the order asked.
        if self.unsent().is_empty() {
            match self.try_apply(action, invoker)? {
                Write::Arriving => {}
                Write::Done(index, created) => {
                    // Only what the window's person did themselves: an
                    // agent's step is its own to take back.
                    if invoker.acting.by.member_id() == Some(&self.workspace.me) {
                        self.written.borrow_mut().push(index);
                        self.undone.borrow_mut().clear();
                    }
                    return Ok(Some((index, created)));
                }
            }
        }
        let id = self.keep_unsent(action, invoker)?;
        if let Some(later) = later.take() {
            ANSWERS.with(|answers| answers.borrow_mut().insert(id, later));
        }
        if self.follow_unsent() {
            self.moved();
        }
        Ok(None)
    }

    /// What the actions waiting for their part write ([`Self::apply_as`]),
    /// read as a part holding only that: what a view shows as being sent.
    /// `None` while nothing waits.
    pub(crate) fn unwritten(&self) -> Option<Rc<P::State>> {
        if self.follow_unsent() {
            self.moved();
        }
        self.unwritten.borrow().1.clone()
    }

    /// Read the record of waiting actions again when it changed (here, in
    /// another window, on another of the person's devices). Whether it did.
    fn follow_unsent(&self) -> bool {
        // Not here (nothing waits), or read by whatever runs now: as it was.
        let Some(heads) = self
            .open_unsent(false)
            .then(|| {
                self.workspace.store.read(
                    &self.waiting_resource.id,
                    ui_core::automerge::AutoCommit::base_heads,
                )
            })
            .flatten()
        else {
            return false;
        };
        if self.unwritten.borrow().0 == heads {
            return false;
        }
        let unwritten = self.read_unwritten(&self.unsent());
        *self.unwritten.borrow_mut() = (heads, unwritten);
        true
    }

    fn read_unwritten(&self, unsent: &[Unsent<P>]) -> Option<Rc<P::State>> {
        if unsent.is_empty() {
            return None;
        }
        let mut doc = ui_core::automerge::AutoCommit::new();
        P::initialize(&mut doc).ok()?;
        let base = (self.base)();
        // As held, without a refresh: this is read while one runs.
        let parts = self.held();
        for entry in unsent {
            let mut action = entry.action.clone();
            if P::prepare(&mut action, &base, &parts).is_err() {
                continue;
            }
            let Ok(encoded) = tilo_resource::encode::<P>(&action) else {
                continue;
            };
            let context = ActionContext::for_resource(&self.id, P::KIND, P::LABEL)
                .with_app(self.workspace.app_id, self.workspace.app_title)
                .with_invoker(entry.invoker.clone());
            let writer = ui_core::bound_dispatch::document_writer(&entry.invoker.acting.by, &doc);
            if let Err(error) = tilo_actions::apply_local_with_context(
                &mut doc,
                &action,
                encoded,
                Some(&writer),
                context,
            ) {
                tracing::warn!(kind = P::KIND, %error, "a write waiting for its part cannot be shown");
            }
        }
        Reader::<P>::read(&doc)
            .map(|reader| Rc::clone(reader.state()))
            .ok()
    }

    /// Open the record of waiting actions where this device keeps it, once;
    /// with `create`, start it where nothing of it is here.
    fn open_unsent(&self, create: bool) -> bool {
        let store = &self.workspace.store;
        if store.is_ready(&self.waiting_resource.id) {
            return true;
        }
        if self.unsent_looked.replace(true) && !create {
            return false;
        }
        match store.open_if_held(&self.waiting_resource) {
            Ok(_) => true,
            // Nothing of it here, or a store that keeps no canonical records
            // (a test's): started as every record is.
            Err(ResourceOpenError::MissingResource | ResourceOpenError::IdentityMismatch) if create => {
                self.create(&self.waiting_resource, |_| Ok(()))
                    .map_err(|error| {
                        tracing::warn!(kind = P::KIND, %error, "the record of waiting writes cannot be started");
                    })
                    .is_ok()
            }
            Err(ResourceOpenError::MissingResource | ResourceOpenError::IdentityMismatch) => false,
            // Read by whatever runs now: looked for again at the next refresh.
            Err(ResourceOpenError::Busy) => {
                self.unsent_looked.set(false);
                false
            }
            Err(error) => {
                tracing::warn!(kind = P::KIND, %error, "the record of waiting writes cannot be opened");
                false
            }
        }
    }

    /// The actions waiting for a part of this chain, oldest first.
    fn unsent(&self) -> Vec<Unsent<P>> {
        if !self.open_unsent(false) {
            return Vec::new();
        }
        self.workspace
            .store
            .read(&self.waiting_resource.id, |doc| {
                read_unsent::<P>(doc, &self.chain)
            })
            .unwrap_or_default()
    }

    /// Keep `action`, invoked by `invoker`, in the record of waiting
    /// actions, after the others. Its entry there.
    fn keep_unsent(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<String, ActionError> {
        if !self.open_unsent(true) {
            return Err(ActionError::Refused(PART_ARRIVING.into()));
        }
        let entry = serde_json::to_string(&(action, invoker)).map_err(ActionError::Encode)?;
        let chain = self.chain.as_str().to_owned();
        self.edit_unsent(move |doc| {
            let list = match doc.get(ROOT, chain.as_str())? {
                Some((Value::Object(ObjType::List), list)) => list,
                _ => doc.put_object(ROOT, chain.as_str(), ObjType::List)?,
            };
            doc.insert(&list, doc.length(&list), entry)?;
            let kept = doc.get(&list, doc.length(&list).saturating_sub(1))?;
            Ok(kept.map(|(_, id)| id.to_string()).unwrap_or_default())
        })
        .map_err(ActionError::Refused)
    }

    /// Take the entry `id` out of the record of waiting actions.
    fn forget_unsent(&self, id: &str) {
        let chain = self.chain.as_str().to_owned();
        let forgotten = self.edit_unsent(move |doc| {
            let Some((Value::Object(ObjType::List), list)) = doc.get(ROOT, chain.as_str())? else {
                return Ok(());
            };
            let at = (0..doc.length(&list)).find(
                |at| matches!(doc.get(&list, *at), Ok(Some((_, entry))) if entry.to_string() == id),
            );
            if let Some(at) = at {
                doc.delete(&list, at)?;
            }
            Ok(())
        });
        if let Err(error) = forgotten {
            tracing::error!(kind = P::KIND, %error, "a written write still waits in its record");
        }
    }

    /// Change the record of waiting actions: nobody's undo step, saved as
    /// any edit of the app's records.
    fn edit_unsent<T>(
        &self,
        edit: impl FnOnce(
            &mut ui_core::automerge::AutoCommit,
        ) -> Result<T, ui_core::automerge::AutomergeError>,
    ) -> Result<T, String> {
        let id = &self.waiting_resource.id;
        let edited = self
            .workspace
            .store
            .with_history(id, |doc, history| {
                let edited = edit(doc).map_err(|error| error.to_string())?;
                doc.commit_with(
                    ui_core::automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Native.change_message()),
                );
                history.rebase(doc);
                Ok::<_, String>(edited)
            })
            .ok_or("the record of waiting writes is in use")??;
        if let Err(error) = self.workspace.store.schedule_committed_history(id) {
            tracing::error!(%error, "the record of waiting writes could not be saved");
        }
        Ok(edited)
    }

    /// Whether to look for a newer head and at the parts waited for now:
    /// the store was told a record arrived or what the workspace said of one
    /// ([`ResourceStore::note_kept`], [`ResourceStore::announce`],
    /// [`ResourceStore::note_workspace_answered`]), each of which wakes the window.
    /// Not while a frame reads the store (a view's sync): opening a part
    /// takes the store's writer, so the look waits for the frame's settle,
    /// where the window takes up what it was sent, and the view's sync reads
    /// the part it opened.
    fn due(&self) -> bool {
        self.workspace.store.takes_writes()
            && self.looked.get() != Some(self.workspace.store.kept_revision())
    }

    /// Open every part waited for that arrived since, and mark unavailable
    /// those the workspace said it has no more of. Whether any changed.
    fn follow_waiting(&self) -> bool {
        let waiting: Vec<(u64, PartArrival)> = self
            .waiting
            .borrow()
            .iter()
            .map(|(index, arrival)| (*index, *arrival))
            .collect();
        let mut moved = false;
        for (index, arrival) in waiting {
            let held = self.parts.borrow().contains_key(&index);
            if !held {
                match self.open_or_start(index) {
                    // Here now, with what the workspace sent of it.
                    Ok(Opened::Held | Opened::Started) => {
                        self.waiting.borrow_mut().remove(&index);
                        moved = true;
                        continue;
                    }
                    Ok(Opened::Asked) => {}
                    Err(Unopened {
                        reason,
                        retry: true,
                    }) => {
                        // Another writer held the part's store just now: it is
                        // waited for again at the next refresh.
                        tracing::warn!(kind = P::KIND, part = index, %reason, "earlier part not opened yet");
                        self.looked.set(None);
                    }
                    Err(Unopened { reason, .. }) => {
                        tracing::warn!(kind = P::KIND, part = index, %reason, "earlier part not opened");
                    }
                }
            }
            let now = if self
                .workspace
                .store
                .workspace_answered(&self.chain.part(index))
            {
                PartArrival::Unavailable
            } else {
                PartArrival::Arriving
            };
            if now != arrival {
                self.waiting.borrow_mut().insert(index, now);
                moved = true;
            }
        }
        moved
    }

    /// Open every part newer than the head that this device holds. Whether
    /// one was.
    fn look(&self) -> bool {
        let looked = self.workspace.store.kept_revision();
        // Holding no part, the chain asks for its head, should its store have
        // come to read records through the workspace since it opened.
        if self.parts.borrow().is_empty() {
            let _ = self.head_on_its_way();
        }
        let head = self.parts.borrow().keys().next_back().copied();
        // Only the newest is the head; the parts between are opened when
        // someone reads back to them.
        let kept = self.workspace.store.held_parts(&self.chain).last().copied();
        // A part a peer started is the same record here (each part's genesis
        // is the same everywhere): started here, it syncs on its route.
        let started = self
            .workspace
            .store
            .announced_parts(&self.chain)
            .last()
            .copied();
        let Some(newest) = kept.max(started) else {
            self.looked.set(Some(looked));
            return false;
        };
        if head.is_some_and(|head| newest <= head) {
            self.looked.set(Some(looked));
            return false;
        }
        let opened = if kept.is_some_and(|kept| kept >= newest) {
            self.hold_kept(newest)
        } else {
            // A part a peer started, started here from its genesis: what
            // they wrote in it is on its way, and a view of it waits for
            // that rather than reading back from an empty head.
            self.open_or_start(newest).and_then(|opened| match opened {
                Opened::Held => Ok(()),
                Opened::Started => {
                    self.waiting
                        .borrow_mut()
                        .insert(newest, PartArrival::Arriving);
                    Ok(())
                }
                Opened::Asked => Err(Unopened {
                    reason: PART_ARRIVING.into(),
                    retry: false,
                }),
            })
        };
        match opened {
            Ok(()) => {
                self.looked.set(Some(looked));
                true
            }
            Err(Unopened { reason, retry }) => {
                // A part another writer held for this moment is not gone: it
                // is looked for again at the next refresh, rather than only
                // once this device is told of something new. Nothing is
                // marked moved, so a failed look wakes nobody and cannot
                // make a pass of its own.
                tracing::warn!(kind = P::KIND, part = newest, %reason, "chain head not opened");
                self.looked.set(if retry { None } else { Some(looked) });
                false
            }
        }
    }

    /// Refuse an action on a part that is not held here, arming the next
    /// refresh to take it up when another writer held its store for this
    /// moment ([`Unopened::retry`]).
    fn refused(&self, error: Unopened) -> ActionError {
        if error.retry {
            self.looked.set(None);
        }
        ActionError::Refused(error.reason)
    }

    /// Hold part `index`, which this device keeps: a part a history verb
    /// names is one someone here wrote to.
    fn hold_kept(&self, index: u64) -> Result<(), Unopened> {
        if self.parts.borrow().contains_key(&index) {
            return Ok(());
        }
        let checkout = self.checkout(index);
        if !self.workspace.store.is_ready(&checkout.id) {
            self.workspace
                .store
                .try_open_resource(&checkout)
                .map_err(Unopened::from)?;
        }
        self.follow(index).map_err(|reason| Unopened {
            reason,
            retry: false,
        })
    }

    /// Hold part `index`: read it from this device, or, when this device
    /// has none of it, ask the workspace for it. A store that reads records
    /// through the workspace (a native app's) asks for it as it opens it; any
    /// other starts it from its genesis (the same change on every device),
    /// so it is open here and the workspace's record sync asks the workspace for its
    /// history, which merges in as it arrives. It is waited for until then
    /// ([`Self::waiting`]); its arrival, or the workspace saying it has none of
    /// it, wakes the window ([`ResourceStore::wake_on_arrival`]).
    pub(crate) fn hold(&self, index: u64) -> Result<(), String> {
        match self.open_or_start(index) {
            Ok(Opened::Held) => {}
            Ok(Opened::Started | Opened::Asked) => {
                self.waiting
                    .borrow_mut()
                    .insert(index, PartArrival::Arriving);
                // Looked at again at the next refresh, with what the store
                // already knows of it.
                self.looked.set(None);
                self.moved();
            }
            Err(Unopened {
                retry: true,
                reason,
            }) => {
                // Another writer holds the part's store for this moment: the
                // part is on its way here, and reading it at the next
                // refresh succeeds.
                tracing::warn!(kind = P::KIND, part = index, %reason, "part waits for another writer");
                self.waiting
                    .borrow_mut()
                    .insert(index, PartArrival::Arriving);
                self.looked.set(None);
                self.moved();
            }
            Err(Unopened { reason, .. }) => return Err(reason),
        }
        Ok(())
    }

    /// Hold every part before the head that is neither held nor asked for:
    /// a chain whose views list all of it ([`Chained::HELD_WHOLE`]).
    fn hold_earlier(&self) {
        let missing: Vec<u64> = {
            let parts = self.parts.borrow();
            let waiting = self.waiting.borrow();
            let Some(&head) = parts.keys().next_back() else {
                return;
            };
            (0..head)
                .filter(|index| !parts.contains_key(index) && !waiting.contains_key(index))
                .collect()
        };
        for index in missing {
            // A part before the head was started before it, so it exists
            // somewhere: one not on this device is asked for, never started
            // here empty.
            let checkout = self.checkout(index);
            if !self.workspace.store.is_ready(&checkout.id)
                && matches!(
                    self.workspace.store.try_open_resource(&checkout),
                    Err(ResourceOpenError::MissingResource)
                )
            {
                self.workspace.store.want(checkout.id);
                continue;
            }
            if let Err(reason) = self.hold(index) {
                tracing::warn!(kind = P::KIND, part = index, %reason, "earlier part not held");
            }
        }
    }

    /// Where part `index`, asked for and not read here yet, stands; `None`
    /// once it is read (or was never asked for).
    pub(crate) fn waiting(&self, index: u64) -> Option<PartArrival> {
        self.waiting.borrow().get(&index).copied()
    }

    /// Every part asked for and not read here yet, oldest first.
    pub(crate) fn waited(&self) -> Vec<(u64, PartArrival)> {
        self.waiting
            .borrow()
            .iter()
            .map(|(index, arrival)| (*index, *arrival))
            .collect()
    }

    /// Hold part `index`, starting it when nobody has.
    fn start(&self, index: u64) -> Result<(), String> {
        match self.open_or_start(index) {
            Ok(Opened::Held | Opened::Started) => Ok(()),
            Ok(Opened::Asked) => Err(PART_ARRIVING.into()),
            Err(error) => {
                if error.retry {
                    // Another writer's write is done by the next refresh,
                    // which holds the part then.
                    self.looked.set(None);
                }
                Err(error.reason)
            }
        }
    }

    /// Hold part `index`, kept on this device or started from its genesis,
    /// or ask the workspace for it where the store reads records through it.
    fn open_or_start(&self, index: u64) -> Result<Opened, Unopened> {
        if self.parts.borrow().contains_key(&index) {
            return Ok(Opened::Held);
        }
        let checkout = self.checkout(index);
        let mut opened = Opened::Held;
        if !self.workspace.store.is_ready(&checkout.id) {
            match self.workspace.store.try_open_resource(&checkout) {
                Ok(_) => {}
                Err(ResourceOpenError::Arriving) => return Ok(Opened::Asked),
                // Nothing of it on this device, or a store that keeps no
                // canonical records (a test's) and has no record it could
                // open: it is started as every record is.
                Err(ResourceOpenError::MissingResource | ResourceOpenError::IdentityMismatch) => {
                    self.create(&checkout, |doc| {
                        P::initialize(doc).map_err(|error| error.to_string())
                    })?;
                    opened = Opened::Started;
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.follow(index).map_err(|reason| Unopened {
            reason,
            retry: false,
        })?;
        Ok(opened)
    }

    /// Create the record `checkout` names, with `initialize` its genesis:
    /// the same on every device, so two that start it converge.
    fn create(
        &self,
        checkout: &DocumentCheckout,
        initialize: impl Fn(&mut ui_core::automerge::AutoCommit) -> Result<(), String>,
    ) -> Result<(), String> {
        match self.workspace.store.reserve_resource(checkout) {
            Ok(mut reservation) => reservation
                .initialize(initialize)
                .map_err(|error| error.to_string()),
            // A store that keeps no canonical records (a test's) creates it
            // the way it creates every record.
            Err(ResourceOpenError::IdentityMismatch) => {
                self.workspace
                    .store
                    .create_resource(checkout)
                    .map_err(|error| error.to_string())?;
                self.workspace
                    .store
                    .with(&checkout.id, |doc| {
                        initialize(doc)?;
                        doc.commit();
                        Ok::<(), String>(())
                    })
                    .ok_or("the new part is not open")?
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn follow(&self, index: u64) -> Result<(), String> {
        let part = self.chain.part(index);
        // An older part is brought up to the part's storage version before
        // it is read, as the app's record is when it opens: once, as the
        // same housekeeping change on every device.
        let older = self
            .workspace
            .store
            .read(&part, tilo_resource::evolution::needs_migrate::<P>)
            .ok_or("the part is not open here")?;
        if older {
            // Not while a frame reads the store: the part is held at the
            // next refresh, as a part this device asked for is.
            if !self.workspace.store.takes_writes() {
                return Err("the part is migrated once the store takes writes".into());
            }
            self.workspace.store.migrate(&part, |doc| {
                tilo_resource::evolution::migrate::<P>(doc).map_err(|error| error.to_string())
            })?;
        }
        let reader = self
            .workspace
            .store
            .read(&part, Reader::<P>::read)
            .ok_or("the part is not open here")?
            .map_err(|error| error.to_string())?;
        self.parts.borrow_mut().insert(index, Live::new(reader));
        self.moved();
        Ok(())
    }

    /// Write `action` into its part now, as [`Self::apply_as`], or say the
    /// part is on its way.
    fn try_apply(&self, action: &P::Action, invoker: &ActionInvoker) -> Result<Write, ActionError> {
        // Which part is the newest is on its way: a part started now would be
        // an old one, or a second first one.
        if self.head_arriving() {
            return Ok(Write::Arriving);
        }
        let mut action = action.clone();
        let base = (self.base)();
        let parts = self.parts();
        P::prepare(&mut action, &base, &parts).map_err(ActionError::Refused)?;
        let place = P::place(&action, &base);
        drop(base);
        let index = P::part_for(&action, &parts).map_err(ActionError::Refused)?;
        drop(parts);
        if let Some(permissions) = &self.workspace.permissions {
            // Decided where the chain publishes the action, as the API
            // decides it: a grant on the app, the parts' kind or the verb
            // covers it.
            let need = tilo_resource::need_of::<P>(&action);
            let node = if let Some(verb) = super::workspace::verb_of::<P>(&action) {
                tilo_actions::action_node(
                    &TreePath::root(),
                    self.workspace.app_id,
                    P::KIND,
                    &self.id,
                    need.field.as_deref(),
                    &verb,
                )
            } else {
                // A field edit, at the field it changes, as the published
                // `field-edit`: a grant on that action covers it too.
                TreePath::root()
                    .app(self.workspace.app_id)
                    .kind(P::KIND)
                    .resource(self.id.clone())
                    .field(need.field.as_deref().unwrap_or_default())
                    .action(tilo_resource::FIELD_EDIT)
            };
            if !permissions.check(&PermissionScope::Action {
                node,
                kind: need.kind,
            }) {
                return Err(ActionError::Refused(format!(
                    "you may not do this to these {}",
                    P::LABEL.to_lowercase()
                )));
            }
        }
        let head = self.parts.borrow().keys().next_back().copied();
        match self
            .open_or_start(index)
            .map_err(|error| self.refused(error))?
        {
            Opened::Held => {}
            Opened::Started => {
                if let Some(previous) = head.filter(|head| head.checked_add(1) == Some(index)) {
                    self.carry(previous, index, invoker)?;
                }
            }
            Opened::Asked => return Ok(Write::Arriving),
        }
        let encoded = tilo_resource::encode::<P>(&action)?;
        let created = self.write(index, &action, encoded, invoker, place)?;
        Ok(Write::Done(index, created))
    }

    /// Write into part `index`, just started after part `previous`, what it
    /// carries of the parts before it ([`Chained::carry`]), as `invoker`'s.
    fn carry(&self, previous: u64, index: u64, invoker: &ActionInvoker) -> Result<(), ActionError> {
        let Some(carried) = self
            .parts
            .borrow()
            .get(&previous)
            .and_then(|part| P::carry(previous, &part.get()))
        else {
            return Ok(());
        };
        let action = carried
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        let encoded = tilo_resource::encode::<P>(&action)?;
        self.write(index, &action, encoded, invoker, None).map(drop)
    }

    /// Apply a history verb to the part it names ([`PartEdit`]), invoked by
    /// `invoker`: checked as an undoable change where the chain publishes
    /// the verb, and written into the part's history as any of its steps.
    /// Answers the part and its version after.
    fn apply_history(
        &self,
        edit: &PartEdit,
        invoker: &ActionInvoker,
    ) -> Result<serde_json::Value, ActionError> {
        if let Some(permissions) = &self.workspace.permissions
            && !permissions.check(&PermissionScope::Action {
                node: tilo_actions::action_node(
                    &TreePath::root(),
                    self.workspace.app_id,
                    P::KIND,
                    &self.id,
                    None,
                    edit.verb(),
                ),
                kind: tilo_actions::ActionKind::Undoable,
            })
        {
            return Err(ActionError::Refused(format!(
                "you may not change the history of these {}",
                P::LABEL.to_lowercase()
            )));
        }
        self.hold_kept(edit.part)
            .map_err(|error| self.refused(error))?;
        let encoded = edit.encoded().map_err(ActionError::Refused)?;
        self.write(
            edit.part,
            &super::history::part_action::<P>(edit),
            encoded,
            invoker,
            None,
        )?;
        let now = self
            .workspace
            .store
            .with(&self.chain.part(edit.part), |doc| {
                super::history::version_name(&doc.get_heads())
            })
            .unwrap_or_default();
        Ok(serde_json::json!({ "part": edit.part, "now": now }))
    }

    /// Write `action` into part `index`, which is open here, attributed to
    /// `invoker` and recorded in the part's history, with where in the
    /// app's record it happened (`place`, [`Chained::place`]). The item
    /// resources it created.
    fn write(
        &self,
        index: u64,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
        place: Option<String>,
    ) -> Result<Vec<String>, ActionError> {
        let mut context = ActionContext::for_resource(&self.id, P::KIND, P::LABEL)
            .with_app(self.workspace.app_id, self.workspace.app_title)
            .with_invoker(invoker.clone())
            .with_change(action.change_kind());
        if let Some(place) = place.filter(|place| !place.trim().is_empty()) {
            context = context.with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::PLACE_FACET,
                place,
            ));
        }
        let id = self.chain.part(index);
        let created = self
            .workspace
            .store
            .with_history(&id, |doc, history| {
                let before = doc.get_heads();
                let writer = ui_core::bound_dispatch::document_writer(&invoker.acting.by, doc);
                let applied = tilo_actions::apply_local_with_context(
                    doc,
                    action,
                    encoded,
                    Some(&writer),
                    context,
                )
                .map(|envelope| {
                    ui_core::bound_dispatch::minted_items(doc, P::KIND, &envelope, &before)
                })
                .map_err(|error| ActionError::Refused(error.to_string()));
                // Nobody's undo step: a part's edits are kept in its
                // history, not the app's undo.
                history.rebase(doc);
                applied
            })
            .ok_or_else(|| ActionError::Refused("the part is not open here".into()))??;
        if let Err(error) = self.workspace.store.schedule_committed_history(&id) {
            tracing::error!(%error, "an edited part could not be saved");
        }
        self.refresh();
        self.moved();
        Ok(created)
    }

    /// The chain as it is published: the head's actions and every held
    /// part's items, each carried out through [`CHAIN_ACTION_TYPE`] on the
    /// part it changes; checked against the member's roles in the workspace
    /// as the app's own actions are.
    pub(crate) fn publish(&self) -> tilo_actions::ResourceSpec {
        let revision = (self.revision(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let parts = self.parts();
        let mut resource = match parts.last() {
            Some((index, head)) => self.part_published(*index, head, revision.1),
            None => tilo_resource::publish::<P>(&self.id, P::LABEL, &self.empty),
        };
        let mut seen: std::collections::HashSet<tilo_actions::ResourceId> = resource
            .children
            .iter()
            .map(|child| child.id.clone())
            .collect();
        for (index, state) in parts.iter().rev().skip(1) {
            let earlier = self.part_published(*index, state, revision.1);
            resource.children.extend(
                earlier
                    .children
                    .into_iter()
                    .filter(|child| seen.insert(child.id.clone())),
            );
        }
        // The places the app derives from everything the chain holds, at the
        // address each is published under.
        for place in
            tilo_resource::derived::published::<P>(P::places(&(self.base)(), &parts)).to_resources()
        {
            if seen.insert(place.id.clone()) {
                resource.children.push(place);
            }
        }
        // Parts no longer held are forgotten with their publication.
        self.part_published
            .borrow_mut()
            .retain(|index, _| parts.iter().any(|(held, _)| held == index));
        let mut published = route(resource, &self.id);
        // What the chain holds, read newest part first and paged, and each
        // item read from the part keeping it ([`Self::read`]): a device holds
        // a chat's head, and what a caller wants of a chat is its messages,
        // not the part they were written into.
        published.actions.push(read_spec::<P>(&self.id, None));
        for child in &mut published.children {
            let item = child.id.as_str().to_owned();
            if tilo_resource::items::item_of_resource::<P>(&item).is_none() {
                continue;
            }
            if !child.actions.iter().any(|spec| spec.id == "read") {
                child.actions.push(read_spec::<P>(&self.id, Some(&item)));
            }
            // What an item holds as a resource of its own (a rich-text
            // body) is read through the same read, from the part keeping it.
            if let Some(table) = P::RESOURCE_TABLE {
                offer_nested_reads::<P>(&mut child.children, table, &self.id);
            }
            // Shown where the app's window shows it, as its search hit
            // opens it (the window's openers answer the item's address), and
            // named by a link of its own, as an item of the record is.
            super::root::offer_item(child);
        }
        let own: std::collections::HashSet<String> = published
            .actions
            .iter()
            .map(|spec| spec.id.clone())
            .collect();
        published.actions.extend(
            super::history::part_edit_specs()
                .into_iter()
                .filter(|spec| !own.contains(&spec.id))
                .map(|spec| super::record::route_operation(spec, CHAIN_ACTION_TYPE, &self.id)),
        );
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    /// What search finds in every held part, each part's entries built
    /// again only when that part changed: the chain's items, in the window's
    /// search beside the app's own.
    pub(crate) fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        let parts = self.parts();
        let mut kept = self.part_search.borrow_mut();
        kept.retain(|index, _| parts.iter().any(|(held, _)| held == index));
        let mut entries = Vec::new();
        for (index, state) in &parts {
            let fresh = kept
                .get(index)
                .is_some_and(|(read, _)| Rc::ptr_eq(read, state));
            if !fresh {
                kept.insert(*index, (Rc::clone(state), Rc::new(P::search(state))));
            }
            if let Some((_, found)) = kept.get(index) {
                entries.extend(found.iter().cloned());
            }
        }
        entries
    }

    /// What part `index`, read as `state`, publishes: built again only when
    /// the part changed since (or the language did), so a change to one
    /// part costs that part's publication and not every part's.
    fn part_published(
        &self,
        index: u64,
        state: &Rc<P::State>,
        language: u64,
    ) -> tilo_actions::ResourceSpec {
        if let Some((kept, at, published)) = self.part_published.borrow().get(&index)
            && Rc::ptr_eq(kept, state)
            && *at == language
        {
            return published.clone();
        }
        let published = tilo_resource::publish::<P>(&self.id, P::LABEL, state);
        self.part_published
            .borrow_mut()
            .insert(index, (Rc::clone(state), language, published.clone()));
        published
    }

    /// Answer the read the chain publishes ([`read_spec`]) from the parts this
    /// device holds: the items of the newest part first, a page at a time,
    /// each named by the id a read of it takes; or one item's content, from
    /// the part keeping it. A page that reached the oldest part this device
    /// holds asks for the part before it and says so (`earlier`), so a caller
    /// reading on walks back through the chat a part at a time.
    pub(crate) fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        let read = ChainRead::of(payload)?;
        let table = P::RESOURCE_TABLE.ok_or_else(|| format!("{} answer no reads", P::LABEL))?;
        if let Some(item) = read.item.as_deref() {
            return self.read_item(table, item);
        }
        let parts = self.parts();
        let Some(&(newest, _)) = parts.last() else {
            return Ok(serde_json::json!({ "kind": P::KIND }));
        };
        let (mut at, mut skip) = match read.cursor.as_deref() {
            Some(cursor) => parse_read_cursor(cursor)
                .ok_or_else(|| "cursor is not one a read of this chain issued".to_owned())?,
            None => (newest, 0),
        };
        if !parts.iter().any(|(index, _)| *index == at) {
            // A part asked for before, and not here yet: the page is what the
            // answer before it said, and the same cursor reads on once it
            // arrives.
            return Ok(serde_json::json!({
                "kind": P::KIND,
                "earlier": true,
                "nextCursor": read_cursor(at, skip),
            }));
        }
        let mut lists: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        let mut taken = 0usize;
        let mut next: Option<String> = None;
        // Where the page ran out of parts, rather than out of room.
        let mut ended = None;
        for (index, state) in parts.iter().rev() {
            if *index > at {
                continue;
            }
            if *index < at {
                // The part the cursor names is no longer held: the page goes
                // on from the next part this device has.
                at = *index;
                skip = 0;
            }
            let items = tilo_resource::table::resource_items(table, &**state);
            // What the part holds now, newest first: a cursor counted against
            // the part as it was reads from its end where items went since.
            let newest_first: Vec<(&str, tilo_resource::Id, String)> = items
                .into_iter()
                .flat_map(|(field, found)| {
                    found
                        .into_iter()
                        .rev()
                        .map(move |(id, label)| (field, id, label))
                })
                .collect();
            let skip_here = skip.min(newest_first.len());
            let mut room = true;
            for (field, id, label) in newest_first.iter().skip(skip_here) {
                if taken >= read.page_size {
                    next = Some(read_cursor(*index, skip));
                    room = false;
                    break;
                }
                let item = tilo_resource::items::item_resource_id_in(P::KIND, field, id);
                lists
                    .entry((*field).to_owned())
                    .or_default()
                    .push(serde_json::json!({ "item": item, "name": label }));
                taken = taken.saturating_add(1);
                skip = skip.saturating_add(1);
            }
            if !room {
                break;
            }
            ended = Some((*index, skip.min(newest_first.len())));
        }
        let mut answer = serde_json::Map::new();
        answer.insert("kind".into(), serde_json::json!(P::KIND));
        for (field, list) in lists {
            answer.insert(field, serde_json::Value::Array(list));
        }
        // The part before the oldest this device holds holds more than this
        // answer: it is asked for, and the page after this one reads it.
        if next.is_none()
            && let Some((index, answered)) = ended
            && index > 0
        {
            next = Some(read_cursor(index, answered));
            let _ = self.hold(index.saturating_sub(1));
        }
        let stopped_in = next
            .as_deref()
            .and_then(parse_read_cursor)
            .map(|(part, _)| part);
        if let Some(next) = next.filter(|_| taken > 0) {
            answer.insert("nextCursor".into(), serde_json::json!(next));
        }
        if stopped_in.is_some_and(|part| part > 0) {
            answer.insert("earlier".into(), serde_json::json!(true));
        }
        Ok(serde_json::Value::Object(answer))
    }

    /// One item of a part this device holds, as the read of it answers.
    fn read_item(
        &self,
        table: &'static tilo_resource::table::ModelTable,
        item: &str,
    ) -> Result<serde_json::Value, String> {
        for (_, state) in self.parts().iter().rev() {
            if let Ok(value) = tilo_resource::table::item_json(table, &**state, P::KIND, item) {
                return Ok(value);
            }
        }
        Err(format!(
            "{item} is in a part of these {} this device does not hold",
            P::LABEL.to_lowercase()
        ))
    }
}

/// Offer the chain's read on every resource under an item that its part's
/// table reads (an item's rich-text body), bound to that resource.
fn offer_nested_reads<P: Chained>(
    children: &mut [tilo_actions::ResourceSpec],
    table: &'static tilo_resource::table::ModelTable,
    chain: &str,
) {
    for child in children {
        let id = child.id.as_str().to_owned();
        if tilo_resource::table::reads_item(table, P::KIND, &id)
            && !child.actions.iter().any(|spec| spec.id == "read")
        {
            child.actions.push(read_spec::<P>(chain, Some(&id)));
        }
        offer_nested_reads::<P>(&mut child.children, table, chain);
    }
}

/// The read `P`'s chain publishes: on the chain, what it holds, newest part
/// first and paged; on an item of a part, that item's content. `item` is the
/// item's resource id, bound here rather than passed, as the record's own
/// read binds the item it is offered on.
fn read_spec<P: Chained>(chain: &str, item: Option<&str>) -> tilo_actions::ActionSpec {
    let (chain, item) = (chain.to_owned(), item.map(str::to_owned));
    let label = format!("Read the {}", P::LABEL.to_lowercase());
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "read",
        label.clone(),
        label,
        OperationKind::View,
        CHAIN_READ_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "cursor",
                tilo_actions::ParamType::Text,
                "Where the page read before ended, as its `nextCursor` said (default: the newest part)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "pageSize",
                tilo_actions::ParamType::Integer,
                "How many items to answer with at most",
            )
            .with_schema(serde_json::json!({
                "type": "integer",
                "minimum": 1,
                "maximum": READ_PAGE_MAX,
            })),
        ],
        move |args| {
            let payload = serde_json::json!({
                "chain": chain,
                "item": item,
                "cursor": args.text("cursor"),
                "pageSize": args.integer("pageSize"),
            });
            tilo_actions::EncodedAction::encode_operation(CHAIN_READ_TYPE, &payload)
                .unwrap_or_else(|_| tilo_actions::EncodedAction {
                    type_id: CHAIN_READ_TYPE.into(),
                    payload: Vec::new(),
                })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as typed data: the items of its parts, the newest part first, a page at \
         a time with `cursor` passing on where a page ended; an item's own content when the read \
         is of that item.",
    )
    // Typed data for a program to reason about; a person reads what the
    // app shows.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}

/// What one read of a chain asks for, as its published action carries it.
struct ChainRead {
    item: Option<String>,
    cursor: Option<String>,
    page_size: usize,
}

impl ChainRead {
    fn of(payload: &[u8]) -> Result<Self, String> {
        let args: serde_json::Value = if payload.is_empty() {
            serde_json::Value::Null
        } else {
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
        };
        let text = |key: &str| {
            args.get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let page_size = args
            .get("pageSize")
            .and_then(serde_json::Value::as_u64)
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(READ_PAGE)
            .clamp(1, READ_PAGE_MAX);
        Ok(Self {
            item: text("item"),
            cursor: text("cursor"),
            page_size,
        })
    }
}

/// Where a page of a chain's read ended: the part it was reading and how many
/// of that part's items, newest first, it has answered with.
fn read_cursor(part: u64, taken: usize) -> String {
    format!("{part}:{taken}")
}

fn parse_read_cursor(cursor: &str) -> Option<(u64, usize)> {
    let (part, taken) = cursor.split_once(':')?;
    Some((part.parse().ok()?, taken.parse().ok()?))
}

/// `resource`'s document actions, and those of its children, carried out on
/// the chain `chain`, each keeping the need it declares. An item's views
/// and the app's other effects on it stay as the part publishes them,
/// answered by the app's window: an item of a part opens at the address a
/// workspace's host knows it by. The chain itself offers only document actions;
/// the app's own record offers its effects.
fn route(resource: tilo_actions::ResourceSpec, chain: &str) -> tilo_actions::ResourceSpec {
    route_at(resource, chain, false)
}

fn route_at(
    mut resource: tilo_actions::ResourceSpec,
    chain: &str,
    item: bool,
) -> tilo_actions::ResourceSpec {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter_map(|spec| {
            if spec.kind == OperationKind::Document {
                Some(super::record::route_operation(
                    spec,
                    CHAIN_ACTION_TYPE,
                    chain,
                ))
            } else {
                item.then_some(spec)
            }
        })
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route_at(child, chain, true))
        .collect();
    resource
}

impl<P: Chained> ResourceApi for ChainState<P> {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        true
    }

    fn revision(&self) -> u64 {
        ChainState::revision(self)
    }

    fn publish(&self) -> tilo_actions::ResourceSpec {
        ChainState::publish(self)
    }

    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        ChainState::search(self)
    }

    fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        ChainState::read(self, payload)
    }

    fn apply_encoded(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<(), String> {
        if action.type_id == PART_EDIT_TYPE {
            let edit: PartEdit =
                tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
            return self
                .apply_history(&edit, invoker)
                .map(drop)
                .map_err(|error| error.to_string());
        }
        if action.type_id != P::ACTION_TYPE {
            return Err(format!("{} take no `{}` action", P::LABEL, action.type_id));
        }
        let action: P::Action =
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
        super::record::not_by_app::<P>(&action)?;
        self.apply_as(&action, invoker, &mut None)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn apply_answering(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if action.type_id == PART_EDIT_TYPE {
            let edit: PartEdit =
                tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
            let value = self
                .apply_history(&edit, invoker)
                .map_err(|error| error.to_string())?;
            return Ok(tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            });
        }
        if action.type_id != P::ACTION_TYPE {
            return Err(format!("{} take no `{}` action", P::LABEL, action.type_id));
        }
        let action: P::Action =
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
        super::record::not_by_app::<P>(&action)?;
        // One that waits for its part creates its items once it is written.
        let resources = self
            .apply_as(&action, invoker, &mut None)
            .map_err(|error| error.to_string())?
            .map(|(_, created)| created)
            .unwrap_or_default();
        Ok(tilo_actions::ActionOutput {
            value: None,
            resources,
        })
    }
}

/// A chain of records the app keeps beside its own, held for a view or
/// surface. See the module docs.
pub struct Chain<P: Chained> {
    state: Rc<ChainState<P>>,
    dispatch: Dispatcher<P>,
}

/// A part of a chain as a record a view part is built over
/// ([`Chain::part_record`]).
pub struct PartRecord<P: Chained> {
    chain: Rc<ChainState<P>>,
    held: Rc<super::record::Held<P>>,
}

impl<P: Chained> super::record::sealed::Hold<P> for PartRecord<P> {
    fn held(
        &self,
        _storage: super::record::sealed::Storage<'_>,
    ) -> Option<Rc<super::record::Held<P>>> {
        Some(Rc::clone(&self.held))
    }

    fn api(
        &self,
        _storage: super::record::sealed::Storage<'_>,
    ) -> Option<super::record::sealed::Api> {
        Some(super::record::sealed::Api(
            Rc::clone(&self.chain) as Rc<dyn ResourceApi>
        ))
    }
}

/// The chain as a record whose actions a view binds
/// ([`ViewCx::resource_actions`](super::ViewCx::resource_actions)): its verbs and
/// every held part's items, carried out on the part each names, whether or
/// not a part is held yet (a first issue made in an empty tracker).
impl<P: Chained> super::record::sealed::Hold<P> for Chain<P> {
    fn held(
        &self,
        storage: super::record::sealed::Storage<'_>,
    ) -> Option<Rc<super::record::Held<P>>> {
        let head = self.head()?;
        self.part_record(head)?.held(storage)
    }

    fn api(
        &self,
        _storage: super::record::sealed::Storage<'_>,
    ) -> Option<super::record::sealed::Api> {
        Some(super::record::sealed::Api(
            Rc::clone(&self.state) as Rc<dyn ResourceApi>
        ))
    }
}

impl<P: Chained> Clone for Chain<P> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
            dispatch: self.dispatch.clone(),
        }
    }
}

impl<P: Chained> Chain<P> {
    pub(crate) fn new(state: Rc<ChainState<P>>, dispatch: Dispatcher<P>) -> Self {
        Self { state, dispatch }
    }

    /// The address the chain is published under: what an agent or a
    /// script invokes its actions on.
    #[must_use]
    pub fn id(&self) -> &str {
        self.state.id()
    }

    /// Every part this window holds, oldest first, as it stands now.
    #[must_use]
    pub fn parts(&self) -> Vec<(u64, Rc<P::State>)> {
        self.state.parts()
    }

    /// The parts this window holds, oldest first, as view state that
    /// follows every change of any of them: what a view that reads across
    /// parts (a list of every item) follows, the way [`ViewCx::value`]
    /// follows the app's own record.
    ///
    /// [`ViewCx::value`]: super::ViewCx::value
    #[must_use]
    pub fn state(&self) -> ui_core::LocalState<Parts<P>> {
        let _ = self.state.refresh();
        self.state.state.clone()
    }

    /// Part `index` as a record a part of the view is built over
    /// ([`ViewCx::record`](super::ViewCx::record)): its controls bind the
    /// part's paths (an item's title, its rich-text body), and what they
    /// change is carried out through the chain, on the part that holds it,
    /// as the person. `None` while the part is not held here.
    #[must_use]
    pub fn part_record(&self, index: u64) -> Option<PartRecord<P>> {
        let live = self.state.parts.borrow().get(&index)?.clone();
        let syncing = Rc::downgrade(&self.state);
        let applying = Rc::downgrade(&self.state);
        let held = super::record::Held::new(
            self.state.id.clone(),
            P::LABEL.to_owned(),
            true,
            live,
            move |_| {
                if let Some(state) = syncing.upgrade() {
                    let _ = state.refresh();
                }
            },
            move |action, invoker| {
                let state = applying
                    .upgrade()
                    .ok_or_else(|| ActionError::Refused("the chain is closed".into()))?;
                state.apply_as(action, invoker, &mut None).map(drop)
            },
        );
        Some(PartRecord {
            chain: Rc::clone(&self.state),
            held: Rc::new(held),
        })
    }

    /// The number of the chain's head: the newest part held. `None` while
    /// nothing was written to the chain.
    #[must_use]
    pub fn head(&self) -> Option<u64> {
        self.state.head()
    }

    /// Whether the chain's newest part is still on its way: no part is held
    /// here and the workspace's hosts, asked for the chain's head, have not
    /// answered. A view shows the chain loading rather than empty; an action
    /// is refused until then.
    #[must_use]
    pub fn head_arriving(&self) -> bool {
        self.state.head_arriving()
    }

    /// Increases whenever a held part changed or a newer head arrived.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state.revision()
    }

    /// Hold the earlier part `index` too: read it from this device, or ask
    /// the workspace for it when this device has none of it. Its history merges
    /// in as it arrives ([`Self::revision`] moves, the window is woken);
    /// [`Self::waiting`] says where it stands until then.
    pub fn hold(&self, index: u64) -> Result<(), String> {
        self.state.hold(index)
    }

    /// Where part `index`, asked for and not read here yet, stands: on its
    /// way (a view shows it loading, not empty), or unavailable (a view says
    /// so, with nothing spinning). `None` once it is read.
    #[must_use]
    pub fn waiting(&self, index: u64) -> Option<PartArrival> {
        self.state.refresh();
        self.state.waiting(index)
    }

    /// Every part asked for and not read here yet, oldest first.
    #[must_use]
    pub fn waited(&self) -> Vec<(u64, PartArrival)> {
        self.state.refresh();
        self.state.waited()
    }

    /// Apply `action` as the window's person, or whoever the invocation
    /// carried out here is for ([`super::record::acting`]), to the part it changes. The
    /// part's number; `None` while that part is on its way from the
    /// workspace: the action waits for it, shows in [`Self::unwritten`]
    /// meanwhile, and is written once it is here, in the order asked.
    ///
    /// # Errors
    /// As the API refuses it: the person may not, the app's record or the
    /// part refused it.
    pub fn apply(&self, action: &P::Action) -> Result<Option<u64>, ActionError> {
        let me = self.dispatch.me().unwrap_or(&self.state.workspace.me);
        self.state
            .apply_as(action, &super::record::acting(me), &mut None)
            .map(|written| written.map(|(index, _)| index))
    }

    /// Apply what `invoke` names, as [`Self::apply`].
    ///
    /// # Errors
    /// As [`Self::apply`], or the arguments do not build the action.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<Option<u64>, ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply(&action)
    }

    /// Apply what `invoke` names as the window's person, as [`Self::invoke`],
    /// and tell `then` the part's number or why it was refused: at once, or,
    /// while the part it changes is on its way from the workspace, once it
    /// arrived.
    pub fn invoke_then(
        &self,
        invoke: &Invoke<P>,
        then: impl FnOnce(Result<u64, ActionError>) + 'static,
    ) {
        let action = match invoke.build(tilo_resource::origin_now_ms()) {
            Ok(action) => action,
            Err(reason) => return then(Err(ActionError::Refused(reason))),
        };
        let me = self.dispatch.me().unwrap_or(&self.state.workspace.me);
        let mut later: Option<Later> = Some(Box::new(then));
        let answer = match self
            .state
            .apply_as(&action, &super::record::acting(me), &mut later)
        {
            Ok(None) => return,
            Ok(Some((index, _))) => Ok(index),
            Err(error) => Err(error),
        };
        if let Some(then) = later {
            then(answer);
        }
    }

    /// What the actions waiting for their part write, read as a part holding
    /// only that: what a view shows as being sent until it is written
    /// ([`Self::apply`]). `None` while nothing waits.
    #[must_use]
    pub fn unwritten(&self) -> Option<Rc<P::State>> {
        self.state.unwritten()
    }

    /// Take back the window's person's last step in part `index`, through
    /// the part's history as the `undo` verb the chain publishes on it does:
    /// the way back a surface offers for a step it took at the person's own
    /// gesture (Undo beside a message they deleted).
    ///
    /// A refusal is reported through the dispatcher's
    /// [`refused`](Dispatcher::refused), as the API's are.
    ///
    /// # Errors
    /// As the API refuses it: the person may not change the part's history,
    /// the part is not here, or they have no step in it to take back.
    pub fn undo_in(&self, index: u64) -> Result<(), ActionError> {
        let me = self.dispatch.me().unwrap_or(&self.state.workspace.me);
        let edit = PartEdit {
            part: index,
            edit: super::history::HistoryEdit::Undo {
                steps: None,
                invocation: None,
                by: None,
                to: None,
                step: None,
            },
        };
        let undone = self
            .state
            .apply_history(&edit, &ActionInvoker::member(me.clone()))
            .map(drop);
        if let Err(error) = &undone {
            self.dispatch.refused().set(Some(super::dispatch::Refused {
                action: edit.verb().to_owned(),
                item: None,
                reason: error.to_string(),
                by_workspace: false,
            }));
        }
        undone
    }

    /// Take back the person's newest step in the part they last wrote into
    /// through this window, as the window's Undo does when that write is
    /// newer than any of the app's own record. Whether there was one.
    pub fn undo_last(&self) -> bool {
        let Some(index) = self.state.written.borrow_mut().pop() else {
            return false;
        };
        let undone = self.undo_in(index).is_ok();
        if undone {
            self.state.undone.borrow_mut().push(index);
        }
        undone
    }

    /// Bring back the step [`Self::undo_last`] took back last. Whether there
    /// was one.
    pub fn redo_last(&self) -> bool {
        let Some(index) = self.state.undone.borrow_mut().pop() else {
            return false;
        };
        let me = self.dispatch.me().unwrap_or(&self.state.workspace.me);
        let edit = PartEdit {
            part: index,
            edit: super::history::HistoryEdit::Redo { steps: None },
        };
        let redone = self
            .state
            .apply_history(&edit, &ActionInvoker::member(me.clone()))
            .is_ok();
        if redone {
            self.state.written.borrow_mut().push(index);
        }
        redone
    }

    /// Whether this window's person wrote into a part here that Undo would
    /// take back ([`Self::undo_last`]), and one it took back that Redo would
    /// bring back.
    #[must_use]
    pub fn undoable(&self) -> (bool, bool) {
        (
            !self.state.written.borrow().is_empty(),
            !self.state.undone.borrow().is_empty(),
        )
    }

    /// Apply what `invoke` names as `author` wrote it: a screenshot
    /// scenario staging what other people wrote, or a test acting as
    /// someone else. Checked and recorded as [`Self::apply`] is, attributed
    /// to `author`.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn stage_as(
        &self,
        invoke: &Invoke<P>,
        author: &tilo_resource::MemberId,
    ) -> Result<Option<u64>, ActionError> {
        self.stage_at(invoke, author, tilo_resource::origin_now_ms())
    }

    /// [`Self::stage_as`] at `now_ms`, the action's time: what someone
    /// wrote at another time.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn stage_at(
        &self,
        invoke: &Invoke<P>,
        author: &tilo_resource::MemberId,
        now_ms: i64,
    ) -> Result<Option<u64>, ActionError> {
        let action = invoke.build(now_ms).map_err(ActionError::Refused)?;
        self.state
            .apply_as(&action, &ActionInvoker::member(author.clone()), &mut None)
            .map(|written| written.map(|(index, _)| index))
    }

    /// Write `state` into part `index` directly, as what the part starts
    /// with: a capture's or a test's fixture (items under fixed ids,
    /// written by several people) in fields the parts keep to their own
    /// actions (`#[model(readonly)]`), which refuse every generic edit.
    /// Not an action: nothing checks who may, nobody is its author, and it
    /// is committed as the part's genesis, so only a fixture writes it.
    pub fn write_start(&self, index: u64, state: &P::State) -> Result<(), String>
    where
        P::State: tilo_resource::Fields,
    {
        use tilo_resource::Fields as _;
        self.state.start(index)?;
        let id = self.state.chain.part(index);
        self.state
            .workspace
            .store
            .with_history(&id, |doc, history| {
                let root = ui_core::automerge::ROOT;
                let written = <P::State as tilo_resource::Fields>::read_fields(doc, &root)
                    .and_then(|before| {
                        state.write_changed_fields(
                            &before,
                            doc,
                            &root,
                            &tilo_resource::Delta::default(),
                        )
                    })
                    .map_err(|error| error.to_string());
                doc.commit_with(
                    ui_core::automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
                );
                history.rebase(doc);
                written
            })
            .ok_or("the part is not open here")??;
        if let Err(error) = self.state.workspace.store.schedule_committed_history(&id) {
            tracing::error!(%error, "a created part could not be saved");
        }
        self.state.refresh();
        self.state.moved();
        Ok(())
    }

    /// Whether the chain takes a change now: not while a frame reads the
    /// app's records (a view's sync), when [`Self::stage_as`] waits for a
    /// later pass (an event, a settle pass).
    #[must_use]
    pub fn takes_changes(&self) -> bool {
        self.state.workspace.store.takes_writes()
    }

    /// The chain's dispatcher: what it submits goes through the host's API
    /// as a click on a control does (a destructive verb asks first), and
    /// [`apply_now`](Dispatcher::apply_now) applies at once.
    #[must_use]
    pub fn dispatcher(&self) -> Dispatcher<P> {
        self.dispatch.clone()
    }

    /// A chain outside any host, for a tool or a test that holds the app's
    /// records without a window: held in a store of its own, edited as `me`
    /// (or whoever [`Self::stage_as`] names), who may do anything, its
    /// actions reading the app's record from `base`. `None` when `P` does
    /// not read as a part.
    #[must_use]
    pub fn detached(
        app_id: &'static str,
        me: &tilo_resource::MemberId,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Self> {
        Self::detached_in(ResourceStore::in_memory(), app_id, me, base)
    }

    /// [`Self::detached`], held in `store`: one device's records of the
    /// app in a workspace, for a test of what reaches that device's store from
    /// another one.
    #[must_use]
    pub fn detached_in(
        store: ResourceStore,
        app_id: &'static str,
        me: &tilo_resource::MemberId,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Self> {
        let workspace = WorkspaceResources {
            store,
            me: me.clone(),
            permissions: None,
            app_id,
            app_title: app_id,
        };
        let state = Rc::new(ChainState::<P>::open(workspace, Rc::new(base))?);
        let dispatch = detached_dispatch(app_id, &state, me);
        Some(Self { state, dispatch })
    }

    /// The same detached chain edited as `me`: another person's window
    /// over it, in a test.
    #[must_use]
    pub fn as_reader(&self, me: &tilo_resource::MemberId) -> Self {
        Self {
            state: Rc::clone(&self.state),
            dispatch: detached_dispatch(self.state.workspace.app_id, &self.state, me),
        }
    }

    /// Part `index` as a document's bytes, for a tool that carries it to
    /// another device itself; `None` when the chain holds no such part.
    #[must_use]
    pub fn part_bytes(&self, index: u64) -> Option<Vec<u8>> {
        self.state
            .workspace
            .store
            .read(&self.state.chain.part(index), |doc| doc.clone().save())
    }

    /// What this chain's dispatcher handed to the API and nothing took yet
    /// (a destructive verb waiting for confirmation), applied as the API
    /// does once the person agreed. Taking them empties the queue.
    ///
    /// # Errors
    /// The first that did not build or apply.
    #[cfg(any(test, feature = "test-support"))]
    pub fn confirm_asked(&self) -> Result<(), ActionError> {
        for action in self.dispatch.take_asked().map_err(ActionError::Refused)? {
            self.apply(&action)?;
        }
        Ok(())
    }
}

/// The dispatcher of a chain outside any host, applying as `me`.
fn detached_dispatch<P: Chained>(
    app_id: &'static str,
    state: &Rc<ChainState<P>>,
    me: &tilo_resource::MemberId,
) -> Dispatcher<P> {
    dispatching(state, me, |apply| {
        Dispatcher::<P>::new(app_id, state.id(), false, Some(me)).for_workspace_resource::<P>(
            state.id(),
            false,
            apply,
        )
    })
}

/// The dispatcher `make` makes of what applies an action to `state`'s parts
/// as `me` ([`ChainState::apply_as`]): one that waits for its part is taken,
/// as written; refused once its part arrived, it is told in the
/// dispatcher's [`refused`](Dispatcher::refused) as one the workspace
/// refused after this device showed it.
pub(crate) fn dispatching<P: Chained>(
    state: &Rc<ChainState<P>>,
    me: &tilo_resource::MemberId,
    make: impl FnOnce(Box<dyn Fn(&P::Action) -> Result<(), ActionError>>) -> Dispatcher<P>,
) -> Dispatcher<P> {
    let applying = Rc::downgrade(state);
    let me = me.clone();
    let told: Rc<std::cell::OnceCell<ui_core::LocalState<Option<super::dispatch::Refused>>>> =
        Rc::default();
    let telling = Rc::clone(&told);
    let dispatch = make(Box::new(move |action| {
        let state = applying
            .upgrade()
            .ok_or_else(|| ActionError::Refused("the chain is closed".into()))?;
        let refused = telling.get().cloned();
        let named = action.clone();
        let later: Later = Box::new(move |written| {
            let Err(error) = written else {
                return;
            };
            tracing::warn!(kind = P::KIND, %error, "a write that waited for its part was refused");
            if let Some(refused) = refused {
                refused.set(Some(super::dispatch::Refused {
                    action: super::workspace::verb_of::<P>(&named).unwrap_or_default(),
                    item: None,
                    reason: error.to_string(),
                    by_workspace: true,
                }));
            }
        });
        // Whoever the invocation carried out here is for (an agent's call
        // of an effect that writes here), else the window's person.
        let invoker = super::record::acting(&me);
        state.apply_as(action, &invoker, &mut Some(later)).map(drop)
    }));
    let _ = told.set(dispatch.refused());
    dispatch
}

/// The record of `me`'s actions waiting for their part, in the workspace the
/// app's records are kept in: their own there, as the app's records of a
/// kind are ([`DocumentId::of_member`]), or the one a person without a root
/// identity keeps on this device.
fn unsent_checkout(me: &tilo_resource::MemberId) -> DocumentCheckout {
    let kind = ResourceType::new(UNSENT_KIND);
    let id = me
        .person_id()
        .and_then(|person| DocumentId::of_member(person, UNSENT_KIND))
        .unwrap_or_else(|| ui_core::resource::person::document_id(&kind));
    DocumentCheckout::new(id, kind)
}

/// The actions the record of waiting actions `doc` keeps for `chain`, in
/// order, each with its entry there; one that does not read is left out and
/// logged.
fn read_unsent<P: Chained>(
    doc: &ui_core::automerge::AutoCommit,
    chain: &DocumentId,
) -> Vec<Unsent<P>> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(ROOT, chain.as_str()) else {
        return Vec::new();
    };
    doc.list_range(&list, ..)
        .filter_map(|item| {
            let id = item.id().to_string();
            let value = item.value.into_value();
            serde_json::from_str::<(P::Action, ActionInvoker)>(value.as_str().unwrap_or_default())
                .map(|(action, invoker)| Unsent {
                    id,
                    action,
                    invoker,
                })
                .map_err(
                    |error| tracing::warn!(kind = P::KIND, %error, "a waiting write does not read"),
                )
                .ok()
        })
        .collect()
}

/// Tell whoever waits for the action kept as `id` what became of it; a
/// refusal nobody waits for is logged.
fn answer<P: Chained>(id: &str, written: Result<u64, ActionError>) {
    match ANSWERS.with(|answers| answers.borrow_mut().remove(id)) {
        Some(later) => later(written),
        None => {
            if let Err(error) = written {
                tracing::warn!(kind = P::KIND, %error, "a write that waited for its part was refused");
            }
        }
    }
}

/// The chains a window's views opened, by part type and chain: one per each;
/// and the child records they opened, by container.
/// The chains a window holds, by part type and chain.
type HeldChains = BTreeMap<(&'static str, String), Rc<dyn Any>>;

#[derive(Default)]
pub(crate) struct Chains {
    held: RefCell<HeldChains>,
    /// The child records held, by their container's address.
    children: RefCell<BTreeMap<String, Rc<dyn Any>>>,
    apis: RefCell<Vec<Rc<dyn ResourceApi>>>,
}

impl Chains {
    /// The chain of `P`, opened in `workspace` on first use.
    pub(crate) fn open<P: Chained>(
        &self,
        workspace: Option<&WorkspaceResources>,
        record: Option<&DocumentId>,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState<P>>> {
        // Beside the record the window shows: the app's own record of the
        // workspace, or the one per member where the app keeps those.
        let chain = record.map_or_else(P::chain, P::chain_beside);
        if let Some(held) = self.opened::<P>(&chain) {
            return Some(held);
        }
        let workspace = workspace?.clone();
        let state = Rc::new(ChainState::<P>::open(workspace, Rc::new(base))?);
        self.hold(&chain, &state);
        Some(state)
    }

    /// The chain of `P` that lives beside the record `base`
    /// ([`Chained::chain_beside`]), opened in `workspace` on first use.
    pub(crate) fn open_beside<P: Chained>(
        &self,
        workspace: Option<&WorkspaceResources>,
        base: &DocumentId,
        base_state: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState<P>>> {
        self.open_at::<P>(workspace?, &P::chain_beside(base), base_state)
    }

    /// The chain of `P` kept at `chain` in `workspace`, opened on first use.
    /// The base is one of the app's own record, or a record of a workspace it
    /// keeps beside its own (a chat, whose parts hold its messages).
    pub(crate) fn open_at<P: Chained>(
        &self,
        workspace: &WorkspaceResources,
        chain: &DocumentId,
        base_state: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState<P>>> {
        if let Some(held) = self.opened::<P>(chain) {
            return Some(held);
        }
        let state = Rc::new(ChainState::<P>::open_at(
            workspace.clone(),
            Rc::new(base_state),
            chain.clone(),
        )?);
        self.hold(chain, &state);
        Some(state)
    }

    /// The chain of `P` held for `chain`, if the views opened it.
    fn opened<P: Chained>(&self, chain: &DocumentId) -> Option<Rc<ChainState<P>>> {
        let held = self.held.borrow();
        let held = held.get(&(P::KIND, chain.as_str().to_owned()))?;
        Rc::clone(held).downcast::<ChainState<P>>().ok()
    }

    /// Keep `state` for the views that ask for its chain again, published
    /// and answered beside the others.
    fn hold<P: Chained>(&self, chain: &DocumentId, state: &Rc<ChainState<P>>) {
        self.held.borrow_mut().insert(
            (P::KIND, chain.as_str().to_owned()),
            Rc::clone(state) as Rc<dyn Any>,
        );
        self.apis
            .borrow_mut()
            .push(Rc::clone(state) as Rc<dyn ResourceApi>);
    }

    /// The children of `C` the parent lists, held in `workspace` on first use:
    /// published and answered beside the chains, through the same API.
    pub(crate) fn open_children<C: super::children::ChildResource>(
        &self,
        workspace: Option<&WorkspaceResources>,
        parent: impl Fn() -> Rc<<C::Parent as Resource>::State> + 'static,
    ) -> Option<Rc<super::children::ChildrenState<C>>> {
        let key = ui_core::resource::children_resource_id(&C::parent_resource(), C::KIND);
        if let Some(held) = self.children.borrow().get(&key) {
            return Rc::clone(held)
                .downcast::<super::children::ChildrenState<C>>()
                .ok();
        }
        let state = Rc::new(super::children::ChildrenState::<C>::open(
            workspace?.clone(),
            Rc::new(parent),
        )?);
        self.children
            .borrow_mut()
            .insert(key, Rc::clone(&state) as Rc<dyn Any>);
        self.apis
            .borrow_mut()
            .push(Rc::clone(&state) as Rc<dyn ResourceApi>);
        Some(state)
    }

    pub(crate) fn apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        self.apis.borrow().clone()
    }

    /// Answer one published action of a chain: apply it to the part it
    /// changes, for whoever the host admitted to invoke it.
    pub(crate) fn answer(
        &self,
        me: Option<&tilo_resource::MemberId>,
        payload: &[u8],
        live: &mut ui_core::widget::Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let request: super::record::ResourceAction =
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
        let api = self
            .apis
            .borrow()
            .iter()
            .find(|api| api.id() == request.record)
            .cloned()
            .ok_or_else(|| format!("no chain `{}` is open here", request.record))?;
        let invoker = ui_core::invocation_scope::current_invoker()
            .or_else(|| me.cloned().map(ActionInvoker::member))
            .ok_or("nobody changes a past version")?;
        api.apply_in(&request.action, &invoker, live)
    }

    /// Answer a read of the chain `chain` ([`CHAIN_READ_TYPE`]) from the
    /// parts this device holds.
    pub(crate) fn read(&self, chain: &str, payload: &[u8]) -> Result<serde_json::Value, String> {
        let api = self
            .apis
            .borrow()
            .iter()
            .find(|api| api.id() == chain)
            .cloned()
            .ok_or_else(|| format!("no chain `{chain}` is open here"))?;
        api.read(payload)
    }
}
