//! Resources an app keeps beside its own as a chain of parts: what grows
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
//! item the head or the part after it), what an action reads of the app's
//! own record and of the other parts where it is invoked
//! ([`Chained::prepare`]), and what one action means for the items other
//! parts keep ([`Chained::spread`]: the links to a renamed note,
//! relabelled in every part holding one, each part's change its own step
//! there, by the same invoker). A part's action reads only its own part, so
//! it replays the same everywhere. Everything else is the framework's:
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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use tilo_actions::{ActionContext, ActionError, ActionInvoker};
use tilo_permissions::LivePermissions;
use tilo_permissions::PermissionScope;
use tilo_permissions::TreePath;
use tilo_resource::{Invoke, Refusal, Resource};
use ui_core::automerge::transaction::Transactable as _;
use ui_core::automerge::{ChangeHash, ObjType, ROOT, ReadDoc as _, Value};
use ui_core::resource::{
    DocumentCheckout, DocumentId, ResourceOpenError, ResourceStore, ResourceType,
};

use super::dispatch::Dispatcher;
use super::history::PartEdit;
use super::resources::ResourceApi;

mod api;
mod demanding;
mod dispatching;
mod follow;
mod held;
pub(crate) use held::Chains;
mod listing;
mod named;
mod part;
mod past_page;
mod place;
mod reading;
mod routing;
use routing::{route, route_at};
mod shares;
mod tree;
mod waiting;
pub(crate) use waiting::QueuedWrites;
mod whole;

use super::made::Made;
use dispatching::detached_dispatch;
pub(crate) use dispatching::dispatching;
use listing::{Listing, Page};
use part::{HeldPart, OnWritten, PartType, by_part, live_of, part_edit};
pub(crate) use reading::PastPart;
use reading::{ChainRead, offer_nested_reads, parse_read_cursor, read_cursor, read_spec};
use shares::Written;
use tree::nest;

#[cfg(test)]
mod tests;

pub(crate) use place::ItemIndex;
pub use place::{PART_BYTES, PART_ITEMS, PartLimits, placed};

/// The operation type a chain's published actions are carried out under:
/// the host applies the action to the part of the chain it names.
pub(crate) const CHAIN_ACTION_TYPE: &str = "tilo.app.chain-action";

