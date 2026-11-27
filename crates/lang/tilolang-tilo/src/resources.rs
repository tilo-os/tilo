//! [`ResourceStore`]: the runtime's [`Store`] over the device's one resource store.
//!
//! An app holds no store of its own. Each resource a tilolang app keeps (a root and every item
//! nested in it) is a resource of the device's store, of the kind its type is published as
//! ([`kinds`]), nested in the resource whose collection holds it and placed among its siblings by
//! an order key. Every read and write is a command to that store ([`Port`]), answered before the
//! next is sent.
//!
//! **What a run wrote, before it is sent.** The runtime reads its own writes at once, and one
//! action's writes go to the store as one request, under the action that made them. So what a
//! transaction writes is kept here ([`Inner::local`]) and read before anything the store
//! answers; committing it turns it into commands stamped with the action, which are sent as one
//! batch when the run ends ([`Store::end_run`]), or at once outside a run. A run that fails, a
//! trial run ([`ResourceStore::checkpoint`]) and a rolled-back transaction drop them, and the
//! store never saw them.
//!
//! **What was read.** Resources the store answered are kept ([`Inner::cache`]), and so are the
//! pages of open listings ([`Inner::listings`]). A write here lets go of the listings of the
//! kinds it wrote, and keeps every other; both are dropped whole when the workspace's resources
//! moved on elsewhere ([`ResourceStore::moved`]).

pub mod kinds;
mod steps;

use std::cell::{Cell as StdCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::{Rc, Weak};

pub use collab_types::store::wire::Turn;
use collab_types::store::wire::{Command, Edit, Reply};
use collab_types::store::{
    Absent, Cell, Cursor, Filter, Indexed, Order, PARENT, Query, Resource, Value,
};
use serde_json::Value as Json;
use tilo_resource_store::Stamp;
use tilolang_runtime::Journal;
use tilolang_runtime::ids::{Actor, DocId, FieldId, ItemId, RunId, TypeId};
use tilolang_runtime::meta::{FieldKind, Meta, Ty};
use tilolang_runtime::store::{
    ANY_FIELD, Applied, BytesData, ChangeNote, ChangeSummary, ClockField, DeliveryState,
    EffectEntry, EffectState, FieldKey, FieldRead, FieldSpec, FieldValue, Link, LinkHow, Listing,
    Lit, Mutation, NewResource, NextChange, PagedItems, Pred, RecordReach, Scalar, Settled, Store,
    StoreError, SubId, Txn, Version, VersionKind, doc_of, field_json, field_of, ordered_cmp,
};
use tilolang_runtime::value::ResourceRef;

pub use kinds::{Kinds, TypeEntry, kind_of};
pub use tilo_resource_store::{LocalPort, Port};

/// The content field holding the migrations that ran on a resource, by action id.
const MIGRATED: &str = "$migrated";

/// The content field holding what each trigger last ran for a resource, by trigger id: the
/// period it ran in, or the instant it fired at.
const TRIGGERED: &str = "$triggered";

/// What the platform writes itself rather than an action someone invoked: the person whose
/// device writes it is its invoker. Any other write names its own, or none, and a write naming
/// none is refused where the workspace admits it. A name added here carries, beside it, the
/// reason its writer cannot name its invoker itself: an action's write never belongs here.
const PLATFORM_WRITES: &[&str] = &[
    tilolang_runtime::store::CREATE_ACTION,
    "$settle",
    "$settle-block",
];

/// The collection an item stands in, by its field's id, where its holder has another
/// collection that could hold it too (a notebook's notes and the notes it set aside): the
/// parent alone does not say which.
const STANDS_IN: &str = "$in";

/// The kind an outside effect waiting in the workspace is kept as: one resource per entry,
/// nested in the resource whose action queued it so it leaves with that resource, and read by
/// its author alone so what it carries (a message's words, an address) reaches no other member.
/// Its fields are the `OUT_*` names below.
pub const EFFECT: &str = "tilolang.effect";
pub(crate) const OUT_DOC: &str = "doc";
pub(crate) const OUT_ENTRY: &str = "entry";
/// The app whose package queued the effect, stamped by the writer: what carry-out checks holds
/// the capability's grant.
pub(crate) const OUT_APP: &str = "app";
pub(crate) const OUT_CAPABILITY: &str = "capability";
/// What is handed over, as JSON text.
pub(crate) const OUT_PAYLOAD: &str = "payload";
pub(crate) const OUT_HELD_UNTIL: &str = "held_until";
/// The device that carries it out.
pub(crate) const OUT_OWNER: &str = "owner";
/// Where it is on its way out: empty while queued, `sending` or `waiting`, with `detail`.
pub(crate) const OUT_STATE: &str = "state";
pub(crate) const OUT_DETAIL: &str = "detail";
pub(crate) const OUT_SETTLED: &str = "settled";
pub(crate) const OUT_OUTCOME: &str = "outcome";
pub(crate) const OUT_REASON: &str = "reason";

/// The id the effect entry `entry` of the document `doc` is kept under.
fn effect_id(doc: &str, entry: &str) -> String {
    format!("{doc}!{entry}")
}

/// The text the field `name` of `fields` holds; empty where it holds none.
fn text_field(fields: &BTreeMap<String, Cell>, name: &str) -> String {
    match fields.get(name) {
        Some(Cell::One(Value::Text(text))) => text.clone(),
        _ => String::new(),
    }
}

/// How an effect entry settled: the word for it and why.
type Outcome = (String, String);

/// The effect entry `fields` hold, and how it settled when it did: its outcome and reason.
fn effect_entry(fields: &BTreeMap<String, Cell>) -> (EffectEntry, Option<Outcome>) {
    let detail = text_field(fields, OUT_DETAIL);
    let state = match text_field(fields, OUT_STATE).as_str() {
        "sending" => EffectState::Sending { by: detail },
        "awaiting" => EffectState::Awaiting { by: detail },
        "waiting" => EffectState::Waiting { reason: detail },
        _ => EffectState::Queued,
    };
    let settled =
        matches!(fields.get(OUT_SETTLED), Some(Cell::One(Value::Bool(true)))).then(|| {
            (
                text_field(fields, OUT_OUTCOME),
                text_field(fields, OUT_REASON),
            )
        });
    let held_until = text_field(fields, OUT_HELD_UNTIL);
    (
        EffectEntry {
            doc: text_field(fields, OUT_DOC),
            id: text_field(fields, OUT_ENTRY),
            app: text_field(fields, OUT_APP),
            capability: text_field(fields, OUT_CAPABILITY),
            payload: tilo_failure::ignored!(
                "an effect entry written with no payload carries none",
                serde_json::from_str(&text_field(fields, OUT_PAYLOAD))
            )
            .unwrap_or(Json::Null),
            held_until: (!held_until.is_empty()).then_some(held_until),
            state,
        },
        settled,
    )
}

/// How many resources one page of a listing reads.
const PAGE: usize = 200;

/// The runtime's name for a bound control's edit (`ViewInstance::edit`).
const RUNTIME_EDIT: &str = "$edit";

/// A resource as this store reads it: what the store indexes of it, and its content once read.
#[derive(Clone, Debug)]
struct Held {
    kind: String,
    parent: Option<String>,
    order: Option<String>,
    fields: BTreeMap<String, Cell>,
    /// The content; `None` until it is read.
    content: Option<serde_json::Map<String, Json>>,
}

impl Held {
    fn of(kind: &str, indexed: Indexed) -> Self {
        Self {
            kind: kind.to_owned(),
            parent: indexed.parent,
            order: indexed.order,
            fields: indexed.fields,
            content: None,
        }
    }
}

/// A resource as the store held it at one change: the fields its document's index holds, and
/// its content.
#[derive(Clone, Debug)]
struct Look {
    indexed: Option<Indexed>,
    content: Option<Json>,
}

/// What a transaction did to one resource, in the order it first touched them.
#[derive(Clone, Debug, Default)]
struct Dirty {
    /// Its indexed part changed, or it was made: it is put whole.
    put: bool,
    removed: bool,
    /// The kind it is removed as, when the store holds it: one made and removed here is
    /// nothing to the store.
    remove: Option<String>,
    edits: Vec<Edit>,
    /// The person it is made for, when a migration made it as the person who wrote the item it
    /// carries ([`Store::made_for`]): its change names them as the one it acted for.
    made_for: Option<String>,
}

#[derive(Clone, Default)]
struct TxnState {
    id: u64,
    /// The resources written, in the order first touched.
    order: Vec<String>,
    dirty: HashMap<String, Dirty>,
    touched: BTreeMap<DocId, BTreeSet<FieldKey>>,
    collections: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The key of the item last put at the end of each collection (holder, field) here.
    last_keys: HashMap<(String, String), String>,
    /// What [`Inner::local`] held when it began, put back on rollback.
    before: HashMap<String, Before>,
}

/// What [`Inner::local`] held of a resource before a transaction wrote it.
#[derive(Clone)]
enum Before {
    /// Nothing: the resource read from the store.
    Unwritten,
    /// What was written of it, `None` for its removal.
    Was(Option<Held>),
}

/// The index field holding what a rich text field mentions, its name after this.
const MENTIONS: &str = "$mentions.";

/// The text written since it was last asked ([`ResourceStore::take_wrote`]): the first plain text
/// field written, the resource holding it and the characters written there, counted in
/// characters as the text now reads; and whether a resource was made, which an action names
/// instead of its words.
#[derive(Default)]
struct Wrote {
    field: Option<(String, String, u64, u64)>,
    made: bool,
}

/// The characters a write of `new` over `old` changed, as `new` reads: from where the two first
/// differ to where they agree again. A write that only took characters away names those it
/// took, as `old` read.
fn written_span(old: &str, new: &str) -> (u64, u64) {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let (start, agree) = common_ends(&old, &new);
    let new_end = new.len().saturating_sub(agree);
    let end = if new_end > start {
        new_end
    } else {
        old.len().saturating_sub(agree)
    };
    let count = |at: usize| u64::try_from(at).unwrap_or(u64::MAX);
    (count(start), count(end))
}

/// How many characters `a` and `b` share at their start, and how many after that at their end.
fn common_ends(a: &[char], b: &[char]) -> (usize, usize) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let rest = |held: &[char]| held.get(prefix..).unwrap_or_default().to_vec();
    let (a_rest, b_rest) = (rest(a), rest(b));
    let suffix = a_rest
        .iter()
        .rev()
        .zip(b_rest.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, suffix)
}

/// A host mark a run's first commit writes ([`ResourceStore::mark_triggered_in_run`]).
struct RunMark {
    run: String,
    doc: String,
    key: String,
    value: Json,
}

/// What [`Inner::local`] and [`Inner::unsent`] held at a point, to go back to.
#[derive(Clone, Default)]
struct Saved {
    local: HashMap<String, Option<Held>>,
    unsent: usize,
}

/// A point to go back to after a trial run; see [`ResourceStore::checkpoint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint(usize);

struct Subscription {
    id: SubId,
    doc: DocId,
    callback: Box<dyn FnMut(&ChangeSummary)>,
}

struct Inner {
    port: Rc<dyn Port>,
    kinds: RefCell<Kinds>,
    /// The person whose device writes here: who the platform's own writes are by (a root
    /// made, an outside effect handed over, a mark), [`PLATFORM_WRITES`].
    writer: RefCell<Option<Actor>>,
    cache: RefCell<HashMap<String, Option<Held>>>,
    /// The ids the store answered it holds none of as a kind, by kind: asked again, they read
    /// none without asking, until their record moves or this store writes.
    absent: RefCell<HashSet<(String, String)>>,
    /// Written and not yet sent: `None` for a resource removed.
    local: RefCell<HashMap<String, Option<Held>>>,
    /// Committed and not yet sent.
    unsent: RefCell<Vec<Command>>,
    txn: RefCell<Option<TxnState>>,
    next_txn: StdCell<u64>,
    /// The runs open, outermost first, with what to go back to when one fails and the
    /// invocation their writes are stamped with.
    runs: RefCell<Vec<(String, Saved, String)>>,
    /// What the first commit of a run writes besides what the run does, by the run: the host's
    /// note that a trigger ran is part of the change its run made, not a change of its own.
    run_marks: RefCell<Vec<RunMark>>,
    /// The migration running and the document it runs on: its first commit notes that it ran,
    /// as part of the change it makes.
    migrating: RefCell<Option<(String, String)>>,
    /// The trial runs open; see [`ResourceStore::checkpoint`].
    trials: RefCell<Vec<Saved>>,
    /// What the commits of the runs and trial runs open changed, told once the last of them
    /// ends and the store holds what they wrote.
    heard: RefCell<Vec<steps::Heard>>,
    /// Whether the store held everything written here when the outermost of the runs and trial
    /// runs open began: then what they wrote is all that is unsent.
    held_all: StdCell<bool>,
    /// The listings read, by query, with the pages they are read from. In key order, which is
    /// kind first: a write reaches the listings of the kinds it wrote, and no others.
    listings: RefCell<BTreeMap<String, Read>>,
    /// What whole queries listed, every page of each, by the query: asked of the store once and
    /// again only after a write or a moved record, so a loop that asks what effects wait or what
    /// notices there are costs nothing while nothing changed.
    queried: RefCell<HashMap<String, Rows>>,
    subs: RefCell<Vec<Subscription>>,
    next_sub: StdCell<u64>,
    journals: RefCell<HashMap<String, Journal>>,
    /// The device whose outside effects this store carries out: an entry of the effects is
    /// carried out by the device that queued it, and of two that queued it at once, by the one
    /// whose write the workspace kept.
    device: RefCell<String>,
    /// The run what is written now is a step of, when the host is carrying out work it groups
    /// as one: an agent's session. Its changes say so, and a review of the run lists them.
    run: RefCell<Option<tilolang_runtime::store::Run>>,
    /// The invocation the writes made now serve, as whoever delivered it names it: what they
    /// are stamped with, so the workspace and its history tell them by the call that made them.
    serving: RefCell<Option<String>>,
    /// Who the writes made now are made for, when the invoker is not the person at this device
    /// (a page whose reader is a member): the actor the changes name as theirs.
    serving_invoker: RefCell<Option<String>>,
    /// How the invoker acts, when software does it for a person (an agent): the records it makes
    /// are that person's, which a type that names `readers: author` seals them to, and each
    /// change records how the agent acted, handed its person's session or under a key of its own.
    serving_acting: RefCell<Acted>,
    /// The invocation as the caller signed it, when the writes made now serve one a caller asked
    /// for under their own key: what each change keeps, for the holders that check it.
    serving_signed: RefCell<Option<Box<tilo_actions::remote::SignedCall>>>,
    /// The resources made here since [`ResourceStore::take_made`] last took them.
    made: RefCell<Vec<ResourceRef>>,
    /// Why the last batch was refused, for the host to say.
    refused: RefCell<Option<String>>,
    /// What each reader that watches the store has not seen yet.
    watches: RefCell<Vec<Weak<RefCell<Moved>>>>,
    /// The invocation of the last write made from here, when one was since this store opened:
    /// what a reader of this instance's own writes tells a change by. Undo and redo read the
    /// record's history, never a list kept here.
    last_write: RefCell<Option<String>>,
    /// The burst of typing the person is in: its keystrokes are one invocation.
    burst: RefCell<Option<Burst>>,
    /// How long a pause in the typing ends the burst.
    edit_pause: StdCell<std::time::Duration>,
    /// The moment the last write from here was stamped with ([`Stamp::moment`]).
    moment: StdCell<u64>,
    /// What the store answered of the resources nested in a parent at one version, by id: a
    /// reader reading a collection at a version reads its items at the same one next. Only the
    /// latest version read is kept.
    past: RefCell<Option<(String, HashMap<String, Look>)>>,
    /// The moment the last id minted here leads with.
    minted: StdCell<u64>,
    /// The text written here since [`ResourceStore::take_wrote`] was last asked.
    wrote: RefCell<Wrote>,
    /// What was read of each resource's history, by id, until a write of it or of what it
    /// holds: a list sorted by when its rows last changed reads every row's, each time it is
    /// read, and each row that says when its list was made reads that list's first version.
    versions: RefCell<HashMap<String, History>>,
    /// The page of each document's newest versions last read, by document, and what moved in it
    /// since ([`NewestPage`]).
    newest: RefCell<HashMap<DocId, NewestPage>>,
    /// How far this device's changes of each record have gone, as last asked, by record: until
    /// the record moves or this store writes ([`Store::reach`]).
    reach: RefCell<HashMap<DocId, RecordReach>>,
    /// The time a scenario's sample data is written at, while its writes are held back
    /// ([`ResourceStore::hold_sample`]).
    sample: RefCell<Option<String>>,
    /// What a query that compares a field with the clock is answered at, as the host names it:
    /// what a view that waits for an instant wakes at ([`ResourceStore::set_now`]).
    now: RefCell<Rc<dyn Fn() -> String>>,
}

/// A moment's steps in a millisecond and in a second: a moment counts the second a change is
/// dated in, then where in it the device's clock read ([`ResourceStore::next_moment`]).
const PER_MILLISECOND: u64 = 1_000;
const PER_SECOND: u64 = 1_000_000 * PER_MILLISECOND;

/// How long a pause in a person's typing ends the burst they are in
/// ([`ResourceStore::set_edit_pause`]).
pub const DEFAULT_EDIT_PAUSE: std::time::Duration = std::time::Duration::from_millis(1200);

/// The burst of typing a person is in: the letters typed into one field of one resource, each
/// less than the pause after the one before it. Every keystroke of it is written as it is typed,
/// and all of them are one invocation: History lists the burst as one entry and one undo takes
/// it back.
struct Burst {
    resource: String,
    field: String,
    by: Option<Actor>,
    invocation: String,
    at: tilo_clock::time::Instant,
    /// The shape of the rich text the last keystroke left: its blocks' kinds and attributes.
    shape: Option<Json>,
    /// The words the last keystroke left.
    text: String,
}

/// The words a text field's `value` holds: a plain text's, or each block of a rich text's on a
/// line of its own.
fn words_of(value: &Json) -> String {
    fn blocks(held: &[Json], out: &mut String) {
        for block in held {
            if let Some(text) = block.get("text").and_then(Json::as_str) {
                out.push_str(text);
                out.push('\n');
            }
            if let Some(children) = block.get("children").and_then(Json::as_array) {
                blocks(children, out);
            }
        }
    }
    if let Json::String(text) = value {
        return text.clone();
    }
    let mut out = String::new();
    if let Some(rich) = value.get("rich").and_then(Json::as_array) {
        blocks(rich, &mut out);
    }
    out
}

/// Whether `now` is `was` with one letter typed into it or taken out of it: what a keystroke
/// does. A paste or a replacement changes more at once.
fn one_keystroke(was: &str, now: &str) -> bool {
    let (was, now): (Vec<char>, Vec<char>) = (was.chars().collect(), now.chars().collect());
    let (prefix, suffix) = common_ends(&was, &now);
    let kept = prefix.saturating_add(suffix);
    let changed = (
        was.len().saturating_sub(kept),
        now.len().saturating_sub(kept),
    );
    matches!(changed, (0 | 1, 0 | 1))
}

/// The shape of the rich text `value` holds, `None` for plain text: each block's kind and
/// attributes, its words left out. A keystroke that changes it (`# ` turning a line into a
/// heading) gives the text a structure rather than more words.
fn shape_of(value: &Json) -> Option<Json> {
    fn blocks(held: &[Json]) -> Json {
        Json::Array(
            held.iter()
                .map(|block| {
                    serde_json::json!([
                        block.get("kind"),
                        block.get("attrs"),
                        blocks_of(block.get("children")),
                    ])
                })
                .collect(),
        )
    }
    fn blocks_of(children: Option<&Json>) -> Json {
        children
            .and_then(Json::as_array)
            .map_or(Json::Null, |children| blocks(children))
    }
    value
        .get("rich")
        .and_then(Json::as_array)
        .map(|rich| blocks(rich))
}

/// A listing as it was read, with the pages it is read from.
type Read = (Rc<Listing>, Rc<Pages>);

/// What moved in the store since a reader last looked ([`Watch::take`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Everything may have: the workspace moved on elsewhere, or the reader never looked.
    pub everything: bool,
    /// The documents something was written in.
    pub docs: BTreeSet<DocId>,
    /// Each field the changes in a document of `docs` wrote, as resource and field id:
    /// [`tilolang_runtime::store::ANY_FIELD`] of the document itself where a change named no
    /// fields.
    pub fields: BTreeMap<DocId, BTreeSet<(String, String)>>,
}

impl Moved {
    /// Whether the document `doc` may have moved.
    #[must_use]
    pub fn names(&self, doc: &str) -> bool {
        self.everything || self.docs.contains(doc)
    }

    /// Whether nothing moved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.everything && self.docs.is_empty()
    }
}

/// A reader's view of what moved in the store ([`ResourceStore::watch`]).
#[derive(Clone, Debug)]
pub struct Watch(Rc<RefCell<Moved>>);

impl Watch {
    /// What moved since the last take, which starts over.
    #[must_use]
    pub fn take(&self) -> Moved {
        std::mem::take(&mut *self.0.borrow_mut())
    }

    /// What moved since the last take, kept for the next.
    #[must_use]
    pub fn peek(&self) -> Moved {
        self.0.borrow().clone()
    }

    /// Whether anything moved since the last take, without copying what.
    #[must_use]
    pub fn moved(&self) -> bool {
        !self.0.borrow().is_empty()
    }
}

/// What was read of one resource's history: all of it, or one of its ends.
#[derive(Default)]
struct History {
    all: Option<Vec<Version>>,
    first: Option<End>,
    last: Option<End>,
}

/// One end of a history, as it was read: its version, or none for a history with no changes.
struct End(Option<Version>);

/// The newest version of each resource of a page, most recent first, and whether the document
/// holds more resources than the page.
type NewestRows = (Vec<(String, Version)>, bool);

/// A page of a document's newest versions, as the store answered it ([`Store::newest_page`]),
/// and the resources of it that moved since: a window's change mark reads the page whenever it
/// draws, and between two reads only the resources that moved are read again, each by its newest
/// version, never the page.
struct NewestPage {
    /// How many resources the page holds at most.
    page: usize,
    /// The newest version of each resource of the page, most recent first.
    rows: Vec<(String, Version)>,
    /// Whether the document holds more resources than the page.
    more: bool,
    /// The resources that moved since the page was read, whose newest version it no longer
    /// knows.
    moved: BTreeSet<String>,
}

impl History {
    fn end(&self, last: bool) -> Option<&Option<Version>> {
        if last { &self.last } else { &self.first }
            .as_ref()
            .map(|end| &end.0)
    }

    fn end_mut(&mut self, last: bool) -> &mut Option<End> {
        if last {
            &mut self.last
        } else {
            &mut self.first
        }
    }
}

/// What a query lists across all its pages: each resource's id and what the index holds of it.
type Rows = Vec<(String, Indexed)>;

/// The runtime's store over the device's resource store. Clones share one store.
#[derive(Clone)]
pub struct ResourceStore {
    inner: Rc<Inner>,
}

impl std::fmt::Debug for ResourceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceStore").finish_non_exhaustive()
    }
}

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

/// Forget the versions read of the resource `id` and of what holds it (an item's id is under the
/// ids of what holds it, `doc/item`), and note `id` as moved in its document's page of newest
/// versions. What holds it has a version of its own only when a write names it too, as one
/// that puts an item in names the holder it puts it in.
fn forget_versions(
    versions: &mut HashMap<String, History>,
    newest: &mut HashMap<DocId, NewestPage>,
    id: &str,
) {
    if let Some(page) = newest.get_mut(doc_of(id)) {
        page.moved.insert(id.to_owned());
    }
    let mut held = id;
    versions.remove(held);
    while let Some((holder, _)) = held.rsplit_once('/') {
        versions.remove(holder);
        held = holder;
    }
}

/// The id of the resource `at` names in the store: its document, then `/item` for an item.
fn id_of(at: &ResourceRef) -> String {
    at.id_string()
}

/// The item id of the store's resource `id`: what follows its document, which for a document a
/// device keeps its own is several steps long.
fn item_of(id: &str) -> ItemId {
    tilolang_runtime::store::item_of(id).to_owned()
}