/// A part's number beside the share of the action that part carries out.
pub(super) type PartShares<A> = Vec<(u64, A)>;

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

    /// Each item's place in the tree the chain publishes: the item it sits
    /// under, by its id (`None` at the top). A kind whose items nest (a
    /// notebook's pages) returns every item it shows; the chain then
    /// publishes each under the one it sits under, and only the roots beside
    /// its own children. Empty (the default) for a kind whose items all sit
    /// at the top. An item whose parent is not among those published stays at
    /// the top, so a note whose parent is gone is never lost.
    fn tree(
        parts: &[(u64, Rc<Self::State>)],
    ) -> Vec<(tilo_resource::Id, Option<tilo_resource::Id>)> {
        let _ = parts;
        Vec::new()
    }

    /// What search finds beyond the parts themselves, over everything the
    /// chain holds: the places [`Self::places`] publishes that a person finds
    /// by name (a book's groups). Built where search runs, from the app's
    /// record and the parts held.
    fn search_over(
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Vec<tilo_resource::SearchEntry> {
        let _ = (base, parts);
        Vec::new()
    }

    /// The part `action` changes, given the parts this device holds, oldest
    /// first, when the app places it itself. `None` (the default) leaves it
    /// to the chain's one placement for every app's items: the part holding
    /// the item it names, or for a new item the newest part, or the next
    /// once that is full.
    fn part_for(
        action: &Self::Action,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Option<Result<u64, String>> {
        let _ = (action, parts);
        None
    }

    /// The item a new item `action` makes is kept beside, in the same
    /// part: a reply beside the message it answers. `None` (the default)
    /// keeps it with the newest items.
    fn beside(action: &Self::Action) -> Option<tilo_resource::Id> {
        let _ = action;
        None
    }

    /// The share of `action` each part carries out, when it names items
    /// kept in several parts (archive these messages): one action per part
    /// holding some of them, written into each as one step that Undo takes
    /// back whole. `None` (the default) for an action one part carries out,
    /// the one [`Self::part_for`] names.
    ///
    /// # Errors
    /// Refuses an action naming no item a part held here keeps.
    fn split(
        action: &Self::Action,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Result<Option<PartShares<Self::Action>>, String> {
        let _ = (action, parts);
        Ok(None)
    }

    /// Fill in, where the action is invoked, what it records from the app's
    /// own record (who a mention names), and refuse what that record does
    /// not allow (a post to a channel it has not got). A part's action
    /// itself reads only its part, so it replays the same on every replica.
    ///
    /// `parts` are the parts this device holds, oldest first: what an
    /// action checks across items of other parts reads them here (a task
    /// placed before one kept in another part, a parent that must exist),
    /// so the verb itself still reads only its own part. `invoker` is who
    /// it is invoked by: what an action naming "mine" resolves against (the
    /// one note they made).
    ///
    /// `actions` are invoked as one, in order: a single action, or every
    /// member of a group. What they read across the parts is read once for
    /// all of them, and each takes up where the one before it left off (a
    /// new item placed after the one made before it), so a group costs what
    /// its members do, not its members times every item held.
    ///
    /// A refusal a caller decides on rather than reads keeps its code
    /// ([`Refusal::coded`]): the answer names it as the verb's own refusal
    /// would, so an agent told a message is in the mailbox already reads
    /// that as the code it gave, whichever half of the delivery refused it.
    fn prepare(
        actions: &mut [Self::Action],
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
        invoker: &ActionInvoker,
    ) -> Result<(), Refusal> {
        let _ = (actions, base, parts, invoker);
        Ok(())
    }

    /// Further actions the same invocation writes into other parts, right
    /// after `action` landed in part `index`: what one change means for
    /// items kept elsewhere (the links to a renamed note, relabelled in
    /// every part holding one). `parts` are the parts held, oldest first,
    /// `index` with `action` applied. Each is written as the same invoker's,
    /// a history step of its own in the part it names; one naming `index`
    /// or a part not held here is not written. None by default.
    fn spread(
        action: &Self::Action,
        index: u64,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Vec<(u64, Self::Action)> {
        let _ = (action, index, parts);
        Vec::new()
    }

    /// The items the parts keep that the chain does not show, by their
    /// resource ids ([`tilo_resource::items::item_resource_id`]), given the
    /// parts held, oldest first: what reads across parts says is not there
    /// (a note under one that was deleted, kept in another part). Neither
    /// published, found by search nor read, and shown again once what hid
    /// it is back. None by default.
    fn hidden(parts: &[(u64, Rc<Self::State>)]) -> std::collections::HashSet<String> {
        let _ = parts;
        std::collections::HashSet::new()
    }

    /// Whether the chain exists only beside the window's own record, never
    /// beside the app's record of the workspace: the parts of an app that
    /// keeps a record per member (Mail) are that person's, so a window that
    /// does not show their record holds no chain at all.
    const BESIDE_RESOURCE: bool = false;

    /// Whether a window holds every part of the chain rather than its head
    /// and what a reader scrolled back to: for a chain whose views list all
    /// of it (a tracker's issues), where a part is a bound on what one
    /// change costs, not on what a device holds.
    const HELD_WHOLE: bool = false;

    /// Turn what a read's `where` says in the kind's own words into
    /// equalities on its items' fields, where the read is answered: a
    /// project as people name it into the resource an issue keeps. What
    /// [`Self::prepare`] is to an action's arguments. `parts` are the parts
    /// this device holds, oldest first.
    ///
    /// # Errors
    /// Refuses what names nothing the app's record or the parts hold.
    fn prepare_read(
        filter: &mut serde_json::Map<String, serde_json::Value>,
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
    ) -> Result<(), String> {
        let _ = (filter, base, parts);
        Ok(())
    }

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

    /// Answer a read of `item` that names no item a part keeps but
    /// something the kind derives over every part held: the occurrences of
    /// a calendar's events in a span of time, the free time in it. `None`
    /// (the default) leaves the item to the part that keeps it.
    fn read_over(
        base: &<Self::Base as Resource>::State,
        parts: &[(u64, Rc<Self::State>)],
        item: &str,
    ) -> Option<Result<serde_json::Value, String>> {
        let _ = (base, parts, item);
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
    /// The part's store was busy with it while it was looked for (an action
    /// holding it): the part opens at the next refresh, so a view keeps
    /// waiting for it rather than being told it is unavailable.
    retry: bool,
}

impl From<ResourceOpenError> for Unopened {
    fn from(error: ResourceOpenError) -> Self {
        Self {
            retry: matches!(error, ResourceOpenError::Busy),
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
    pub(crate) device_queue: Option<ui_core::storage::Settings<waiting::QueuedWrites>>,
}

/// The parts a window holds, oldest first, by their numbers.
pub type Parts<P> = Rc<[(u64, Rc<<P as Resource>::State>)]>;

/// What each held part published, by its number.
type PartsPublished = BTreeMap<u64, PartKept>;

/// What one held part last published, and what it was built of: the next
/// version of the part builds only the items that changed.
#[derive(Default)]
struct PartKept {
    /// The part's state ([`HeldPart::held_at`]) and the language it was built at.
    at: Option<((u64, u64), u64)>,
    published: Option<Rc<PartPublished>>,
    /// The part's kept children ([`tilo_resource::items::ChildrenParts::kept`]), as carried.
    kept: Option<(KeptChildren, Vec<ui_core::published::SharedResource>)>,
    /// Its items, as carried.
    items: Option<super::root::ItemChildren>,
}

/// [`tilo_resource::items::ChildrenParts::kept`].
type KeptChildren = Rc<[Rc<tilo_actions::ResourceSpec>]>;

/// What one part publishes as the chain carries it: the part's own resource without its items,
/// and its items, each routed to the chain and offering the chain's reads and opener, shared
/// from one version of the chain's publication to the next while the part is unchanged.
struct PartPublished {
    head: tilo_actions::ResourceSpec,
    items: Vec<ui_core::published::SharedResource>,
}

/// The items of every part before the newest, newest first, each id once: gathered again only
/// when one of those parts changed, not when the newest one takes another item.
struct EarlierItems {
    parts: Vec<Rc<PartPublished>>,
    items: Vec<ui_core::published::SharedResource>,
    ids: std::collections::HashSet<String>,
}

impl EarlierItems {
    fn of(parts: Vec<Rc<PartPublished>>) -> Self {
        let mut ids = std::collections::HashSet::new();
        let mut items = Vec::new();
        for part in &parts {
            items.extend(
                part.items
                    .iter()
                    .filter(|item| ids.insert(item.id().to_owned()))
                    .cloned(),
            );
        }
        Self { parts, items, ids }
    }

    fn holds(&self, parts: &[Rc<PartPublished>]) -> bool {
        self.parts.len() == parts.len()
            && self
                .parts
                .iter()
                .zip(parts)
                .all(|(kept, part)| Rc::ptr_eq(kept, part))
    }
}

/// What search found in each held part, by its number, with the state it was
/// read at ([`HeldPart::held_at`]).
type PartsSearched = BTreeMap<u64, ((u64, u64), Rc<Vec<tilo_resource::SearchEntry>>)>;

/// The app's resource as a part's [`Chained::prepare`] reads it.
type Base = Rc<dyn Fn() -> Rc<dyn Any>>;

/// A chain held for a window: its parts, read as they change. See the
/// [module docs](self). Compiled once, whatever resource the parts are: what
/// it reads of their resource is the resource's [`PartType`].
pub(crate) struct ChainState {
    core: ChainCore,
    base: Base,
    /// What a part holds before anything was written to it: what the chain
    /// publishes before it has a head.
    empty: Rc<dyn Any>,
    published: RefCell<Option<((u64, u64), ui_core::published::SharedResource)>>,
    /// The parts held, as view state that follows every change
    /// ([`Chain::state`]), and the revision it was last written at.
    view: Box<dyn Any>,
    view_at: Cell<Option<u64>>,
    /// The steps this window's person took here, each the parts it wrote
    /// into, newest last: what the window's Undo takes back first
    /// ([`Chain::undo_last`]), and the steps it took back, for Redo.
    written: RefCell<Vec<Written>>,
    undone: RefCell<Vec<Written>>,
    /// What the waiting actions would write into this chain, projected
    /// as one part ([`ChainState::unwritten`]).
    unwritten: RefCell<Unwritten>,
    /// Whether the actions that waited are being applied now.
    draining: Cell<bool>,
    /// Every item the parts held keep, by its id ([`ItemIndex`]): where an
    /// action goes, and what [`Chain::find`] looks up.
    index: RefCell<ItemIndex>,
    /// What the verb written last answers its caller with
    /// ([`Resource::answer`]), until [`ResourceApi::apply_answering`] takes it.
    answered: RefCell<Option<serde_json::Value>>,
}

/// What a held chain does without reading its parts' state: opening, holding
/// and writing parts. Compiled once, whatever resource the parts are.
pub(crate) struct ChainCore {
    workspace: WorkspaceResources,
    part: &'static PartType,
    /// The chain the parts belong to.
    chain: DocumentId,
    id: String,
    kind: ResourceType,
    parts: RefCell<BTreeMap<u64, Box<dyn HeldPart>>>,
    /// Earlier parts asked for ([`Self::hold`]) that are not read here
    /// yet: held from their genesis, or not open at all while the workspace
    /// sends them.
    waiting: RefCell<BTreeMap<u64, PartArrival>>,
    /// Earlier parts of a chain held whole that this device keeps and has
    /// not opened: each opens in a turn of its own ([`whole`]), waited for
    /// as on its way meanwhile.
    queued: RefCell<BTreeSet<u64>>,
    turns: whole::Turns,
    revision: Cell<u64>,
    part_published: RefCell<PartsPublished>,
    /// The items of the parts before the newest, as last gathered ([`EarlierItems`]).
    earlier: RefCell<Option<EarlierItems>>,
    part_search: RefCell<PartsSearched>,
    /// Readers of every field for search, where the window reads fewer ([`PartType::search_all`]).
    search_readers: RefCell<HashMap<u64, Option<Box<dyn Any>>>>,
    /// The store's [`ResourceStore::kept_revision`] when the window last
    /// looked for a newer head and at the parts it waits for.
    looked: Cell<Option<u64>>,
    /// The store's [`ResourceStore::write_generation`] when the held parts
    /// were last followed: while it stays, no part changed, and following
    /// them again reads nothing.
    followed_at: Cell<Option<u64>>,
    /// The store's [`ResourceStore::kept_revision`] when the earlier parts
    /// this chain lacks were last looked for ([`Self::hold_earlier`]).
    earlier_at: Cell<Option<u64>>,
    /// The pass ([`tilo_ui_base::component_inputs::running_pass`]) in which the
    /// held parts were last read for it, so a pass reads them once (see the
    /// [`follow`] module).
    read_at: Cell<Option<u64>>,
    /// The person's record of the actions asked for while the part they
    /// change was on its way from the workspace ([`ChainState::apply_as`]),
    /// kept with the app's records: every window of the app shows them and
    /// applies them in order once the part is here, across a closed window
    /// and a restart.
    waiting_resource: DocumentCheckout,
    workspace_key: String,
    /// Whether [`Self::waiting_resource`] was looked for on this device.
    unsent_looked: Cell<bool>,
}

/// What to do once an action that waited for its part was applied (the
/// part's number) or refused.
pub(crate) type Later = Box<dyn FnOnce(Result<u64, ActionError>)>;

/// What the actions waiting for their part write, read as a part, at the
/// heads of the record that keeps them.
type QueueRevision = (Vec<ChangeHash>, Option<ui_core::storage::SettingsRevision>);
type Unwritten = (QueueRevision, Option<Rc<dyn Any>>);

/// The kind of the record of a person's actions waiting for their part.
const UNSENT_KIND: &str = tilo_actions::surface::CHAIN_UNSENT_KIND;

thread_local! {
    /// Who waits to be told what became of an action that waited for its
    /// part, by its entry in the record of them: told by whichever window
    /// applies it.
    static ANSWERS: RefCell<HashMap<String, Later>> = RefCell::default();
    /// The published action a chain's answer is carrying out, when it
    /// declares an answer ([`super::resources::ResourceAction::answers`]).
    static ASKED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// What became of an action applied to the part it changes.
enum Write {
    /// Written into the parts named first, creating the item resources
    /// listed second: each item's id, and the address it is published under.
    Done(Written, Made),
    /// The part is on its way from the workspace; nothing was written.
    Arriving,
}

/// Why a part still on its way cannot be opened for what needs it now.
const PART_ARRIVING: &str = "the part is on its way from the workspace";

impl ChainCore {
    /// Apply `action`, one waiting for its part, to `doc`, which shows what
    /// the waiting actions write.
    fn show_unsent(
        &self,
        doc: &mut ui_core::automerge::AutoCommit,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) {
        let context = ActionContext::for_resource(&self.id, self.part.kind, self.part.label)
            .with_app(self.workspace.app_id, self.workspace.app_title)
            .with_invoker(invoker.clone());
        let writer = ui_core::bound_dispatch::document_writer(&invoker.acting.by, doc);
        if let Err(error) =
            tilo_actions::apply_local_with_context(doc, action, encoded, Some(&writer), context)
        {
            tracing::warn!(kind = self.part.kind, %error, "a write waiting for its part cannot be shown");
        }
    }

    /// What part `index` publishes: its head (`head`) and its children
    /// (`children`), each carried by the chain. Built again only when the
    /// part changed since (or the language did), and then only the children
    /// that changed: an item whose label is unchanged is the very node the
    /// last version carried. So a write into a part of many items costs the
    /// items it touched, not every item the part holds.
    fn part_published(
        &self,
        index: u64,
        head: &dyn Fn() -> tilo_actions::ResourceSpec,
        children: &dyn Fn() -> tilo_resource::items::ChildrenParts,
    ) -> Rc<PartPublished> {
        let at = self
            .held_at(index)
            .map(|held| (held, tilo_ui_base::i18n::I18nManager::revision()));
        let mut all = self.part_published.borrow_mut();
        let kept = all.entry(index).or_default();
        if let Some(published) = kept
            .published
            .as_ref()
            .filter(|_| at.is_some() && kept.at == at)
        {
            return Rc::clone(published);
        }
        let _profile = tilo_resource::profiling::Phase::start("framework.chain_part_publish");
        let table = self.part.table;
        let parts = children();
        let carry = |child: tilo_actions::ResourceSpec| {
            ui_core::published::SharedResource::from_resource(
                self.carried_item(route_at(child, &self.id, true), table),
            )
        };
        let mut items: Vec<ui_core::published::SharedResource> =
            parts.others.into_iter().map(&carry).collect();
        let carried: Vec<ui_core::published::SharedResource> = {
            let before: HashMap<
                *const tilo_actions::ResourceSpec,
                &ui_core::published::SharedResource,
            > = kept
                .kept
                .as_ref()
                .map(|(specs, carried)| specs.iter().map(Rc::as_ptr).zip(carried).collect())
                .unwrap_or_default();
            parts
                .kept
                .iter()
                .map(|child| match before.get(&Rc::as_ptr(child)) {
                    Some(&carried) => carried.clone(),
                    None => carry((**child).clone()),
                })
                .collect()
        };
        items.extend(carried.iter().cloned());
        let own = super::root::ItemChildren::build(
            &mut kept.items,
            &(self.part.resource)(),
            parts.items,
            &parts.item_actions,
            true,
            &carry,
        );
        items.extend(own.iter().cloned());
        let published = Rc::new(PartPublished {
            head: head(),
            items,
        });
        kept.kept = Some((parts.kept, carried));
        kept.at = at;
        kept.published = Some(Rc::clone(&published));
        published
    }

    /// [`HeldPart::held_at`] of part `index`, while it is held.
    fn held_at(&self, index: u64) -> Option<(u64, u64)> {
        self.parts.borrow().get(&index).map(|part| part.held_at())
    }

    /// `child`, an item routed to the chain, offering the chain's read of it and its opener.
    fn carried_item(
        &self,
        mut child: tilo_actions::ResourceSpec,
        table: Option<&'static tilo_resource::table::ModelTable>,
    ) -> tilo_actions::ResourceSpec {
        let item = child.id.as_str().to_owned();
        if tilo_resource::items::item_of_resource_in(self.part.kind, &item).is_none() {
            return child;
        }
        // What the chain holds, read newest part first and paged, and each
        // item read from the part keeping it ([`Self::read`]): a device holds
        // a chat's head, and what a caller wants of a chat is its messages,
        // not the part they were written into.
        if !child.actions.iter().any(|spec| spec.id == "read") {
            child
                .actions
                .push(read_spec(self.part.label, &self.id, Some(&item)));
        }
        // What an item holds as a resource of its own (a rich-text
        // body) is read through the same read, from the part keeping it.
        if let Some(table) = table {
            offer_nested_reads(&mut child.children, table, self.part, &self.id);
        }
        // Shown where the app's window shows it, as its search hit
        // opens it (the window's openers answer the item's address), and
        // named by a link of its own, as an item of the record is.
        super::root::offer_item(&mut child);
        child
    }

    /// What search finds in part `index`, read as `state` (`search`): built
    /// again only when the part changed since.
    fn part_searched(
        &self,
        index: u64,
        search: &dyn Fn() -> Vec<tilo_resource::SearchEntry>,
    ) -> Rc<Vec<tilo_resource::SearchEntry>> {
        let at = self.held_at(index);
        if let Some((kept, found)) = self.part_search.borrow().get(&index)
            && Some(*kept) == at
        {
            return Rc::clone(found);
        }
        let found = Rc::new(search());
        if let Some(at) = at {
            self.part_search
                .borrow_mut()
                .insert(index, (at, Rc::clone(&found)));
        }
        found
    }

    /// Forget what was published and found of the parts no longer held.
    fn forget_unheld(&self, held: &[u64]) {
        self.part_published
            .borrow_mut()
            .retain(|index, _| held.contains(index));
        self.part_search
            .borrow_mut()
            .retain(|index, _| held.contains(index));
    }

    /// Whether the window's person may do what `need` and `verb` (the
    /// action's verb, or none for a field edit) say an action does, where
    /// the chain publishes it.
    fn may(&self, need: &tilo_actions::ActionNeed, verb: Option<&str>) -> Result<(), ActionError> {
        let Some(permissions) = &self.workspace.permissions else {
            return Ok(());
        };
        // Decided where the chain publishes the action, as the API
        // decides it: a grant on the app, the parts' kind or the verb
        // covers it.
        let node = if let Some(verb) = verb {
            tilo_actions::action_node(
                &TreePath::root(),
                self.workspace.app_id,
                self.part.kind,
                &self.id,
                need.field.as_deref(),
                verb,
            )
        } else {
            // A field edit, at the field it changes, as the published
            // `field-edit`: a grant on that action covers it too.
            TreePath::root()
                .app(self.workspace.app_id)
                .kind(self.part.kind)
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
                self.part.label.to_lowercase()
            )));
        }
        Ok(())
    }

    /// The chain as it is published, from what its head published
    /// (`head`), its earlier parts, newest first (`earlier`), and the
    /// places the app derives from them (`places`): their items, each
    /// carried out through [`CHAIN_ACTION_TYPE`] on the part it changes, and
    /// the chain's read. Each part's items are the very nodes the last
    /// version carried while that part is unchanged, so a change to one part
    /// publishes that part's items anew and only points at the others'.
    fn publish(
        &self,
        head: &PartPublished,
        earlier: Vec<Rc<PartPublished>>,
        places: Vec<tilo_actions::ResourceSpec>,
        tree: &[(tilo_resource::Id, Option<tilo_resource::Id>)],
        hidden: &std::collections::HashSet<String>,
        table: Option<&'static tilo_resource::table::ModelTable>,
    ) -> ui_core::published::SharedResource {
        let _profile = tilo_resource::profiling::Phase::start("framework.chain_publish");
        let mut kept = self.earlier.borrow_mut();
        if !kept.as_ref().is_some_and(|kept| kept.holds(&earlier)) {
            *kept = Some(EarlierItems::of(earlier));
        }
        let (earlier, earlier_ids) = kept.as_ref().map_or((&[][..], None), |kept| {
            (kept.items.as_slice(), Some(&kept.ids))
        });
        let mut items = Vec::with_capacity(
            head.items
                .len()
                .saturating_add(earlier.len())
                .saturating_add(places.len()),
        );
        let mut own: std::collections::HashSet<String> = std::collections::HashSet::new();
        items.extend(
            head.items
                .iter()
                .filter(|item| own.insert(item.id().to_owned()))
                .cloned(),
        );
        // An earlier part's item the newest part claims too is the newest part's.
        if earlier_ids.is_some_and(|ids| own.iter().any(|id| ids.contains(id))) {
            items.extend(
                earlier
                    .iter()
                    .filter(|item| !own.contains(item.id()))
                    .cloned(),
            );
        } else {
            items.extend(earlier.iter().cloned());
        }
        for place in places {
            let id = place.id.as_str();
            if own.contains(id) || earlier_ids.is_some_and(|ids| ids.contains(id)) {
                continue;
            }
            own.insert(id.to_owned());
            items.push(ui_core::published::SharedResource::from_resource(
                self.carried_item(route_at(place, &self.id, true), table),
            ));
        }
        drop(kept);
        let published = self.head_resource(&head.head);
        if !hidden.is_empty() {
            items.retain(|item| !hidden.contains(item.id()));
        }
        let items = nest(items, tree);
        ui_core::published::SharedResource::new(
            published,
            ui_core::published::SharedChildren::new(items),
        )
    }

    /// The chain's own resource as [`Self::publish`] carries it: the head
    /// part's resource, the chain's read and its edit verbs, without the
    /// items it holds. What a name the app's level carries is answered with,
    /// at the head's cost and never the whole chain's.
    fn head_resource(&self, head: &tilo_actions::ResourceSpec) -> tilo_actions::ResourceSpec {
        let mut published = route(head.clone(), &self.id);
        published
            .actions
            .push(read_spec(self.part.label, &self.id, None));
        let named: std::collections::HashSet<String> = published
            .actions
            .iter()
            .map(|spec| spec.id.clone())
            .collect();
        published.actions.extend(
            super::history::part_edit_specs()
                .into_iter()
                .filter(|spec| !named.contains(&spec.id))
                .map(|spec| super::resources::route_operation(spec, CHAIN_ACTION_TYPE, &self.id)),
        );
        published
    }

    /// Answer the read the chain publishes ([`read_spec`]) from the parts this
    /// device holds: the items of the newest part first, a page at a time,
    /// each as an inline collection holds it (`{id, value}`, the value kept
    /// to the fields asked for), so one type reads both, and only the items
    /// its `where` holds of ([`Listing`], `prepare`
    /// putting the kind's own words into fields first); or one item's
    /// content, from the part keeping it. A page that reached the oldest part
    /// this device holds asks for the part before it and says so (`earlier`),
    /// so a caller reading on walks back through the chat a part at a time.
    fn read(
        &self,
        table: Option<&'static tilo_resource::table::ModelTable>,
        parts: &[(u64, &dyn Any)],
        read: &ChainRead,
        prepare: &dyn Fn(&mut listing::Filter) -> Result<(), String>,
        hidden: &std::collections::HashSet<String>,
    ) -> Result<serde_json::Value, String> {
        let table = table.ok_or_else(|| format!("{} answer no reads", self.part.label))?;
        let Some(&(newest, _)) = parts.last() else {
            return Ok(serde_json::json!({ "kind": self.part.kind }));
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
                "kind": self.part.kind,
                "earlier": true,
                "nextCursor": read_cursor(at, skip),
            }));
        }
        let listing = Listing::of(read, table, self.part, parts, prepare)?;
        let mut page = Page::new(read.page_size);
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
            let kind = self.part.kind;
            let Some(passed) = page.take(*index, *state, table, kind, &listing, hidden, skip)
            else {
                break;
            };
            ended = Some((*index, passed));
        }
        let mut next = page.next.take();
        let mut answer = serde_json::Map::new();
        answer.insert("kind".into(), serde_json::json!(self.part.kind));
        for (field, list) in std::mem::take(&mut page.lists) {
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
        // A page whose `where` no item met still says where to read on.
        if let Some(next) = next.filter(|_| page.taken > 0 || listing.filters()) {
            answer.insert("nextCursor".into(), serde_json::json!(next));
        }
        if stopped_in.is_some_and(|part| part > 0) {
            answer.insert("earlier".into(), serde_json::json!(true));
        }
        Ok(serde_json::Value::Object(answer))
    }

    /// Apply a history verb to the part it names ([`PartEdit`]), invoked by
    /// `invoker` (`action` is that verb): checked as an undoable change where the chain publishes
    /// the verb, and written into the part's history as any of its steps.
    /// Answers the part and its version after.
    fn apply_history(
        &self,
        edit: &PartEdit,
        action: &dyn tilo_actions::Action,
        invoker: &ActionInvoker,
    ) -> Result<serde_json::Value, ActionError> {
        if let Some(permissions) = &self.workspace.permissions
            && !permissions.check(&PermissionScope::Action {
                node: tilo_actions::action_node(
                    &TreePath::root(),
                    self.workspace.app_id,
                    self.part.kind,
                    &self.id,
                    None,
                    edit.verb(),
                ),
                kind: tilo_actions::ActionKind::Undoable,
            })
        {
            return Err(ActionError::Refused(format!(
                "you may not change the history of these {}",
                self.part.label.to_lowercase()
            )));
        }
        self.hold_kept(edit.part)
            .map_err(|error| self.refused(error))?;
        let encoded = edit.encoded().map_err(ActionError::Refused)?;
        self.write(edit.part, action, encoded, invoker, None, &mut |_, _, _| {})?;
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
    /// resources it created. The chain follows it once its caller refreshes.
    fn write(
        &self,
        index: u64,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
        place: Option<String>,
        written: &mut OnWritten<'_>,
    ) -> Result<Made, ActionError> {
        let mut context = ActionContext::for_resource(&self.id, self.part.kind, self.part.label)
            .with_app(self.workspace.app_id, self.workspace.app_title)
            .with_invoker(invoker.clone())
            .with_change(tilo_actions::Action::change_kind(action))
            .in_step();
        // The agent session and purpose the invocation came with.
        if let Some(ambient) = tilo_actions::invoking_context() {
            context = context.enriched_by(&ambient);
        }
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
                // Its diff from the change log: from the heads alone, a scan of the whole part.
                doc.update_diff_cursor();
                let before = doc.get_heads();
                // Nothing minted by whatever ran before belongs to this write.
                drop(tilo_resource::take_minted());
                let writer = ui_core::bound_dispatch::document_writer(&invoker.acting.by, doc);
                let applied = tilo_actions::apply_local_with_context(
                    doc,
                    action,
                    encoded,
                    Some(&writer),
                    context,
                )
                .map(|envelope| {
                    // What it minted, a group's members' together, in order, put back once the
                    // part's items are worked out: whoever invoked the write answers what it made
                    // from the same list.
                    let minted = tilo_resource::take_minted();
                    written(doc, &before, &minted);
                    let items = ui_core::bound_dispatch::minted_items(
                        doc,
                        self.part.kind,
                        &minted,
                        &envelope,
                    );
                    tilo_resource::restore_minted(minted);
                    items
                })
                .map_err(|error| ActionError::Refused(error.to_string()));
                doc.reset_diff_cursor();
                // Nobody's undo step: a part's edits are kept in its
                // history, not the app's undo.
                history.rebase(doc);
                applied
            })
            .ok_or_else(|| ActionError::Refused("the part is not open here".into()))??;
        if let Err(error) = self.workspace.store.schedule_committed_history(&id) {
            tracing::error!(%error, "an edited part could not be saved");
        }
        Ok(created)
    }

    fn new(workspace: WorkspaceResources, part: &'static PartType, chain: DocumentId) -> Self {
        Self {
            id: ui_core::resource::chain_resource_id(&chain),
            kind: ResourceType::new(part.kind),
            part,
            chain,
            parts: RefCell::default(),
            waiting: RefCell::default(),
            queued: RefCell::default(),
            turns: whole::Turns::of_running_window(),
            revision: Cell::new(0),
            part_published: RefCell::default(),
            earlier: RefCell::new(None),
            part_search: RefCell::default(),
            search_readers: RefCell::default(),
            looked: Cell::new(None),
            followed_at: Cell::new(None),
            workspace_key: workspace.store.app_and_home().1,
            earlier_at: Cell::new(None),
            read_at: Cell::new(None),
            waiting_resource: unsent_checkout(&workspace.me),
            unsent_looked: Cell::new(false),
            workspace,
        }
    }

    /// Follow each held part's document, and hold a part opened in the store
    /// since. Whether any moved.
    fn follow_held(&self) -> bool {
        let mut moved = false;
        for (index, part) in self.parts.borrow().iter() {
            match self
                .workspace
                .store
                .read(&self.chain.part(*index), |doc| part.sync(doc))
            {
                Some(Ok(true)) => {
                    // Its history arrived, or a first change was made here.
                    self.waiting.borrow_mut().remove(index);
                    moved = true;
                }
                Some(Err(error)) => {
                    tracing::warn!(kind = self.part.kind, part = index, %error, "chain part unreadable");
                }
                _ => {}
            }
            part.take_change();
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
        moved
    }

    fn checkout(&self, index: u64) -> DocumentCheckout {
        DocumentCheckout::new(self.chain.part(index), self.kind.clone())
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
        match self
            .workspace
            .store
            .try_open_resource(&DocumentCheckout::new(head.clone(), self.kind.clone()))
        {
            Err(ResourceOpenError::Arriving) => true,
            // A store written at this moment (a chain opened in a view's sync) cannot say
            // which part is the newest: the head is waited for rather than taken as the
            // workspace's word that there is none, and asked for now. Opening it would have
            // asked; without this nothing asked until a write needed it, and that write
            // found the head still on its way.
            Err(ResourceOpenError::Busy) => {
                self.workspace.store.want(head);
                true
            }
            _ => false,
        }
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
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
            // A part kept here opens in its turn, not with those that arrive;
            // one held meanwhile (read back to) is no longer waited for.
            if self.waits_its_turn(index) {
                if held {
                    self.queued.borrow_mut().remove(&index);
                    self.waiting.borrow_mut().remove(&index);
                    moved = true;
                }
                continue;
            }
            // A part before one held that the workspace has none of is not started empty
            // here: its items were written, and the view says they are not available until
            // someone who holds them shares them.
            let earlier = self
                .parts
                .borrow()
                .keys()
                .next_back()
                .is_some_and(|head| index < *head);
            let none_sent = earlier
                && self
                    .workspace
                    .store
                    .workspace_answered(&self.chain.part(index));
            if !held && !none_sent {
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
                        tracing::warn!(kind = self.part.kind, part = index, %reason, "earlier part not opened yet");
                        self.looked.set(None);
                    }
                    Err(Unopened { reason, .. }) => {
                        tracing::warn!(kind = self.part.kind, part = index, %reason, "earlier part not opened");
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
            self.hold_kept(newest).map(|()| true)
        } else {
            // A part a peer started, started here from its genesis: what
            // they wrote in it is on its way, and a view of it waits for
            // that rather than reading back from an empty head. One asked
            // for opens at the look its arrival brings.
            self.open_or_start(newest).map(|opened| match opened {
                Opened::Held => true,
                Opened::Started => {
                    self.waiting
                        .borrow_mut()
                        .insert(newest, PartArrival::Arriving);
                    true
                }
                Opened::Asked => false,
            })
        };
        match opened {
            Ok(moved) => {
                self.looked.set(Some(looked));
                moved
            }
            Err(Unopened { reason, retry }) => {
                // A part another writer held for this moment is not gone: it
                // is looked for again at the next refresh, rather than only
                // once this device is told of something new. Nothing is
                // marked moved, so a failed look wakes nobody and cannot
                // make a pass of its own.
                tracing::warn!(kind = self.part.kind, part = newest, %reason, "chain head not opened");
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
        })?;
        // What the workspace sent of it is what it was waited for: a part asked for and
        // opened here from what arrived is on its way no longer, and reading back goes on.
        self.waiting.borrow_mut().remove(&index);
        Ok(())
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
                tracing::warn!(kind = self.part.kind, part = index, %reason, "part waits for another writer");
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

    /// Whether a part before the head of a chain held whole is still on its
    /// way here: what an action reads across parts (the issue it is a
    /// sub-issue of, made long ago) may be in it, so a reference this device
    /// does not find yet is waited on rather than refused. A part the
    /// workspace said it has none of is not waited for.
    pub(super) fn earlier_on_its_way(&self) -> bool {
        if !self.part.held_whole {
            return false;
        }
        let parts = self.parts.borrow();
        let Some(&head) = parts.keys().next_back() else {
            return false;
        };
        let waiting = self.waiting.borrow();
        (0..head).any(|index| match waiting.get(&index) {
            Some(PartArrival::Arriving) => true,
            Some(PartArrival::Unavailable) => false,
            None => {
                !parts.contains_key(&index)
                    && !self
                        .workspace
                        .store
                        .workspace_answered(&self.chain.part(index))
            }
        })
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
        self.start_where_none_is_held(&checkout.id);
        if !self.workspace.store.is_ready(&checkout.id) {
            match self.workspace.store.try_open_resource(&checkout) {
                Ok(_) => {}
                Err(ResourceOpenError::Arriving) => return Ok(Opened::Asked),
                // Nothing of it on this device, or a store that keeps no
                // canonical records (a test's) and has no record it could
                // open: it is started as every record is.
                Err(ResourceOpenError::MissingResource | ResourceOpenError::IdentityMismatch) => {
                    self.create(&checkout, self.part.initialize)?;
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
        self.workspace
            .store
            .declare_type(self.part.kind, self.part.surface);
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
        let part = (self.part.read)(&self.workspace.store, &self.chain.part(index))?;
        // A part held anew counts its state's revisions afresh: nothing
        // published or found of the one before stands for it.
        self.part_published.borrow_mut().remove(&index);
        self.part_search.borrow_mut().remove(&index);
        self.parts.borrow_mut().insert(index, part);
        self.moved();
        Ok(())
    }

    /// Open the record of waiting writes this device holds, starting it when
    /// `create` and this chain keeps no device queue to write them to.
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
            Err(ResourceOpenError::MissingResource | ResourceOpenError::IdentityMismatch)
                if create =>
            {
                self.create(&self.waiting_resource, |_| Ok(()))
                    .map_err(|error| {
                        tracing::warn!(kind = self.part.kind, %error, "the record of waiting writes cannot be started");
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
                tracing::warn!(kind = self.part.kind, %error, "the record of waiting writes cannot be opened");
                false
            }
        }
    }

    /// The actions waiting for a part of this chain, oldest first: each
    /// entry's id, its action's JSON and who invoked it.
    fn unsent(&self) -> Vec<(String, serde_json::Value, ActionInvoker)> {
        // A chain with no device queue (an import's, a tool's, a test's)
        // keeps them in the app's record of them.
        let Some(settings) = &self.workspace.device_queue else {
            return self.legacy_unsent();
        };
        self.take_legacy_unsent(settings);
        settings
            .get()
            .workspaces
            .get_mut(&self.workspace_key)
            .and_then(|chains| chains.remove(self.chain.as_str()))
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.id, entry.action, entry.invoker))
            .collect()
    }

    /// Move what still waits in the person's record of waiting writes into
    /// the device queue, after what the queue holds, each entry once. That
    /// record is a per-member one, which every replica only reads now: the
    /// queue remembers which of its entries it took, and nothing edits it.
    fn take_legacy_unsent(&self, settings: &ui_core::storage::Settings<QueuedWrites>) {
        let recorded = self.legacy_unsent();
        if recorded.is_empty() {
            return;
        }
        let chain = self.chain.as_str();
        let queue = settings.get();
        let taken = queue
            .taken
            .get(&self.workspace_key)
            .and_then(|chains| chains.get(chain));
        let new: Vec<_> = recorded
            .into_iter()
            .filter(|(id, ..)| taken.is_none_or(|taken| !taken.contains(id)))
            .collect();
        if new.is_empty() {
            return;
        }
        if let Err(error) = settings.update(|queue| {
            for (recorded, action, invoker) in new {
                queue
                    .taken
                    .entry(self.workspace_key.clone())
                    .or_default()
                    .entry(chain.to_owned())
                    .or_default()
                    .insert(recorded);
                queue
                    .workspaces
                    .entry(self.workspace_key.clone())
                    .or_default()
                    .entry(chain.to_owned())
                    .or_default()
                    .push(waiting::QueuedWrite {
                        id: format!("local:{}", tilo_resource::action::fresh_uuid()),
                        action,
                        invoker,
                    });
            }
        }) {
            tracing::error!(kind = self.part.kind, %error, "the earlier waiting writes could not join the device queue");
        }
    }

    fn legacy_unsent(&self) -> Vec<(String, serde_json::Value, ActionInvoker)> {
        if !self.open_unsent(false) {
            return Vec::new();
        }
        let kind = self.part.kind;
        self.workspace
            .store
            .read(&self.waiting_resource.id, |doc| {
                let Ok(Some((Value::Object(ObjType::List), list))) =
                    doc.get(ROOT, self.chain.as_str())
                else {
                    return Vec::new();
                };
                doc.list_range(&list, ..)
                    .filter_map(|item| {
                        let id = item.id().to_string();
                        let value = item.value.into_value();
                        serde_json::from_str::<(serde_json::Value, ActionInvoker)>(
                            value.as_str().unwrap_or_default(),
                        )
                        .map(|(action, invoker)| (id, action, invoker))
                        .map_err(
                            |error| tracing::warn!(kind, %error, "a waiting write does not read"),
                        )
                        .ok()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Take the entry `id` out of the record of waiting actions.
    fn forget_unsent(&self, id: &str) {
        if let Some(settings) = &self.workspace.device_queue {
            if let Err(error) = settings.update(|queue| {
                if let Some(chains) = queue.workspaces.get_mut(&self.workspace_key) {
                    if let Some(entries) = chains.get_mut(self.chain.as_str()) {
                        entries.retain(|entry| entry.id != id);
                        if entries.is_empty() {
                            chains.remove(self.chain.as_str());
                        }
                    }
                    if chains.is_empty() {
                        queue.workspaces.remove(&self.workspace_key);
                    }
                }
            }) {
                tracing::error!(kind = self.part.kind, %error, "could not finish a queued workspace write");
            }
            return;
        }
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
            tracing::error!(kind = self.part.kind, %error, "a written write still waits in its record");
        }
    }

    /// Change the record of waiting actions: nobody's undo step, saved as
    /// any edit of the app's records, naming the person whose window takes its write out.
    fn edit_unsent<T>(
        &self,
        edit: impl FnOnce(
            &mut ui_core::automerge::AutoCommit,
        ) -> Result<T, ui_core::automerge::AutomergeError>,
    ) -> Result<T, String> {
        let id = &self.waiting_resource.id;
        let acting = tilo_actions::Acting::member(self.workspace.me.clone());
        let edited = self
            .workspace
            .store
            .with_history(id, |doc, history| {
                let edited = edit(doc).map_err(|error| error.to_string())?;
                let message = tilo_actions::ActionContext::whole_edit_by(&acting, &*doc);
                doc.commit_with(
                    ui_core::automerge::transaction::CommitOptions::default().with_message(message),
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
}

impl ChainState {
    /// Hold the chain of `P` kept at `chain` in `workspace`, with its head
    /// when this device has one: the app's own ([`Chained::chain`]), or one
    /// beside a resource: the messages of one direct conversation, which
    /// lives beside the resource the two people share
    /// ([`Chained::chain_beside`]). Its actions read the app's resource from
    /// `base`.
    pub(crate) fn open<P: Chained>(
        workspace: WorkspaceResources,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
        chain: DocumentId,
    ) -> Option<Self> {
        Self::open_as(
            workspace,
            &part::Of::<P>::PART,
            Rc::new(move || base() as Rc<dyn Any>),
            chain,
        )
    }

    /// [`Self::open`], whatever resource the parts are.
    fn open_as(
        workspace: WorkspaceResources,
        part: &'static PartType,
        base: Base,
        chain: DocumentId,
    ) -> Option<Self> {
        let mut doc = ui_core::automerge::AutoCommit::new();
        (part.initialize)(&mut doc)
            .map_err(|error| tracing::warn!(kind = part.kind, %error, "chain part has no genesis"))
            .ok()?;
        let empty = (part.state)(&doc)?;
        let state = Self {
            core: ChainCore::new(workspace, part, chain),
            base,
            empty,
            published: RefCell::new(None),
            view: (part.new_view)(),
            view_at: Cell::new(None),
            written: RefCell::default(),
            undone: RefCell::default(),
            unwritten: RefCell::default(),
            draining: Cell::new(false),
            index: RefCell::default(),
            answered: RefCell::default(),
        };
        // Asked for as the chain opens, with the window's other resources:
        // the newest part arrives in the same round trip. A chain with a part
        // here already knows its newest one, and asks for nothing.
        if state.head().is_none() {
            let _ = state.core.head_on_its_way();
        }
        state.core.look();
        Some(state)
    }

    fn part(&self) -> &'static PartType {
        self.core.part
    }

    /// The parts held, oldest first.
    fn parts(&self) -> Vec<(u64, Rc<dyn Any>)> {
        self.refresh();
        self.held()
    }

    /// The parts held, oldest first, as they were last followed.
    fn held(&self) -> Vec<(u64, Rc<dyn Any>)> {
        self.core
            .parts
            .borrow()
            .iter()
            .map(|(index, part)| (*index, part.state()))
            .collect()
    }

    pub(crate) fn head(&self) -> Option<u64> {
        self.refresh();
        self.core.parts.borrow().keys().next_back().copied()
    }

    /// Whether no part is held here and the workspace has not yet said which
    /// one is the newest ([`ChainCore::head_on_its_way`]).
    pub(crate) fn head_arriving(&self) -> bool {
        self.head().is_none() && self.core.head_on_its_way()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.refresh();
        self.core.revision.get()
    }

    /// Follow every held part that changed, and take up a newer head that
    /// arrived on this device. Whether anything moved.
    pub(crate) fn refresh(&self) -> bool {
        let mut moved = self.core.follow_parts();
        moved |= self.follow_unsent();
        if self.core.workspace.store.takes_writes() {
            self.apply_pending();
        }
        if moved {
            self.core.moved();
        }
        let revision = self.core.revision.get();
        if self.view_at.replace(Some(revision)) != Some(revision) {
            (self.part().set_view)(&*self.view, (self.part().typed)(&self.held()));
        }
        moved
    }

    /// Apply `action`, a `P::Action` invoked by `invoker`, to the part it
    /// changes: read against the app's resource ([`Chained::prepare`]),
    /// checked against what they may do in the workspace, attributed to them
    /// and written into the part's history. The part it was written to, with
    /// the item resources it created.
    ///
    /// While that part is on its way from the workspace (a device back from
    /// away, before its head arrived) the action waits for it: `Ok(None)`.
    /// It is kept in the person's list of such actions with the app's
    /// resources, shown in [`Self::unwritten`] by every window of the app,
    /// and applied by whichever holds the chain once the part is here, after
    /// those that waited before it — after the window closed or the app
    /// restarted too. `later` is taken then: told the part or why it was
    /// refused if this process still runs; a refusal nobody waits for is
    /// logged. Otherwise it is left for the caller to answer now.
    ///
    /// # Errors
    /// As the API refuses it now: the person may not, the app's resource or
    /// the part refused it, or the list of waiting actions cannot be written.
    pub(crate) fn apply_as(
        &self,
        action: &dyn Any,
        invoker: &ActionInvoker,
        later: &mut Option<Later>,
    ) -> Result<Option<(u64, Made)>, ActionError> {
        // Behind the ones already waiting: actions are applied in the order asked.
        if self.unsent().is_empty() {
            match self.try_apply(action, invoker)? {
                Write::Arriving => {}
                Write::Done(written, created) => {
                    let index = written.last();
                    self.wrote(invoker, written, (self.part().is_step)(action));
                    return Ok(Some((index, created)));
                }
            }
        }
        let id = self
            .core
            .keep_unsent(&(self.part().to_json)(action)?, invoker)?;
        if let Some(later) = later.take() {
            ANSWERS.with(|answers| answers.borrow_mut().insert(id, later));
        }
        if self.follow_unsent() {
            self.core.moved();
        }
        Ok(None)
    }

    /// What the actions waiting for their part write ([`Self::apply_as`]),
    /// read as a part holding only that: what a view shows as being sent.
    /// `None` while nothing waits.
    pub(crate) fn unwritten(&self) -> Option<Rc<dyn Any>> {
        if self.follow_unsent() {
            self.core.moved();
        }
        self.unwritten.borrow().1.clone()
    }

    /// Follow edits to the queue from other windows on this device.
    fn follow_unsent(&self) -> bool {
        let heads = self
            .core
            .open_unsent(false)
            .then(|| {
                self.core.workspace.store.read(
                    &self.core.waiting_resource.id,
                    ui_core::automerge::AutoCommit::base_heads,
                )
            })
            .flatten()
            .unwrap_or_default();
        let revision = (
            heads,
            self.core
                .workspace
                .device_queue
                .as_ref()
                .map(ui_core::storage::Settings::revision),
        );
        if self.unwritten.borrow().0 == revision {
            return false;
        }
        let unwritten = self.read_unwritten(&self.unsent());
        *self.unwritten.borrow_mut() = (revision, unwritten);
        true
    }

    /// Write `action` into its part now, as [`Self::apply_as`], or say the
    /// part — or an earlier one what it names may be kept in — is on its way.
    fn try_apply(&self, action: &dyn Any, invoker: &ActionInvoker) -> Result<Write, ActionError> {
        let part = self.part();
        // Which part is the newest is on its way: a part started now would be
        // an old one, or a second first one.
        if self.head_arriving() {
            tracing::warn!(kind = part.kind, "a write waits for the chain head");
            return Ok(Write::Arriving);
        }
        let base = (self.base)();
        let kept = self.parts();
        let parts = (part.typed)(&kept);
        let action = match (part.prepare)(&[action], &*base, &*parts, invoker) {
            Ok(prepared) => prepared,
            // What it names may be in a part on its way: it waits for it.
            Err(_) if self.core.earlier_on_its_way() => return Ok(Write::Arriving),
            Err(refused) => return Err(ActionError::from(refused)),
        }
        .pop()
        .ok_or_else(|| ActionError::Refused("the action was not prepared".into()))?;
        let (place, by_app) = (part.route)(&*action, &*base, &*parts);
        drop(base);
        if let Some(shares) = self
            .shares(&*action, &*parts, &kept)
            .map_err(ActionError::Refused)?
        {
            drop(parts);
            return self.write_shares(shares, invoker, place.as_deref());
        }
        let by_app = by_app.or_else(|| self.listed_part(&*action, &kept).map(Ok));
        let index = self
            .place(&*action, by_app, &kept)
            .map_err(ActionError::Refused)?;
        drop(parts);
        self.may(&*action)?;
        let newest = self.core.parts.borrow().keys().next_back().copied();
        match self
            .core
            .open_or_start(index)
            .map_err(|error| self.core.refused(error))?
        {
            Opened::Held => {}
            Opened::Started => {
                if let Some(previous) = newest.filter(|newest| newest.checked_add(1) == Some(index))
                {
                    self.carry(previous, index, invoker)?;
                }
            }
            Opened::Asked => return Ok(Write::Arriving),
        }
        // A field edit and a verb declared to answer nothing (a move) answer
        // nothing, whatever they made; the rest answer what the part says
        // they made (a note's id, the blocks a body's action touched).
        // An edit naming no verb answers as the published action it carries
        // out, when that declares an answer (a body's `replace-range`).
        let verb = (part.verb)(&*action);
        let asked = ASKED.with(|asked| asked.borrow().clone());
        let answering = verb.as_deref().or(asked.as_deref()).filter(|verb| {
            *verb != tilo_resource::FIELD_EDIT
                && *verb != tilo_resource::APPLY
                && !(part.answers_nothing)(verb)
        });
        let (written, encoded) = (part.written)(&*action)?;
        let created = self.write_answering(index, written, encoded, invoker, place, answering)?;
        self.spread(&*action, index, invoker);
        Ok(Write::Done(Written::one(index), created))
    }

    /// Write what `action`, just written into part `index`, means for the
    /// other parts held here ([`Chained::spread`]), each as `invoker`'s step
    /// in its part. One a part refuses is logged: the action itself stands.
    fn spread(&self, action: &dyn Any, index: u64, invoker: &ActionInvoker) {
        let part = self.part();
        let parts = self.parts();
        let typed = (part.typed)(&parts);
        for (at, further) in (part.spread)(action, index, &*typed) {
            if at == index || !parts.iter().any(|(held, _)| *held == at) {
                continue;
            }
            let written = (part.written)(&*further)
                .and_then(|(further, encoded)| self.write(at, further, encoded, invoker, None));
            if let Err(error) = written {
                tracing::warn!(kind = part.kind, part = at, %error, "a change was not carried to another part");
            }
        }
    }

    /// Apply `group` — several actions invoked as one — as the part (or
    /// parts) it changes: one change per part, each carrying that part's
    /// members as one group. Nothing is written while a part a member changes
    /// is on its way.
    fn try_apply_group(
        &self,
        group: &[Box<dyn Any>],
        invoker: &ActionInvoker,
    ) -> Result<Write, ActionError> {
        if group.is_empty() {
            return Ok(Write::Done(
                Written::one(self.head().unwrap_or(0)),
                Vec::new(),
            ));
        }
        if self.head_arriving() {
            return Ok(Write::Arriving);
        }
        let part = self.part();
        let base = (self.base)();
        let kept = self.parts();
        let parts = (part.typed)(&kept);
        let members: Vec<&dyn Any> = group.iter().map(|action| &**action).collect();
        let prepared = match (part.prepare)(&members, &*base, &*parts, invoker) {
            Ok(prepared) => prepared,
            Err(_) if self.core.earlier_on_its_way() => return Ok(Write::Arriving),
            Err(refused) => return Err(ActionError::from(refused)),
        };
        let mut places = Vec::with_capacity(group.len());
        let mut indices = Vec::with_capacity(group.len());
        for action in &prepared {
            let (place, by_app) = (part.route)(&**action, &*base, &*parts);
            let by_app = by_app.or_else(|| self.listed_part(&**action, &kept).map(Ok));
            places.push(place);
            self.may(&**action)?;
            indices.push(
                self.place(&**action, by_app, &kept)
                    .map_err(ActionError::Refused)?,
            );
        }
        drop(base);
        drop(parts);
        let per_part = by_part(&indices);
        // Every part a member changes must be here: the group is one step, so
        // none of it is written while a part is on its way.
        for (index, _) in &per_part {
            match self
                .core
                .open_or_start(*index)
                .map_err(|error| self.core.refused(error))?
            {
                Opened::Held | Opened::Started => {}
                Opened::Asked => return Ok(Write::Arriving),
            }
        }
        let newest = self.core.parts.borrow().keys().next_back().copied();
        let mut written = Vec::with_capacity(per_part.len());
        let mut created = Vec::new();
        for (index, members) in &per_part {
            let index = *index;
            if let Some(previous) = newest.filter(|newest| newest.checked_add(1) == Some(index)) {
                self.carry(previous, index, invoker)?;
            }
            let place = members
                .first()
                .and_then(|at| places.get(*at).cloned().flatten());
            let members: Vec<Box<dyn Any>> = members
                .iter()
                .filter_map(|at| Some((part.clone)(&**prepared.get(*at)?)))
                .collect();
            let (group, encoded) = (part.group)(members)?;
            created = self.write(index, &*group, encoded, invoker, place)?;
            written.push(index);
        }
        for (index, members) in &per_part {
            for at in members {
                if let Some(action) = prepared.get(*at) {
                    self.spread(&**action, *index, invoker);
                }
            }
        }
        if written.is_empty() {
            written.push(0);
        }
        Ok(Write::Done(Written(written), created))
    }

    /// Apply `group` — several actions invoked as one — as the part (or
    /// parts) they change: one change per part, one step. While a part it
    /// changes is on its way from the workspace, the whole group waits for
    /// it, as one action does ([`Self::apply_as`]), and is written once it
    /// arrives. A group answered `ok` while nothing was written would be
    /// writes lost without a word.
    fn apply_group(
        &self,
        group: &[Box<dyn Any>],
        invoker: &ActionInvoker,
    ) -> Result<Write, ActionError> {
        // Behind the ones already waiting: actions are applied in the order asked.
        if group.is_empty() || self.unsent().is_empty() {
            match self.try_apply_group(group, invoker)? {
                Write::Done(written, created) => return Ok(Write::Done(written, created)),
                Write::Arriving => {}
            }
        }
        if group.is_empty() {
            return Ok(Write::Done(
                Written::one(self.head().unwrap_or(0)),
                Vec::new(),
            ));
        }
        tracing::debug!(
            kind = self.part().kind,
            members = group.len(),
            "a group waits for the part it changes"
        );
        self.core.keep_unsent_group(group, invoker)?;
        Ok(Write::Arriving)
    }

    /// Whether `invoker` may change this chain by `action`, refused as the
    /// API refuses it where the chain publishes the action: a grant on the
    /// app, the parts' kind or the verb covers it.
    fn may(&self, action: &dyn Any) -> Result<(), ActionError> {
        if self.core.workspace.permissions.is_none() {
            return Ok(());
        }
        let part = self.part();
        let Some(need) = (part.need)(action) else {
            return Ok(());
        };
        self.core.may(&need, (part.verb)(action).as_deref())
    }

    /// Write into part `index`, just started after part `previous`, what it
    /// carries of the parts before it ([`Chained::carry`]), as `invoker`'s.
    fn carry(&self, previous: u64, index: u64, invoker: &ActionInvoker) -> Result<(), ActionError> {
        let part = self.part();
        let Some(carried) = self
            .core
            .parts
            .borrow()
            .get(&previous)
            .and_then(|held| (part.carry)(previous, &*held.state()))
        else {
            return Ok(());
        };
        let action = carried.map_err(ActionError::Refused)?;
        let (action, encoded) = (part.written)(&*action)?;
        self.write(index, action, encoded, invoker, None).map(drop)
    }

    /// [`ChainCore::apply_history`] of `edit`, then the chain followed.
    fn apply_history(
        &self,
        edit: &PartEdit,
        invoker: &ActionInvoker,
    ) -> Result<serde_json::Value, ActionError> {
        let action = (self.part().history)(edit);
        let answer = self.core.apply_history(edit, &*action, invoker)?;
        self.refresh();
        self.core.moved();
        Ok(answer)
    }

    /// [`ChainCore::write`], then the chain followed.
    fn write(
        &self,
        index: u64,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
        place: Option<String>,
    ) -> Result<Made, ActionError> {
        let created = self
            .core
            .write(index, action, encoded, invoker, place, &mut |_, _, _| {})?;
        self.refresh();
        self.core.moved();
        Ok(created)
    }

    /// [`Self::write`] of the action its caller invoked, keeping what it answers the caller with
    /// for [`ResourceApi::apply_answering`]: what the verb `answering` answers
    /// ([`Resource::answer`]: the note it made), and the text it wrote.
    fn write_answering(
        &self,
        index: u64,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
        place: Option<String>,
        answering: Option<&str>,
    ) -> Result<Made, ActionError> {
        let part = self.part();
        let mut answered = |doc: &mut _, before: &[ChangeHash], minted: &[String]| {
            *self.answered.borrow_mut() = part.answered(answering, doc, before, minted);
        };
        let created = self
            .core
            .write(index, action, encoded, invoker, place, &mut answered)?;
        self.refresh();
        self.core.moved();
        Ok(created)
    }

    /// The chain as it is published: the head's actions and every held
    /// part's items, each carried out through [`CHAIN_ACTION_TYPE`] on the
    /// part it changes; checked against the member's roles in the workspace
    /// as the app's own actions are.
    pub(crate) fn publish(&self) -> tilo_actions::ResourceSpec {
        self.publish_shared().to_resource()
    }

    /// [`Self::publish`] without its items: what a table of the chain's own
    /// actions reads, and what a name the app's level carries is answered
    /// with — the head part's resource, never a read of every part the chain
    /// holds.
    pub(crate) fn publish_head(&self) -> tilo_actions::ResourceSpec {
        let part = self.part();
        let head = match self.parts().last() {
            Some((_index, head)) => (part.publish_head)(&self.core.id, &**head),
            None => (part.publish)(&self.core.id, &*self.empty),
        };
        self.core.head_resource(&head)
    }

    /// [`Self::publish`] as a shared tree ([`ChainCore::publish`]).
    pub(crate) fn publish_shared(&self) -> ui_core::published::SharedResource {
        let revision = (self.revision(), tilo_ui_base::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let part = self.part();
        let parts = self.parts();
        let newest = match parts.last() {
            Some((index, head)) => self.part_published(*index, head),
            None => Rc::new(PartPublished {
                head: (part.publish)(&self.core.id, &*self.empty),
                items: Vec::new(),
            }),
        };
        let earlier: Vec<Rc<PartPublished>> = parts
            .iter()
            .rev()
            .skip(1)
            .map(|(index, state)| self.part_published(*index, state))
            .collect();
        let typed = (part.typed)(&parts);
        // The places the app derives from everything the chain holds, at the
        // address each is published under, and the tree its items make.
        let places = (part.places)(&*(self.base)(), &*typed);
        let tree = (part.tree)(&*typed);
        let held: Vec<u64> = parts.iter().map(|(index, _)| *index).collect();
        self.core.forget_unheld(&held);
        // What the chain does not show (`Chained::hidden`) is left out of what it publishes.
        let published = self.core.publish(
            &newest,
            earlier,
            places,
            &tree,
            &(part.hidden)(&*typed),
            part.table,
        );
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    /// What search finds in every held part, each part's entries built
    /// again only when that part changed: the chain's items, in the window's
    /// search beside the app's own.
    pub(crate) fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        let part = self.part();
        let parts = self.parts();
        let held: Vec<u64> = parts.iter().map(|(index, _)| *index).collect();
        self.core.forget_unheld(&held);
        let typed = (part.typed)(&parts);
        let mut entries = (part.search_over)(&*(self.base)(), &*typed);
        self.forget_search_readers(&held);
        for (index, state) in &parts {
            entries.extend(self.searched_part(*index, &**state).iter().cloned());
        }
        let hidden = (part.hidden)(&*typed);
        if !hidden.is_empty() {
            entries.retain(|entry| {
                entry
                    .anchor
                    .as_deref()
                    .is_none_or(|anchor| !hidden.contains(anchor))
            });
        }
        entries
    }

    /// [`ChainCore::part_published`] of part `index`, read as `state`.
    fn part_published(&self, index: u64, state: &Rc<dyn Any>) -> Rc<PartPublished> {
        let part = self.part();
        self.core.part_published(
            index,
            &|| (part.publish_head)(&self.core.id, &**state),
            &|| (part.children)(&**state),
        )
    }
}

/// A chain of records the app keeps beside its own, held for a view or
/// surface. See the module docs.
pub struct Chain<P: Chained> {
    state: Rc<ChainState>,
    dispatch: Dispatcher<P>,
}

/// A part of a chain as a record a view part is built over
/// ([`Chain::part_record`]).
pub struct PartRecord<P: Chained> {
    chain: Rc<ChainState>,
    held: Rc<super::resources::Held<P>>,
}

impl<P: Chained> super::resources::sealed::Hold<P> for PartRecord<P> {
    fn held(
        &self,
        _storage: super::resources::sealed::Storage<'_>,
    ) -> Option<Rc<super::resources::Held<P>>> {
        Some(Rc::clone(&self.held))
    }

    fn api(
        &self,
        _storage: super::resources::sealed::Storage<'_>,
    ) -> Option<super::resources::sealed::Api> {
        Some(super::resources::sealed::Api(
            Rc::clone(&self.chain) as Rc<dyn ResourceApi>
        ))
    }
}

/// The chain as a record whose actions a view binds
/// ([`ViewCx::resource_actions`](super::ViewCx::resource_actions)): its verbs and
/// every held part's items, carried out on the part each names, whether or
/// not a part is held yet (a first issue made in an empty tracker).
impl<P: Chained> super::resources::sealed::Hold<P> for Chain<P> {
    fn held(
        &self,
        storage: super::resources::sealed::Storage<'_>,
    ) -> Option<Rc<super::resources::Held<P>>> {
        let head = self.head()?;
        self.part_record(head)?.held(storage)
    }

    fn api(
        &self,
        _storage: super::resources::sealed::Storage<'_>,
    ) -> Option<super::resources::sealed::Api> {
        Some(super::resources::sealed::Api(
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
    pub(crate) fn new(state: Rc<ChainState>, dispatch: Dispatcher<P>) -> Self {
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
        self.state
            .parts()
            .into_iter()
            .filter_map(|(index, state)| Some((index, state.downcast().ok()?)))
            .collect()
    }

    /// The parts this window holds, oldest first, as view state that
    /// follows every change of any of them: what a view that reads across
    /// parts (a list of every item) follows, the way [`ViewCx::value`]
    /// follows the app's own record.
    ///
    /// [`ViewCx::value`]: super::ViewCx::value
    #[must_use]
    pub fn state(&self) -> tilo_ui_base::LocalState<Parts<P>> {
        let _ = self.state.refresh();
        self.state
            .view
            .downcast_ref::<tilo_ui_base::LocalState<Parts<P>>>()
            .cloned()
            .unwrap_or_else(|| tilo_ui_base::LocalState::new(Rc::default()))
    }

    /// Part `index` as a record a part of the view is built over
    /// ([`ViewCx::record`](super::ViewCx::record)): its controls bind the
    /// part's paths (an item's title, its rich-text body), and what they
    /// change is carried out through the chain, on the part that holds it,
    /// as the person. `None` while the part is not held here.
    #[must_use]
    pub fn part_record(&self, index: u64) -> Option<PartRecord<P>> {
        let live = live_of::<P>(&**self.state.core.parts.borrow().get(&index)?)?.clone();
        let syncing = Rc::downgrade(&self.state);
        let applying = Rc::downgrade(&self.state);
        let held = super::resources::Held::new(
            self.state.core.id.clone(),
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

    /// Read part `index`'s document as this window holds it: what a hand
    /// widget showing an item of the part (a rich-text body) reads, as a
    /// view's widgets read the app's own record. `None` while the part is
    /// not held here.
    pub fn read_part<T>(
        &self,
        index: u64,
        read: impl FnOnce(&ui_core::automerge::AutoCommit) -> T,
    ) -> Option<T> {
        if !self.state.core.parts.borrow().contains_key(&index) {
            return None;
        }
        self.state
            .core
            .workspace
            .store
            .read(&self.state.core.chain.part(index), read)
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
        self.state.core.hold(index)
    }

    /// Where part `index`, asked for and not read here yet, stands: on its
    /// way (a view shows it loading, not empty), or unavailable (a view says
    /// so, with nothing spinning). `None` once it is read.
    #[must_use]
    pub fn waiting(&self, index: u64) -> Option<PartArrival> {
        self.state.refresh();
        self.state.core.waiting(index)
    }

    /// Every part asked for and not read here yet, oldest first.
    #[must_use]
    pub fn waited(&self) -> Vec<(u64, PartArrival)> {
        self.state.refresh();
        self.state.core.waited()
    }

    /// Apply `action` as the window's person, or whoever the invocation
    /// carried out here is for ([`super::resources::acting`]), to the part it changes. The
    /// part's number; `None` while that part is on its way from the
    /// workspace: the action waits for it, shows in [`Self::unwritten`]
    /// meanwhile, and is written once it is here, in the order asked.
    ///
    /// # Errors
    /// As the API refuses it: the person may not, the app's record or the
    /// part refused it.
    pub fn apply(&self, action: &P::Action) -> Result<Option<u64>, ActionError> {
        let me = self.dispatch.me().unwrap_or(&self.state.core.workspace.me);
        self.state
            .apply_as(action, &super::resources::acting(me), &mut None)
            .map(|written| written.map(|(index, _)| index))
    }

    /// Apply what `invoke` names, as [`Self::apply`].
    ///
    /// # Errors
    /// As [`Self::apply`], or the arguments do not build the action.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<Option<u64>, ActionError> {
        if invoke.is_group() {
            let group = tilo_resource::group::build_group::<P>(
                &invoke.arguments,
                tilo_resource::origin_now_ms(),
            )
            .map_err(ActionError::Refused)?;
            let mut members: Vec<Box<dyn Any>> = Vec::with_capacity(group.actions.len());
            for member in group.actions {
                super::resources::not_by_app::<P>(&member).map_err(ActionError::Refused)?;
                members.push(Box::new(member));
            }
            let me = self.dispatch.me().unwrap_or(&self.state.core.workspace.me);
            let invoker = super::resources::acting(me);
            return self
                .state
                .apply_group(&members, &invoker)
                .map(|written| match written {
                    Write::Done(written, _) => {
                        let index = written.last();
                        self.state.wrote(&invoker, written, true);
                        Some(index)
                    }
                    Write::Arriving => None,
                });
        }
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
        let me = self.dispatch.me().unwrap_or(&self.state.core.workspace.me);
        let mut later: Option<Later> = Some(Box::new(then));
        let answer = match self
            .state
            .apply_as(&action, &super::resources::acting(me), &mut later)
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
        self.state.unwritten()?.downcast().ok()
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
        let me = self.dispatch.me().unwrap_or(&self.state.core.workspace.me);
        let edit = PartEdit {
            part: index,
            edit: super::history::HistoryEdit::Undo {
                steps: None,
                invocation: None,
                by: None,
                to: None,
                step: None,
                chosen: None,
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
        self.state.core.start(index)?;
        let id = self.state.core.chain.part(index);
        self.state
            .core
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
        if let Err(error) = self
            .state
            .core
            .workspace
            .store
            .schedule_committed_history(&id)
        {
            tracing::error!(%error, "a created part could not be saved");
        }
        self.state.refresh();
        self.state.core.moved();
        Ok(())
    }

    /// Whether the chain takes a change now: not while a frame reads the
    /// app's records (a view's sync), when [`Self::stage_as`] waits for a
    /// later pass (an event, a settle pass).
    #[must_use]
    pub fn takes_changes(&self) -> bool {
        self.state.core.workspace.store.takes_writes()
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
        Self::detached_beside_in(store, app_id, None, me, base)
    }

    /// [`Self::detached_in`] of the chain beside the record `record` rather
    /// than beside the app's record of the workspace
    /// ([`Chained::chain_beside`]): the parts of an app that keeps a record
    /// per member, the person's record being `record`.
    pub fn detached_beside_in(
        store: ResourceStore,
        app_id: &'static str,
        record: Option<&DocumentId>,
        me: &tilo_resource::MemberId,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Self> {
        let workspace = WorkspaceResources {
            store,
            me: me.clone(),
            permissions: None,
            app_id,
            app_title: app_id,
            device_queue: None,
        };
        let chain = match record {
            Some(record) => P::chain_beside(record),
            None if P::BESIDE_RESOURCE => return None,
            None => P::chain(),
        };
        let state = Rc::new(ChainState::open::<P>(workspace, base, chain)?);
        let dispatch = detached_dispatch(app_id, &state, me);
        Some(Self { state, dispatch })
    }

    /// The same detached chain edited as `me`: another person's window
    /// over it, in a test.
    #[must_use]
    pub fn as_reader(&self, me: &tilo_resource::MemberId) -> Self {
        Self {
            state: Rc::clone(&self.state),
            dispatch: detached_dispatch(self.state.core.workspace.app_id, &self.state, me),
        }
    }

    /// Part `index` as a document's bytes, for a tool that carries it to
    /// another device itself; `None` when the chain holds no such part.
    #[must_use]
    pub fn part_bytes(&self, index: u64) -> Option<Vec<u8>> {
        self.state
            .core
            .workspace
            .store
            .read(&self.state.core.chain.part(index), |doc| doc.clone().save())
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

/// The record of `me`'s actions waiting for their part, in the workspace the
/// app's records are kept in: their own there, as the app's records of a
/// kind are ([`DocumentId::of_member`]), or the one a person without a root
/// identity keeps on this device.
fn unsent_checkout(me: &tilo_resource::MemberId) -> DocumentCheckout {
    let kind = ResourceType::new(UNSENT_KIND);
    let id = me
        .person_id()
        .and_then(|person| DocumentId::of_member(person, UNSENT_KIND))
        .unwrap_or_else(|| DocumentId::derived(kind.as_str(), &DocumentId::for_tab(0)));
    DocumentCheckout::new(id, kind)
}

/// Tell whoever waits for the action kept as `id` what became of it; a
/// refusal nobody waits for is logged.
fn answer(kind: &str, id: &str, written: Result<u64, ActionError>) {
    match ANSWERS.with(|answers| answers.borrow_mut().remove(id)) {
        Some(later) => later(written),
        None => {
            if let Err(error) = written {
                tracing::warn!(kind, %error, "a write that waited for its part was refused");
            }
        }
    }
}