/// The invocations `command` writes under, in order.
fn invocations_of(command: &Command) -> Vec<String> {
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Edit { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::UndoMany { stamp, .. }
        | Command::Checkpoint { stamp, .. }
        | Command::Restore { stamp, .. } => vec![stamp.invocation.clone()],
        Command::Batch(commands) => {
            let mut out: Vec<String> = Vec::new();
            for invocation in commands.iter().flat_map(invocations_of) {
                if !out.contains(&invocation) {
                    out.push(invocation);
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The verb a committed change is stamped with: what the runtime named it, except a bound
/// control's edit, which the package publishes as `edit-flag` (a checkbox) or `edit-text`.
fn published_verb(note: &ChangeNote) -> String {
    if note.action != RUNTIME_EDIT {
        return note.action.clone();
    }
    match note.args.get(1) {
        Some(Json::Bool(_)) => tilolang_runtime::store::EDIT_FLAG_ACTION.to_owned(),
        _ => tilolang_runtime::store::EDIT_TEXT_ACTION.to_owned(),
    }
}

/// A field's value, by the field's id.
pub type FieldOf = (String, FieldValue);

/// How the writes it is held around are made (see [`ResourceStore::acting_for`]).
pub struct ActingFor {
    inner: Rc<Inner>,
    previous: Acted,
}

/// The person software acts for, and how it acts, as the writes made for it record them.
#[derive(Clone, Debug, Default)]
struct Acted {
    person: Option<String>,
    acting: Option<tilo_actions::Acting>,
}

impl Drop for ActingFor {
    fn drop(&mut self) {
        *self.inner.serving_acting.borrow_mut() = std::mem::take(&mut self.previous);
    }
}

impl ResourceStore {
    /// A store sending its commands to `port`.
    #[must_use]
    pub fn new(port: Rc<dyn Port>) -> Self {
        port.declare(crate::personal::readers());
        Self {
            inner: Rc::new(Inner {
                port,
                kinds: RefCell::new(Kinds::default()),
                writer: RefCell::default(),
                cache: RefCell::default(),
                absent: RefCell::default(),
                local: RefCell::default(),
                unsent: RefCell::default(),
                txn: RefCell::default(),
                next_txn: StdCell::new(1),
                runs: RefCell::default(),
                run_marks: RefCell::default(),
                migrating: RefCell::default(),
                trials: RefCell::default(),
                heard: RefCell::default(),
                held_all: StdCell::new(false),
                listings: RefCell::default(),
                queried: RefCell::default(),
                subs: RefCell::default(),
                next_sub: StdCell::new(1),
                journals: RefCell::default(),
                device: RefCell::new(uuid::Uuid::new_v4().to_string()),
                run: RefCell::default(),
                serving: RefCell::default(),
                serving_invoker: RefCell::default(),
                serving_acting: RefCell::default(),
                serving_signed: RefCell::default(),
                made: RefCell::default(),
                burst: RefCell::default(),
                edit_pause: StdCell::new(DEFAULT_EDIT_PAUSE),
                moment: StdCell::new(0),
                past: RefCell::default(),
                minted: StdCell::new(0),
                wrote: RefCell::default(),
                versions: RefCell::default(),
                newest: RefCell::default(),
                reach: RefCell::default(),
                sample: RefCell::default(),
                refused: RefCell::default(),
                watches: RefCell::default(),
                last_write: RefCell::default(),
                now: RefCell::new(Rc::new(crate::clock::system_time)),
            }),
        }
    }

    /// A store held in this process, in memory: a window with no workspace, and the tests.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Rc::new(LocalPort::in_memory()))
    }

    /// The resource `at` as it stood before or after the change `change` (its hash in hex):
    /// what its history reads a step's fields from.
    fn at_change(&self, at: &ResourceRef, change: &str, after: bool) -> Result<Look, StoreError> {
        if after
            && let Some((read, looks)) = &*self.inner.past.borrow()
            && read == change
            && let Some(look) = looks.get(&id_of(at))
        {
            return Ok(look.clone());
        }
        let kind = self.kind_of_type(&at.ty)?;
        let reply = self.command(Command::At {
            kind,
            id: id_of(at),
            change: change.to_owned(),
            after,
        })?;
        match reply {
            Reply::At(Some((indexed, content))) => Ok(Look { indexed, content }),
            Reply::At(None) => Err(StoreError::Invalid(format!(
                "this store keeps no log to read the change {change} in"
            ))),
            other => Err(backend(format!("a read of a change answered {other:?}"))),
        }
    }

    /// The reference `scalar` names, with the type the store holds it as: the index keeps a
    /// reference by its id alone, so a field of the host's `Resource` type reads back as the
    /// one thing it is stored as.
    fn concrete_ref(&self, scalar: Scalar) -> Scalar {
        let Scalar::Json(Json::Object(mut map)) = scalar else {
            return scalar;
        };
        // A reference that may be absent is a reference all the same.
        if let Some(inner) = map.remove("some") {
            let Scalar::Json(inner) = self.concrete_ref(Scalar::Json(inner)) else {
                return Scalar::Json(Json::Object(map));
            };
            map.insert("some".to_owned(), inner);
            return Scalar::Json(Json::Object(map));
        }
        let generic = format!("host-{}", tilolang::host::RESOURCE_TYPE_NAME.to_lowercase());
        if map.get("ty").and_then(Json::as_str) != Some(generic.as_str()) {
            return Scalar::Json(Json::Object(map));
        }
        let Some(id) = map.get("ref").and_then(Json::as_str) else {
            return Scalar::Json(Json::Object(map));
        };
        let Some(ty) = self.type_of_id(id) else {
            return Scalar::Json(Json::Object(map));
        };
        map.insert("ty".to_owned(), Json::String(ty));
        Scalar::Json(Json::Object(map))
    }

    /// The value the field `field` holds, given what the index holds of it (`cell`) and its
    /// content (`json`), as [`Self::read`] reads it.
    fn value_from(
        &self,
        field: &FieldSpec,
        entry: Option<&kinds::FieldEntry>,
        cell: Option<&Cell>,
        json: Option<&Json>,
    ) -> Result<FieldValue, StoreError> {
        let indexed = entry.is_some_and(|entry| self.indexed(entry));
        if indexed {
            let ty = entry.map_or(Ty::Unit, |entry| entry.ty.clone());
            return Ok(match (field.kind, cell) {
                (FieldKind::Set, Some(Cell::Set(members))) => FieldValue::Keys(
                    members
                        .iter()
                        .filter_map(|member| member.as_text().map(str::to_owned))
                        .collect(),
                ),
                (FieldKind::Set, _) => FieldValue::Keys(Vec::new()),
                (FieldKind::Text, Some(Cell::One(Value::Text(text)))) => {
                    FieldValue::Text(text.clone())
                }
                (FieldKind::Text, _) => FieldValue::Text(String::new()),
                (_, Some(Cell::One(value))) => self
                    .inner
                    .kinds
                    .borrow()
                    .decode(&ty, value)
                    .map_or(FieldValue::Absent, |scalar| {
                        FieldValue::Scalar(self.concrete_ref(scalar))
                    }),
                (_, _) => match json {
                    // A composite value of a field the index holds the scalars of.
                    Some(json) => field_of(json).map_err(StoreError::Invalid)?,
                    None => FieldValue::Absent,
                },
            });
        }
        let held_json = json;
        Ok(match (field.kind, held_json) {
            (FieldKind::Counter, Some(json)) => FieldValue::Counter(json.as_i64().unwrap_or(0)),
            (FieldKind::Counter, None) => FieldValue::Counter(0),
            (FieldKind::Text, Some(Json::String(text))) => FieldValue::Text(text.clone()),
            (FieldKind::Text, _) => FieldValue::Text(String::new()),
            (FieldKind::RichText, Some(json)) => FieldValue::Rich(
                serde_json::from_value(json.clone())
                    .map_err(|error| StoreError::Invalid(error.to_string()))?,
            ),
            (FieldKind::RichText, None) => {
                FieldValue::Rich(tilolang_runtime::rich::RichValue::default())
            }
            // Bytes keep the content store's stored form: the data itself when small, else the
            // address the store holds it at.
            (FieldKind::Bytes, Some(json)) => {
                FieldValue::Bytes(BytesData::from_json(json).map_err(StoreError::Invalid)?)
            }
            (FieldKind::Bytes, None) => FieldValue::Bytes(BytesData::Inline(Rc::new(Vec::new()))),
            (FieldKind::Map, Some(Json::Object(map))) => FieldValue::Entries(
                map.iter()
                    .filter_map(|(key, value)| match field_of(value) {
                        Ok(FieldValue::Scalar(scalar)) => Some((key.clone(), scalar)),
                        _ => None,
                    })
                    .collect(),
            ),
            (FieldKind::Map, _) => FieldValue::Entries(Vec::new()),
            (FieldKind::Set, _) => FieldValue::Keys(Vec::new()),
            (_, Some(json)) => field_of(json).map_err(StoreError::Invalid)?,
            (_, None) => FieldValue::Absent,
        })
    }

    /// The value the field `field` of `at` held as `look` read it: what [`Store::read_before`]
    /// and its `read_at` answer, of the state each read.
    fn read_look(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        look: &Look,
    ) -> Result<FieldValue, StoreError> {
        let entry = self.field(&at.ty, &field.id);
        let name = entry
            .as_ref()
            .map_or_else(|| field.id.clone(), |entry| entry.name.clone());
        let cell = look
            .indexed
            .as_ref()
            .and_then(|held| held.fields.get(&name));
        let json = look.content.as_ref().and_then(|content| content.get(&name));
        self.value_from(field, entry.as_ref(), cell, json)
    }

    /// Whether `other` is this store: one handle of the same resources.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// Where this store sends its commands: for what the platform keeps beside an app's types in
    /// the same store (a person's own state of a workspace), which no type of a package declares.
    #[must_use]
    pub fn port(&self) -> Rc<dyn Port> {
        Rc::clone(&self.inner.port)
    }

    /// The resources the store's document `document` holds now, by kind and id: what search
    /// reads of one document at a time ([`Port::held`]). Empty where the port keeps no document,
    /// as a window's does.
    #[must_use]
    pub fn resources_in(&self, document: &str) -> Vec<(String, String)> {
        self.inner.port.held(document).unwrap_or_default()
    }

    /// Takes in the types `meta` declares, so its resources are stored as their kinds.
    /// Who places the reader fields this store writes, as the workspace's one read decision
    /// answers: what a host gives it where it gives the store the workspace's access.
    pub fn set_record_reader(&self, reader: std::sync::Arc<dyn tilo_resource_store::RecordReader>) {
        self.inner.port.set_record_reader(reader);
    }

    pub fn declare(&self, meta: &Meta) {
        self.declare_with(meta, &[]);
    }

    /// [`Self::declare`], with `former` — the retired declarations of the package's ids lock,
    /// `(name, id)` each — so a rename keeps the resources the earlier name wrote readable as
    /// the declaration that carries its id now.
    pub fn declare_with(&self, meta: &Meta, former: &[(String, String)]) {
        self.inner.kinds.borrow_mut().declare(meta, former);
        // The app's kinds are its own, as its surface declares them: a store that judges a
        // change against them -- a window kept here, a test's workspace -- knows whose they are.
        let mine = || BTreeSet::from([meta.app.clone()]);
        let owners: BTreeMap<String, BTreeSet<String>> = meta
            .types
            .iter()
            .map(|ty| (crate::meta::MetaExt::kind_of(meta, &ty.name), mine()))
            .chain(std::iter::once((crate::personal::KIND.to_owned(), mine())))
            .collect();
        self.inner.port.declare(tilo_resource_store::Schema {
            readers: meta
                .types
                .iter()
                .filter_map(|ty| {
                    Some((
                        crate::meta::MetaExt::kind_of(meta, &ty.name),
                        ty.readers.clone()?,
                    ))
                })
                .collect(),
            owners,
            ..tilo_resource_store::Schema::default()
        });
    }

    /// What every loaded package's types are stored as.
    #[must_use]
    pub fn kinds(&self) -> std::cell::Ref<'_, Kinds> {
        self.inner.kinds.borrow()
    }

    /// The person whose device writes here: who the platform's own writes are by.
    pub fn set_writer(&self, writer: Actor) {
        *self.inner.writer.borrow_mut() = Some(writer);
    }

    /// Why the store last refused a write, taken once.
    #[must_use]
    pub fn take_refusal(&self) -> Option<String> {
        self.inner.refused.borrow_mut().take()
    }

    /// Takes back the invocation `invocation`, everywhere it wrote, as an undo by `invoker`.
    /// Returns the undo's own invocation, which a redo takes back in turn: the invocation the
    /// store serves, as every write made while serving it carries, so history shows which call
    /// took the change back.
    ///
    /// # Errors
    /// When the store refused it.
    pub fn undo(&self, invocation: &str, invoker: Option<&Actor>) -> Result<String, StoreError> {
        // As any write: the invocation served, else the run's, and only a step nobody called (a
        // person's own undo at a window serving nothing) is an invocation of its own.
        let served = self
            .inner
            .serving
            .borrow()
            .clone()
            .or_else(|| {
                self.inner
                    .runs
                    .borrow()
                    .first()
                    .map(|(_, _, invocation)| invocation.clone())
            })
            .filter(|own| own != invocation);
        let stamp = self.history_stamp(Stamp::UNDO_VERB, invoker);
        let stamp = Stamp {
            undoes: Some(invocation.to_owned()),
            invocation: served.unwrap_or_else(|| stamp.invocation.clone()),
            ..stamp
        };
        let own = stamp.invocation.clone();
        self.command(Command::Undo {
            invocation: invocation.to_owned(),
            stamp,
        })?;
        self.moved_by(invoker);
        Ok(own)
    }

    /// Takes back `steps` of `invoker`'s last steps still in effect, read from the record's own
    /// log rather than from a list kept by this store: what the person's undo at a window, and an
    /// app's undo of its last steps where its store is opened for each call, both are. Answers how
    /// many it took back.
    ///
    /// # Errors
    /// When the store refused it.
    pub fn undo_steps(&self, steps: u32, invoker: Option<&Actor>) -> Result<u32, StoreError> {
        self.step_back(steps, invoker, false)
    }

    /// Brings back `steps` of the last undos by `invoker` that are still in effect, read from the
    /// record's own log. Answers how many it brought back.
    ///
    /// # Errors
    /// When the store refused it.
    pub fn redo_steps(&self, steps: u32, invoker: Option<&Actor>) -> Result<u32, StoreError> {
        self.step_back(steps, invoker, true)
    }

    /// Takes back [`Self::undo_steps`]'s steps, or brings back [`Self::redo_steps`]'s: the steps
    /// the record's log names, as one move of `invoker`'s, and answers how many there were.
    fn step_back(
        &self,
        steps: u32,
        invoker: Option<&Actor>,
        redo: bool,
    ) -> Result<u32, StoreError> {
        let read = self
            .inner
            .serving_invoker
            .borrow()
            .clone()
            .or_else(|| invoker.map(Stamp::by))
            .unwrap_or_default();
        // The kinds this app's record keeps: its undo takes back its own steps, never a change of
        // another app's record the same store holds.
        let kinds: Vec<String> = self
            .inner
            .kinds
            .borrow()
            .types()
            .map(|entry| entry.kind.clone())
            .collect();
        let message = self.command(Command::Steps {
            steps,
            invoker: read,
            kinds,
            redo,
        })?;
        let Reply::Ids(invocations) = message else {
            return Err(backend("a read of the last steps answered no steps"));
        };
        if invocations.is_empty() {
            return Ok(0);
        }
        // As any write: the invocation served, else the run's, and only a move nobody called (a
        // person's own undo at a window serving nothing) is an invocation of its own.
        let served = self.inner.serving.borrow().clone().or_else(|| {
            self.inner
                .runs
                .borrow()
                .first()
                .map(|(_, _, invocation)| invocation.clone())
        });
        let stamp = self.history_stamp(Stamp::UNDO_VERB, invoker);
        let stamp = Stamp {
            invocation: served.unwrap_or_else(|| stamp.invocation.clone()),
            ..stamp
        };
        let count = u32::try_from(invocations.len()).unwrap_or(u32::MAX);
        self.command(Command::UndoMany { invocations, stamp })?;
        self.moved_by(invoker);
        Ok(count)
    }

    /// Takes back the last step of `invoker`'s still in effect, read from the record's log.
    /// Whether there was one.
    ///
    /// # Errors
    /// When the store refused it.
    pub fn undo_last(&self, invoker: Option<&Actor>) -> Result<bool, StoreError> {
        Ok(self.undo_steps(1, invoker)? > 0)
    }

    /// Brings back the last undo by `invoker` still in effect, read from the record's log.
    /// Whether there was one.
    ///
    /// # Errors
    /// When the store refused it.
    pub fn redo_last(&self, invoker: Option<&Actor>) -> Result<bool, StoreError> {
        Ok(self.redo_steps(1, invoker)? > 0)
    }

    /// Names the version `at` stands at now `name`, as `invoker`'s checkpoint: a version its
    /// history lists, which a restore can go back to.
    ///
    /// # Errors
    /// When `at` is of no type loaded here, or the store refused it.
    pub fn name_version(
        &self,
        at: &ResourceRef,
        name: &str,
        invoker: Option<&Actor>,
    ) -> Result<(), StoreError> {
        let at = &*self.typed(at);
        let kind = self.kind_of_type(&at.ty)?;
        self.command(Command::Checkpoint {
            kind,
            id: id_of(at),
            name: name.to_owned(),
            stamp: self.history_stamp(Stamp::CHECKPOINT_VERB, invoker),
        })?;
        self.moved();
        Ok(())
    }

    /// Brings `resources`, and what is nested in them, back to the version `version` (a
    /// version's change, a checkpoint's, or a checkpoint's name), as `invoker`'s one step: with `only`, just the
    /// resources it names (by id); with `live`, only while they stand where a review saw them.
    /// Returns the restore's invocation, which an undo takes back.
    ///
    /// # Errors
    /// When the store refused it: the version is none of theirs, they moved since the review,
    /// or they stand as they did then.
    pub fn restore_version(
        &self,
        resources: &[ResourceRef],
        version: &str,
        only: Option<Vec<String>>,
        live: Option<Vec<String>>,
        invoker: Option<&Actor>,
    ) -> Result<String, StoreError> {
        let stamp = self.history_stamp(Stamp::RESTORE_VERB, invoker);
        let own = stamp.invocation.clone();
        self.command(Command::Restore {
            resources: self.kinds_and_ids(resources),
            change: version.to_owned(),
            only,
            live,
            stamp,
        })?;
        // The person's own step: their next undo, read from the record's history, takes it back.
        *self.inner.last_write.borrow_mut() = Some(own.clone());
        self.moved_by(invoker);
        Ok(own)
    }

    /// What [`Self::restore_version`] of `resources` to `version` would change, as this device's
    /// person may read it.
    ///
    /// # Errors
    /// When the version is none of theirs.
    pub fn review_restore(
        &self,
        resources: &[ResourceRef],
        version: &str,
    ) -> Result<tilo_resource_store::Review, StoreError> {
        let reply = self.command(Command::ReviewRestore {
            resources: self.kinds_and_ids(resources),
            change: version.to_owned(),
        })?;
        let Reply::Review(review) = reply else {
            return Err(backend("a review of a restore answered no review"));
        };
        review.map_err(StoreError::Backend)
    }

    /// Each of `resources` by its kind and id in the store; one of a type not loaded here is
    /// another app's, and none of these.
    fn kinds_and_ids(&self, resources: &[ResourceRef]) -> Vec<(String, String)> {
        resources
            .iter()
            .filter_map(|at| {
                let at = self.typed(at);
                let kind = self.kind_of_type(&at.ty).ok()?;
                Some((kind, id_of(&at)))
            })
            .collect()
    }

    /// The stamp of a step through history by `invoker`, an invocation of its own: it ends the
    /// burst of typing, so what is typed next is a step of its own.
    fn history_stamp(&self, verb: &str, invoker: Option<&Actor>) -> Stamp {
        *self.inner.burst.borrow_mut() = None;
        let (acted_for, context) = self.acted_for();
        Stamp {
            verb: verb.to_owned(),
            invoker: invoker
                .and_then(|actor| serde_json::to_string(actor).ok())
                .unwrap_or_default(),
            // A move through history is a write like any other: made for the person its
            // invoker acts for, recording how its agent acted, under the call its caller
            // signed, as a step of the run it is part of.
            acted_for,
            context,
            signed: self.inner.serving_signed.borrow().clone(),
            group: self
                .inner
                .run
                .borrow()
                .clone()
                .map(|run| tilo_resource_store::Group {
                    id: run.id,
                    label: run.title,
                }),
            invocation: uuid::Uuid::new_v4().to_string(),
            at: (self.inner.now.borrow())(),
            moment: self.next_moment(None),
            ..Stamp::default()
        }
    }

    /// The workspace's members' resource as the workspace this store reads answers it: who is in
    /// it (`people`). A store kept by a workspace answers it from the workspace's governance; one
    /// held in this process keeps it as a record of its own, written by the workspace's
    /// add-member and remove-member actions ([`Self::change_workspace_members`]). `None` while it
    /// holds no such record.
    ///
    /// # Errors
    /// The store could not be read.
    pub fn workspace_members(&self) -> Result<Option<Json>, StoreError> {
        match self.command(Command::Content {
            kind: tilo_resource_store::WORKSPACE_MEMBERS_KIND.to_owned(),
            id: tilo_resource_store::WORKSPACE_MEMBERS_ID.to_owned(),
        })? {
            Reply::Content(content) => Ok(content),
            other => Err(backend(format!("a read answered {other:?}"))),
        }
    }

    /// Carry out the workspace's member action `verb` (`add-member`, `remove-member`) on the
    /// members' record of a store held in this process: `people` is who is in it after, and
    /// `invoker` who invoked it. A store a workspace keeps refuses it: its members change
    /// through the workspace's governance.
    ///
    /// # Errors
    /// The store refused the write.
    pub fn change_workspace_members(
        &self,
        verb: &str,
        invoker: &Actor,
        people: Json,
    ) -> Result<(), StoreError> {
        let kind = tilo_resource_store::WORKSPACE_MEMBERS_KIND;
        let id = tilo_resource_store::WORKSPACE_MEMBERS_ID;
        let held = self.workspace_members()?.is_some();
        let stamp = self.history_stamp(verb, Some(invoker));
        let edit = Command::Edit {
            kind: kind.to_owned(),
            id: id.to_owned(),
            edits: vec![Edit::Set {
                field: "people".to_owned(),
                value: people,
            }],
            stamp: stamp.clone(),
        };
        let command = if held {
            edit
        } else {
            Command::Batch(vec![
                Command::Put {
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    resource: Resource {
                        readers: collab_types::EVERY_MEMBER.to_owned(),
                        parent: None,
                        order: None,
                        fields: BTreeMap::new(),
                    },
                    stamp,
                },
                edit,
            ])
        };
        self.command(command)?;
        Ok(())
    }

    /// Hold back what is committed from here on, as a scenario stages a workspace: what it
    /// writes at `sample_at` (its sample data, written without a time of its own) is the
    /// workspace before its story, and [`Self::release_sample`] dates it so.
    pub fn hold_sample(&self, sample_at: &str) {
        *self.inner.sample.borrow_mut() = Some(sample_at.to_owned());
    }

    /// Write what [`Self::hold_sample`] held back. What was written at the sample time is
    /// dated a second before the earliest change the scenario dates before it, so the story
    /// the scenario dates happens to the workspace its sample data made, as a reader of its
    /// history and its marks reads it.
    ///
    /// # Errors
    ///
    /// The store refused what was held back.
    pub fn release_sample(&self) -> Result<(), StoreError> {
        let Some(sample_at) = self.inner.sample.borrow_mut().take() else {
            return Ok(());
        };
        let mut unsent = self.inner.unsent.borrow_mut();
        let mut stamps = Vec::new();
        let mut commands: Vec<&mut Command> = unsent.iter_mut().collect();
        while let Some(command) = commands.pop() {
            match command {
                Command::Put { stamp, .. }
                | Command::Remove { stamp, .. }
                | Command::Edit { stamp, .. }
                | Command::Undo { stamp, .. }
                | Command::UndoMany { stamp, .. }
                | Command::Checkpoint { stamp, .. }
                | Command::Restore { stamp, .. } => stamps.push(stamp),
                Command::Batch(inner) => commands.extend(inner.iter_mut()),
                _ => {}
            }
        }
        let earliest = stamps
            .iter()
            .map(|stamp| stamp.at.as_str())
            .filter(|at| *at < sample_at.as_str())
            .min()
            .and_then(|at| {
                tilo_failure::ignored!(
                    "a stamp with no readable time dates nothing earlier",
                    at.parse::<jiff::Timestamp>().ok()
                )
            })
            .and_then(|at| at.checked_sub(jiff::SignedDuration::from_secs(1)).ok());
        if let Some(earliest) = earliest {
            let at = earliest.to_string();
            let second = u64::try_from(earliest.as_second()).unwrap_or_default();
            for stamp in stamps.into_iter().filter(|stamp| stamp.at == sample_at) {
                // Within the second, the sample data keeps the order it was written in.
                stamp.moment = second
                    .saturating_mul(PER_SECOND)
                    .saturating_add(stamp.moment % PER_SECOND);
                stamp.at.clone_from(&at);
            }
        }
        drop(unsent);
        self.flush()
    }

    /// How long a pause in the person's typing ends the burst they are in, which History and
    /// undo take as one step ([`DEFAULT_EDIT_PAUSE`] unless the host says otherwise).
    pub fn set_edit_pause(&self, pause: std::time::Duration) {
        self.inner.edit_pause.set(pause);
    }

    /// The invocation the change `note` is written as when it is the next letter of the burst
    /// the person is typing: the burst's own. A change that is no letter of a text field typed
    /// by a person (a press, a command, an action, an undo) ends the burst; a letter of
    /// another field, by another person or after a pause starts one.
    fn burst_invocation(&self, note: &ChangeNote) -> Option<String> {
        let typed = note.action == RUNTIME_EDIT
            && note.invocation.is_none()
            && self.inner.runs.borrow().is_empty()
            && note.effects.is_empty()
            && note.count <= 1
            && note.approved_by.is_none()
            && matches!(note.args.get(1), Some(Json::String(_) | Json::Object(_)));
        let field = note.args.get(0).and_then(Json::as_str);
        let (true, Some(field), [resource]) = (typed, field, note.touched.as_slice()) else {
            *self.inner.burst.borrow_mut() = None;
            return None;
        };
        let mut burst = self.inner.burst.borrow_mut();
        let now = tilo_clock::now();
        let shape = note.args.get(1).and_then(shape_of);
        let text = note.args.get(1).map(words_of).unwrap_or_default();
        // A keystroke that reshapes the text is a step of its own, which the letters after it
        // join: undo takes the heading back and leaves the words typed before it.
        let invocation = match burst.take() {
            Some(held)
                if held.resource == *resource
                    && held.field == field
                    && held.by == note.invoker
                    && held.shape == shape
                    && one_keystroke(&held.text, &text)
                    && now.duration_since(held.at) < self.inner.edit_pause.get() =>
            {
                held.invocation
            }
            _ => uuid::Uuid::new_v4().to_string(),
        };
        *burst = Some(Burst {
            resource: resource.clone(),
            field: field.to_owned(),
            by: note.invoker.clone(),
            invocation: invocation.clone(),
            at: now,
            shape,
            text,
        });
        Some(invocation)
    }

    /// The invocation of the last change written from here, when one was since this store
    /// opened. A window reads it to tell its own write from nothing having happened; undo and
    /// redo read the record's history, which every replica agrees on.
    #[must_use]
    pub fn last_written(&self) -> Option<String> {
        self.inner.last_write.borrow().clone()
    }

    /// A reader's watch over the store: everything has moved until it first takes it.
    #[must_use]
    pub fn watch(&self) -> Watch {
        let moved = Rc::new(RefCell::new(Moved {
            everything: true,
            ..Moved::default()
        }));
        let mut watches = self.inner.watches.borrow_mut();
        watches.retain(|held| held.strong_count() > 0);
        watches.push(Rc::downgrade(&moved));
        Watch(moved)
    }

    fn tell_watches(&self, change: Option<&ChangeSummary>) {
        for held in self.inner.watches.borrow().iter() {
            if let Some(moved) = held.upgrade() {
                let mut moved = moved.borrow_mut();
                match change {
                    Some(change) => {
                        moved.docs.insert(change.doc.clone());
                        let fields = moved.fields.entry(change.doc.clone()).or_default();
                        if change.fields.is_empty() {
                            fields.insert((
                                change.doc.clone(),
                                tilolang_runtime::store::ANY_FIELD.to_owned(),
                            ));
                        }
                        fields.extend(
                            change
                                .fields
                                .iter()
                                .map(|key| (key.resource.clone(), key.field.clone())),
                        );
                    }
                    None => moved.everything = true,
                }
            }
        }
    }

    /// The workspace's resources moved on: forget what was read, and tell every view to read
    /// what it shows again.
    pub fn moved(&self) {
        self.moved_by(None);
    }

    /// [`Self::moved`] by `by`'s step through history (an undo, a redo, a restore): what it
    /// changed it does not name, so whoever watches is told every field may have moved, by them.
    /// An origin reads the fields people change again from it, and tells its service what they
    /// say now: undo of marking a message read marks it unread on its server too.
    fn moved_by(&self, by: Option<&Actor>) {
        self.forget_reads();
        self.tell_watches(None);
        let docs: BTreeSet<DocId> = self
            .inner
            .subs
            .borrow()
            .iter()
            .map(|sub| sub.doc.clone())
            .collect();
        for doc in docs {
            self.notify(&ChangeSummary {
                by: by.cloned(),
                ..ChangeSummary::whole(doc)
            });
        }
    }

    fn forget_reads(&self) {
        self.inner.cache.borrow_mut().clear();
        self.inner.absent.borrow_mut().clear();
        self.inner.listings.borrow_mut().clear();
        self.inner.versions.borrow_mut().clear();
        self.inner.queried.borrow_mut().clear();
        self.inner.newest.borrow_mut().clear();
    }

    /// Records of the workspace moved on: another window or device wrote them, or this
    /// device's changes of them went further. What was read of each resource a move names and of
    /// what holds it is read again, and the views that read them move with them; the rest of the
    /// record stands, so a message posted to a channel of ten thousand reads that message again,
    /// not the channel. A move that names no resource forgets the whole record: its document, the
    /// resource whose content it holds, or, for a document of resources the workspace's store
    /// placed, the document of what they are nested in.
    pub fn moved_records(&self, moved: &[collab_types::store::MovedRecord]) {
        if moved.is_empty() {
            return;
        }
        // Each move may be how far this device's changes went, or a write that adds to them.
        self.inner.reach.borrow_mut().clear();
        let mut whole = BTreeSet::new();
        // A resource named with no kind is what a write nested the one it wrote in: its fields
        // are as they were, and its collections are told with the resource's.
        let mut items = BTreeMap::<String, String>::new();
        for record in moved {
            match &record.items {
                Some(moved) => items.extend(
                    moved
                        .iter()
                        .filter_map(|item| Some((item.id.clone(), item.kind.clone()?))),
                ),
                None => {
                    if let Some((kind, id)) = collab_types::touch::content_resource(&record.record)
                    {
                        items.insert(id, kind);
                    } else if let Some(owner) = collab_types::placed_owner(&record.record) {
                        // Records nested in nothing are each their own document: any may have
                        // come or gone.
                        if self.type_of_kind(owner).is_some() {
                            return self.moved();
                        }
                        whole.insert(doc_of(owner).to_owned());
                    } else {
                        whole.insert(record.record.clone());
                    }
                }
            }
        }
        let mut told = BTreeMap::<DocId, (BTreeSet<FieldKey>, BTreeSet<FieldKey>)>::new();
        items.retain(|id, _| !whole.contains(doc_of(id)));
        for (id, kind) in &items {
            if self.moved_item(id, kind, &mut told, &mut whole) {
                return self.moved();
            }
        }
        self.forget_reads_of(&whole);
        for doc in &whole {
            self.notify(&ChangeSummary::whole(doc.clone()));
        }
        told.retain(|doc, _| !whole.contains(doc));
        for (doc, (fields, collections)) in told {
            self.notify(&ChangeSummary {
                doc,
                fields: fields.into_iter().collect(),
                collections: collections.into_iter().collect(),
                listings: Vec::new(),
                by: None,
            });
        }
    }

    /// Forget what was read of the resource `id` of `kind` and read it again, gathering what the
    /// views are told into `told`: the resource, and the collections of what holds it before and
    /// after. One gone before it was read moved its whole document, into `whole`. Whether the
    /// workspace moved as a whole: a record nested in nothing came or went.
    fn moved_item(
        &self,
        id: &str,
        kind: &str,
        told: &mut BTreeMap<DocId, (BTreeSet<FieldKey>, BTreeSet<FieldKey>)>,
        whole: &mut BTreeSet<DocId>,
    ) -> bool {
        let was = self.inner.cache.borrow_mut().remove(id).flatten();
        self.inner.absent.borrow_mut().retain(|held| held.1 != id);
        self.inner.versions.borrow_mut().remove(id);
        // The page of its document's newest changes reads it again, and nothing else of it.
        if let Some(page) = self.inner.newest.borrow_mut().get_mut(doc_of(id)) {
            page.moved.insert(id.to_owned());
        }
        self.inner
            .listings
            .borrow_mut()
            .retain(|_, (_, pages)| pages.kind != kind);
        let now = self.held(kind, id).ok().flatten();
        let holders: BTreeSet<String> = [&was, &now]
            .into_iter()
            .filter_map(|held| held.as_ref()?.parent.clone())
            .collect();
        match (&was, &now) {
            (None, None) => {
                whole.insert(doc_of(id).to_owned());
                return false;
            }
            (None, Some(_)) | (Some(_), None) if holders.is_empty() => return true,
            _ => {}
        }
        let any = FieldKey {
            resource: id.to_owned(),
            field: ANY_FIELD.to_owned(),
        };
        told.entry(doc_of(id).into()).or_default().0.insert(any);
        let Some(ty) = self.type_of_kind(kind) else {
            return false;
        };
        for holder in holders {
            let holder_ty = self.type_of_id(&holder);
            let kinds = self.inner.kinds.borrow();
            let fields: Vec<&String> = holder_ty
                .and_then(|holder_ty| kinds.get(&holder_ty))
                .map(|entry| {
                    let held = entry.fields.iter().filter(|(_, f)| f.items.contains(&ty));
                    held.map(|(field, _)| field).collect()
                })
                .unwrap_or_default();
            if fields.is_empty() {
                whole.insert(doc_of(&holder).to_owned());
            }
            let entry = told.entry(doc_of(&holder).to_owned()).or_default();
            for field in fields {
                let key = FieldKey {
                    resource: holder.clone(),
                    field: field.clone(),
                };
                entry.0.insert(key.clone());
                entry.1.insert(key);
            }
        }
        false
    }

    /// Forget what was read of the records `docs`, and of nothing else: each resource's reading
    /// under a moved record (its id leads with the record's), and the listings read from it.
    fn forget_reads_of(&self, docs: &BTreeSet<DocId>) {
        self.inner
            .cache
            .borrow_mut()
            .retain(|id, _| !docs.contains(doc_of(id)));
        self.inner
            .absent
            .borrow_mut()
            .retain(|(_, id)| !docs.contains(doc_of(id)));
        self.inner
            .versions
            .borrow_mut()
            .retain(|id, _| !docs.contains(doc_of(id)));
        self.inner
            .newest
            .borrow_mut()
            .retain(|doc, _| !docs.contains(doc));
        // A record moves when an answer to one of this device's writes of it comes, too.
        self.inner
            .reach
            .borrow_mut()
            .retain(|doc, _| !docs.contains(doc));
        self.inner
            .listings
            .borrow_mut()
            .retain(|_, (_, pages)| !docs.contains(&pages.doc));
        // A query lists across the records, so any record moving may have moved what it lists.
        self.inner.queried.borrow_mut().clear();
    }

    /// Forget the versions read of what `command` moves ([`Command::moves`]) and of what holds
    /// each: an item's id is under the ids of what holds it (`doc/item`). A write whose resources
    /// it does not name (an undo) forgets them all.
    fn forget_versions_of(&self, command: &Command) {
        let mut versions = self.inner.versions.borrow_mut();
        let mut newest = self.inner.newest.borrow_mut();
        let Some(moved) = command.moves() else {
            versions.clear();
            newest.clear();
            return;
        };
        for item in moved {
            forget_versions(&mut versions, &mut newest, &item.id);
        }
    }

    /// The page of `doc`'s newest versions read before, brought up to date by reading the newest
    /// version of each resource that moved since, and nothing else: none when no page of that
    /// size was read, or when what moved leaves the page unsure (a resource of it went, and
    /// the document holds more than the page).
    fn newest_page_since(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Result<Option<NewestRows>, StoreError> {
        let (mut rows, mut more, moved) = {
            let mut held = self.inner.newest.borrow_mut();
            let Some(read) = held.get_mut(doc).filter(|read| read.page == page) else {
                return Ok(None);
            };
            if read.moved.is_empty() {
                return Ok(Some((read.rows.clone(), read.more)));
            }
            (
                read.rows.clone(),
                read.more,
                std::mem::take(&mut read.moved),
            )
        };
        for id in &moved {
            let Some(mut at) = ResourceRef::parse_id(id, String::new()) else {
                self.inner.newest.borrow_mut().remove(doc);
                return Ok(None);
            };
            // A resource the store holds no more, or never held, is read with the page.
            let Ok(ty) = Store::type_of(self, &at) else {
                self.inner.newest.borrow_mut().remove(doc);
                return Ok(None);
            };
            at.ty = ty;
            let Some(last) = self.version_end(&at, true)? else {
                self.inner.newest.borrow_mut().remove(doc);
                return Ok(None);
            };
            let was = rows.iter().position(|(held, _)| held == id);
            if let Some(was) = was {
                rows.remove(was);
            }
            // Most recent first, a resource stands where its newest version does. One older
            // than everything the page holds of a document that holds more is not of the page,
            // unless it was: then what follows it in the document, which the page never read,
            // may be newer.
            let at = rows.partition_point(|(_, held)| held.order() > last.order());
            if at < rows.len() || !more {
                rows.insert(at, (id.clone(), last));
            } else if was.is_some() {
                self.inner.newest.borrow_mut().remove(doc);
                return Ok(None);
            }
        }
        if rows.len() > page {
            rows.truncate(page);
            more = true;
        }
        if let Some(read) = self.inner.newest.borrow_mut().get_mut(doc) {
            read.rows.clone_from(&rows);
            read.more = more;
        }
        Ok(Some((rows, more)))
    }

    /// The page of `doc`'s newest versions, as the store answers it.
    fn newest_page_read(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Result<(Vec<(String, Version)>, bool), StoreError> {
        // The store reads which resources changed last from what each document holds; only the
        // page of them is read whole, as their versions read here.
        let Reply::Newest { versions, more } = self.command(Command::Newest {
            within: doc.clone(),
            limit: page,
        })?
        else {
            return Err(backend("a read of the newest versions answered none"));
        };
        let mut newest: Vec<(String, Version)> = Vec::with_capacity(versions.len());
        for (kind, id, version) in versions {
            let Some(entry) = self.entry_of_kind(&kind) else {
                continue;
            };
            let last = if id == *doc {
                // The document's root holds a version for every item ever made in it, and
                // each of those is a resource of the page itself: its own newest is the
                // store's.
                self.versions_of(&id, vec![version], Some(&entry)).pop()
            } else {
                // An item's history is short, and holds what was made from it, as a comment
                // made from a task is a change of the task.
                let Some(at) = ResourceRef::parse_id(&id, entry.id.clone()) else {
                    continue;
                };
                Store::versions(self, &at)?.pop()
            };
            if let Some(last) = last {
                newest.push((id, last));
            }
        }
        // Most recent first: versions of one second in the order their changes were made.
        newest.sort_by(|a, b| b.1.order().cmp(&a.1.order()));
        Ok((newest, more))
    }

    /// The ids of every resource of the type `ty` the person may read, under its kind and the
    /// kinds an earlier version stored them as.
    ///
    /// # Errors
    /// When the store cannot be read.
    pub fn resources_of(&self, ty: &str) -> Result<Vec<String>, StoreError> {
        let mut ids: Vec<String> = Vec::new();
        for kind in self.kinds_of_type(ty)? {
            ids.extend(self.query_all(Query::of(kind, PAGE))?);
        }
        let mut seen = BTreeSet::new();
        ids.retain(|id| seen.insert(id.clone()));
        Ok(ids)
    }

    /// What the fields naming one of the asked resources link to them: one page of the index for
    /// each kind and field, the rows of the page placed under the resource each names.
    fn links_by_field(
        &self,
        asked: &[(usize, ResourceRef)],
        target: &TypeEntry,
        only: Option<&str>,
        links: &mut [Vec<Link>],
    ) -> Result<(), StoreError> {
        let of_type = |ty: &str| only.is_none_or(|only| only == ty);
        let ids: Vec<Value> = asked.iter().map(|(_, at)| Value::Text(id_of(at))).collect();
        // The place in `links` of the asked resource a row names.
        let named = |value: &Value| {
            ids.iter()
                .position(|id| id == value)
                .and_then(|n| asked.get(n))
                .map(|(index, _)| *index)
        };
        let referring: Vec<(TypeId, String, String)> = {
            let kinds = self.inner.kinds.borrow();
            let mut out = Vec::new();
            for entry in kinds.types().filter(|entry| of_type(&entry.id)) {
                for field in entry.fields.values() {
                    if refers_to(&field.ty, target) {
                        out.push((entry.id.clone(), entry.kind.clone(), field.name.clone()));
                    }
                }
            }
            out
        };
        for (refty, kind, field) in referring {
            let filters = ids
                .iter()
                .map(|id| Filter::Is(field.clone(), id.clone()))
                .collect();
            let mut query = Query::of(kind.clone(), PAGE).filter(Filter::Any(filters));
            loop {
                let page = match self.command(Command::Query(query.clone()))? {
                    Reply::Page(page) => page,
                    other => return Err(backend(format!("a query answered {other:?}"))),
                };
                let shown = !page.resources.is_empty();
                for (id, indexed) in page.resources {
                    let row = named(&indexed.scalar(&field));
                    self.inner
                        .cache
                        .borrow_mut()
                        .entry(id.clone())
                        .or_insert_with(|| Some(Held::of(&kind, indexed)));
                    if let Some(index) = row
                        && let Some(of) = ResourceRef::parse_id(&id, refty.clone())
                        && let Some(links) = links.get_mut(index)
                    {
                        links.push(Link {
                            of,
                            how: LinkHow::Field,
                        });
                    }
                }
                match page.next {
                    Some(next) if shown => query.after = Some(next),
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// What an action made from the asked resources, wherever it placed them, as the changes
    /// that made them say: one read of the page's ids.
    fn links_made_from(
        &self,
        asked: &[(usize, ResourceRef)],
        only: Option<&str>,
        links: &mut [Vec<Link>],
    ) -> Result<(), StoreError> {
        let of_type = |ty: &str| only.is_none_or(|only| only == ty);
        let ids: Vec<String> = asked.iter().map(|(_, at)| id_of(at)).collect();
        let Reply::MadeFrom(made) = self.command(Command::MadeFrom { ids })? else {
            return Ok(());
        };
        for (n, made) in made.into_iter().enumerate() {
            let Some((index, _)) = asked.get(n) else {
                continue;
            };
            for (kind, id) in made {
                if let Some(entry) = self.entry_of_kind(&kind)
                    && of_type(&entry.id)
                    && let Some(of) = ResourceRef::parse_id(&id, entry.id)
                    && let Some(links) = links.get_mut(*index)
                {
                    links.push(Link {
                        of,
                        how: LinkHow::CreatedFrom,
                    });
                }
            }
        }
        Ok(())
    }

    /// What mentions any of the asked resources in its rich text, as the index holds each text's
    /// mentions: one page of the index for each kind that has a text, the rows of the page
    /// placed under each resource they name.
    fn links_by_mention(
        &self,
        asked: &[(usize, ResourceRef)],
        only: Option<&str>,
        links: &mut [Vec<Link>],
    ) -> Result<(), StoreError> {
        let of_type = |ty: &str| only.is_none_or(|only| only == ty);
        let mentioning: Vec<(TypeId, String, Vec<String>)> = {
            let kinds = self.inner.kinds.borrow();
            kinds
                .types()
                .filter(|entry| of_type(&entry.id))
                .filter_map(|entry| {
                    let texts: Vec<String> = entry
                        .fields
                        .values()
                        .filter(|field| field.kind == FieldKind::RichText)
                        .map(|field| format!("{MENTIONS}{}", field.name))
                        .collect();
                    (!texts.is_empty()).then(|| (entry.id.clone(), entry.kind.clone(), texts))
                })
                .collect()
        };
        for (refty, kind, texts) in mentioning {
            let wanted = Filter::Any(
                asked
                    .iter()
                    .flat_map(|(_, at)| {
                        let at = Value::Text(id_of(at));
                        texts
                            .iter()
                            .map(move |text| Filter::Has(text.clone(), at.clone()))
                    })
                    .collect(),
            );
            let mut query = Query::of(kind.clone(), PAGE).filter(wanted);
            loop {
                let page = match self.command(Command::Query(query.clone()))? {
                    Reply::Page(page) => page,
                    other => return Err(backend(format!("a query answered {other:?}"))),
                };
                let shown = !page.resources.is_empty();
                for (id, indexed) in page.resources {
                    // A row naming several of the asked resources links to each of them.
                    let names: Vec<usize> = asked
                        .iter()
                        .filter(|(_, at)| id != id_of(at))
                        .filter(|(_, at)| {
                            let at = Value::Text(id_of(at));
                            texts.iter().any(|text| {
                                matches!(
                                    indexed.fields.get(text),
                                    Some(Cell::Set(members)) if members.contains(&at)
                                )
                            })
                        })
                        .map(|(index, _)| *index)
                        .collect();
                    self.inner
                        .cache
                        .borrow_mut()
                        .entry(id.clone())
                        .or_insert_with(|| Some(Held::of(&kind, indexed)));
                    for index in names {
                        if let Some(of) = ResourceRef::parse_id(&id, refty.clone())
                            && let Some(links) = links.get_mut(index)
                        {
                            links.push(Link {
                                of: of.clone(),
                                how: LinkHow::Mention,
                            });
                        }
                    }
                }
                match page.next {
                    Some(next) if shown => query.after = Some(next),
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// The fields `fields` of every resource of the type `ty`, each by its id as [`Store::read`] reads it,
    /// a page of resources at a time: a page holding only their indexed fields among these, and
    /// their content, where a field is held in it, in one read a page. What search reads of every
    /// resource of a type, without a read a resource and a field.
    ///
    /// # Errors
    /// When the store cannot be read.
    pub fn texts_of(
        &self,
        ty: &str,
        fields: &[FieldSpec],
    ) -> Result<Vec<(ResourceRef, Vec<FieldOf>)>, StoreError> {
        let entries: Vec<Option<kinds::FieldEntry>> = fields
            .iter()
            .map(|field| self.field(ty, &field.id))
            .collect();
        // A field the index does not hold is the resource's content.
        let content = entries
            .iter()
            .any(|entry| !entry.as_ref().is_some_and(|entry| self.indexed(entry)));
        // The indexed fields, under each name they are held by: all a page carries.
        let held: Vec<String> = entries
            .iter()
            .flatten()
            .filter(|entry| self.indexed(entry))
            .flat_map(|entry| entry.names().cloned())
            .collect();
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for kind in self.kinds_of_type(ty)? {
            let mut query = Query::of(kind.clone(), PAGE);
            loop {
                let page = match self.command(Command::Fields {
                    query: query.clone(),
                    fields: held.clone(),
                })? {
                    Reply::Page(page) => page,
                    other => return Err(backend(format!("a query answered {other:?}"))),
                };
                let listed = !page.resources.is_empty();
                let shown: Vec<(String, Indexed)> = page
                    .resources
                    .into_iter()
                    .filter(|(id, _)| seen.insert(id.clone()))
                    .collect();
                let mut contents = if content && !shown.is_empty() {
                    let ids = shown.iter().map(|(id, _)| id.clone()).collect();
                    match self.command(Command::Contents {
                        kind: kind.clone(),
                        ids,
                    })? {
                        Reply::Contents(contents) => contents,
                        other => {
                            return Err(backend(format!("a read of contents answered {other:?}")));
                        }
                    }
                } else {
                    Vec::new()
                }
                .into_iter();
                for (id, indexed) in shown {
                    let json = contents.next().flatten();
                    let Some(at) = ResourceRef::parse_id(&id, ty.to_owned()) else {
                        continue;
                    };
                    // What this instance wrote and has not sent reads as it wrote it.
                    if self.inner.local.borrow().contains_key(&id) {
                        let values = fields
                            .iter()
                            .filter_map(|field| {
                                Some((field.id.clone(), self.read(&at, field).ok()?))
                            })
                            .collect();
                        out.push((at, values));
                        continue;
                    }
                    let values = fields
                        .iter()
                        .zip(&entries)
                        .filter_map(|(field, entry)| {
                            let names: Vec<&String> = entry
                                .as_ref()
                                .map_or_else(|| vec![&field.id], |entry| entry.names().collect());
                            let cell = names.iter().find_map(|name| indexed.fields.get(*name));
                            let json = json.as_ref().and_then(Json::as_object).and_then(|map| {
                                names.iter().find_map(|name| map.get(name.as_str()))
                            });
                            let value = self.value_from(field, entry.as_ref(), cell, json).ok()?;
                            Some((field.id.clone(), value))
                        })
                        .collect();
                    out.push((at, values));
                }
                match page.next {
                    Some(next) if listed => query.after = Some(next),
                    _ => break,
                }
            }
        }
        Ok(out)
    }

    /// Whether the store holds `at`, under its kind or the one an earlier version stored it as.
    #[must_use]
    pub fn contains(&self, at: &ResourceRef) -> bool {
        self.kinds_of_type(&at.ty).is_ok_and(|kinds| {
            kinds
                .iter()
                .any(|kind| self.held(kind, &id_of(at)).is_ok_and(|held| held.is_some()))
        })
    }

    /// The type of the resource `id` (`doc` or `doc/item`), whatever type it is: what an id
    /// alone is read as, a published address or a reference in a text. `None` when the store
    /// holds no such resource of a loaded type.
    ///
    /// Asked of the store once, by the id alone, where nothing was read of it yet: the store's
    /// index says which kind holds it (or held it last, when it was removed), however many kinds
    /// the app knows, so a read of an id the store holds none of costs one round trip.
    #[must_use]
    pub fn type_of_id(&self, id: &str) -> Option<TypeId> {
        // The workspace's members' resource is of its one kind, which few packages read.
        if id == tilo_resource_store::WORKSPACE_MEMBERS_ID {
            return self.type_of_kind(tilo_resource_store::WORKSPACE_MEMBERS_KIND);
        }
        // What was written here is what it is, removed included.
        if let Some(written) = self.inner.local.borrow().get(id) {
            return written
                .as_ref()
                .and_then(|held| self.type_of_kind(&held.kind));
        }
        if let Some(Some(held)) = self.inner.cache.borrow().get(id) {
            return self.type_of_kind(&held.kind);
        }
        let Ok(Reply::Kind(Some(kind))) = self.command(Command::KindOf { id: id.to_owned() })
        else {
            return None;
        };
        self.type_of_kind(&kind)
    }

    /// The type of the resource `at` when the store holds it now: one removed is not its
    /// reader's any more, though its history is.
    #[must_use]
    pub fn holds(&self, at: &ResourceRef) -> Option<TypeId> {
        let id = at.id_string();
        let ty = self.type_of_id(&id)?;
        self.kinds_of_type(&ty)
            .ok()?
            .iter()
            .any(|kind| self.held(kind, &id).is_ok_and(|held| held.is_some()))
            .then_some(ty)
    }

    /// What the field `at`'s type names its resources by holds: its first text, plain or rich.
    #[must_use]
    pub fn title_value(&self, at: &ResourceRef) -> Option<FieldValue> {
        let spec = {
            let kinds = self.inner.kinds.borrow();
            let entry = kinds.get(&at.ty)?;
            let title = entry.title.clone()?;
            let kind = entry.fields.get(&title)?.kind;
            FieldSpec { id: title, kind }
        };
        self.read(at, &spec).ok()
    }

    /// `at` with its type: as it names it, else as the store holds the resource. A reference
    /// built from an id alone (a version's resource, a mark's) names none.
    fn typed<'a>(&self, at: &'a ResourceRef) -> std::borrow::Cow<'a, ResourceRef> {
        if !at.ty.is_empty() {
            return std::borrow::Cow::Borrowed(at);
        }
        match self.type_of_id(&id_of(at)) {
            Some(ty) => {
                let mut typed = at.clone();
                typed.ty = ty;
                std::borrow::Cow::Owned(typed)
            }
            None => std::borrow::Cow::Borrowed(at),
        }
    }

    /// Sets the field `field` of `at` to `value`, as `by`'s change of the verb `verb`: a write
    /// the platform makes for a person outside any action, as a bound control's edit is.
    ///
    /// # Errors
    /// When `at` is not held, or the store refused it.
    pub fn set_register(
        &self,
        at: &ResourceRef,
        field: &str,
        value: Scalar,
        verb: &str,
        by: &Actor,
    ) -> Result<(), StoreError> {
        let mut store = self.clone();
        let txn = store.begin()?;
        let mutation = Mutation::SetRegister {
            field: field.to_owned(),
            value,
        };
        if let Err(error) = store.apply(txn, at, &mutation) {
            store.rollback(txn);
            return Err(error);
        }
        store.commit(
            txn,
            &ChangeNote {
                action: verb.to_owned(),
                invoker: Some(by.clone()),
                ..ChangeNote::default()
            },
        )
    }

    /// What names `at`: the first of its texts that holds any, the one its type names its
    /// resources by first, as its reader reads it.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<String> {
        let specs: Vec<FieldSpec> = {
            let kinds = self.inner.kinds.borrow();
            let entry = kinds.get(&at.ty)?;
            entry
                .texts
                .iter()
                .filter_map(|id| {
                    let kind = entry.fields.get(id)?.kind;
                    Some(FieldSpec {
                        id: id.clone(),
                        kind,
                    })
                })
                .collect()
        };
        // The title, and while it is empty the next text: a note not titled yet is named by
        // what its body says. Rich text reads as its reader holds what it refers to.
        specs.iter().find_map(|spec| {
            let text = match self.read(at, spec).ok()? {
                FieldValue::Scalar(tilolang_runtime::store::Scalar::Text(text))
                | FieldValue::Text(text) => text,
                value => crate::reading::said(&Holdings(self), &at.id_string(), value)?,
            };
            (!text.is_empty()).then_some(text)
        })
    }

    /// Whether this device runs the action `address` (`<app>/<resource>#<action>`) on `inputs`
    /// now, an action that runs once in the workspace whichever device runs it: the device's
    /// workspace service places it. A store that cannot be asked answers [`Turn::Later`], so nothing runs twice.
    #[must_use]
    pub fn take_turn(&self, address: &str, inputs: &Json) -> Turn {
        match self.inner.port.command(Command::Turn {
            address: address.to_owned(),
            inputs: inputs.clone(),
            answer: None,
        }) {
            Ok(Reply::Turn(turn)) => turn,
            Ok(other) => {
                tracing::warn!(address, ?other, "a turn was answered with no turn");
                Turn::Later
            }
            Err(error) => {
                tracing::debug!(address, %error, "a turn could not be asked for");
                Turn::Later
            }
        }
    }

    /// Keep that this device ran the action [`Self::take_turn`] gave it, and what it answered:
    /// no device runs it again, and each that asks is given `answer`.
    pub fn turn_done(&self, address: &str, inputs: &Json, answer: Json) {
        if let Err(error) = self.inner.port.command(Command::Turn {
            address: address.to_owned(),
            inputs: inputs.clone(),
            answer: Some(answer),
        }) {
            tracing::warn!(address, %error, "a turn that ran was not kept done");
        }
    }

    /// Whether the migration `migration` ran on `at`, or `at` was made by a version that
    /// declares it.
    #[must_use]
    pub fn migrated(&self, at: &ResourceRef, migration: &str) -> bool {
        self.mark_of(at, MIGRATED, migration).is_some()
    }

    /// What the trigger `trigger` last ran for `at`: the period it ran in, or the instant it
    /// fired at.
    #[must_use]
    pub fn triggered(&self, at: &ResourceRef, trigger: &str) -> Option<String> {
        self.mark_of(at, TRIGGERED, trigger)
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// Notes that the trigger `trigger` ran for `at` at `value` with the first commit of the run
    /// `run`, as part of the change it makes: what the trigger did is one change.
    pub fn mark_triggered_in_run(&self, run: &str, at: &ResourceRef, trigger: &str, value: &str) {
        self.inner.run_marks.borrow_mut().push(RunMark {
            run: run.to_owned(),
            doc: id_of(at),
            key: trigger.to_owned(),
            value: Json::String(value.to_owned()),
        });
    }

    /// Drops what [`Self::mark_triggered_in_run`] left for the run `run` to write: a run that
    /// committed nothing notes nothing.
    pub fn drop_run_marks(&self, run: &str) {
        self.inner
            .run_marks
            .borrow_mut()
            .retain(|mark| mark.run != run);
    }

    fn mark_in(
        &self,
        txn: Txn,
        doc: &str,
        field: &str,
        key: &str,
        value: Json,
    ) -> Result<(), StoreError> {
        let Some(mut held) = self
            .inner
            .local
            .borrow()
            .get(doc)
            .cloned()
            .flatten()
            .or_else(|| self.inner.cache.borrow().get(doc).cloned().flatten())
        else {
            return Err(not_found(doc.to_owned()));
        };
        let edit = Edit::MapPut {
            field: field.to_owned(),
            key: key.to_owned(),
            value,
        };
        if let Some(content) = held.content.as_mut() {
            apply_edit(content, &edit);
        }
        self.write(txn, doc, Some(held), |dirty| dirty.edits.push(edit))
    }

    fn mark_of(&self, at: &ResourceRef, field: &str, key: &str) -> Option<Json> {
        let content = self.content_of(at).ok()?;
        content.get(field)?.as_object()?.get(key).cloned()
    }

    /// The migration `migration` of `at` runs from here: its first commit notes that it ran,
    /// as part of the change it makes.
    pub fn migrating(&self, at: &ResourceRef, migration: &str) {
        *self.inner.migrating.borrow_mut() = Some((migration.to_owned(), id_of(at)));
    }

    /// The migration [`Self::migrating`] named ended, as `by`'s: one that ran and wrote nothing
    /// is noted as a change of its own; one that did not run is not noted.
    ///
    /// # Errors
    /// When the store refused the note.
    pub fn migrated_by(
        &self,
        at: &ResourceRef,
        migration: &str,
        by: &Actor,
        ran: bool,
    ) -> Result<(), StoreError> {
        if self.inner.migrating.borrow_mut().take().is_none() || !ran {
            return Ok(());
        }
        let note = ChangeNote {
            action: migration.to_owned(),
            invoker: Some(by.clone()),
            ..ChangeNote::default()
        };
        self.mark(at, MIGRATED, migration, Json::Bool(true), &note)
    }

    /// Puts `key: value` in the map `field` of `at`'s content, as the change `note` says.
    fn mark(
        &self,
        at: &ResourceRef,
        field: &str,
        key: &str,
        value: Json,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        let mut store = self.clone();
        let txn = store.begin()?;
        let marked = store.mark_in(txn, &id_of(at), field, key, value);
        if let Err(error) = marked {
            store.rollback(txn);
            return Err(error);
        }
        store.commit(txn, note)
    }

    /// Begins a trial run: what is committed from here is read, but kept here until
    /// [`Self::keep`] sends it or [`Self::rewind`] drops it.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        self.note_held_all();
        let saved = self.saved();
        let mut trials = self.inner.trials.borrow_mut();
        trials.push(saved);
        Checkpoint(trials.len())
    }

    /// Ends the trial run `at`, keeping what it wrote: it is sent with the next batch.
    ///
    /// # Errors
    /// When the store refused what was sent.
    pub fn keep(&self, at: Checkpoint) -> Result<(), StoreError> {
        self.inner
            .trials
            .borrow_mut()
            .truncate(at.0.saturating_sub(1));
        self.settle()
    }

    /// Ends the trial run `at`, dropping what it wrote.
    pub fn rewind(&self, at: Checkpoint) {
        let saved = {
            let mut trials = self.inner.trials.borrow_mut();
            let saved = trials.get(at.0.saturating_sub(1)).cloned();
            trials.truncate(at.0.saturating_sub(1));
            // What the trial wrote is dropped, so nothing of it is told.
            self.inner
                .heard
                .borrow_mut()
                .retain(|heard| heard.trial < at.0);
            saved
        };
        if let Some(saved) = saved {
            self.restore(saved);
        }
    }

    /// Notes, as the first run or trial run opens, whether the store holds all written here.
    fn note_held_all(&self) {
        if self.inner.runs.borrow().is_empty() && self.inner.trials.borrow().is_empty() {
            self.inner
                .held_all
                .set(self.inner.local.borrow().is_empty() && self.inner.unsent.borrow().is_empty());
        }
    }

    /// Once no run or trial run is open: sends what they wrote and tells the views what it
    /// changed, with the steps it took in the listings it changed when what they wrote is all
    /// that was unsent.
    fn settle(&self) -> Result<(), StoreError> {
        if !self.inner.runs.borrow().is_empty() || !self.inner.trials.borrow().is_empty() {
            return Ok(());
        }
        let heard = std::mem::take(&mut *self.inner.heard.borrow_mut());
        let alone = self.inner.held_all.get() && self.inner.sample.borrow().is_none();
        let stepping = if alone {
            let mut collections: BTreeMap<DocId, BTreeSet<FieldKey>> = BTreeMap::new();
            let mut order: Vec<String> = Vec::new();
            for heard in &heard {
                for (doc, keys) in &heard.collections {
                    collections
                        .entry(doc.clone())
                        .or_default()
                        .extend(keys.iter().cloned());
                }
                for id in &heard.order {
                    if !order.contains(id) {
                        order.push(id.clone());
                    }
                }
            }
            self.steps_before(&collections, &order)
        } else {
            Vec::new()
        };
        let flushed = self.flush();
        self.tell(
            heard,
            if flushed.is_ok() {
                stepping
            } else {
                Vec::new()
            },
        );
        flushed
    }

    fn saved(&self) -> Saved {
        Saved {
            local: self.inner.local.borrow().clone(),
            unsent: self.inner.unsent.borrow().len(),
        }
    }

    fn restore(&self, saved: Saved) {
        *self.inner.local.borrow_mut() = saved.local;
        self.inner.unsent.borrow_mut().truncate(saved.unsent);
        self.inner.listings.borrow_mut().clear();
    }

    fn command(&self, command: Command) -> Result<Reply, StoreError> {
        let _phase = tilo_resource::profiling::Phase::start("store.command");
        if command.writes() {
            self.forget_versions_of(&command);
            // A write may bring back what was read as none (an undo of a removal).
            self.inner.absent.borrow_mut().clear();
            // What this write adds is pending until another device takes it in.
            self.inner.reach.borrow_mut().clear();
            self.inner.queried.borrow_mut().clear();
        }
        self.inner.port.command(command).map_err(backend)
    }

    /// Sends what was committed, as one batch, unless a run or a trial run is open.
    fn flush(&self) -> Result<(), StoreError> {
        if !self.inner.runs.borrow().is_empty()
            || !self.inner.trials.borrow().is_empty()
            || self.inner.sample.borrow().is_some()
        {
            return Ok(());
        }
        let commands = std::mem::take(&mut *self.inner.unsent.borrow_mut());
        let local = std::mem::take(&mut *self.inner.local.borrow_mut());
        if commands.is_empty() {
            return Ok(());
        }
        let command = if commands.len() == 1 {
            commands
                .into_iter()
                .next()
                .unwrap_or(Command::Batch(Vec::new()))
        } else {
            Command::Batch(commands)
        };
        let mut written = Written::default();
        if ids_written(&command, &mut written) {
            // What the store held of each is in the cache until the store takes what is sent now.
            self.freeze_listings(
                &written.reshaped,
                |id| self.inner.cache.borrow().get(id).cloned(),
                |id| local.get(id).cloned(),
            );
            // A resource only edited (a note's body as a person types it) neither joins, leaves
            // nor moves in a listing: only one ordered or filtered by when it last changed reads
            // differently.
            let updated = |key: &str, _: &Pages| key.contains(tilo_resource_store::UPDATED);
            self.freeze_where(&written.edited_only(), updated);
        } else {
            self.forget_listings(None);
        }
        let invocations = invocations_of(&command);
        match self.command(command) {
            Ok(_) => {
                if let Some(last) = invocations.last() {
                    *self.inner.last_write.borrow_mut() = Some(last.clone());
                }
                // What was sent is what the store holds now, so it is read from here.
                let mut cache = self.inner.cache.borrow_mut();
                for (id, held) in local {
                    cache.insert(id, held);
                }
                Ok(())
            }
            Err(error) => {
                let mut cache = self.inner.cache.borrow_mut();
                for id in local.keys() {
                    cache.remove(id);
                }
                *self.inner.refused.borrow_mut() = Some(error.to_string());
                Err(error)
            }
        }
    }

    fn notify(&self, summary: &ChangeSummary) {
        // What changed has versions it had not: a change that names no fields may be anything
        // of the document's.
        {
            let mut versions = self.inner.versions.borrow_mut();
            let mut newest = self.inner.newest.borrow_mut();
            if summary.fields.is_empty() {
                let under = format!("{}/", summary.doc);
                versions.retain(|id, _| *id != summary.doc && !id.starts_with(&under));
                newest.remove(&summary.doc);
            }
            for key in &summary.fields {
                forget_versions(&mut versions, &mut newest, &key.resource);
            }
        }
        self.tell_watches(Some(summary));
        let mut subs = std::mem::take(&mut *self.inner.subs.borrow_mut());
        for sub in &mut subs {
            if sub.doc == summary.doc {
                (sub.callback)(summary);
            }
        }
        let mut held = self.inner.subs.borrow_mut();
        let added = std::mem::take(&mut *held);
        *held = subs;
        held.extend(added);
    }

    fn kind_of_type(&self, ty: &str) -> Result<String, StoreError> {
        self.inner
            .kinds
            .borrow()
            .get(ty)
            .map(|entry| entry.kind.clone())
            .ok_or_else(|| not_found(format!("no type {ty} is loaded")))
    }

    /// The kinds the resources of the type `ty` are of: the one it publishes now, then the ones
    /// an earlier version stored them as.
    fn kinds_of_type(&self, ty: &str) -> Result<Vec<String>, StoreError> {
        self.inner
            .kinds
            .borrow()
            .get(ty)
            .map(|entry| entry.kinds().cloned().collect())
            .ok_or_else(|| not_found(format!("no type {ty} is loaded")))
    }

    fn entry_of_kind(&self, kind: &str) -> Option<TypeEntry> {
        self.inner.kinds.borrow().of_kind(kind).cloned()
    }

    /// The type whose resources are of `kind`.
    fn type_of_kind(&self, kind: &str) -> Option<TypeId> {
        self.inner
            .kinds
            .borrow()
            .of_kind(kind)
            .map(|entry| entry.id.clone())
    }

    /// The resource `id` of `kind` as it reads now: what was written here, else what the store
    /// holds. `None` where there is none.
    fn held(&self, kind: &str, id: &str) -> Result<Option<Held>, StoreError> {
        // What is kept is kept by id: one of another kind is not this kind's, and an absence
        // kept says only that the id is of none of the kinds it was asked as, so it is asked
        // again as this one.
        let of_kind = |held: &Held| (held.kind == kind).then(|| held.clone());
        if let Some(local) = self.inner.local.borrow().get(id) {
            return Ok(local.as_ref().and_then(of_kind));
        }
        if let Some(Some(cached)) = self.inner.cache.borrow().get(id) {
            return Ok(of_kind(cached));
        }
        let asked = (kind.to_owned(), id.to_owned());
        if self.inner.absent.borrow().contains(&asked) {
            return Ok(None);
        }
        let reply = self.command(Command::Get {
            kind: kind.to_owned(),
            id: id.to_owned(),
        })?;
        let held = match reply {
            Reply::Resource(found) => found.map(|indexed| Held::of(kind, indexed)),
            other => return Err(backend(format!("a read answered {other:?}"))),
        };
        match &held {
            Some(_) => {
                self.inner
                    .cache
                    .borrow_mut()
                    .insert(id.to_owned(), held.clone());
            }
            None => {
                self.inner.absent.borrow_mut().insert(asked);
            }
        }
        Ok(held)
    }

    fn held_at(&self, at: &ResourceRef) -> Result<Held, StoreError> {
        // An item of a union's collection is named before its type is known: what it is
        // stored as is the answer.
        let ty = if at.ty.is_empty() {
            self.type_of_id(&id_of(at))
                .ok_or_else(|| not_found(id_of(at)))?
        } else {
            at.ty.clone()
        };
        let kinds = self.kinds_of_type(&ty)?;
        for kind in kinds {
            if let Some(held) = self.held(&kind, &id_of(at))? {
                return Ok(held);
            }
        }
        Err(not_found(id_of(at)))
    }

    /// The content of `id` of `kind`, read once.
    fn content(&self, held: &mut Held, id: &str) -> Result<(), StoreError> {
        if held.content.is_some() {
            return Ok(());
        }
        let reply = self.command(Command::Content {
            kind: held.kind.clone(),
            id: id.to_owned(),
        })?;
        let content = match reply {
            Reply::Content(Some(Json::Object(map))) => map,
            Reply::Content(_) => serde_json::Map::new(),
            other => return Err(backend(format!("a read answered {other:?}"))),
        };
        held.content = Some(content);
        Ok(())
    }

    /// The content of `at`, as it reads now.
    fn content_of(&self, at: &ResourceRef) -> Result<serde_json::Map<String, Json>, StoreError> {
        let id = id_of(at);
        let mut held = self.held_at(at)?;
        if held.content.is_none() {
            self.content(&mut held, &id)?;
            let mut local = self.inner.local.borrow_mut();
            if let Some(Some(written)) = local.get_mut(&id) {
                written.content.clone_from(&held.content);
            } else {
                drop(local);
                if let Some(Some(cached)) = self.inner.cache.borrow_mut().get_mut(&id) {
                    cached.content.clone_from(&held.content);
                }
            }
        }
        Ok(held.content.unwrap_or_default())
    }

    /// The cell the field named `name` of the resource `at` holds, or the one it was held under
    /// before (`former`), read without taking the resource whole: the field a list's ten
    /// thousand rows read of each.
    fn cell_of(
        &self,
        at: &ResourceRef,
        name: &str,
        former: &[String],
    ) -> Result<Option<Cell>, StoreError> {
        let cell = |held: &Held| {
            held.fields
                .get(name)
                .or_else(|| former.iter().find_map(|name| held.fields.get(name)))
                .cloned()
        };
        let id = id_of(at);
        {
            let local = self.inner.local.borrow();
            if let Some(held) = local.get(&id) {
                return held.as_ref().map(cell).ok_or_else(|| not_found(id.clone()));
            }
        }
        {
            let cache = self.inner.cache.borrow();
            if let Some(held) = cache.get(&id) {
                return held.as_ref().map(cell).ok_or_else(|| not_found(id.clone()));
            }
        }
        Ok(cell(&self.held_at(at)?))
    }

    /// The name a field of the type `ty` is held under, and what it is.
    fn field(&self, ty: &str, field: &str) -> Option<kinds::FieldEntry> {
        self.inner
            .kinds
            .borrow()
            .get(ty)
            .and_then(|entry| entry.fields.get(field).cloned())
    }

    fn indexed(&self, entry: &kinds::FieldEntry) -> bool {
        entry.indexed(&self.inner.kinds.borrow())
    }

    /// Whether the items of the collection `field` of `holder` share their holder with those
    /// of another of its collections of the same type, or with a record of that type a field of
    /// it holds: each then says which it stands in.
    fn shares_holder(&self, holder: &str, field: &str) -> bool {
        let kinds = self.inner.kinds.borrow();
        let Some(entry) = kinds.get(holder) else {
            return false;
        };
        let placed = |entry: &kinds::FieldEntry| -> Vec<TypeId> {
            entry.items.iter().chain(&entry.held).cloned().collect()
        };
        let Some(mine) = entry.fields.get(field).map(placed) else {
            return false;
        };
        entry.fields.iter().any(|(other, entry)| {
            other != field && placed(entry).iter().any(|ty| mine.contains(ty))
        })
    }

    /// Mark `held` as standing in the collection `field` of a resource of the type `holder`,
    /// where that is more than its parent says.
    fn stand_in(&self, held: &mut Held, holder: &str, field: &str) {
        if self.shares_holder(holder, field) {
            held.fields.insert(
                STANDS_IN.to_owned(),
                Cell::One(Value::Text(field.to_owned())),
            );
        } else {
            held.fields.remove(STANDS_IN);
        }
    }

    // ---- writing ----------------------------------------------------------------------------

    fn txn_mut<R>(&self, txn: Txn, f: impl FnOnce(&mut TxnState) -> R) -> Result<R, StoreError> {
        let mut state = self.inner.txn.borrow_mut();
        match state.as_mut() {
            Some(open) if open.id == txn.0 => Ok(f(open)),
            _ => Err(StoreError::Invalid(format!(
                "no open transaction {}",
                txn.0
            ))),
        }
    }

    /// Writes `held` as `id` in the open transaction, noting what changed.
    fn write(
        &self,
        txn: Txn,
        id: &str,
        held: Option<Held>,
        dirty: impl FnOnce(&mut Dirty),
    ) -> Result<(), StoreError> {
        let before = self
            .inner
            .local
            .borrow()
            .get(id)
            .cloned()
            .map_or(Before::Unwritten, Before::Was);
        let held_before = matches!(before, Before::Was(Some(_)))
            || self
                .inner
                .cache
                .borrow()
                .get(id)
                .is_some_and(Option::is_some);
        let indexed = self.txn_mut(txn, |state| {
            state.before.entry(id.to_owned()).or_insert(before);
            if !state.dirty.contains_key(id) {
                state.order.push(id.to_owned());
            }
            let written = state.dirty.entry(id.to_owned()).or_default();
            dirty(written);
            written.put || written.removed || written.remove.is_some()
        })?;
        let kind = held
            .as_ref()
            .map(|held| held.kind.clone())
            .or_else(|| self.kind_held(id));
        match kind {
            // An edit of what the index does not hold (a note's body as a person types it)
            // neither adds, removes nor moves an item: a listing reads the index alone, so only
            // one ordered or filtered by when its items last changed reads differently.
            // Freezing every listing of the kind would read each again, whole, at every key.
            Some(kind) if held.is_some() && held_before && !indexed => {
                let updated = |key: &str, _: &Pages| key.contains(tilo_resource_store::UPDATED);
                self.freeze_where(
                    &BTreeMap::from([(kind, BTreeSet::from([id.to_owned()]))]),
                    updated,
                );
            }
            Some(kind) => {
                let was = self.txn_mut(txn, |state| match state.before.get(id) {
                    Some(Before::Was(was)) => Some(was.clone()),
                    _ => None,
                })?;
                let was = was.or_else(|| self.inner.cache.borrow().get(id).cloned());
                self.freeze_listings(
                    &BTreeMap::from([(kind, BTreeSet::from([id.to_owned()]))]),
                    |_| was.clone(),
                    |_| Some(held.clone()),
                );
            }
            None => self.forget_listings(None),
        }
        // What holds it changes when it last changed: the listings of its kind ordered or
        // filtered by that time read it again. All such listings, where its kind is not known.
        let holder = held
            .as_ref()
            .and_then(|held| held.parent.clone())
            .or_else(|| self.parent_held(id));
        if let Some(holder) = holder {
            let updated = |key: &str, _: &Pages| key.contains(tilo_resource_store::UPDATED);
            let kinds: Vec<String> = match self.kind_held(&holder) {
                Some(kind) => vec![kind],
                None => self
                    .inner
                    .listings
                    .borrow()
                    .values()
                    .map(|(_, pages)| pages.kind.clone())
                    .collect(),
            };
            let written = kinds
                .into_iter()
                .map(|kind| (kind, BTreeSet::from([holder.clone()])))
                .collect();
            self.freeze_where(&written, updated);
        }
        self.inner.local.borrow_mut().insert(id.to_owned(), held);
        Ok(())
    }

    /// The kind of the resource `id`, as written here or read from the store.
    /// What holds the resource `id`, as this store last read or wrote it.
    fn parent_held(&self, id: &str) -> Option<String> {
        let held = |map: &HashMap<String, Option<Held>>| {
            map.get(id)
                .and_then(|held| held.as_ref().and_then(|held| held.parent.clone()))
        };
        held(&self.inner.local.borrow()).or_else(|| held(&self.inner.cache.borrow()))
    }

    fn kind_held(&self, id: &str) -> Option<String> {
        let held = |map: &HashMap<String, Option<Held>>| {
            map.get(id)
                .and_then(|held| held.as_ref().map(|held| held.kind.clone()))
        };
        held(&self.inner.local.borrow()).or_else(|| held(&self.inner.cache.borrow()))
    }

    /// Lets go of the open listings of the kinds `written` names, which the write about to be
    /// sent changes; each is left as it read before the write. A view holding one compares it
    /// with the listing read after the write, and moves the rows the write moved: where each
    /// resource the write names stood in it is read from the store's index now, while the
    /// store holds it as it was, and the listing reads nothing more.
    ///
    /// Only the listings the write reaches: one the written resources match, as they stood
    /// before (`was`) or as they are written (`now`). A task checked off leaves the list of
    /// another task's subtasks as it was, so a list showing a row per task reads none of them
    /// again. A resource whose standing either way is not known here reaches every listing of
    /// its kind, and so does any write a listing ordered or filtered by update time.
    fn freeze_listings(
        &self,
        written: &BTreeMap<String, BTreeSet<String>>,
        was: impl Fn(&str) -> Option<Option<Held>>,
        now: impl Fn(&str) -> Option<Option<Held>>,
    ) {
        // Each written resource as the index holds it, before and after: `None` where that is
        // not known.
        let standing: BTreeMap<&str, Option<[Option<Indexed>; 2]>> = written
            .values()
            .flatten()
            .map(|id| {
                let known = was(id).zip(now(id)).map(|(was, now)| {
                    [was, now].map(|held| {
                        held.map(|held| Indexed {
                            doc: String::new(),
                            parent: held.parent,
                            order: held.order,
                            fields: held.fields,
                        })
                    })
                });
                (id.as_str(), known)
            })
            .collect();
        self.freeze_where(written, |key, pages| {
            key.contains(tilo_resource_store::UPDATED)
                || written.get(&pages.kind).is_none_or(|ids| {
                    ids.iter().any(|id| {
                        standing.get(id.as_str()).is_none_or(|standing| {
                            standing.as_ref().is_none_or(|both| {
                                both.iter().flatten().any(|stood| {
                                    pages.query.filters.iter().all(|filter| filter.holds(stood))
                                })
                            })
                        })
                    })
                })
        });
    }

    /// [`Self::freeze_listings`] for the listings `which` names, by their key and pages, of the
    /// kinds `written` names: each of them all the same.
    fn freeze_where(
        &self,
        written: &BTreeMap<String, BTreeSet<String>>,
        which: impl Fn(&str, &Pages) -> bool,
    ) {
        let frozen: Vec<(String, Rc<Pages>)> = {
            let mut listings = self.inner.listings.borrow_mut();
            // A listing's key leads with its kind, so the listings of the kinds written are the
            // run of keys from each kind: a write that names one kind never looks at a listing of
            // another, however many kinds the store has read.
            let mut keys: Vec<&str> = Vec::new();
            for kind in written.keys() {
                for key in listings.range(kind.clone()..).map(|(key, _)| key.as_str()) {
                    if !listing_of(key, kind) {
                        break;
                    }
                    keys.push(key);
                }
            }
            let keys: Vec<String> = keys
                .into_iter()
                .filter(|key| {
                    listings
                        .get(*key)
                        .is_some_and(|(_, pages)| which(key, pages))
                })
                .map(str::to_owned)
                .collect();
            keys.into_iter()
                .filter_map(|key| listings.remove(&key).map(|(_, pages)| (key, pages)))
                .collect()
        };
        for (_, pages) in frozen {
            let ids = written.get(&pages.kind).cloned().unwrap_or_default();
            pages.freeze(&ids);
        }
    }

    /// Lets go of the open listings of `kind`, which a write of one of its resources changed:
    /// the listings of every other kind stay as they were read. Every listing, for a write of
    /// a kind not known.
    fn forget_listings(&self, kind: Option<&str>) {
        let mut listings = self.inner.listings.borrow_mut();
        match kind {
            Some(kind) => listings.retain(|key, _| !listing_of(key, kind)),
            None => listings.clear(),
        }
    }

    fn touch(
        &self,
        txn: Txn,
        at: &ResourceRef,
        field: &str,
        collection: bool,
    ) -> Result<(), StoreError> {
        let key = FieldKey::new(at, field);
        self.txn_mut(txn, |state| {
            if collection {
                state
                    .collections
                    .entry(at.doc.clone())
                    .or_default()
                    .insert(key.clone());
            }
            state.touched.entry(at.doc.clone()).or_default().insert(key);
        })
    }

    /// Tells whoever reads the links of each resource `ids` names that they changed: a field
    /// or a text now names it, or no longer does.
    fn touch_links<'a>(
        &self,
        txn: Txn,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), StoreError> {
        let keys: Vec<(DocId, FieldKey)> = ids
            .into_iter()
            .map(|id| {
                (
                    tilolang_runtime::store::doc_of(id).to_owned(),
                    FieldKey {
                        resource: id.to_owned(),
                        field: tilolang_runtime::store::LINKS.to_owned(),
                    },
                )
            })
            .collect();
        self.txn_mut(txn, |state| {
            for (doc, key) in keys {
                state.touched.entry(doc).or_default().insert(key);
            }
        })
    }

    /// A resource of `record`'s type, as it starts.
    fn new_held(
        &self,
        record: &NewResource,
        parent: Option<String>,
        order: Option<String>,
    ) -> Result<Held, StoreError> {
        let kind = self.kind_of_type(&record.ty)?;
        let mut fields = BTreeMap::new();
        let mut content = serde_json::Map::new();
        for (field, text) in &record.text_defaults {
            match self.field(&record.ty, field) {
                Some(entry) if self.indexed(&entry) => {
                    fields.insert(entry.name, Cell::One(Value::Text(text.clone())));
                }
                Some(entry) => {
                    content.insert(entry.name, Json::String(text.clone()));
                }
                None => {
                    content.insert(field.clone(), Json::String(text.clone()));
                }
            }
        }
        // What a read of a field it is made without answers, the index holds too: a query for
        // what is not done finds a task nobody checked, however it was made.
        let initials: Vec<(String, Value)> = {
            let kinds = self.inner.kinds.borrow();
            kinds
                .get(&record.ty)
                .map(|entry| {
                    entry
                        .fields
                        .values()
                        .filter(|field| field.indexed(&kinds))
                        .filter_map(|field| Some((field.name.clone(), field.initial.clone()?)))
                        .collect()
                })
                .unwrap_or_default()
        };
        for (name, initial) in initials {
            fields.entry(name).or_insert(Cell::One(initial));
        }
        Ok(Held {
            kind,
            parent,
            order,
            fields,
            content: Some(content),
        })
    }

    /// What the content of a new resource of the type `ty` starts with, written with its
    /// making: an empty map for each map it keeps. Two people putting the first entry of one
    /// at once then put it in the same map, and both entries stay; a map each made of their own
    /// would keep only one of them.
    fn initial_edits(&self, ty: &TypeId, held: &mut Held) -> Vec<Edit> {
        let maps: Vec<String> = {
            let kinds = self.inner.kinds.borrow();
            let Some(entry) = kinds.get(ty) else {
                return Vec::new();
            };
            entry
                .fields
                .values()
                .filter(|field| field.kind == FieldKind::Map && !field.indexed(&kinds))
                .map(|field| field.name.clone())
                .collect()
        };
        let mut edits = Vec::with_capacity(maps.len());
        for name in maps {
            let edit = Edit::Set {
                field: name,
                value: Json::Object(serde_json::Map::new()),
            };
            if let Some(content) = held.content.as_mut() {
                apply_edit(content, &edit);
            }
            edits.push(edit);
        }
        edits
    }

    /// The item ids of the collection `field` of `at` as they read now, with each one's order key.
    /// The items of the collection `field` of `at` as they stood at the version the change
    /// `change` made, in their order then: the store's read of what `at` held then, each item's
    /// fields and content kept for the reads of them at the same version that follow.
    fn children_at(
        &self,
        at: &ResourceRef,
        field: &str,
        change: &str,
    ) -> Result<Vec<ItemId>, StoreError> {
        let Some(item_types) = self.field(&at.ty, field).map(|entry| entry.items) else {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        };
        let kinds: BTreeSet<String> = item_types
            .iter()
            .map(|ty| self.kinds_of_type(ty))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let holder = id_of(at);
        let shared = self.shares_holder(&at.ty, field);
        let mut children = Vec::new();
        let mut from = 0;
        loop {
            let reply = self
                .command(Command::ChildrenAt {
                    parent: holder.clone(),
                    change: change.to_owned(),
                    from,
                })
                .map_err(|error| match error {
                    // A service older than this read does not know it, and says so.
                    StoreError::Backend(reason) if reason.contains("unknown variant") => {
                        StoreError::Invalid(format!(
                            "this device's Tilo cannot read what a collection held at a past \
                             version yet; update Tilo to compare versions ({reason})"
                        ))
                    }
                    other => other,
                })?;
            let page = match reply {
                Reply::ChildrenAt(Some(page)) => page,
                Reply::ChildrenAt(None) => {
                    return Err(StoreError::Invalid(format!(
                        "this store keeps no log to read the change {change} in"
                    )));
                }
                other => return Err(backend(format!("a read of a version answered {other:?}"))),
            };
            children.extend(page.children);
            match page.next {
                Some(next) if next > from => from = next,
                _ => break,
            }
        }
        let mut past = self.inner.past.borrow_mut();
        if past.as_ref().is_none_or(|(read, _)| read != change) {
            *past = Some((change.to_owned(), HashMap::new()));
        }
        let mut items = Vec::new();
        if let Some((_, looks)) = past.as_mut() {
            for child in children {
                let stands_here = !shared
                    || matches!(
                        child.indexed.fields.get(STANDS_IN),
                        Some(Cell::One(Value::Text(stands))) if stands == field
                    );
                if kinds.contains(&child.kind) && stands_here {
                    items.push(item_of(&child.id));
                }
                looks.insert(
                    child.id,
                    Look {
                        indexed: Some(child.indexed),
                        content: child.content,
                    },
                );
            }
        }
        Ok(items)
    }

    fn children(&self, at: &ResourceRef, field: &str) -> Result<Vec<(String, String)>, StoreError> {
        self.children_where(at, field, &[])
    }

    /// The item ids of the collection `field` of `at` the store's index holds to `filters`, and
    /// every item of it this store wrote and has not sent, as they read now, with each one's order
    /// key. The index has not seen those writes, so a caller narrowing by `filters` checks them
    /// again on what it is given.
    fn children_where(
        &self,
        at: &ResourceRef,
        field: &str,
        filters: &[Filter],
    ) -> Result<Vec<(String, String)>, StoreError> {
        let Some(item_types) = self.field(&at.ty, field).map(|entry| entry.items) else {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        };
        if item_types.is_empty() {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        }
        let kinds: Vec<String> = item_types
            .iter()
            .map(|ty| self.kinds_of_type(ty))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let holder = id_of(at);
        let shared = self.shares_holder(&at.ty, field);
        let stands_here = |fields: &BTreeMap<String, Cell>| {
            !shared
                || matches!(fields.get(STANDS_IN), Some(Cell::One(Value::Text(stands))) if stands == field)
        };
        let mut out: BTreeMap<String, (String, String)> = BTreeMap::new();
        for kind in &kinds {
            let mut after: Option<Cursor> = None;
            loop {
                let mut query = Query::of(kind.clone(), PAGE)
                    .filter(Filter::Is(PARENT.to_owned(), Value::Text(holder.clone())))
                    .after(after.take());
                query.filters.extend_from_slice(filters);
                let page = match self.command(Command::Query(query))? {
                    Reply::Page(page) => page,
                    other => return Err(backend(format!("a query answered {other:?}"))),
                };
                {
                    let mut cache = self.inner.cache.borrow_mut();
                    for (id, indexed) in &page.resources {
                        let order = indexed.order.clone().unwrap_or_default();
                        if stands_here(&indexed.fields) {
                            out.insert(id.clone(), (order, id.clone()));
                        }
                        cache
                            .entry(id.clone())
                            .or_insert_with(|| Some(Held::of(kind, indexed.clone())));
                    }
                }
                match page.next {
                    Some(next) if !page.resources.is_empty() => after = Some(next),
                    _ => break,
                }
            }
        }
        for (id, local) in self.inner.local.borrow().iter() {
            match local {
                Some(held)
                    if kinds.contains(&held.kind)
                        && held.parent.as_deref() == Some(&holder)
                        && stands_here(&held.fields) =>
                {
                    out.insert(
                        id.clone(),
                        (held.order.clone().unwrap_or_default(), id.clone()),
                    );
                }
                _ => {
                    out.remove(id);
                }
            }
        }
        let mut listed: Vec<(String, String)> = out.into_values().collect();
        listed.sort();
        Ok(listed)
    }

    /// The items of `kind` in the resource `holder` whose field held as `name` names `above`, as
    /// they read now, each with its order key: a page query of the store's index, with what this
    /// store wrote and has not sent read over it.
    fn children_by(
        &self,
        kind: &str,
        holder: &str,
        name: &str,
        above: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let names = Cell::One(Value::Text(above.to_owned()));
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        let mut after: Option<Cursor> = None;
        loop {
            let query = Query::of(kind.to_owned(), PAGE)
                .filter(Filter::Is(
                    PARENT.to_owned(),
                    Value::Text(holder.to_owned()),
                ))
                .filter(Filter::Is(name.to_owned(), Value::Text(above.to_owned())))
                .after(after.take());
            let page = match self.command(Command::Query(query))? {
                Reply::Page(page) => page,
                other => return Err(backend(format!("a query answered {other:?}"))),
            };
            {
                let mut cache = self.inner.cache.borrow_mut();
                for (id, indexed) in &page.resources {
                    out.insert(id.clone(), indexed.order.clone().unwrap_or_default());
                    cache
                        .entry(id.clone())
                        .or_insert_with(|| Some(Held::of(kind, indexed.clone())));
                }
            }
            match page.next {
                Some(next) if !page.resources.is_empty() => after = Some(next),
                _ => break,
            }
        }
        for (id, local) in self.inner.local.borrow().iter() {
            match local {
                Some(held)
                    if held.kind == kind
                        && held.parent.as_deref() == Some(holder)
                        && held.fields.get(name) == Some(&names) =>
                {
                    out.insert(id.clone(), held.order.clone().unwrap_or_default());
                }
                _ => {
                    out.remove(id);
                }
            }
        }
        Ok(out.into_iter().map(|(id, order)| (order, id)).collect())
    }

    /// Removes `id` of the type `ty`, and every item nested in it.
    fn remove_tree(&self, txn: Txn, at: &ResourceRef) -> Result<(), StoreError> {
        let collections: Vec<FieldId> = self
            .inner
            .kinds
            .borrow()
            .get(&at.ty)
            .map(|entry| {
                entry
                    .fields
                    .iter()
                    .filter(|(_, field)| !field.items.is_empty())
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for field in collections {
            for (_, id) in self.children(at, &field)? {
                let child = at.item(&item_of(&id), String::new());
                let ty = self.type_of_id(&child.id_string()).unwrap_or_default();
                self.remove_tree(txn, &at.item(&item_of(&id), ty))?;
            }
        }
        // The records its fields hold go with it.
        let held: Vec<(FieldId, TypeId)> = self
            .inner
            .kinds
            .borrow()
            .get(&at.ty)
            .map(|entry| {
                entry
                    .fields
                    .iter()
                    .filter_map(|(id, field)| Some((id.clone(), field.held.clone()?)))
                    .collect()
            })
            .unwrap_or_default();
        for (field, ty) in held {
            let child = at.item(&tilolang_runtime::held::held_id(at, &field), ty);
            if self.type_of_id(&child.id_string()).is_some() {
                self.remove_tree(txn, &child)?;
            }
        }
        let id = id_of(at);
        // The kind it is stored as: the one it publishes now, else the one an earlier version
        // stored it as.
        let stored = self.stored_kind(&at.ty, &id)?;
        // Its whole change: what a reader of its text, a search index, reads it whole for.
        self.touch(txn, at, ANY_FIELD, false)?;
        self.write(txn, &id, None, |dirty| {
            dirty.removed = true;
            dirty.put = false;
            dirty.edits.clear();
            dirty.remove = stored;
        })
    }

    /// The kind the store holds `id`, a resource of the type `ty`, as: the one it publishes now,
    /// else the one an earlier version stored it as. `None` where the store holds none.
    fn stored_kind(&self, ty: &str, id: &str) -> Result<Option<String>, StoreError> {
        for kind in self.kinds_of_type(ty)? {
            if self.stored(&kind, id)? {
                return Ok(Some(kind));
            }
        }
        Ok(None)
    }

    /// Whether the store holds `id` of `kind`, whatever was written here.
    fn stored(&self, kind: &str, id: &str) -> Result<bool, StoreError> {
        if let Some(cached) = self.inner.cache.borrow().get(id) {
            return Ok(cached.is_some());
        }
        let reply = self.command(Command::Get {
            kind: kind.to_owned(),
            id: id.to_owned(),
        })?;
        Ok(matches!(reply, Reply::Resource(Some(_))))
    }

    fn insert_key(
        &self,
        at: &ResourceRef,
        field: &str,
        index: Option<usize>,
        leaving: Option<&str>,
    ) -> Result<String, StoreError> {
        if index.is_none() && leaving.is_none() {
            return Ok(tilolang_runtime::place::key_between(
                self.last_key(at, field)?.as_deref(),
                None,
            ));
        }
        if let Some(index) = index
            && let Some(key) = self.indexed_key(at, field, index, leaving)?
        {
            return Ok(key);
        }
        let others: Vec<(String, String)> = self
            .children(at, field)?
            .into_iter()
            .filter(|(_, id)| Some(item_of(id).as_str()) != leaving)
            .collect();
        let index = index.unwrap_or(others.len()).min(others.len());
        let low = index
            .checked_sub(1)
            .and_then(|before| others.get(before))
            .map(|(key, _)| key.as_str());
        let high = others.get(index).map(|(key, _)| key.as_str());
        Ok(tilolang_runtime::place::key_between(low, high))
    }

    /// The order key of the place `index` among the items of the collection `field` of `at` other
    /// than `leaving`, from the store's index: the two items the place falls between are read,
    /// however many the collection holds. `None` where the index does not say where they stand
    /// now: items of more than one kind, or writes not sent yet that place items there.
    fn indexed_key(
        &self,
        at: &ResourceRef,
        field: &str,
        index: usize,
        leaving: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let Some(items) = self.field(&at.ty, field).map(|entry| entry.items) else {
            return Ok(None);
        };
        let [item_ty] = items.as_slice() else {
            return Ok(None);
        };
        let kinds = self.kinds_of_type(item_ty)?;
        let [kind] = kinds.as_slice() else {
            return Ok(None);
        };
        if self.places_written(kind, &id_of(at)) {
            return Ok(None);
        }
        let (listing, pages) = self.paged_pages(at, field, kind, Vec::new(), Order::default());
        let stands = match leaving {
            Some(item) => match listing.position(item) {
                Some(stands) => stands,
                None => return Ok(None),
            },
            None => None,
        };
        let others = listing.len().saturating_sub(usize::from(stands.is_some()));
        let index = index.min(others);
        // Where the place `i` of the others stands in the listing, which still holds `leaving`.
        let past = |i: usize| match stands {
            Some(stands) if stands <= i => i.saturating_add(1),
            _ => i,
        };
        // The two neighbours, each read with the few items around it: a place far down a long
        // collection reads no page of the items before it.
        let low = index.checked_sub(1).map(|before| pages.probe(past(before)));
        let high = (index < others).then(|| pages.probe(past(index)));
        // A place the listing could not read is told by reading every item.
        if matches!(low, Some(None)) || matches!(high, Some(None)) {
            return Ok(None);
        }
        let key = |id: Option<Option<Rc<str>>>| -> Result<Option<String>, StoreError> {
            match id.flatten() {
                Some(id) => Ok(self.held_at(&at.item(&id, item_ty.clone()))?.order),
                None => Ok(None),
            }
        };
        let (low, high) = (key(low)?, key(high)?);
        Ok(Some(tilolang_runtime::place::key_between(
            low.as_deref(),
            high.as_deref(),
        )))
    }

    /// The order key of the last item of the collection `field` of `at`: one item read, however
    /// many the collection holds, and none after a transaction put one at its end.
    fn last_key(&self, at: &ResourceRef, field: &str) -> Result<Option<String>, StoreError> {
        let holder = id_of(at);
        if let Some(open) = self.inner.txn.borrow().as_ref()
            && let Some(last) = open.last_keys.get(&(holder.clone(), field.to_owned()))
        {
            return Ok(Some(last.clone()));
        }
        let Some(item_types) = self.field(&at.ty, field).map(|entry| entry.items) else {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        };
        // A union's items are of more than one kind, so each page would miss the others'.
        if item_types.len() != 1 {
            return Ok(self
                .children(at, field)?
                .into_iter()
                .map(|(key, _)| key)
                .max());
        }
        let Some(item_ty) = item_types.first() else {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        };
        let kind = self.kind_of_type(item_ty)?;
        let local = self.inner.local.borrow();
        // What was written here and not sent stands in for what the store holds of it: the
        // store's last is the first of its items, newest first, that none of them overrides, so
        // one page as long as what is unsent reaches it.
        let query = Query::of(kind.clone(), local.len().saturating_add(1))
            .filter(Filter::Is(PARENT.to_owned(), Value::Text(holder.clone())))
            .order(Order::default().descending());
        let page = match self.command(Command::Query(query))? {
            Reply::Page(page) => page,
            other => return Err(backend(format!("a query answered {other:?}"))),
        };
        let mut last = page
            .resources
            .iter()
            .find(|(id, _)| !local.contains_key(id))
            .and_then(|(_, indexed)| indexed.order.clone());
        for held in local.values().flatten() {
            if held.kind == kind && held.parent.as_deref() == Some(&holder) && held.order > last {
                last.clone_from(&held.order);
            }
        }
        Ok(last)
    }

    /// Queues an outside effect of the document `at` names, in `txn`: what its capability
    /// carries out, with `payload`, held until `held_until` when that is given. The same entry
    /// (`key`) queued again keeps what became of it.
    fn push_effect(
        &self,
        txn: Txn,
        at: &ResourceRef,
        app: &str,
        capability: &str,
        payload: &Json,
        held_until: Option<&str>,
        key: Option<&str>,
    ) -> Result<(), StoreError> {
        let entry = key.map_or_else(|| self.fresh("effect-"), str::to_owned);
        let id = effect_id(&at.doc, &entry);
        if self.held(EFFECT, &id)?.is_some() {
            return Ok(());
        }
        let text = |value: &str| Cell::One(Value::Text(value.to_owned()));
        let mut fields = BTreeMap::new();
        fields.insert(OUT_DOC.to_owned(), text(&at.doc));
        fields.insert(OUT_ENTRY.to_owned(), text(&entry));
        fields.insert(OUT_APP.to_owned(), text(app));
        fields.insert(OUT_CAPABILITY.to_owned(), text(capability));
        fields.insert(OUT_PAYLOAD.to_owned(), text(&payload.to_string()));
        if let Some(held_until) = held_until {
            fields.insert(OUT_HELD_UNTIL.to_owned(), text(held_until));
        }
        fields.insert(OUT_OWNER.to_owned(), text(&self.inner.device.borrow()));
        fields.insert(OUT_SETTLED.to_owned(), Cell::One(Value::Bool(false)));
        let held = Held {
            kind: EFFECT.to_owned(),
            parent: Some(at.doc.clone()),
            order: None,
            fields,
            content: Some(serde_json::Map::new()),
        };
        self.write(txn, &id, Some(held), |dirty| dirty.put = true)?;
        self.touch_effects(txn, &at.doc)
    }

    /// What a write `m` of the field `name` of the resource `id` changes of links: the
    /// resources a reference field named and names now, and those a rich text mentioned and
    /// mentions now, whose links read again; a rich text's mentions are indexed beside it, so
    /// what mentions a resource is asked of the index. Whether the index entry changed.
    fn write_links(
        &self,
        txn: Txn,
        m: &Mutation,
        id: &str,
        name: &str,
        entry: Option<&kinds::FieldEntry>,
        held: &mut Held,
    ) -> Result<bool, StoreError> {
        let mut linked: BTreeSet<String> = BTreeSet::new();
        let mut put = false;
        if let Some(entry) = entry.filter(|entry| names_a_resource(&entry.ty))
            && let Mutation::SetRegister { value, .. } = m
        {
            if let Some(Cell::One(Value::Text(id))) = held.fields.get(name) {
                linked.insert(id.clone());
            }
            if let Some(Value::Text(id)) = self.inner.kinds.borrow().encode(&entry.ty, value) {
                linked.insert(id);
            }
        }
        if let Mutation::SetRich { value, .. } = m {
            let mentioned = |rich: &tilolang_runtime::rich::RichValue| -> Vec<String> {
                rich.references()
                    .iter()
                    .filter_map(|reference| {
                        tilolang_runtime::rich::resource_id(reference).map(str::to_owned)
                    })
                    .collect()
            };
            linked.extend(mentioned(value));
            self.content(held, id)?;
            if let Some(was) = held
                .content
                .as_ref()
                .and_then(|content| content.get(name))
                .and_then(|json| {
                    tilo_failure::ignored!(
                        "content that is not rich text is read as the field's plain value",
                        serde_json::from_value::<tilolang_runtime::rich::RichValue>(json.clone())
                            .ok()
                    )
                })
            {
                linked.extend(mentioned(&was));
            }
            // What the text mentions is indexed beside it, so what mentions a resource is asked
            // of the index rather than read out of every text.
            let mentions: BTreeSet<Value> = mentioned(value).into_iter().map(Value::Text).collect();
            let key = format!("{MENTIONS}{name}");
            let had = held.fields.contains_key(&key);
            if mentions.is_empty() {
                held.fields.remove(&key);
                put |= had;
            } else {
                held.fields.insert(key, Cell::Set(mentions));
                put = true;
            }
        }
        if !linked.is_empty() {
            self.touch_links(txn, linked.iter().map(String::as_str))?;
        }
        Ok(put)
    }

    /// Note what a write of `text` into the text field `field_id` (named `name`, `indexed` or
    /// kept as content) of `at` wrote ([`Self::take_wrote`]).
    fn note_text_written(
        &self,
        at: &ResourceRef,
        field_id: &str,
        name: &str,
        indexed: bool,
        held: &mut Held,
        text: &str,
    ) -> Result<(), StoreError> {
        let was = if indexed {
            match held.fields.get(name) {
                Some(Cell::One(Value::Text(was))) => was.clone(),
                _ => String::new(),
            }
        } else {
            self.content(held, &id_of(at))?;
            held.content
                .as_ref()
                .and_then(|content| content.get(name))
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        self.note_wrote(field_id, &at.id_string(), &was, text);
        Ok(())
    }

    /// The item `item` of `at`'s collection `field`.
    fn item_of(
        &self,
        at: &ResourceRef,
        field: &str,
        item: &str,
    ) -> Result<ResourceRef, StoreError> {
        if self
            .field(&at.ty, field)
            .is_none_or(|entry| entry.items.is_empty())
        {
            return Err(StoreError::Invalid(format!("{field} is not a collection")));
        }
        let ty = self
            .type_of_id(&at.item(item, String::new()).id_string())
            .unwrap_or_default();
        Ok(at.item(item, ty))
    }

    /// Makes `item` the item `item_id` of `at`'s `field`: at `index` among the others, or, for a
    /// record the field holds, as the field's one item, which has no order among others.
    fn insert(
        &self,
        txn: Txn,
        at: &ResourceRef,
        field: &str,
        item: &NewResource,
        index: Option<usize>,
        item_id: &str,
    ) -> Result<(), StoreError> {
        let id = id_of(at);
        let holds = self
            .field(&at.ty, field)
            .is_some_and(|entry| entry.held.is_some());
        let order = if holds {
            None
        } else {
            let order = self.insert_key(at, field, index, None)?;
            let collection = (id.clone(), field.to_owned());
            self.txn_mut(txn, |state| match index {
                None => state.last_keys.insert(collection, order.clone()),
                Some(_) => state.last_keys.remove(&collection),
            })?;
            Some(order)
        };
        let mut held = self.new_held(item, Some(id), order)?;
        self.stand_in(&mut held, &at.ty, field);
        let edits = self.initial_edits(&item.ty, &mut held);
        let child = at.item(item_id, item.ty.clone());
        self.write(txn, &id_of(&child), Some(held), |dirty| {
            dirty.put = true;
            dirty.edits.extend(edits);
        })?;
        self.inner.made.borrow_mut().push(child.clone());
        self.inner.wrote.borrow_mut().made = true;
        self.touch(txn, at, field, !holds)?;
        self.touch(txn, &child, ANY_FIELD, false)?;
        Ok(())
    }

    /// [`Store::apply`] of a mutation of a collection or an effect entry: `None` for a write of a
    /// field.
    fn apply_structure(
        &self,
        txn: Txn,
        at: &ResourceRef,
        m: &Mutation,
    ) -> Result<Option<Applied>, StoreError> {
        let id = id_of(at);
        match m {
            Mutation::CollInsert {
                field,
                item,
                index,
                id: given,
            } => {
                let item_id = given
                    .clone()
                    .unwrap_or_else(|| self.fresh(&format!("{}-", short(&item.ty))));
                self.insert(txn, at, field, item, *index, &item_id)?;
                Ok(Some(Applied::Item(item_id)))
            }
            Mutation::CollRemove { field, item } => {
                let child = self.item_of(at, field, item)?;
                self.remove_tree(txn, &child)?;
                self.touch(txn, at, field, true)?;
                self.touch(txn, &child, ANY_FIELD, false)?;
                Ok(Some(Applied::Done))
            }
            Mutation::CollMove { field, item, index } => {
                let child = self.item_of(at, field, item)?;
                let mut held = self.held_at(&child)?;
                let old = held.parent.clone();
                let collection = (id.clone(), field.clone());
                self.txn_mut(txn, |state| state.last_keys.remove(&collection))?;
                held.order = Some(self.insert_key(at, field, Some(*index), Some(item))?);
                held.parent = Some(id.clone());
                self.stand_in(&mut held, &at.ty, field);
                self.write(txn, &id_of(&child), Some(held), |dirty| dirty.put = true)?;
                self.touch(txn, at, field, true)?;
                if let Some(old) = old.filter(|old| *old != id) {
                    // The collection it left is a collection of the resource it was in.
                    let doc = tilolang_runtime::store::doc_of(&old).to_owned();
                    let key = FieldKey {
                        resource: old,
                        field: field.clone(),
                    };
                    self.txn_mut(txn, |state| {
                        state
                            .collections
                            .entry(doc.clone())
                            .or_default()
                            .insert(key.clone());
                        state.touched.entry(doc).or_default().insert(key);
                    })?;
                }
                Ok(Some(Applied::Done))
            }
            Mutation::EffectPush {
                app,
                capability,
                payload,
                held_until,
                key,
            } => {
                self.push_effect(
                    txn,
                    at,
                    app,
                    capability,
                    payload,
                    held_until.as_deref(),
                    key.as_deref(),
                )?;
                Ok(Some(Applied::Done))
            }
            Mutation::EffectWithdraw => {
                self.withdraw_in(txn, &at.doc, "")?;
                self.touch_effects(txn, &at.doc)?;
                Ok(Some(Applied::Done))
            }
            _ => Ok(None),
        }
    }

    fn apply_to(&self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        if let Some(applied) = self.apply_structure(txn, at, m)? {
            return Ok(applied);
        }
        let id = id_of(at);
        let Some(field_id) = field_written(m) else {
            return Ok(Applied::Done);
        };
        let entry = self.field(&at.ty, &field_id);
        let name = entry
            .as_ref()
            .map_or_else(|| field_id.clone(), |entry| entry.name.clone());
        let indexed = entry.as_ref().is_some_and(|entry| self.indexed(entry));
        let mut held = self.held_at(at)?;
        let mut put = false;
        let mut edits = Vec::new();
        put |= self.write_links(txn, m, &id, &name, entry.as_ref(), &mut held)?;
        if let Mutation::SetText { text, .. } = m {
            self.note_text_written(at, &field_id, &name, indexed, &mut held, text)?;
        }
        match m {
            Mutation::SetRegister { value, .. } if indexed => {
                let ty = entry.as_ref().map_or(Ty::Unit, |entry| entry.ty.clone());
                let encoded = self.inner.kinds.borrow().encode(&ty, value);
                put = true;
                if let Some(value) = encoded {
                    held.fields.insert(name.clone(), Cell::One(value));
                } else {
                    held.fields.remove(&name);
                    edits.push(Edit::Set {
                        field: name.clone(),
                        value: field_json(&FieldValue::Scalar(value.clone())),
                    });
                }
            }
            Mutation::SetText { text, .. } if indexed => {
                held.fields
                    .insert(name.clone(), Cell::One(Value::Text(text.clone())));
                put = true;
            }
            Mutation::SetRegister { value, .. } => edits.push(Edit::Set {
                field: name.clone(),
                value: field_json(&FieldValue::Scalar(value.clone())),
            }),
            Mutation::SetText { text, .. } => edits.push(Edit::Set {
                field: name.clone(),
                value: Json::String(text.clone()),
            }),
            Mutation::SetRich { value, .. } => edits.push(Edit::Set {
                field: name.clone(),
                value: serde_json::to_value(value).map_err(backend)?,
            }),
            Mutation::SetBytes { data, .. } => edits.push(Edit::Set {
                field: name.clone(),
                value: data.to_json(),
            }),
            Mutation::AddCounter { delta, .. } => edits.push(Edit::Add {
                field: name.clone(),
                by: *delta,
            }),
            Mutation::SetAdd { key, .. } | Mutation::SetRemove { key, .. } => {
                let mut members = match held.fields.remove(&name) {
                    Some(Cell::Set(members)) => members,
                    _ => BTreeSet::new(),
                };
                if matches!(m, Mutation::SetAdd { .. }) {
                    members.insert(Kinds::member(key));
                } else {
                    members.remove(&Kinds::member(key));
                }
                held.fields.insert(name.clone(), Cell::Set(members));
                put = true;
            }
            Mutation::MapPut { key, value, .. } => edits.push(Edit::MapPut {
                field: name.clone(),
                key: key.clone(),
                value: field_json(&FieldValue::Scalar(value.clone())),
            }),
            Mutation::MapRemove { key, .. } => edits.push(Edit::MapDelete {
                field: name.clone(),
                key: key.clone(),
            }),
            _ => {}
        }
        if !edits.is_empty() {
            self.content(&mut held, &id)?;
            if let Some(content) = held.content.as_mut() {
                for edit in &edits {
                    apply_edit(content, edit);
                }
            }
        }
        self.write(txn, &id, Some(held), |dirty| {
            dirty.put |= put;
            dirty.edits.extend(edits);
        })?;
        // The item's field, and nothing of the collection it is in: an edit neither adds, removes
        // nor moves an item, and the views that filter or order the collection by this field
        // read it again from the field itself (`Live::reach_item`). Naming the collection too
        // would read its whole listing again, for every view that saw it, at every keystroke.
        self.touch(txn, at, &field_id, false)?;
        Ok(Applied::Done)
    }

    /// The effect entries the store holds that hold to `filters`, by id.
    fn effect_rows(&self, filters: Vec<Filter>) -> Vec<(String, BTreeMap<String, Cell>)> {
        let mut query = Query::of(EFFECT, PAGE);
        query.filters = filters;
        self.query_rows(query)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, indexed)| (id, indexed.fields))
            .collect()
    }

    /// Everything `query` lists, all pages: asked of the store once, then answered from what
    /// was read until a write or a moved record forgets it.
    fn query_rows(&self, query: Query) -> Result<Rows, StoreError> {
        let key = serde_json::to_string(&query).map_err(|error| backend(error.to_string()))?;
        if let Some(rows) = self.inner.queried.borrow().get(&key) {
            return Ok(rows.clone());
        }
        let mut rows = Vec::new();
        let mut query = query;
        loop {
            let page = match self.command(Command::Query(query.clone()))? {
                Reply::Page(page) => page,
                other => return Err(backend(format!("a query answered {other:?}"))),
            };
            let shown = !page.resources.is_empty();
            rows.extend(page.resources);
            match page.next {
                Some(next) if shown => query.after = Some(next),
                _ => break,
            }
        }
        self.inner.queried.borrow_mut().insert(key, rows.clone());
        Ok(rows)
    }

    /// Note that the commit of `txn` changed what the document `doc` has waiting to go out:
    /// whoever carries out a device's outside effects, and every reader of how the document's
    /// delivery stands, hears of it with the rest of the commit.
    fn touch_effects(&self, txn: Txn, doc: &str) -> Result<(), StoreError> {
        let key = FieldKey {
            resource: doc.to_owned(),
            field: EFFECT.to_owned(),
        };
        self.txn_mut(txn, |state| {
            state.touched.entry(doc.to_owned()).or_default().insert(key);
        })
    }

    /// Settles every effect entry of the document `doc` not settled yet as withdrawn, for
    /// `reason`, in the open transaction. How many it settled.
    fn withdraw_in(&self, txn: Txn, doc: &str, reason: &str) -> Result<usize, StoreError> {
        let rows = self.effect_rows(vec![
            Filter::Is(OUT_DOC.to_owned(), Value::Text(doc.to_owned())),
            Filter::Is(OUT_SETTLED.to_owned(), Value::Bool(false)),
        ]);
        let mut withdrawn = 0_usize;
        for (id, _) in rows {
            let Some(mut held) = self.held(EFFECT, &id)? else {
                continue;
            };
            if matches!(
                held.fields.get(OUT_SETTLED),
                Some(Cell::One(Value::Bool(true)))
            ) {
                continue;
            }
            settle_fields(&mut held.fields, &Settled::Withdrawn(reason.to_owned()));
            self.write(txn, &id, Some(held), |dirty| dirty.put = true)?;
            withdrawn = withdrawn.saturating_add(1);
        }
        Ok(withdrawn)
    }

    /// Writes what `change` makes of the effect entry `entry`'s fields, as the change `note`.
    fn effect_write(
        &self,
        entry: &EffectEntry,
        note: &ChangeNote,
        change: impl FnOnce(&mut BTreeMap<String, Cell>),
    ) -> Result<(), StoreError> {
        let id = effect_id(&entry.doc, &entry.id);
        let mut store = self.clone();
        let txn = store.begin()?;
        let written = match self.held(EFFECT, &id) {
            Ok(Some(mut held)) => {
                change(&mut held.fields);
                self.write(txn, &id, Some(held), |dirty| dirty.put = true)
            }
            Ok(None) => Err(not_found(entry.id.clone())),
            Err(error) => Err(error),
        };
        if let Err(error) = written {
            store.rollback(txn);
            return Err(error);
        }
        store.commit(txn, note)
    }

    /// Sets the device whose outside effects this store carries out: what it queues from now
    /// on is its own to carry out.
    pub fn set_device(&self, device: &str) {
        device.clone_into(&mut self.inner.device.borrow_mut());
    }

    /// What the clock reads for a query that compares a field with it, from the host: the
    /// instant a view that asked wakes at ([`Store::next_clock_change`]).
    pub fn set_now(&self, now: impl Fn() -> String + 'static) {
        *self.inner.now.borrow_mut() = Rc::new(now);
    }

    /// Sets the run what is written from now on is a step of, `None` for none: the changes of
    /// an agent's session say they are its steps, so a mark names the session and a review of
    /// it lists what it did.
    pub fn set_run(&self, run: Option<tilolang_runtime::store::Run>) {
        *self.inner.run.borrow_mut() = run;
    }

    /// The writes made from now on serve the invocation `invocation` (none: their own), and are
    /// stamped with it.
    pub fn serve(&self, invocation: Option<String>) {
        *self.inner.serving.borrow_mut() = invocation;
    }

    /// The invocation the writes made now serve ([`Self::serve`]), if any.
    #[must_use]
    pub fn serving(&self) -> Option<String> {
        self.inner.serving.borrow().clone()
    }

    /// The writes made from now on are made for the actor `invoker` (none: the person at this
    /// device), as a page's writes are made for the member who reads it.
    pub fn serve_invoker(&self, invoker: Option<String>) {
        *self.inner.serving_invoker.borrow_mut() = invoker;
    }

    /// The writes made from now on are made for `person`, whom the invoker acts for, as
    /// `acting` (how it acts: handed its person's session, or under a key of its own), until the
    /// guard drops: the records an agent makes are that person's, so a type that names
    /// `readers: author` seals them to them and their devices, and each change records how the
    /// agent acted. What was in place before is restored, so a call made while another acts for
    /// someone leaves that one in place.
    #[must_use]
    pub fn acting_for(
        &self,
        person: Option<String>,
        acting: Option<tilo_actions::Acting>,
    ) -> ActingFor {
        let previous = std::mem::replace(
            &mut *self.inner.serving_acting.borrow_mut(),
            Acted { person, acting },
        );
        ActingFor {
            inner: Rc::clone(&self.inner),
            previous,
        }
    }

    /// The person the writes made now are made for, and how their agent acted, as each change
    /// records them.
    fn acted_for(&self) -> (Option<String>, Option<Box<tilo_actions::ActionContext>>) {
        let acted = self.inner.serving_acting.borrow();
        let context = acted
            .acting
            .as_ref()
            .filter(|acting| acting.agent_id().is_some())
            .map(|acting| {
                Box::new(tilo_actions::ActionContext {
                    invoker: Some(tilo_actions::ActionInvoker::admitted(acting)),
                    ..tilo_actions::ActionContext::default()
                })
            });
        (acted.person.clone(), context)
    }

    /// The context a change's stamp carries: how an agent acted for its person
    /// ([`Self::acted_for`]), and the members who alone read the change where what it touched
    /// says who reads it ([`ChangeNote::readers`]), as history lists it to them alone
    /// ([`tilo_actions::READERS_FACET`]). `None` for a change every member reads that records
    /// nothing else.
    fn change_context(
        acting: Option<Box<tilo_actions::ActionContext>>,
        readers: Option<&[tilo_member_types::MemberId]>,
    ) -> Option<Box<tilo_actions::ActionContext>> {
        let Some(readers) = readers else {
            return acting;
        };
        let mut context =
            acting.map_or_else(tilo_actions::ActionContext::default, |context| *context);
        context
            .facets
            .retain(|facet| facet.key != tilo_actions::READERS_FACET);
        context.facets.push(tilo_actions::ActionFacet::new(
            tilo_actions::READERS_FACET,
            tilo_actions::contract::readers_facet(readers),
        ));
        Some(Box::new(context))
    }

    /// The writes made from now on serve the call `call` as its caller signed it (none: none),
    /// and each change keeps it.
    pub fn serve_signed(&self, call: Option<Box<tilo_actions::remote::SignedCall>>) {
        *self.inner.serving_signed.borrow_mut() = call;
    }

    /// The resources made here since this was last asked, oldest first: what an action that
    /// stopped at a step a person must answer made before it stopped. A write taken back since
    /// may have made one that is no more.
    pub fn take_made(&self) -> Vec<ResourceRef> {
        std::mem::take(&mut *self.inner.made.borrow_mut())
    }

    /// The resources made here since [`Self::take_made`] last took them, without taking them.
    #[must_use]
    pub fn made_since(&self) -> Vec<ResourceRef> {
        self.inner.made.borrow().clone()
    }

    /// Where the writes since this was last asked put text, as the anchor an invocation names
    /// text by: the plain text field first written (its id), in the resource of `app` holding
    /// it as the window publishes it (`tl:app:document/item`), over the characters written
    /// there. A window draws them in the writer's colour. `None` where no text was written, or
    /// a resource was made: an action that made one names what it made.
    pub fn take_wrote(&self, app: &str) -> Option<tilo_actions::InteractionAnchor> {
        let wrote = std::mem::take(&mut *self.inner.wrote.borrow_mut());
        if wrote.made {
            return None;
        }
        let (region, block, start, end) = wrote.field?;
        Some(tilo_actions::InteractionAnchor::Text {
            region,
            ranges: vec![tilo_actions::InteractionTextRange {
                block: crate::naming::resource_id(app, &block),
                start,
                end,
            }],
        })
    }

    /// Note that the text `field` of `resource` read `was` and now reads `now`
    /// ([`Self::take_wrote`]). A later write in the same text widens what was written there.
    fn note_wrote(&self, field: &str, resource: &str, was: &str, now: &str) {
        if was == now {
            return;
        }
        let (start, end) = written_span(was, now);
        let mut wrote = self.inner.wrote.borrow_mut();
        match &mut wrote.field {
            None => wrote.field = Some((field.to_owned(), resource.to_owned(), start, end)),
            Some((region, block, from, to)) if region == field && block == resource => {
                *from = (*from).min(start);
                *to = (*to).max(end).min(now.chars().count() as u64);
            }
            Some(_) => {}
        }
    }

    /// A new id: the moment it is minted, then random digits. Ids minted here sort in the order
    /// they were made, so what lists resources by id lists them oldest first, as a person made
    /// them; ids minted on two devices at once still never meet.
    fn fresh(&self, prefix: &str) -> String {
        let minted = tilo_clock::unix_now_ms().max(self.inner.minted.get().saturating_add(1));
        self.inner.minted.set(minted);
        let random = uuid::Uuid::new_v4().simple().to_string();
        format!(
            "{prefix}{minted:012x}{}",
            random.get(12..).unwrap_or_default()
        )
    }

    /// The moment a write from here is stamped with: the second the change says it was made in
    /// (else the second the host's clock reads now), and within it the milliseconds the device's
    /// clock read as it wrote, and then a count of the writes made here in the same millisecond.
    /// Two writers' changes order as the times they say do, and as they were written within one
    /// second; the writes of one writer in the order they were made. A write that names no time
    /// (a record the host makes, an undo) is dated by the same clock the app's own writes are,
    /// so it never stands before or after them by a clock they do not read.
    fn next_moment(&self, at: Option<&str>) -> u64 {
        let host_now = at.is_none().then(|| (self.inner.now.borrow())());
        let second = at
            .or(host_now.as_deref())
            .and_then(|at| {
                tilo_failure::ignored!(
                    "a write with no readable time is ordered by this device's clock",
                    at.parse::<jiff::Timestamp>().ok()
                )
            })
            .and_then(|at| u64::try_from(at.as_second()).ok())
            .unwrap_or_else(|| tilo_clock::unix_now_ms() / 1000);
        let base = second.saturating_mul(PER_SECOND);
        // The device's milliseconds, which a clock held still (a test's, a scenario's) leaves
        // running: what was written later there stands later.
        let within = (tilo_clock::unix_now_ms() % 1_000_000).saturating_mul(PER_MILLISECOND);
        let mut moment = base.saturating_add(within);
        let last = self.inner.moment.get();
        if last >= base && last < base.saturating_add(PER_SECOND) {
            moment = moment.max(last.saturating_add(1));
        }
        self.inner.moment.set(moment);
        moment
    }

    /// The stamp a commit of `note` carries: the action, who invoked it and when, and what the
    /// versions it makes say of it on every replica (what it was made from, the run it is a
    /// step of, who proposed it).
    fn stamp(&self, note: &ChangeNote, migration: bool) -> Stamp {
        let burst = self.burst_invocation(note);
        let (acted_for, acting) = self.acted_for();
        let context = Self::change_context(acting, note.readers.as_deref());
        // A write made while serving an invocation is that invocation's, whatever run of the
        // runtime made it: the caller's id is what history and the workspace know it by.
        let invocation = self
            .inner
            .serving
            .borrow()
            .clone()
            .or_else(|| note.invocation.clone())
            .or_else(|| {
                self.inner
                    .runs
                    .borrow()
                    .first()
                    .map(|(_, _, invocation)| invocation.clone())
            })
            .or(burst)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let actor_text = Stamp::by;
        let from = note.from.as_ref().and_then(|from| {
            let ty = if from.ty.is_empty() {
                self.type_of_id(&id_of(from))?
            } else {
                from.ty.clone()
            };
            // A resource no package here declares (an event, another app's record) is named by
            // the kind its reference carries, so what was made from it still reads so.
            let kind = tilo_failure::ignored!(
                "a type no package here loads is a kind of its own",
                self.kind_of_type(&ty)
            )
            .unwrap_or(ty);
            Some(tilo_resource_store::Origin {
                kind,
                id: id_of(from),
            })
        });
        // A scenario's writes are the package's seed data, under the verb every store admits
        // for it.
        // What opening a resource does is the platform's run, under the verb every store admits
        // for it.
        let verb = {
            let kinds = self.inner.kinds.borrow();
            if kinds.is_scenario(&note.action) {
                crate::action::SCENARIO_ACTION.to_owned()
            } else if kinds.is_opened(&note.action) {
                crate::action::OPENED_ACTION.to_owned()
            } else {
                published_verb(note)
            }
        };
        Stamp {
            verb,
            // A step of another action says which action was invoked.
            invoked: note.invoked.clone().unwrap_or_default(),
            invoker: note
                .invoker
                .as_ref()
                .map(|invoker| {
                    // A migration is the platform bringing a package's document up to date on
                    // this device, not an action someone invoked: its change is the person
                    // whose device runs it, as the platform's other own writes are, so their
                    // undo takes the carrying back as one step.
                    match invoker {
                        Actor::App(_) if migration => self
                            .inner
                            .writer
                            .borrow()
                            .as_ref()
                            .map_or_else(|| actor_text(invoker), actor_text),
                        _ => actor_text(invoker),
                    }
                })
                // A write a page's reader made is theirs, not the person at this device's.
                .or_else(|| self.inner.serving_invoker.borrow().clone())
                .unwrap_or_else(|| {
                    PLATFORM_WRITES
                        .contains(&note.action.as_str())
                        .then(|| self.inner.writer.borrow().as_ref().map(actor_text))
                        .flatten()
                        .unwrap_or_default()
                }),
            acted_for,
            invocation,
            membership: Vec::new(),
            undoes: None,
            at: note
                .at
                .clone()
                .unwrap_or_else(|| (self.inner.now.borrow())()),
            from,
            group: note
                .group
                .clone()
                .or_else(|| self.inner.run.borrow().clone())
                .map(|run| tilo_resource_store::Group {
                    id: run.id,
                    label: run.title,
                }),
            suggested_by: String::new(),
            // The arguments of an action someone invoked, never a bound control's edit or the
            // platform's own write: those carry what the document already holds, or a mark.
            args: if note.action.starts_with('$') {
                Vec::new()
            } else {
                note.args.as_array().cloned().unwrap_or_default()
            },
            moment: self.next_moment(note.at.as_deref()),
            signed: self.inner.serving_signed.borrow().clone(),
            context,
            // The store names the call by digest and keeps the parts of its argument each
            // change holds; the workspace service names the device a write was made on, as it
            // names the invoker it certifies: the app's own word for either is never taken.
            ..Stamp::default()
        }
    }

    /// The version `held` is of the resource `at`, as the runtime reads it: the action its
    /// stamp names, and what the resource it made was made from.
    fn version_of(
        &self,
        at: &str,
        held: collab_types::history::Version,
        entry: Option<&TypeEntry>,
    ) -> Version {
        let stamp = held.stamp.unwrap_or_default();
        let kind = match held.kind {
            collab_types::history::VersionKind::Made => VersionKind::Created,
            collab_types::history::VersionKind::Changed => VersionKind::Changed,
            collab_types::history::VersionKind::Removed => VersionKind::Removed,
        };
        let field_id = |name: &str| -> FieldId {
            entry
                .and_then(|entry| {
                    entry
                        .fields
                        .iter()
                        .find(|(_, field)| field.name == name)
                        .map(|(id, _)| id.clone())
                })
                .unwrap_or_else(|| name.to_owned())
        };
        let from = stamp.from.as_ref().and_then(|from| {
            let ty = self
                .entry_of_kind(&from.kind)
                .map_or_else(|| from.kind.clone(), |entry| entry.id);
            ResourceRef::parse_id(&from.id, ty)
        });
        let left = held
            .parent
            .as_ref()
            .and_then(|parent| self.left_of(at, parent, entry, &held.change));
        // A change of the resources nested in this one changed the collections that hold them.
        let mut fields: Vec<FieldId> = held.fields.iter().map(|name| field_id(name)).collect();
        // A rich text is held as the resource's content, which names no field of the index: a
        // body edit is a change of the content and of no field the store records. The type's
        // rich text fields are what it may have written, and what each held before and after
        // the change says which one did (the runtime reads them). Without this a body edit is
        // a change of no field, and history has nothing to say of it but the action's name.
        if held.content
            && let Some(entry) = entry
        {
            for (id, field) in &entry.fields {
                if field.kind == FieldKind::RichText && !fields.contains(id) {
                    fields.push(id.clone());
                }
            }
        }
        let mut placed = Vec::new();
        for nested in &held.nested {
            let Some(child) = self.entry_of_kind(&nested.kind) else {
                continue;
            };
            if let Some(field) = entry.and_then(|entry| {
                entry
                    .fields
                    .iter()
                    .find(|(_, field)| field.items.contains(&child.id))
                    .map(|(id, _)| id.clone())
            }) && !fields.contains(&field)
            {
                fields.push(field);
            }
            if nested.made {
                placed.push(nested.id.clone());
            }
        }
        let who = stamp.invoked_by();
        let acted_for = stamp.acted_for.clone();
        let named = (stamp.verb == Stamp::CHECKPOINT_VERB)
            .then(|| stamp.args.first()?.as_str().map(str::to_owned))
            .flatten();
        // A step that took changes back is History's, not an action's; a restore keeps the
        // name history says it by, which no `$` names to the app.
        let action = if stamp.undoes.is_some() {
            tilolang_runtime::store::STEP_ACTION.to_owned()
        } else if stamp.verb == Stamp::RESTORE_VERB {
            "restore".to_owned()
        } else {
            stamp.verb
        };
        Version {
            who,
            at: stamp.at,
            action,
            run: stamp.group.map(|group| tilolang_runtime::store::Run {
                id: group.id,
                title: group.label,
            }),
            kind,
            fields,
            placed,
            resource: at.to_owned(),
            change: held.change,
            op: held.op,
            left,
            from: (kind == VersionKind::Created).then_some(from).flatten(),
            suggested_by: tilo_failure::ignored!(
                "a write no one suggested names no one",
                serde_json::from_str::<Actor>(&stamp.suggested_by).ok()
            ),
            count: 1,
            gesture: Some(stamp.invocation.clone()),
            invocation: Some(stamp.invocation),
            acted_for,
            named,
        }
    }

    /// One end of the history of `at`, as the store answers it.
    fn read_version_end(
        &self,
        at: &ResourceRef,
        last: bool,
    ) -> Result<Option<Version>, StoreError> {
        let at = &*self.typed(at);
        let Ok(kind) = self.kind_of_type(&at.ty) else {
            return Ok(None);
        };
        let entry = self.entry_of_kind(&kind);
        let Reply::Versions(held) = self.command(Command::VersionEnd {
            kind,
            id: id_of(at),
            last,
        })?
        else {
            return Err(backend("a read of a version answered no versions"));
        };
        let end = self.versions_of(&id_of(at), held, entry.as_ref()).pop();
        if !last {
            return Ok(end);
        }
        let made = self
            .made_from(at)?
            .into_iter()
            .max_by(|a, b| a.at.cmp(&b.at));
        Ok(match (end, made) {
            (Some(end), Some(made)) if made.at >= end.at => Some(made),
            (end, made) => end.or(made),
        })
    }

    /// The versions that made what was made from `at`, as changes of `at` of none of its
    /// fields, each the version of what it made.
    fn made_from(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        let Reply::MadeFrom(mut made) = self.command(Command::MadeFrom {
            ids: vec![id_of(at)],
        })?
        else {
            return Ok(Vec::new());
        };
        let made = made.pop().unwrap_or_default();
        let mut out = Vec::with_capacity(made.len());
        for (kind, id) in made {
            let Some(entry) = self.entry_of_kind(&kind) else {
                continue;
            };
            let Reply::Versions(held) = self.command(Command::Versions {
                kind,
                id: id.clone(),
            })?
            else {
                continue;
            };
            let Some(mut making) = self
                .versions_of(&id, held, Some(&entry))
                .into_iter()
                .find(|version| version.kind == VersionKind::Created)
            else {
                continue;
            };
            making.kind = VersionKind::Changed;
            making.fields = Vec::new();
            making.placed = Vec::new();
            making.left = None;
            out.push(making);
        }
        Ok(out)
    }

    /// Where the removed resource `at` stood when it was nested in `parent`: the collection of
    /// `parent` that holds resources of its type.
    fn left_of(
        &self,
        at: &str,
        parent: &str,
        entry: Option<&TypeEntry>,
        change: &str,
    ) -> Option<tilolang_runtime::store::Left> {
        let ty = entry.map(|entry| entry.id.clone())?;
        let holder_ty = self.type_of_id(parent)?;
        let fields: Vec<FieldId> = self
            .inner
            .kinds
            .borrow()
            .get(&holder_ty)?
            .fields
            .iter()
            .filter(|(_, field)| field.items.contains(&ty))
            .map(|(id, _)| id.clone())
            .collect();
        // Where the holder has two collections of its type, the resource said which it stood
        // in just before the change that removed it.
        let field = match fields.as_slice() {
            [] => return None,
            [only] => only.clone(),
            [first, ..] => {
                let stood = match self.command(Command::At {
                    kind: self.kind_of_type(&ty).ok()?,
                    id: at.to_owned(),
                    change: change.to_owned(),
                    after: false,
                }) {
                    Ok(Reply::At(Some((Some(indexed), _)))) => {
                        match indexed.fields.get(STANDS_IN) {
                            Some(Cell::One(Value::Text(stood))) => Some(stood.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                stood
                    .filter(|stood| fields.contains(stood))
                    .unwrap_or_else(|| first.clone())
            }
        };
        Some(tilolang_runtime::store::Left {
            holder: parent.to_owned(),
            field,
            ty: Some(ty),
        })
    }

    /// What makes two changes of the resource one act: the invocation and moment they name,
    /// without what each keeps of its call. That is its own document's — the first change of a
    /// call in a document keeps the call and the parts of its argument that document holds, and
    /// the call's other changes there name it — so two changes of one commit would otherwise read
    /// as two acts.
    fn act_identity(stamp: &Stamp) -> String {
        let mut act = stamp.clone();
        act.acted_for = None;
        act.args = Vec::new();
        act.parts = None;
        act.signed = None;
        act.call = None;
        if let Some(context) = act.context.as_deref_mut() {
            context.signed = None;
        }
        serde_json::to_string(&act).unwrap_or_default()
    }

    /// The versions the store holds of the resource `at`, as the runtime reads them: a commit
    /// that changed both the resource's indexed fields and its content wrote a change to each
    /// of its two documents, and is one version, as it is one act. Oldest first, its making
    /// first.
    fn versions_of(
        &self,
        at: &str,
        held: Vec<collab_types::history::Version>,
        entry: Option<&TypeEntry>,
    ) -> Vec<Version> {
        let mut versions: Vec<Version> = Vec::with_capacity(held.len());
        // Per version: whether it holds a change of the resource's own document and one of its
        // content's.
        let mut parts: Vec<(bool, bool)> = Vec::with_capacity(held.len());
        // A change taken back stays in the resource's history but is the last change of none of
        // its fields, and the step that took it back changed none either: it put back what they
        // held. An undo taken back in turn takes nothing back.
        let undos: Vec<(&str, &str)> = held
            .iter()
            .filter_map(|held| {
                let stamp = held.stamp.as_ref()?;
                Some((stamp.invocation.as_str(), stamp.undoes.as_deref()?))
            })
            .collect();
        let undone: BTreeSet<String> = undos
            .iter()
            .filter(|(undo, _)| !undos.iter().any(|(_, taken)| taken == undo))
            .map(|(_, taken)| (*taken).to_owned())
            .collect();
        let held: Vec<collab_types::history::Version> = held
            .into_iter()
            .map(|mut held| {
                if held.stamp.as_ref().is_some_and(|stamp| {
                    stamp.undoes.is_some() || undone.contains(&stamp.invocation)
                }) {
                    held.fields = Vec::new();
                }
                held
            })
            .collect();
        // The versions so far of each act, by the invocation and moment its changes name: a
        // commit's changes are one act, whoever each of its records was made for
        // ([`Store::made_for`]) and whatever each keeps of the call. Looked up rather than
        // compared with every version before, so a history of thousands reads in one pass.
        let mut of_act: HashMap<String, Vec<usize>> = HashMap::new();
        for held in held {
            let stamp = held.stamp.clone();
            let content = held.content;
            let of_nested = !held.nested.is_empty();
            let version = self.version_of(at, held, entry);
            let act = stamp.as_ref().map(Self::act_identity);
            // A commit's two changes name one act, and are each the first of their document not
            // paired yet: two commits of one action, each a version, are two of either. A
            // change of the resources nested in this one joins the latest version of its act.
            let same = act.as_ref().and_then(|act| {
                let earlier = of_act.get(act)?;
                if of_nested {
                    return earlier.last().copied();
                }
                earlier.iter().copied().find(|at| {
                    parts.get(*at).is_some_and(
                        |(own, of_content)| {
                            if content { !of_content } else { !own }
                        },
                    )
                })
            });
            let Some(same) = same else {
                if let Some(act) = act {
                    of_act.entry(act).or_default().push(versions.len());
                }
                parts.push((!content && !of_nested, content));
                versions.push(version);
                continue;
            };
            if !of_nested && let Some(part) = parts.get_mut(same) {
                if content {
                    part.1 = true;
                } else {
                    part.0 = true;
                }
            }
            let Some(into) = versions.get_mut(same) else {
                continue;
            };
            // The act's kind is the strongest of its parts', and it changed what any part did.
            if version.kind == VersionKind::Created || into.kind == VersionKind::Changed {
                into.kind = version.kind;
            }
            for field in version.fields {
                if !into.fields.contains(&field) {
                    into.fields.push(field);
                }
            }
            into.from = into.from.take().or(version.from);
            into.acted_for = into.acted_for.take().or(version.acted_for);
            into.left = into.left.take().or(version.left);
            for placed in version.placed {
                if !into.placed.contains(&placed) {
                    into.placed.push(placed);
                }
            }
            // The change that placed or named the resource stands for the act, not its content's.
            if !content && !of_nested {
                into.change = version.change;
                into.op = version.op;
            }
        }
        // The store reads them in the order they were written, across the resource's documents;
        // a resource's making is its first version, whichever document's change it is read of.
        if let Some(made) = versions
            .iter()
            .position(|version| version.kind == VersionKind::Created)
        {
            let made = versions.remove(made);
            versions.insert(0, made);
        }
        versions
    }

    /// The commands a transaction's writes are, under `stamp`.
    fn commands(&self, state: &TxnState, stamp: &Stamp) -> Vec<Command> {
        let local = self.inner.local.borrow();
        let mut puts = Vec::new();
        let mut removes = Vec::new();
        for id in &state.order {
            let Some(dirty) = state.dirty.get(id) else {
                continue;
            };
            let held = local.get(id).cloned().flatten();
            // A record made for its author matters where its type says who reads it.
            let made_for = dirty
                .made_for
                .as_ref()
                .filter(|_| held.as_ref().is_some_and(|held| self.ruled(&held.kind)))
                .map(|person| Stamp {
                    acted_for: Some(person.clone()),
                    ..stamp.clone()
                });
            let stamp = made_for.as_ref().unwrap_or(stamp);
            if dirty.removed {
                if let Some(kind) = &dirty.remove {
                    removes.push(Command::Remove {
                        kind: kind.clone(),
                        id: id.clone(),
                        stamp: stamp.clone(),
                    });
                }
                continue;
            }
            let Some(held) = held else { continue };
            if dirty.put {
                puts.push(Command::Put {
                    kind: held.kind.clone(),
                    id: id.clone(),
                    resource: Resource {
                        // The store works out who reads a resource whose type says, and one
                        // nested in another is read by whoever reads that one; every member
                        // reads the rest.
                        readers: if held.parent.is_some() || self.ruled(&held.kind) {
                            String::new()
                        } else {
                            collab_types::EVERY_MEMBER.to_owned()
                        },
                        parent: held.parent.clone(),
                        order: held.order.clone(),
                        fields: held.fields.clone(),
                    },
                    stamp: stamp.clone(),
                });
            }
            if !dirty.edits.is_empty() {
                puts.push(Command::Edit {
                    kind: held.kind.clone(),
                    id: id.clone(),
                    edits: dirty.edits.clone(),
                    stamp: stamp.clone(),
                });
            }
        }
        // Items go before what holds them, so nothing is left nested in a resource removed.
        removes.reverse();
        puts.extend(removes);
        puts
    }

    /// Whether the type of `kind` says who reads its resources.
    fn ruled(&self, kind: &str) -> bool {
        self.inner
            .kinds
            .borrow()
            .of_kind(kind)
            .is_some_and(|entry| entry.readers.is_some())
    }

    /// The listing of the collection `field` of `at`, read page by page from the store.
    /// `filters`, narrowed to the items of the collection `field` of `at`: those nested in it,
    /// and of those, the ones standing in that field where its holder keeps more than one.
    fn held_in(&self, at: &ResourceRef, field: &str, filters: Vec<Filter>) -> Vec<Filter> {
        let mut filters = filters;
        filters.insert(0, Filter::Is(PARENT.to_owned(), Value::Text(id_of(at))));
        if self.shares_holder(&at.ty, field) {
            filters.insert(
                1,
                Filter::Is(STANDS_IN.to_owned(), Value::Text(field.to_owned())),
            );
        }
        filters
    }

    fn paged(
        &self,
        at: &ResourceRef,
        field: &str,
        kind: &str,
        filters: Vec<Filter>,
        order: Order,
    ) -> Rc<Listing> {
        self.paged_pages(at, field, kind, filters, order).0
    }

    /// [`Self::paged`], with the pages it reads.
    fn paged_pages(
        &self,
        at: &ResourceRef,
        field: &str,
        kind: &str,
        filters: Vec<Filter>,
        order: Order,
    ) -> (Rc<Listing>, Rc<Pages>) {
        let filters = self.held_in(at, field, filters);
        let key = format!("{kind}#{}#{field}#{filters:?}#{order:?}", id_of(at));
        if let Some((listing, pages)) = self.inner.listings.borrow().get(&key) {
            return (Rc::clone(listing), Rc::clone(pages));
        }
        let query = Query {
            kind: kind.to_owned(),
            filters,
            order,
            after: None,
            limit: PAGE,
        };
        let pages = Rc::new(Pages {
            store: Rc::downgrade(&self.inner),
            doc: at.doc.clone(),
            kind: kind.to_owned(),
            query,
            ids: RefCell::default(),
            next: RefCell::default(),
            done: StdCell::new(false),
            len: StdCell::new(None),
            far: RefCell::default(),
            before: RefCell::default(),
        });
        let listing = Rc::new(Listing::paged(Rc::clone(&pages) as Rc<dyn PagedItems>));
        self.inner
            .listings
            .borrow_mut()
            .insert(key, (Rc::clone(&listing), Rc::clone(&pages)));
        (listing, pages)
    }

    /// The listing of the collection `field` of `at` the index holds to `filters`, in its
    /// order, with the writes this store has not sent read over it: an item written here that no
    /// longer holds to `pred` leaves it, and one that now holds comes in where it stands. A
    /// filter of a column of three thousand finished tasks then reads the page or two it is
    /// asked for, not every task of the column, after an action wrote one task's status.
    ///
    /// `None` when a write here added, removed or moved an item of the collection, so the
    /// index's places are not the items' places now, or when the index cannot say where an item
    /// stands: the caller reads the items one by one.
    fn written_over(
        &self,
        at: &ResourceRef,
        field: &str,
        kind: &str,
        item: &TypeId,
        pred: Option<&Pred>,
        filters: Vec<Filter>,
    ) -> Result<Option<Rc<Listing>>, StoreError> {
        let holder = id_of(at);
        if self.places_written(kind, &holder) {
            return Ok(None);
        }
        let (base, pages) = self.paged_pages(at, field, kind, filters, Order::default());
        let written: Vec<String> = self
            .inner
            .local
            .borrow()
            .iter()
            .filter_map(|(id, held)| {
                let held = held.as_ref()?;
                (held.kind == kind && held.parent.as_deref() == Some(holder.as_str()))
                    .then(|| id.clone())
            })
            .collect();
        let mut gone = Vec::new();
        let mut come = Vec::new();
        for id in written {
            let item_id = item_of(&id);
            let target = at.item(&item_id, item.clone());
            let holds = match pred {
                Some(pred) => pred.eval(&mut |read: &FieldRead| self.field_lit(&target, read))?,
                None => true,
            };
            match (base.position(&item_id), holds) {
                (None, _) => return Ok(None),
                (Some(Some(stands)), false) => gone.push(stands),
                (Some(None), true) => {
                    let Some(slot) = self.slot_in(&pages, &at.doc, &id) else {
                        return Ok(None);
                    };
                    come.push((slot, Rc::from(item_id)));
                }
                (Some(Some(_)), true) | (Some(None), false) => {}
            }
        }
        if gone.is_empty() && come.is_empty() {
            return Ok(Some(base));
        }
        gone.sort_unstable();
        come.sort();
        Ok(Some(Rc::new(Listing::paged(Rc::new(WrittenOver {
            base,
            gone,
            come,
        })))))
    }

    /// How many items of `base`, a listing in the index's order of places, stand before the
    /// resource `id`: where it stands among them. Read by halving the listing, a page or so at
    /// each step; `None` when a place is not known.
    fn slot_in(&self, base: &Pages, doc: &str, id: &str) -> Option<usize> {
        let key = |id: &str| -> Option<(Value, String)> {
            let cache = self.inner.cache.borrow();
            let held = cache.get(id)?.as_ref()?;
            Some((
                held.order.clone().map_or(Value::Null, Value::Text),
                id.to_owned(),
            ))
        };
        let own = {
            let local = self.inner.local.borrow();
            let held = local.get(id)?.as_ref()?;
            (
                held.order.clone().map_or(Value::Null, Value::Text),
                id.to_owned(),
            )
        };
        let (mut low, mut high) = (0, PagedItems::len(base));
        while low < high {
            let mid = low.midpoint(high);
            let other = format!("{doc}/{}", base.probe(mid)?);
            if key(&other)? < own {
                low = mid.saturating_add(1);
            } else {
                high = mid;
            }
        }
        Some(low)
    }

    /// Whether this store holds writes not sent yet of resources of `kind`.
    fn writes_of(&self, kind: &str) -> bool {
        let local = self.inner.local.borrow();
        if local.is_empty() {
            return false;
        }
        let cache = self.inner.cache.borrow();
        local.iter().any(|(id, held)| match held {
            Some(held) => held.kind == kind,
            None => cache
                .get(id)
                .and_then(Option::as_ref)
                .is_none_or(|held| held.kind == kind),
        })
    }

    /// Whether this store holds writes not sent yet that add, remove or move resources of `kind`
    /// in `holder`: where they stand in the store's index is then not where they stand now. A
    /// write of their other fields leaves the index's order and membership as they are.
    fn places_written(&self, kind: &str, holder: &str) -> bool {
        let local = self.inner.local.borrow();
        if local.is_empty() {
            return false;
        }
        let cache = self.inner.cache.borrow();
        let here = |held: &Held| held.kind == kind && held.parent.as_deref() == Some(holder);
        local.iter().any(
            |(id, now)| match (now.as_ref(), cache.get(id).and_then(Option::as_ref)) {
                (Some(now), Some(was)) => {
                    (here(now) || here(was))
                        && (now.parent != was.parent
                            || now.order != was.order
                            || now.fields.get(STANDS_IN) != was.fields.get(STANDS_IN))
                }
                (Some(held), None) | (None, Some(held)) => here(held),
                (None, None) => true,
            },
        )
    }

    /// The filters `pred` is, over the fields of `entry`; `None` where the store cannot say it.
    fn filters_of(&self, pred: &Pred, entry: &TypeEntry) -> Option<Vec<Filter>> {
        let filter = self.filter_of(pred, entry)?;
        let mut all = Vec::new();
        conjuncts(filter, &mut all);
        Some(all)
    }

    fn filter_of(&self, pred: &Pred, entry: &TypeEntry) -> Option<Filter> {
        store_filter(
            pred,
            &|read: &FieldRead| Some((self.indexed_as(&read.spec.id, entry)?, read.absent.clone())),
            &|lit| self.value_of_lit(lit),
        )
    }

    fn value_of_lit(&self, lit: &Lit) -> Option<Value> {
        store_value(lit, &self.inner.kinds.borrow())
    }

    /// `order` as the store sorts, over the fields of `entry`; `None` where it cannot.
    /// The name the store's index holds the field `field` of `entry`'s resources under, when it
    /// holds it: a declared field it indexes, or the time a version reflects (`$created`,
    /// `$updated`, `$changed.<field>`), which the index keeps for every resource.
    fn indexed_as(&self, field: &str, entry: &TypeEntry) -> Option<String> {
        use tilolang_runtime::store::Reflected;
        match Reflected::of(field) {
            Some(Reflected::Created) => Some(tilo_resource_store::CREATED.to_owned()),
            Some(Reflected::Updated) => Some(tilo_resource_store::UPDATED.to_owned()),
            Some(Reflected::Changed(changed)) => {
                let field = entry.fields.get(&changed)?;
                Some(format!("{}{}", tilo_resource_store::CHANGED, field.name))
            }
            None => {
                let field = entry.fields.get(field)?;
                self.indexed(field).then(|| field.name.clone())
            }
        }
    }

    fn order_of(&self, order: &tilolang_runtime::query::Order, entry: &TypeEntry) -> Option<Order> {
        let levels = order.levels();
        let first = levels.first()?;
        let name = |level: &tilolang_runtime::query::Order| self.indexed_as(&level.field, entry);
        let mut out = Order::by(name(first)?);
        out.descending = first.descending;
        out = out.absent(if first.absent_last || first.descending {
            Absent::Last
        } else {
            Absent::First
        });
        for level in levels.iter().skip(1) {
            out = out.then(name(level)?, level.descending);
        }
        Some(out)
    }

    /// Every resource of `kind` the store lists for `query`, all pages.
    fn query_all(&self, query: Query) -> Result<Vec<String>, StoreError> {
        let kind = query.kind.clone();
        let rows = self.query_rows(query)?;
        let mut cache = self.inner.cache.borrow_mut();
        Ok(rows
            .into_iter()
            .map(|(id, indexed)| {
                cache
                    .entry(id.clone())
                    .or_insert_with(|| Some(Held::of(&kind, indexed)));
                id
            })
            .collect())
    }

    /// The instant the first resource of `kind` names in the field `field` after `clock`: the
    /// moment a filter comparing that field with the clock next reads differently.
    fn next_value(&self, kind: &str, field: &str, clock: i64) -> Result<Option<i64>, StoreError> {
        let query = Query {
            kind: kind.to_owned(),
            filters: vec![Filter::Within {
                field: field.to_owned(),
                from: std::ops::Bound::Excluded(Value::Text(tilolang::calendar::time_text(clock))),
                to: std::ops::Bound::Unbounded,
            }],
            order: Order::by(field.to_owned()),
            after: None,
            limit: 1,
        };
        let page = match self.command(Command::Query(query))? {
            Reply::Page(page) => page,
            other => return Err(backend(format!("a query answered {other:?}"))),
        };
        let Some((_, indexed)) = page.resources.first() else {
            return Ok(None);
        };
        Ok(match indexed.fields.get(field) {
            Some(Cell::One(Value::Text(text))) => tilolang::calendar::instant(text),
            _ => None,
        })
    }

    /// The instants the field `field` of a resource of `ty` written here, not yet sent to the
    /// store, names after `clock`: a snooze an action set is known before the document holds it.
    fn next_written(
        &self,
        ty: &TypeId,
        field: &str,
        entry: &kinds::FieldEntry,
        kind: &str,
        clock: i64,
    ) -> Result<Vec<i64>, StoreError> {
        let ids: Vec<String> = self
            .inner
            .local
            .borrow()
            .iter()
            .filter(|(_, held)| held.as_ref().is_some_and(|held| held.kind == kind))
            .map(|(id, _)| id.clone())
            .collect();
        let spec = FieldSpec {
            id: field.to_owned(),
            kind: entry.kind,
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(at) = ResourceRef::parse_id(&id, ty.clone()) else {
                continue;
            };
            let (FieldValue::Text(text) | FieldValue::Scalar(Scalar::Text(text))) =
                self.read(&at, &spec)?
            else {
                continue;
            };
            if let Some(moment) = tilolang::calendar::instant(&text)
                && moment > clock
            {
                out.push(moment);
            }
        }
        Ok(out)
    }

    /// The listing of `ids`, filtered by `pred` and sorted by `order` here, read field by field.
    fn evaluated(
        &self,
        at: &ResourceRef,
        ids: Vec<Rc<str>>,
        item: &TypeId,
        pred: Option<&Pred>,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        let mut items = Vec::new();
        for id in ids {
            // An item of a union collection is of its own kind, not the field's first.
            let ty = self
                .type_of_id(&at.item(&id, String::new()).id_string())
                .unwrap_or_else(|| item.clone());
            let target = at.item(&id, ty);
            let holds = match pred {
                Some(pred) => pred.eval(&mut |read: &FieldRead| self.field_lit(&target, read))?,
                None => true,
            };
            if holds {
                items.push(id);
            }
        }
        let Some(order) = order else {
            return Ok(Rc::new(Listing::from_items(items)));
        };
        let levels = order.levels();
        let mut keyed = Vec::with_capacity(items.len());
        for id in items {
            let ty = self
                .type_of_id(&at.item(&id, String::new()).id_string())
                .unwrap_or_else(|| item.clone());
            let target = at.item(&id, ty);
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                keys.push(self.order_key(&target, &level.field, level.kind)?);
            }
            keyed.push((keys, id));
        }
        keyed.sort_by(|(a, _), (b, _)| ordered_cmp(order, a, b));
        Ok(Rc::new(Listing::from_items(
            keyed.into_iter().map(|(_, id)| id).collect(),
        )))
    }
}

impl ResourceStore {
    /// The runs this store holds that wait on a child call: the invocation the call went out
    /// under, the run, and the action it invokes. What a host starting over this store waits
    /// on, so the answer resumes the run. Empty when nothing waits.
    #[must_use]
    pub fn open_runs(&self) -> Vec<(String, RunId, String)> {
        self.inner
            .journals
            .borrow()
            .iter()
            .filter(|(_, journal)| !journal.finished)
            .filter_map(|(run, journal)| match &journal.pending {
                Some(tilolang_runtime::run::Pending::Call { call, action }) => {
                    Some((call.clone(), run.clone(), action.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// What an order or a filter reads of `target` as `field`: the field, or the time of the
    /// version a reflected pseudo-field names (`$created`, `$updated`, `$changed.<field>`).
    fn order_key(
        &self,
        target: &ResourceRef,
        field: &str,
        kind: FieldKind,
    ) -> Result<Lit, StoreError> {
        use tilolang_runtime::store::Reflected;
        Ok(match Reflected::of(field) {
            Some(Reflected::Changed(changed)) => self
                .field_changed(target, &changed)?
                .map_or(Lit::Absent, |version| Lit::Time(version.at)),
            Some(end) => self
                .version_end(target, end == Reflected::Updated)?
                .map_or(Lit::Absent, |version| Lit::Time(version.at)),
            None => tilolang_runtime::store::lit_of(
                field,
                self.read(
                    target,
                    &FieldSpec {
                        id: field.to_owned(),
                        kind,
                    },
                )?,
                &Lit::Absent,
            )?,
        })
    }
}

/// A short tag of a type id, for the ids of its items.
fn short(ty: &str) -> String {
    ty.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(4)
        .collect()
}

/// The instant `at` where the clock reads it: a reading is whole minutes, so the first minute
/// at or after it.
fn minute(at: i64) -> i64 {
    at.saturating_sub(at.rem_euclid(60))
        .saturating_add(if at.rem_euclid(60) == 0 { 0 } else { 60 })
}

/// Sets `fields` of an effect entry to what settling it as `outcome` leaves.
fn settle_fields(fields: &mut BTreeMap<String, Cell>, outcome: &Settled) {
    fields.insert(OUT_SETTLED.to_owned(), Cell::One(Value::Bool(true)));
    fields.insert(
        OUT_OUTCOME.to_owned(),
        Cell::One(Value::Text(outcome.word().to_owned())),
    );
    fields.insert(
        OUT_REASON.to_owned(),
        Cell::One(Value::Text(outcome.reason().to_owned())),
    );
}

/// Makes `edit` in a resource's `content` as read here.
fn apply_edit(content: &mut serde_json::Map<String, Json>, edit: &Edit) {
    match edit {
        Edit::Set { field, value } => {
            content.insert(field.clone(), value.clone());
        }
        Edit::Splice {
            field,
            at,
            delete,
            insert,
        } => {
            let mut text: Vec<char> = content
                .get(field)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .chars()
                .collect();
            let at = (*at).min(text.len());
            let end = at.saturating_add(*delete).min(text.len());
            text.splice(at..end, insert.chars());
            content.insert(field.clone(), Json::String(text.into_iter().collect()));
        }
        Edit::Add { field, by } => {
            let held = content.get(field).and_then(Json::as_i64).unwrap_or(0);
            content.insert(field.clone(), Json::from(held.saturating_add(*by)));
        }
        Edit::MapPut { field, key, value } => {
            let map = content
                .entry(field.clone())
                .or_insert_with(|| Json::Object(serde_json::Map::new()));
            if !map.is_object() {
                *map = Json::Object(serde_json::Map::new());
            }
            if let Some(map) = map.as_object_mut() {
                map.insert(key.clone(), value.clone());
            }
        }
        Edit::MapDelete { field, key } => {
            if let Some(map) = content.get_mut(field).and_then(Json::as_object_mut) {
                map.remove(key);
            }
        }
        Edit::Remove { field } => {
            content.remove(field);
        }
    }
}

/// A listing read from the store a page at a time, as it is asked for.
struct Pages {
    store: Weak<Inner>,
    /// The document the items are stored in: an item's id in the store is under it.
    doc: String,
    kind: String,
    query: Query,
    /// The items read from the start, page by page.
    ids: RefCell<Vec<Rc<str>>>,
    next: RefCell<Option<Cursor>>,
    done: StdCell<bool>,
    len: StdCell<Option<usize>>,
    /// Items read far past [`Self::ids`], by where they stand: a page read where a reader looked,
    /// from the store's counts, without the pages before it.
    far: RefCell<BTreeMap<usize, Rc<str>>>,
    /// Once a write moved the store on past it, where each resource the write named stood in
    /// it, by item, `None` for one it did not hold: the listing as it read before the write.
    before: RefCell<Option<HashMap<String, Option<usize>>>>,
}

impl std::fmt::Debug for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages")
            .field("kind", &self.kind)
            .field("read", &self.ids.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Pages {
    /// Keep the listing as it reads before a write of `ids` reaches the store: where each of
    /// them stands in it now, and how many it lists. It reads no page after.
    fn freeze(&self, ids: &BTreeSet<String>) {
        let _ = PagedItems::len(self);
        let mut before = HashMap::with_capacity(ids.len());
        // A resource nested in another document is no item of this listing's.
        let mine: Vec<String> = ids
            .iter()
            .filter(|id| id.starts_with(&format!("{}/", self.doc)))
            .cloned()
            .collect();
        if let Some(inner) = self.store.upgrade()
            && !mine.is_empty()
            && let Ok(Reply::Positions(at)) = inner.port.command(Command::Positions {
                query: self.query.clone(),
                ids: mine.clone(),
            })
            && at.len() == mine.len()
        {
            for (id, at) in mine.iter().zip(at) {
                before.insert(item_of(id), at);
            }
        }
        *self.before.borrow_mut() = Some(before);
    }

    /// Where `id` stood before the write that froze the listing, when it named it.
    #[allow(
        clippy::option_option,
        reason = "whether the write named it, and where it stood then: nowhere is an answer"
    )]
    fn stood(&self, id: &str) -> Option<Option<usize>> {
        self.before.borrow().as_ref()?.get(id).copied()
    }

    fn frozen(&self) -> bool {
        self.before.borrow().is_some()
    }

    /// The store's id of the item `item`.
    fn id_of(&self, item: &str) -> String {
        format!("{}/{item}", self.doc)
    }

    /// Reads the page that starts at `at`, into [`Self::far`].
    fn page_at(&self, at: usize) {
        let Some(inner) = self.store.upgrade() else {
            return;
        };
        let Ok(Reply::Page(page)) = inner.port.command(Command::PageAt {
            query: self.query.clone(),
            at,
        }) else {
            return;
        };
        let mut cache = inner.cache.borrow_mut();
        let mut far = self.far.borrow_mut();
        for (offset, (id, indexed)) in page.resources.iter().enumerate() {
            far.insert(at.saturating_add(offset), Rc::from(item_of(id)));
            cache
                .entry(id.clone())
                .or_insert_with(|| Some(Held::of(&self.kind, indexed.clone())));
        }
    }

    /// The item at `at`, for a search halving the listing: from what was read already, or from a
    /// few items read where it stands. A search through three thousand items then reads a dozen
    /// small pages, not a full page at each step.
    fn probe(&self, at: usize) -> Option<Rc<str>> {
        /// How many items a probe reads around the one it asks for: the last steps of a search
        /// land among them.
        const PROBE: usize = 16;
        if let Some(item) = self.ids.borrow().get(at) {
            return Some(Rc::clone(item));
        }
        if let Some(item) = self.far.borrow().get(&at) {
            return Some(Rc::clone(item));
        }
        // A listing the store moved past reads what it read before the write, never the store.
        if self.frozen() {
            return PagedItems::get(self, at);
        }
        let inner = self.store.upgrade()?;
        let mut query = self.query.clone();
        query.limit = PROBE;
        let Ok(Reply::Page(page)) = inner.port.command(Command::PageAt { query, at }) else {
            return None;
        };
        let mut cache = inner.cache.borrow_mut();
        let mut far = self.far.borrow_mut();
        for (offset, (id, indexed)) in page.resources.iter().enumerate() {
            far.insert(at.saturating_add(offset), Rc::from(item_of(id)));
            cache
                .entry(id.clone())
                .or_insert_with(|| Some(Held::of(&self.kind, indexed.clone())));
        }
        far.get(&at).cloned()
    }

    /// Reads the next page; false when there is none.
    fn more(&self) -> bool {
        if self.done.get() || self.frozen() {
            return false;
        }
        let Some(inner) = self.store.upgrade() else {
            self.done.set(true);
            return false;
        };
        let mut query = self.query.clone();
        query.after.clone_from(&self.next.borrow());
        let Ok(Reply::Page(page)) = inner.port.command(Command::Query(query)) else {
            self.done.set(true);
            return false;
        };
        let mut cache = inner.cache.borrow_mut();
        let mut ids = self.ids.borrow_mut();
        for (id, indexed) in &page.resources {
            ids.push(Rc::from(item_of(id)));
            cache
                .entry(id.clone())
                .or_insert_with(|| Some(Held::of(&self.kind, indexed.clone())));
        }
        match page.next {
            Some(next) if !page.resources.is_empty() => *self.next.borrow_mut() = Some(next),
            _ => self.done.set(true),
        }
        !page.resources.is_empty()
    }
}

impl PagedItems for Pages {
    fn len(&self) -> usize {
        if let Some(len) = self.len.get() {
            return len;
        }
        let counted = self.store.upgrade().and_then(|inner| {
            match inner.port.command(Command::Count {
                kind: self.kind.clone(),
                filters: self.query.filters.clone(),
            }) {
                Ok(Reply::Count(count)) => Some(count),
                _ => None,
            }
        });
        let len = counted.unwrap_or_else(|| {
            while self.more() {}
            self.ids.borrow().len()
        });
        self.len.set(Some(len));
        len
    }

    /// The item at `index`: from the pages read from the start when it is among them or the
    /// next one, and otherwise from a page read where it stands.
    fn get(&self, index: usize) -> Option<Rc<str>> {
        if let Some(item) = self.ids.borrow().get(index) {
            return Some(Rc::clone(item));
        }
        if let Some(item) = self.far.borrow().get(&index) {
            return Some(Rc::clone(item));
        }
        let near = self.ids.borrow().len().saturating_add(self.query.limit);
        if index < near || self.frozen() {
            while self.ids.borrow().len() <= index {
                if !self.more() {
                    break;
                }
            }
            return self.ids.borrow().get(index).cloned();
        }
        self.page_at(index);
        self.far.borrow().get(&index).cloned()
    }

    /// Where the item is: among the pages read so far, or as the store's index has it. What a
    /// view asks about is mostly a row it already drew; one far down the listing reads no page.
    fn position(&self, id: &str) -> Option<Option<usize>> {
        if let Some(stood) = self.stood(id) {
            return Some(stood);
        }
        if self.frozen() {
            let found = self
                .ids
                .borrow()
                .iter()
                .position(|held| &**held == id)
                .or_else(|| {
                    self.far
                        .borrow()
                        .iter()
                        .find_map(|(at, held)| (&**held == id).then_some(*at))
                });
            return match found {
                Some(at) => Some(Some(at)),
                None if self.done.get() => Some(None),
                None => None,
            };
        }
        if let Some(at) = self.ids.borrow().iter().position(|held| &**held == id) {
            return Some(Some(at));
        }
        if let Some(at) = self
            .far
            .borrow()
            .iter()
            .find_map(|(at, held)| (&**held == id).then_some(*at))
        {
            return Some(Some(at));
        }
        // Read from the store's index, however far down the listing the item stands.
        let inner = self.store.upgrade()?;
        match inner.port.command(Command::Positions {
            query: self.query.clone(),
            ids: vec![self.id_of(id)],
        }) {
            Ok(Reply::Positions(at)) => at.into_iter().next(),
            _ => None,
        }
    }

    /// Whether the listing holds the item: among those read so far, or as the store holds the
    /// item itself, checked against the listing's filters.
    fn holds(&self, id: &str) -> Option<bool> {
        if let Some(stood) = self.stood(id) {
            return Some(stood.is_some());
        }
        if self.ids.borrow().iter().any(|held| &**held == id) {
            return Some(true);
        }
        if self.frozen() {
            return self.done.get().then_some(false);
        }
        let inner = self.store.upgrade()?;
        match inner.port.command(Command::Get {
            kind: self.kind.clone(),
            id: format!("{}/{id}", self.doc),
        }) {
            Ok(Reply::Resource(Some(stored))) => Some(
                self.query
                    .filters
                    .iter()
                    .all(|filter| filter.holds(&stored)),
            ),
            Ok(Reply::Resource(None)) => Some(false),
            _ => None,
        }
    }
}

/// A listing of the index with the writes a store has not sent read over it
/// ([`ResourceStore::written_over`]): the items of `base` at the places `gone` left out, and
/// each item of `come` put in before the item of `base` at its place.
#[derive(Debug)]
struct WrittenOver {
    base: Rc<Listing>,
    /// Places in `base`, in order.
    gone: Vec<usize>,
    /// Places in `base`, in order, with the item that comes in before the one there.
    come: Vec<(usize, Rc<str>)>,
}

impl WrittenOver {
    /// Where the item at the place `at` in `base` stands here.
    fn over(&self, at: usize) -> usize {
        let gone = self.gone.partition_point(|gone| *gone < at);
        let come = self.come.partition_point(|(slot, _)| *slot <= at);
        at.saturating_sub(gone).saturating_add(come)
    }
}

impl PagedItems for WrittenOver {
    fn len(&self) -> usize {
        self.base
            .len()
            .saturating_sub(self.gone.len())
            .saturating_add(self.come.len())
    }

    fn len_is_exact(&self) -> bool {
        self.base.len_is_exact()
    }

    fn get(&self, index: usize) -> Option<Rc<str>> {
        // The places in `base` where something leaves or comes, in order: what comes at a place
        // stands before the item there.
        let mut gone = self.gone.iter().peekable();
        let mut come = self.come.iter().peekable();
        let (mut at, mut here) = (0_usize, 0_usize);
        loop {
            let next = match (come.peek(), gone.peek()) {
                (Some((slot, _)), Some(left)) => (*slot).min(**left),
                (Some((slot, _)), None) => *slot,
                (None, Some(left)) => **left,
                (None, None) => {
                    return self.base.get(at.saturating_add(index.saturating_sub(here)));
                }
            };
            let run = next.saturating_sub(at);
            if index < here.saturating_add(run) {
                return self.base.get(at.saturating_add(index.saturating_sub(here)));
            }
            here = here.saturating_add(run);
            at = next;
            if let Some((_, item)) = come.next_if(|(slot, _)| *slot == at) {
                if index == here {
                    return Some(Rc::clone(item));
                }
                here = here.saturating_add(1);
            } else if gone.next_if(|left| **left == at).is_some() {
                at = at.saturating_add(1);
            }
        }
    }

    fn position(&self, id: &str) -> Option<Option<usize>> {
        if let Some(at) = self.come.iter().position(|(_, item)| &**item == id) {
            let slot = self.come.get(at).map_or(0, |(slot, _)| *slot);
            let gone = self.gone.partition_point(|gone| *gone < slot);
            return Some(Some(slot.saturating_sub(gone).saturating_add(at)));
        }
        match self.base.position(id)? {
            Some(at) if self.gone.binary_search(&at).is_ok() => Some(None),
            Some(at) => Some(Some(self.over(at))),
            None => Some(None),
        }
    }

    fn holds(&self, id: &str) -> Option<bool> {
        if self.come.iter().any(|(_, item)| &**item == id) {
            return Some(true);
        }
        self.position(id).map(|at| at.is_some())
    }
}

/// What the store's reader holds, as rich text is read for them.
struct Holdings<'s>(&'s ResourceStore);

impl crate::reading::Holdings for Holdings<'_> {
    fn held(&self, at: &ResourceRef) -> Option<String> {
        self.0.holds(at)
    }

    fn first_value(&self, at: &ResourceRef) -> Option<FieldValue> {
        self.0.title_value(at)
    }

    fn keeps(&self, app: &str) -> bool {
        self.0
            .inner
            .kinds
            .borrow()
            .types()
            .any(|entry| entry.app == app)
    }

    fn unshared(&self) -> std::sync::Arc<str> {
        std::sync::Arc::from(tilolang_runtime::rich::UNSHARED)
    }
}

impl Store for ResourceStore {
    fn fresh_id(&mut self, prefix: &str) -> String {
        self.fresh(prefix)
    }

    fn create(&mut self, record: &NewResource) -> Result<ResourceRef, StoreError> {
        let id = self.fresh("r");
        self.create_with_id(record, &id)
    }

    fn create_with_id(
        &mut self,
        record: &NewResource,
        id: &str,
    ) -> Result<ResourceRef, StoreError> {
        let txn = self.begin()?;
        let made = self.create_in(txn, record, id);
        match made {
            Ok(at) => {
                self.commit(
                    txn,
                    &ChangeNote {
                        action: tilolang_runtime::store::CREATE_ACTION.to_owned(),
                        ..ChangeNote::default()
                    },
                )?;
                Ok(at)
            }
            Err(error) => {
                self.rollback(txn);
                Err(error)
            }
        }
    }

    fn create_in(
        &mut self,
        txn: Txn,
        record: &NewResource,
        id: &str,
    ) -> Result<ResourceRef, StoreError> {
        let kind = self.kind_of_type(&record.ty)?;
        if self.held(&kind, id)?.is_some() {
            return Err(StoreError::Invalid(format!("{id} is there already")));
        }
        let mut held = self.new_held(record, None, None)?;
        let edits = self.initial_edits(&record.ty, &mut held);
        self.write(txn, id, Some(held), |dirty| {
            dirty.put = true;
            dirty.edits.extend(edits);
        })?;
        let at = ResourceRef::root(id.to_owned(), record.ty.clone());
        self.touch(txn, &at, ANY_FIELD, false)?;
        self.inner.made.borrow_mut().push(at.clone());
        self.inner.wrote.borrow_mut().made = true;
        Ok(at)
    }

    fn mark_migrated_in(&mut self, txn: Txn, doc: &str, migration: &str) -> Result<(), StoreError> {
        self.mark_in(txn, doc, MIGRATED, migration, Json::Bool(true))
    }

    fn made_for(&mut self, txn: Txn, at: &ResourceRef, person: &str) -> Result<(), StoreError> {
        let id = id_of(at);
        self.txn_mut(txn, |state| {
            if let Some(dirty) = state.dirty.get_mut(&id) {
                dirty.made_for = Some(person.to_owned());
            }
        })
    }

    /// The store holds only what its reader may read: a resource of a package here reads by
    /// what it is called now when the store holds it, as not shared when it does not.
    fn reading(
        &self,
        reference: &tilolang_runtime::rich::Reference,
    ) -> tilolang_runtime::rich::Reading {
        crate::reading::reading(&Holdings(self), reference)
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let held = self.held_at(at)?;
        self.entry_of_kind(&held.kind)
            .map(|entry| entry.id)
            .ok_or_else(|| not_found(format!("no type of the kind {}", held.kind)))
    }

    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, FieldId)>, StoreError> {
        let at = &*self.typed(at);
        if at.item.is_none() {
            return Ok(None);
        }
        let held = self.held_at(at)?;
        let Some(parent) = held.parent else {
            return Ok(None);
        };
        // The types whose collections hold items of this type, and the one the parent is.
        let holders: Vec<(TypeId, FieldId)> = {
            let kinds = self.inner.kinds.borrow();
            let mut holders = Vec::new();
            for entry in kinds.types() {
                for (field_id, field) in &entry.fields {
                    if field.items.iter().any(|ty| ty == &at.ty) {
                        holders.push((entry.id.clone(), field_id.clone()));
                    }
                }
            }
            holders
        };
        let stands = match held.fields.get(STANDS_IN) {
            Some(Cell::One(Value::Text(stands))) => Some(stands.clone()),
            _ => None,
        };
        for (ty, field) in holders {
            if stands.as_ref().is_some_and(|stands| *stands != field) {
                continue;
            }
            let kind = self.kind_of_type(&ty)?;
            if self.held(&kind, &parent)?.is_some() {
                let holder = match parent.split_once('/') {
                    Some((doc, item)) => {
                        ResourceRef::root(doc.to_owned(), String::new()).item(item, ty)
                    }
                    None => ResourceRef::root(parent.clone(), ty),
                };
                return Ok(Some((holder, field)));
            }
        }
        Err(not_found(format!("the collection holding {}", id_of(at))))
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        let mut state = self.inner.txn.borrow_mut();
        if state.is_some() {
            return Err(StoreError::Invalid("a transaction is open already".into()));
        }
        let id = self.inner.next_txn.get();
        self.inner.next_txn.set(id.saturating_add(1));
        *state = Some(TxnState {
            id,
            ..TxnState::default()
        });
        Ok(Txn(id))
    }

    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let at = &*self.typed(at);
        self.apply_to(txn, at, m)
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        // What the host notes of the run this commit is the first of goes with it.
        let marks: Vec<RunMark> = {
            let runs = self.inner.runs.borrow();
            match runs.first() {
                Some((run, _, _)) => {
                    let mut held = self.inner.run_marks.borrow_mut();
                    let (mine, rest) = std::mem::take(&mut *held)
                        .into_iter()
                        .partition(|mark| mark.run == *run);
                    *held = rest;
                    mine
                }
                None => Vec::new(),
            }
        };
        for mark in marks {
            self.mark_in(txn, &mark.doc, TRIGGERED, &mark.key, mark.value)?;
        }
        let migrating = self.inner.migrating.borrow_mut().take();
        if let Some((migration, doc)) = &migrating {
            self.mark_in(txn, doc, MIGRATED, migration, Json::Bool(true))?;
        }
        let state = {
            let mut open = self.inner.txn.borrow_mut();
            match open.take() {
                Some(state) if state.id == txn.0 => state,
                other => {
                    *open = other;
                    return Err(StoreError::Invalid(format!(
                        "no open transaction {}",
                        txn.0
                    )));
                }
            }
        };
        let stamp = self.stamp(note, migrating.is_some());
        let commands = self.commands(&state, &stamp);
        let by = note.invoker.clone();
        let mut state = state;
        // What was made from a resource links to it: whoever reads its links reads them again.
        if let Some(from) = &note.from {
            state
                .touched
                .entry(from.doc.clone())
                .or_default()
                .insert(FieldKey {
                    resource: id_of(from),
                    field: tilolang_runtime::store::LINKS.to_owned(),
                });
        }
        let depth = self.inner.runs.borrow().len();
        let trial = self.inner.trials.borrow().len();
        let heard = steps::Heard {
            depth,
            trial,
            touched: state.touched,
            collections: state.collections,
            order: state.order,
            by,
        };
        // A run's writes are sent, and told, when it ends, and a trial run's once it is kept.
        if depth > 0 || trial > 0 {
            self.inner.unsent.borrow_mut().extend(commands);
            self.inner.heard.borrow_mut().push(heard);
            return Ok(());
        }
        // Where what leaves a listing stood is read before the write reaches the store, which
        // holds everything else as it was only when nothing else written here is unsent.
        let alone = self.inner.unsent.borrow().is_empty()
            && self.inner.sample.borrow().is_none()
            && !state
                .before
                .values()
                .any(|before| matches!(before, Before::Was(_)));
        let stepping = if alone {
            self.steps_before(&heard.collections, &heard.order)
        } else {
            Vec::new()
        };
        self.inner.unsent.borrow_mut().extend(commands);
        // The views are told once the store holds the write: where what came stands is read
        // from it, and a view reading what changed reads the store.
        let flushed = self.flush();
        self.tell(
            vec![heard],
            if flushed.is_ok() {
                stepping
            } else {
                Vec::new()
            },
        );
        flushed
    }

    fn rollback(&mut self, txn: Txn) {
        let state = {
            let mut open = self.inner.txn.borrow_mut();
            match open.take() {
                Some(state) if state.id == txn.0 => state,
                other => {
                    *open = other;
                    return;
                }
            }
        };
        let mut local = self.inner.local.borrow_mut();
        for (id, before) in state.before {
            match before {
                Before::Was(held) => {
                    local.insert(id, held);
                }
                Before::Unwritten => {
                    local.remove(&id);
                }
            }
        }
        drop(local);
        self.inner.listings.borrow_mut().clear();
    }

    fn begin_run(&mut self, run: &str) {
        // A run that goes on after waiting on a call is the one opened before the wait: what it
        // wrote since it began is still held, and opening it again would hold it twice and leave
        // the first entry open for ever.
        if self
            .inner
            .runs
            .borrow()
            .iter()
            .any(|(open, ..)| open == run)
        {
            return;
        }
        self.note_held_all();
        let saved = self.saved();
        let mut runs = self.inner.runs.borrow_mut();
        // The outermost run writes as the invocation being served, where one is.
        let invocation = self
            .inner
            .serving
            .borrow()
            .clone()
            .filter(|_| runs.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        runs.push((run.to_owned(), saved, invocation));
    }

    fn end_run(&mut self, run: &str) -> Result<(), StoreError> {
        {
            let mut runs = self.inner.runs.borrow_mut();
            let Some(at) = runs.iter().rposition(|(open, _, _)| open == run) else {
                return Ok(());
            };
            runs.truncate(at);
        }
        self.settle()
    }

    fn abort_run(&mut self, run: &str) {
        let saved = {
            let mut runs = self.inner.runs.borrow_mut();
            let Some(at) = runs.iter().rposition(|(open, _, _)| open == run) else {
                return;
            };
            let saved = runs.get(at).map(|(_, saved, _)| saved.clone());
            runs.truncate(at);
            // What the failed run wrote is taken back, so nothing of it is told.
            self.inner
                .heard
                .borrow_mut()
                .retain(|heard| heard.depth <= at);
            saved
        };
        if let Some(saved) = saved {
            self.restore(saved);
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let at = &*self.typed(at);
        let entry = self.field(&at.ty, &field.id);
        // The name it is held under now, and the ones an earlier version held it under.
        let (name, former) = entry.as_ref().map_or_else(
            || (field.id.clone(), Vec::new()),
            |entry| (entry.name.clone(), entry.former.clone()),
        );
        if field.kind == FieldKind::Collection {
            let listing = self.listing(at, field)?;
            return Ok(FieldValue::Items(listing.ids()));
        }
        let indexed = entry.as_ref().is_some_and(|entry| self.indexed(entry));
        if indexed {
            let cell = self.cell_of(at, &name, &former)?;
            if let Some(cell @ (Cell::One(_) | Cell::Set(_))) = cell {
                return self.value_from(field, entry.as_ref(), Some(&cell), None);
            }
            let content = self.content_of(at)?;
            let json = content
                .get(&name)
                .or_else(|| former.iter().find_map(|name| content.get(name)));
            return self.value_from(field, entry.as_ref(), cell.as_ref(), json);
        }
        let content = self.content_of(at)?;
        let json = content
            .get(&name)
            .or_else(|| former.iter().find_map(|name| content.get(name)));
        self.value_from(field, entry.as_ref(), None, json)
    }

    fn field_lit(&self, at: &ResourceRef, read: &FieldRead) -> Result<Lit, StoreError> {
        // When a version came is no field: it is read from the resource's versions.
        if tilolang_runtime::store::Reflected::of(&read.spec.id).is_some() {
            return self.order_key(at, &read.spec.id, read.spec.kind);
        }
        tilolang_runtime::store::field_lit_of(read, self.read(at, &read.spec)?)
    }

    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        let at = &*self.typed(at);
        let item_types = self
            .field(&at.ty, &field.id)
            .map(|entry| entry.items)
            .ok_or_else(|| {
                StoreError::Invalid(format!("field {} is not a collection", field.id))
            })?;
        if item_types.is_empty() {
            return Err(StoreError::Invalid(format!(
                "field {} is not a collection",
                field.id
            )));
        }
        // A union's items are of more than one kind, and items an earlier version stored under
        // another name's kind too: one indexed page cannot answer either.
        let renamed = item_types
            .iter()
            .any(|ty| self.kinds_of_type(ty).is_ok_and(|kinds| kinds.len() > 1));
        let Some(kind) = item_types.first().and_then(|ty| self.kind_of_type(ty).ok()) else {
            return Err(StoreError::Invalid(format!(
                "field {} is not a collection",
                field.id
            )));
        };
        // Writes not sent yet that place items here are read over the store's index; a write of
        // their other fields leaves the listing as the index has it.
        if item_types.len() > 1 || renamed || self.places_written(&kind, &id_of(at)) {
            let ids = self
                .children(at, &field.id)?
                .into_iter()
                .map(|(_, id)| Rc::from(item_of(&id)))
                .collect();
            return Ok(Rc::new(Listing::from_items(ids)));
        }
        Ok(self.paged(at, &field.id, &kind, Vec::new(), Order::default()))
    }

    fn relatives(
        &self,
        at: &ResourceRef,
        field: &FieldId,
        parent: &FieldId,
        node: &ResourceRef,
        down: bool,
    ) -> Result<Option<Vec<(ItemId, TypeId)>>, StoreError> {
        let at = &*self.typed(at);
        let Some(items) = self.field(&at.ty, field).map(|entry| entry.items) else {
            return Ok(None);
        };
        let [item] = items.as_slice() else {
            return Ok(None);
        };
        let kinds = self.kinds_of_type(item)?;
        let [kind] = kinds.as_slice() else {
            return Ok(None);
        };
        // The index answers by the name the field is held under now; an item that still holds
        // it under an earlier one would be missed, so then the caller walks the items itself.
        let renamed = self
            .field(item, parent)
            .is_some_and(|entry| !entry.former.is_empty());
        let name = self
            .entry_of_kind(kind)
            .and_then(|entry| self.indexed_as(parent, &entry));
        let Some(name) = name.filter(|_| !renamed) else {
            return Ok(None);
        };
        let mut walked = std::collections::HashSet::from([id_of(node)]);
        let mut found = Vec::new();
        if down {
            let holder = id_of(at);
            let mut above = vec![id_of(node)];
            let mut under: Vec<(String, String)> = Vec::new();
            while let Some(at_id) = above.pop() {
                for (order, id) in self.children_by(kind, &holder, &name, &at_id)? {
                    if walked.insert(id.clone()) {
                        above.push(id.clone());
                        under.push((order, id));
                    }
                }
            }
            under.sort();
            found.extend(under.into_iter().map(|(_, id)| item_of(&id)));
        } else {
            let mut cur = node.clone();
            while let Some(Cell::One(Value::Text(above))) = self.cell_of(&cur, &name, &[])? {
                if !walked.insert(above.clone()) {
                    break;
                }
                let Some(next) = ResourceRef::parse_id(&above, item.clone()) else {
                    break;
                };
                found.push(item_of(&above));
                cur = next;
            }
        }
        Ok(Some(
            found.into_iter().map(|id| (id, item.clone())).collect(),
        ))
    }

    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        self.listing_query(at, field, item, Some(pred), None)
    }

    fn listing_query(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: Option<&Pred>,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        let at = &*self.typed(at);
        let kind = self.kind_of_type(item)?;
        if let Some(entry) = self.entry_of_kind(&kind) {
            let filters = match pred {
                Some(pred) => self.filters_of(pred, &entry),
                None => Some(Vec::new()),
            };
            let sort = match order {
                Some(order) => self.order_of(order, &entry),
                None => Some(Order::default()),
            };
            if let (Some(filters), Some(sort)) = (filters, sort) {
                if !self.writes_of(&kind) {
                    return Ok(self.paged(at, &field.id, &kind, filters, sort));
                }
                // Writes this store has not sent yet, as an action's own: the index still narrows
                // what is read to the items it holds to the filters, and those this store wrote,
                // and the filters are checked again on what they read now.
                let single = self.field(&at.ty, &field.id).is_some_and(|held| {
                    held.items.len() == 1
                        && self.kinds_of_type(item).is_ok_and(|kinds| kinds.len() == 1)
                });
                if single
                    && order.is_none()
                    && let Some(listing) =
                        self.written_over(at, &field.id, &kind, item, pred, filters.clone())?
                {
                    return Ok(listing);
                }
                if single {
                    let ids = self
                        .children_where(at, &field.id, &filters)?
                        .into_iter()
                        .map(|(_, id)| Rc::from(item_of(&id)))
                        .collect();
                    return self.evaluated(at, ids, item, pred, order);
                }
            }
        }
        let all = self.listing(at, field)?;
        let items = all.items();
        self.evaluated(at, items, item, pred, order)
    }

    fn values(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: Option<&Pred>,
        of: &FieldSpec,
    ) -> Result<Vec<String>, StoreError> {
        let at = &*self.typed(at);
        let kind = self.kind_of_type(item)?;
        // The index answers from what the store holds, under the name the field is held by now:
        // writes not sent yet, and items holding it under an earlier name or kind, are read item
        // by item instead.
        let indexed = self
            .entry_of_kind(&kind)
            .filter(|_| !self.writes_of(&kind))
            .filter(|_| self.kinds_of_type(item).is_ok_and(|kinds| kinds.len() == 1))
            .filter(|entry| {
                entry
                    .fields
                    .get(&of.id)
                    .is_some_and(|held| held.former.is_empty())
            })
            .and_then(|entry| {
                let filters = match pred {
                    Some(pred) => self.filters_of(pred, &entry)?,
                    None => Vec::new(),
                };
                Some((filters, self.indexed_as(&of.id, &entry)?))
            });
        let Some((filters, name)) = indexed else {
            return tilolang_runtime::store::read_values(self, at, field, item, pred, of);
        };
        let values = match self.command(Command::Values {
            kind,
            filters: self.held_in(at, &field.id, filters),
            field: name,
        })? {
            Reply::Values(values) => values,
            other => return Err(backend(format!("asking for values answered {other:?}"))),
        };
        Ok(values
            .into_iter()
            .filter_map(|value| match value {
                Value::Text(text) => Some(text),
                _ => None,
            })
            .collect())
    }

    fn select(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Vec<ResourceRef>, StoreError> {
        let kind = self.kind_of_type(ty)?;
        let entry = self
            .entry_of_kind(&kind)
            .ok_or_else(|| not_found(format!("no type {ty}")))?;
        let filters = self.filters_of(pred, &entry);
        let sort = match order {
            Some(order) => self.order_of(order, &entry),
            None => Some(Order::default()),
        };
        let pushed = filters.is_some() && sort.is_some();
        let query = Query {
            kind,
            filters: filters.unwrap_or_default(),
            order: sort.unwrap_or_default(),
            after: None,
            limit: PAGE,
        };
        let refs: Vec<ResourceRef> = self
            .query_all(query)?
            .into_iter()
            .filter_map(|id| ResourceRef::parse_id(&id, ty.clone()))
            .collect();
        if pushed {
            return Ok(refs);
        }
        let mut held = Vec::new();
        for target in refs {
            if pred.eval(&mut |read: &FieldRead| self.field_lit(&target, read))? {
                held.push(target);
            }
        }
        if let Some(order) = order {
            let levels = order.levels();
            let mut keyed = Vec::with_capacity(held.len());
            for target in held {
                let mut keys = Vec::new();
                for level in &levels {
                    keys.push(self.order_key(&target, &level.field, level.kind)?);
                }
                keyed.push((keys, target));
            }
            keyed.sort_by(|(a, _), (b, _)| ordered_cmp(order, a, b));
            return Ok(keyed.into_iter().map(|(_, target)| target).collect());
        }
        Ok(held)
    }

    fn next_clock_change(
        &self,
        ty: &TypeId,
        fields: &[ClockField],
    ) -> Result<NextChange, StoreError> {
        // A type no package loaded here declares names nothing to wake at: the query that asks
        // of it answers none, as it does for a store that cannot tell.
        let Some(entry) = self.inner.kinds.borrow().get(ty).cloned() else {
            return Ok(NextChange::Unknown);
        };
        let kind = entry.kind.clone();
        let now = tilolang::calendar::instant(&(self.inner.now.borrow())()).unwrap_or(0);
        let mut looked = false;
        let mut next: Option<i64> = None;
        for (field, _day, offset) in fields.iter().filter(|(_, day, _)| !day) {
            let Some(field_entry) = entry.fields.get(field) else {
                continue;
            };
            if !matches!(field_entry.ty, Ty::Time | Ty::Date) {
                continue;
            }
            looked = true;
            let clock = now.saturating_add(*offset);
            // The value the filter compares is the field's, and the clock is ahead of now by
            // the offset: the first item holding one after it is the one that comes next.
            let mut moments = Vec::new();
            if let Some(at) = self.next_value(&kind, &field_entry.name, clock)? {
                moments.push(at);
            }
            moments.extend(self.next_written(ty, field, field_entry, &kind, clock)?);
            // A value `v` the clock passes at `now + offset` changes what the filter holds of
            // its item at `v - offset`: when the clock reaches it, on a whole minute.
            for at in moments {
                let at = minute(at.saturating_sub(*offset));
                next = Some(next.map_or(at, |seen| seen.min(at)));
            }
        }
        if !looked {
            return Ok(NextChange::Unknown);
        }
        Ok(next.map_or(NextChange::Never, NextChange::At))
    }

    fn linked_many(
        &self,
        ats: &[ResourceRef],
        only: Option<&str>,
    ) -> Result<Vec<Vec<Link>>, StoreError> {
        let ats: Vec<ResourceRef> = ats.iter().map(|at| (*self.typed(at)).clone()).collect();
        let mut links: Vec<Vec<Link>> = vec![Vec::new(); ats.len()];
        // The resources asked for, by type: the types that may link to them are found once for
        // each type rather than once for each resource. Each is named with its place in `links`.
        let mut groups: BTreeMap<TypeId, Vec<(usize, ResourceRef)>> = BTreeMap::new();
        for (index, at) in ats.iter().enumerate() {
            groups
                .entry(at.ty.clone())
                .or_default()
                .push((index, at.clone()));
        }
        for (ty, asked) in groups {
            let Some(target) = self.inner.kinds.borrow().get(&ty).cloned() else {
                continue;
            };
            self.links_by_field(&asked, &target, only, &mut links)?;
            self.links_made_from(&asked, only, &mut links)?;
            self.links_by_mention(&asked, only, &mut links)?;
        }
        // What was made from a resource stays in the order it was made, oldest first, as the
        // store answers it; the other links order by the resource's id.
        for links in &mut links {
            links.sort_by(|a, b| {
                a.how.rank().cmp(&b.how.rank()).then_with(|| {
                    if a.how == LinkHow::CreatedFrom {
                        std::cmp::Ordering::Equal
                    } else {
                        a.of.id_string().cmp(&b.of.id_string())
                    }
                })
            });
        }
        Ok(links)
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        let ty = {
            let local = self.inner.local.borrow();
            let cache = self.inner.cache.borrow();
            local
                .get(doc)
                .or_else(|| cache.get(doc))
                .cloned()
                .flatten()
                .and_then(|held| self.type_of_kind(&held.kind))
        };
        let Some(ty) = ty else {
            return Ok(Vec::new());
        };
        let versions = self.versions(&ResourceRef::root(doc.clone(), ty))?;
        Ok(versions
            .into_iter()
            .map(|version| ChangeNote {
                action: version.action,
                invoker: version.who,
                at: Some(version.at),
                touched: vec![version.resource],
                ..ChangeNote::default()
            })
            .collect())
    }

    fn versions_within(
        &self,
        doc: &DocId,
    ) -> Result<std::collections::BTreeMap<String, Vec<Version>>, StoreError> {
        let types: Vec<TypeId> = self
            .inner
            .kinds
            .borrow()
            .types()
            .map(|entry| entry.id.clone())
            .collect();
        let within = format!("{doc}/");
        let mut out = std::collections::BTreeMap::new();
        for ty in types {
            // What it holds and what was removed from it: a removal is a version too.
            let mut ids = self.resources_of(&ty).unwrap_or_default();
            if let Ok(kind) = self.kind_of_type(&ty)
                && let Ok(Reply::Ids(removed)) =
                    self.command(Command::Removed { kind, parent: None })
            {
                ids.extend(removed);
            }
            for id in ids {
                if id != *doc && !id.starts_with(&within) {
                    continue;
                }
                if let Some(at) = ResourceRef::parse_id(&id, ty.clone()) {
                    out.insert(id, Store::versions(self, &at)?);
                }
            }
        }
        Ok(out)
    }

    fn newest_page(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Result<(Vec<(String, Version)>, bool), StoreError> {
        // The page read before stands but for what moved since: each of those is read by its
        // newest version, as a row's mark reads it. A keystroke moves the one resource it
        // types into, so a window's mark reads that one version, whatever the document holds.
        if let Some(read) = self.newest_page_since(doc, page)? {
            return Ok(read);
        }
        let read = self.newest_page_read(doc, page)?;
        self.inner.newest.borrow_mut().insert(
            doc.clone(),
            NewestPage {
                page,
                rows: read.0.clone(),
                more: read.1,
                moved: BTreeSet::new(),
            },
        );
        Ok(read)
    }

    fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        if let Some(read) = self
            .inner
            .versions
            .borrow()
            .get(&id_of(at))
            .and_then(|history| history.all.as_ref())
        {
            return Ok(read.clone());
        }
        let read = self.read_versions(at)?;
        self.inner
            .versions
            .borrow_mut()
            .entry(id_of(at))
            .or_default()
            .all = Some(read.clone());
        Ok(read)
    }

    /// One end of the history of `at`, read as that one version: a root's history names every
    /// item each change put in it, and the whole of it was read, sent and turned into the
    /// runtime's versions for the one at its end. What was made from `at` counts as its last
    /// change when it came after the store's last, as [`Self::read_versions`] places it.
    fn version_end(&self, at: &ResourceRef, last: bool) -> Result<Option<Version>, StoreError> {
        if let Some(history) = self.inner.versions.borrow().get(&id_of(at)) {
            if let Some(all) = &history.all {
                return Ok(if last { all.last() } else { all.first() }.cloned());
            }
            if let Some(end) = history.end(last) {
                return Ok(end.clone());
            }
        }
        let end = self.read_version_end(at, last)?;
        *self
            .inner
            .versions
            .borrow_mut()
            .entry(id_of(at))
            .or_default()
            .end_mut(last) = Some(End(end.clone()));
        Ok(end)
    }

    fn run_changes(&self, run: &str) -> Result<Vec<Version>, StoreError> {
        let reply = self.command(Command::Grouped {
            group: run.to_owned(),
        })?;
        let Reply::Grouped(held) = reply else {
            return Err(backend("a read of a run's changes answered no versions"));
        };
        let mut by_resource: BTreeMap<String, (String, Vec<collab_types::history::Version>)> =
            BTreeMap::new();
        for (kind, id, version) in held {
            by_resource
                .entry(id)
                .or_insert_with(|| (kind, Vec::new()))
                .1
                .push(version);
        }
        let mut out = Vec::new();
        for (id, (kind, held)) in by_resource {
            let entry = self.entry_of_kind(&kind);
            out.extend(self.versions_of(&id, held, entry.as_ref()));
        }
        out.sort_by(|a, b| {
            a.order()
                .cmp(&b.order())
                .then_with(|| a.resource.cmp(&b.resource))
        });
        Ok(out)
    }

    fn removed(
        &self,
        holder: &ResourceRef,
        field: &str,
        item: &TypeId,
    ) -> Result<Vec<(String, Version)>, StoreError> {
        let kind = self.kind_of_type(item)?;
        let held = id_of(holder);
        let Reply::Ids(ids) = self.command(Command::Removed {
            kind,
            parent: Some(held.clone()),
        })?
        else {
            return Err(backend("a read of removed resources answered no ids"));
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(at) = ResourceRef::parse_id(&id, item.clone()) else {
                continue;
            };
            let Some(last) = Store::versions(self, &at)?.pop() else {
                continue;
            };
            let stood_here = last.kind == VersionKind::Removed
                && last
                    .left
                    .as_ref()
                    .is_some_and(|left| left.holder == held && left.field == field);
            if stood_here {
                out.push((id, last));
            }
        }
        out.sort_by(|a, b| a.1.order().cmp(&b.1.order()));
        Ok(out)
    }

    fn read_before(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        change: &str,
    ) -> Result<FieldValue, StoreError> {
        let at = &*self.typed(at);
        let before = self.at_change(at, change, false)?;
        self.read_look(at, field, &before)
    }

    fn read_at(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        change: &str,
    ) -> Result<FieldValue, StoreError> {
        let at = &*self.typed(at);
        if field.kind == FieldKind::Collection {
            return Ok(FieldValue::Items(self.children_at(at, &field.id, change)?));
        }
        let at_version = self.at_change(at, change, true)?;
        self.read_look(at, field, &at_version)
    }

    fn written(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        (first, last): (&str, &str),
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        let at = &*self.typed(at);
        let before = self.at_change(at, first, false)?;
        let after = self.at_change(at, last, true)?;
        let mut written = Vec::new();
        for field in fields {
            let entry = self.field(&at.ty, &field.id);
            let name = entry
                .as_ref()
                .map_or_else(|| field.id.clone(), |entry| entry.name.clone());
            let held = |look: &Look| -> Result<FieldValue, StoreError> {
                let cell = look
                    .indexed
                    .as_ref()
                    .and_then(|held| held.fields.get(&name));
                let json = look.content.as_ref().and_then(|content| content.get(&name));
                self.value_from(field, entry.as_ref(), cell, json)
            };
            let (was, now) = (held(&before)?, held(&after)?);
            if was != now {
                written.push((field.id.clone(), was, now));
            }
        }
        Ok(written)
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId {
        let id = SubId(self.inner.next_sub.get());
        self.inner.next_sub.set(id.0.saturating_add(1));
        self.inner.subs.borrow_mut().push(Subscription {
            id,
            doc: doc.clone(),
            callback: f,
        });
        id
    }

    fn unsubscribe(&mut self, sub: SubId) {
        self.inner.subs.borrow_mut().retain(|held| held.id != sub);
    }

    fn pending_effects(&self) -> Vec<EffectEntry> {
        let device = self.inner.device.borrow().clone();
        self.effect_rows(vec![
            Filter::Is(OUT_SETTLED.to_owned(), Value::Bool(false)),
            Filter::Is(OUT_OWNER.to_owned(), Value::Text(device)),
        ])
        .into_iter()
        .map(|(_, fields)| effect_entry(&fields).0)
        .collect()
    }

    fn next_held_effect(&self) -> Option<String> {
        // Only an entry still waiting for its moment is held: one handed over or waiting to go
        // online had its moment, and nothing comes due when it passes.
        self.pending_effects()
            .into_iter()
            .filter(|entry| entry.state == EffectState::Queued)
            .filter_map(|entry| entry.held_until)
            .min()
    }

    fn withdraw_effects(
        &mut self,
        doc: &DocId,
        reason: &str,
        note: &ChangeNote,
    ) -> Result<usize, StoreError> {
        // Asked by an action while it runs, the withdrawal is one of its writes: it goes, or is
        // taken back, with the rest of them.
        let open = self.inner.txn.borrow().as_ref().map(|state| Txn(state.id));
        if let Some(txn) = open {
            return self.withdraw_in(txn, doc, reason);
        }
        let txn = self.begin()?;
        match self.withdraw_in(txn, doc, reason) {
            Ok(withdrawn) => {
                self.commit(txn, note)?;
                Ok(withdrawn)
            }
            Err(error) => {
                self.rollback(txn);
                Err(error)
            }
        }
    }

    fn mark_effect(
        &mut self,
        entry: &EffectEntry,
        state: &EffectState,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        let (word, detail) = match state {
            EffectState::Queued => ("", ""),
            EffectState::Sending { by } => ("sending", by.as_str()),
            EffectState::Awaiting { by } => ("awaiting", by.as_str()),
            EffectState::Waiting { reason } => ("waiting", reason.as_str()),
        };
        self.effect_write(entry, note, |fields| {
            fields.insert(
                OUT_STATE.to_owned(),
                Cell::One(Value::Text(word.to_owned())),
            );
            fields.insert(
                OUT_DETAIL.to_owned(),
                Cell::One(Value::Text(detail.to_owned())),
            );
        })
    }

    fn settle_effect(
        &mut self,
        entry: &EffectEntry,
        outcome: &Settled,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        self.effect_write(entry, note, |fields| settle_fields(fields, outcome))
    }

    /// Asked of the device's workspace service, which answers from the device's sync, once a
    /// record until it moves.
    fn reach(&self, doc: &DocId) -> RecordReach {
        if let Some(known) = self.inner.reach.borrow().get(doc) {
            return known.clone();
        }
        let known = match self.inner.port.command(Command::Reach {
            record: doc.clone(),
        }) {
            Ok(Reply::Reach(reach)) => RecordReach {
                pending: reach.pending,
                refused: reach.refused,
            },
            Ok(other) => {
                tracing::warn!(
                    ?other,
                    "an ask for a record's reach was answered with something else"
                );
                RecordReach::default()
            }
            Err(error) => {
                tracing::debug!(%error, "a record's reach could not be asked for; it reads as reached");
                RecordReach::default()
            }
        };
        self.inner
            .reach
            .borrow_mut()
            .insert(doc.clone(), known.clone());
        known
    }

    fn effect_states(&self) -> Vec<(DocId, DeliveryState)> {
        let mut by_doc: BTreeMap<DocId, Vec<(EffectEntry, Option<Outcome>)>> = BTreeMap::new();
        for (_, fields) in self.effect_rows(Vec::new()) {
            let (entry, settled) = effect_entry(&fields);
            by_doc
                .entry(entry.doc.clone())
                .or_default()
                .push((entry, settled));
        }
        by_doc
            .into_iter()
            .filter_map(|(doc, mut entries)| {
                entries.sort_by(|a, b| a.0.id.cmp(&b.0.id));
                // The one not settled when there is one, else the last.
                let (entry, settled) = entries
                    .iter()
                    .find(|(_, settled)| settled.is_none())
                    .or_else(|| entries.last())?;
                let settled = settled
                    .as_ref()
                    .map(|(word, reason)| (word.as_str(), reason.as_str()));
                Some((
                    doc,
                    DeliveryState::of_entry(&entry.state, settled, entry.held_until.as_deref()),
                ))
            })
            .collect()
    }

    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError> {
        self.inner
            .journals
            .borrow_mut()
            .insert(run.clone(), journal.clone());
        Ok(())
    }

    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError> {
        Ok(self.inner.journals.borrow().get(run).cloned())
    }
}

/// The field the mutation `m` writes; `None` for one of a collection or an effect entry.
fn field_written(m: &Mutation) -> Option<String> {
    match m {
        Mutation::SetRegister { field, .. }
        | Mutation::AddCounter { field, .. }
        | Mutation::SetText { field, .. }
        | Mutation::SetRich { field, .. }
        | Mutation::SetBytes { field, .. }
        | Mutation::SetAdd { field, .. }
        | Mutation::SetRemove { field, .. }
        | Mutation::MapPut { field, .. }
        | Mutation::MapRemove { field, .. } => Some(field.clone()),
        _ => None,
    }
}

/// Whether a field of type `ty` holds a reference to a resource.
fn names_a_resource(ty: &Ty) -> bool {
    match ty {
        Ty::Record(_) | Ty::Union(_) => true,
        Ty::Option(inner) => names_a_resource(inner),
        _ => false,
    }
}

/// Whether a field of type `ty` names a resource of the type `target`.
fn refers_to(ty: &Ty, target: &TypeEntry) -> bool {
    match ty {
        Ty::Record(key) => {
            tilolang::types::is_any_resource(key)
                || key.app == target.app && key.name == target.name
        }
        Ty::Option(inner) | Ty::Set(inner) | Ty::List(inner) => refers_to(inner, target),
        Ty::Union(keys) => keys
            .iter()
            .any(|key| key.app == target.app && key.name == target.name),
        _ => false,
    }
}

impl ResourceStore {
    /// The versions of `at` as the store reads them, with what was made from it.
    fn read_versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        let at = &*self.typed(at);
        // A resource of a type no package here declares is another app's, which keeps its
        // history: none of it is here.
        let Ok(kind) = self.kind_of_type(&at.ty) else {
            return Ok(Vec::new());
        };
        let entry = self.entry_of_kind(&kind);
        let reply = self.command(Command::Versions {
            kind,
            id: id_of(at),
        })?;
        let Reply::Versions(held) = reply else {
            return Err(backend("a read of versions answered no versions"));
        };
        let mut versions = self.versions_of(&id_of(at), held, entry.as_ref());
        // What was made from a resource is a change to it too: a comment made from a task
        // changed that task, for whoever made or read it, though none of its fields.
        // Each stands after the resource's own versions of its time or before it, which keep the
        // order the store reads them in.
        for made in self.made_from(at)? {
            let place = versions
                .iter()
                .rposition(|version| version.at <= made.at)
                .map_or_else(|| versions.len().min(1), |place| place.saturating_add(1));
            versions.insert(place.max(1).min(versions.len()), made);
        }
        Ok(versions)
    }
}

/// The value the store holds `lit` as, the cases of `kinds`' enums by name; `None` for a set,
/// which no single value is.
pub fn store_value(lit: &Lit, kinds: &Kinds) -> Option<Value> {
    Some(match lit {
        Lit::Bool(flag) => Value::Bool(*flag),
        Lit::Int(number) => Value::Int(*number),
        Lit::Float(number) => Value::Float(number.0),
        Lit::Text(text) | Lit::Date(text) | Lit::Time(text) | Lit::Ref(text) | Lit::Ident(text) => {
            Value::Text(text.clone())
        }
        Lit::Variant(id) => Value::Text(kinds.variant_name(id)?),
        Lit::Absent => Value::Null,
        Lit::Actor(actor) => Value::Text(tilolang_runtime::ids::actor_text(actor)),
        Lit::Set(_) => return None,
    })
}

/// `pred` as the store's filter: each field as `field` names it, the name the index holds it under
/// and what a resource that never wrote it holds; each literal as `value_of` stores it. `None`
/// where the store cannot say it: what a query then checks item by item, and a type's live
/// condition the store cannot keep.
pub fn store_filter<F>(
    pred: &tilolang::pred::Pred<F>,
    field: &dyn Fn(&F) -> Option<(String, Lit)>,
    value_of: &dyn Fn(&Lit) -> Option<Value>,
) -> Option<Filter> {
    use tilolang::pred::{CmpOp, Operand, Pred};
    let always = || Filter::Not(Box::new(Filter::Any(Vec::new())));
    Some(match pred {
        Pred::Const(true) => always(),
        Pred::Const(false) => Filter::Any(Vec::new()),
        Pred::Not(inner) => Filter::Not(Box::new(store_filter(inner, field, value_of)?)),
        Pred::And(all) => {
            let filters = all
                .iter()
                .map(|pred| store_filter(pred, field, value_of))
                .collect::<Option<Vec<_>>>()?;
            // Not any of the negations.
            Filter::Not(Box::new(Filter::Any(
                filters
                    .into_iter()
                    .map(|filter| Filter::Not(Box::new(filter)))
                    .collect(),
            )))
        }
        Pred::Or(any) => Filter::Any(
            any.iter()
                .map(|pred| store_filter(pred, field, value_of))
                .collect::<Option<Vec<_>>>()?,
        ),
        Pred::Cmp(op, Operand::Field(read), Operand::Lit(lit)) => {
            let (name, absent) = field(read)?;
            let value = value_of(lit)?;
            // Whether a resource nobody wrote the field of holds to it: it reads the field's
            // default, which compares as the language compares it (an absent value equals
            // only another, and orders with nothing).
            let unwritten_holds = absent.compare(*op, lit) == Some(true);
            if !matches!(op, CmpOp::Eq | CmpOp::Ne) && matches!(lit, Lit::Variant(_)) {
                return None;
            }
            let held = match op {
                CmpOp::Eq => Filter::Is(name.clone(), value.clone()),
                CmpOp::Ne => Filter::Not(Box::new(Filter::Is(name.clone(), value.clone()))),
                CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                    use std::ops::Bound;
                    let (from, to) = match op {
                        CmpOp::Lt => (Bound::Unbounded, Bound::Excluded(value.clone())),
                        CmpOp::Le => (Bound::Unbounded, Bound::Included(value.clone())),
                        CmpOp::Gt => (Bound::Excluded(value.clone()), Bound::Unbounded),
                        _ => (Bound::Included(value.clone()), Bound::Unbounded),
                    };
                    Filter::Not(Box::new(Filter::Any(vec![
                        Filter::Not(Box::new(Filter::Present(name.clone()))),
                        Filter::Not(Box::new(Filter::Within {
                            field: name.clone(),
                            from,
                            to,
                        })),
                    ])))
                }
            };
            // A resource nobody wrote the field of reads its default.
            let absent = Filter::Not(Box::new(Filter::Present(name.clone())));
            match op {
                CmpOp::Ne if !unwritten_holds => {
                    Filter::Not(Box::new(Filter::Any(vec![Filter::Is(name, value), absent])))
                }
                CmpOp::Ne => Filter::Not(Box::new(Filter::Is(name, value))),
                _ if unwritten_holds => Filter::Any(vec![held, absent]),
                _ => held,
            }
        }
        Pred::Has(read, lit) => {
            let (name, _) = field(read)?;
            Filter::Has(name, value_of(lit)?)
        }
        Pred::Text(read, text) => {
            let (name, _) = field(read)?;
            Filter::Contains(name, text.clone())
        }
        Pred::Like(read, pattern) => {
            let (name, _) = field(read)?;
            Filter::Like(name, pattern.clone())
        }
        Pred::Present(read) => {
            let (name, _) = field(read)?;
            Filter::Present(name)
        }
        Pred::Cmp(..) => return None,
    })
}

/// `filter` as the conjunction it is: the filters a query lists, every one of which holds.
#[must_use]
pub fn conjunction(filter: Filter) -> Vec<Filter> {
    let mut all = Vec::new();
    conjuncts(filter, &mut all);
    all
}

/// Whether the open listing kept under `key` lists resources of `kind`.
/// Adds the filters every one of which `filter` holds to `all`: a query's filters all hold, and
/// one the store's index narrows by (a value, a range) is one of them only when it stands alone,
/// not inside the negation a conjunction is written as.
fn conjuncts(filter: Filter, all: &mut Vec<Filter>) {
    match filter {
        // Not any of the negations of each.
        Filter::Not(inner) if matches!(&*inner, Filter::Any(any) if any.iter().all(|f| matches!(f, Filter::Not(_)))) =>
        {
            let Filter::Any(any) = *inner else {
                return;
            };
            for negated in any {
                if let Filter::Not(each) = negated {
                    conjuncts(*each, all);
                }
            }
        }
        other => all.push(other),
    }
}

fn listing_of(key: &str, kind: &str) -> bool {
    key.strip_prefix(kind)
        .is_some_and(|rest| rest.starts_with('#'))
}

/// The resources a write names, by kind: those it puts, removes or checkpoints, whose place in
/// the index can change, and those it only edits the content of, whose place cannot.
#[derive(Default)]
struct Written {
    reshaped: BTreeMap<String, BTreeSet<String>>,
    edited: BTreeMap<String, BTreeSet<String>>,
}

impl Written {
    /// The resources the write edits and does not also put, remove or checkpoint.
    fn edited_only(&self) -> BTreeMap<String, BTreeSet<String>> {
        self.edited
            .iter()
            .filter_map(|(kind, ids)| {
                let reshaped = self.reshaped.get(kind);
                let only: BTreeSet<String> = ids
                    .iter()
                    .filter(|id| !reshaped.is_some_and(|held| held.contains(*id)))
                    .cloned()
                    .collect();
                (!only.is_empty()).then(|| (kind.clone(), only))
            })
            .collect()
    }
}

/// Adds the resources `command` writes to `written`, by kind; `false` when it writes resources
/// it does not name (an undo).
fn ids_written(command: &Command, written: &mut Written) -> bool {
    match command {
        Command::Edit { kind, id, .. } => {
            written
                .edited
                .entry(kind.clone())
                .or_default()
                .insert(id.clone());
            true
        }
        Command::Put { kind, id, .. }
        | Command::Remove { kind, id, .. }
        | Command::Checkpoint { kind, id, .. } => {
            written
                .reshaped
                .entry(kind.clone())
                .or_default()
                .insert(id.clone());
            true
        }
        Command::Batch(commands) => commands.iter().all(|command| ids_written(command, written)),
        Command::Undo { .. } | Command::UndoMany { .. } | Command::Restore { .. } => false,
        _ => true,
    }
}
