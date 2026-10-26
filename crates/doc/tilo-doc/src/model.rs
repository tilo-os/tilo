//! Borrowed-view model operations over a `&mut AutoCommit`, so a document can live
//! in any host's automerge doc (e.g. `ui::State.doc`), not only an owned [`crate::Document`].

use crate::document::{DocError, MarkSpan};
use crate::schema::{
    ATTR_BLOB_BYTES, ATTR_BLOB_H, ATTR_BLOB_MIME, ATTR_BLOB_W, BLOB_H, BLOB_LEN, BLOB_MIME, BLOB_W,
    BLOCK_ATTRS, BLOCK_CHILDREN, BLOCK_KIND, BLOCK_ORDER, BLOCK_TEXT, BLOCK_TOMBSTONE_PREFIX,
    CHANGE_AUTHOR, CHANGE_DATE, CHANGE_KIND, COMMENT_AUTHOR, COMMENT_BLOCKS, COMMENT_DATE,
    COMMENT_REPLY_AUTHOR, COMMENT_REPLY_BLOCKS, COMMENT_REPLY_DATE, COMMENT_REPLY_PARENT,
    COMMENT_TOMBSTONE_PREFIX, CROSS_REF_PREFIX, DEFAULT_COLUMNS, FIELD_PAGE_NUMBER,
    FIELD_PLACEHOLDER, HEADING_REF_PREFIX, KEY_BLOBS, KEY_BLOCKS, KEY_CHANGES, KEY_COMMENTS,
    KEY_FOOTER, KEY_HEADER, KEY_META, KEY_NOTES, KEY_PRESERVED, KEY_PROPS, KEY_SCHEMA,
    KEY_SCHEMA_VERSION, KEY_STYLES, MARK_BOOKMARK, MARK_COMMENT, MARK_DEL, MARK_FIELD, MARK_INS,
    MARK_LINK, MARK_NOTE_REF, META_COLUMN_GAP, META_COLUMNS, META_LANGUAGE, NOTE_BLOCKS, NOTE_KIND,
    NOTE_KIND_ENDNOTE, NOTE_KIND_FOOTNOTE, PRESERVED_SEG_BYTES, PRESERVED_SEG_ORD, SCHEMA_ID,
    SCHEMA_VERSION, STYLE_TOMBSTONE_PREFIX,
};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{
    AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, ScalarValueRef, Value, ValueRef,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::Arc;
use tilo_automerge::ApplyIncoming;
use tilo_automerge::ReadTyped as _;
use tilo_automerge::read::{map_at, str_at, str_entries};
use tilo_content::{ContentInputs, ContentMeta, ContentSource};
use tilo_sort::CompactSort as _;

mod bulk;
/// Instrumentation for tests comparing the bulk import path with the op-by-op reference.
#[cfg(any(test, feature = "bulk-instrumentation"))]
#[doc(hidden)]
pub use bulk::{change_writes, op_by_op};
#[cfg(test)]
thread_local! { static MARK_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Resolved object ids for the canonical container maps under a document root.
#[derive(Clone, Debug)]
pub struct Containers {
    /// The document root these containers live under (not always `automerge::ROOT` — a host may nest the model).
    pub root: ObjId,
    pub blocks: ObjId,
    pub styles: ObjId,
    pub preserved: ObjId,
    pub meta: ObjId,
    /// Content-addressed binary descriptors (blake3 hex → descriptor). The bytes are binary inputs kept
    /// outside the document, in the caller's content store (see [`tilo_content`]). The map only grows, so it
    /// names every input any version of the document refers to.
    pub blobs: ObjId,
    /// Page-header block container (same shape as `blocks`).
    pub header: ObjId,
    /// Page-footer block container (same shape as `blocks`), mirroring `header`.
    pub footer: ObjId,
    /// Footnote/endnote container (`Map<note_id, note>`).
    pub notes: ObjId,
    /// Comment/annotation container (`Map<comment_id, comment>`).
    pub comments: ObjId,
    /// Tracked-changes container (`Map<change_id, change>`).
    pub changes: ObjId,
}

/// Which page region a block container refers to. The body is the canonical
/// document flow; header/footer are the per-page repeated regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Body,
    Header,
    Footer,
}

/// Which kind of note: a footnote (bottom of page) or an endnote (end of document).
/// Stored as the note's `kind` scalar and round-tripped through the codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Footnote,
    Endnote,
}

impl NoteKind {
    /// The canonical schema string (`"footnote"`/`"endnote"`).
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Footnote => NOTE_KIND_FOOTNOTE,
            NoteKind::Endnote => NOTE_KIND_ENDNOTE,
        }
    }
    /// Parse a schema string back into a [`NoteKind`] (`endnote` → `Endnote`,
    /// anything else → `Footnote`, the default).
    #[allow(
        clippy::should_implement_trait,
        reason = "returns Self infallibly by design, not std::str::FromStr's fallible contract; renaming would ripple into sibling codec files outside this pass's scope"
    )]
    pub fn from_str(s: &str) -> Self {
        if s == NOTE_KIND_ENDNOTE {
            NoteKind::Endnote
        } else {
            NoteKind::Footnote
        }
    }
}

/// A block's `(order_key, id)` pair, ordered exactly as [`ordered_keys`] orders blocks
/// (fractional key via [`tilo_resource::order::bytes::cmp`], then id as a tiebreak). Used as the in-memory order index's key type.
/// Key and id are shared with the `id → key` map, so the index holds each only once.
#[derive(Clone, PartialEq, Eq)]
struct OrderKey {
    key: Arc<[u8]>,
    id: Arc<str>,
}

impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> Ordering {
        tilo_resource::order::bytes::cmp(&self.key, &other.key).then_with(|| self.id.cmp(&other.id))
    }
}
impl PartialOrd for OrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One container's block order held in memory: the ordered `(key, id)` set (O(log n) successor lookup)
/// plus an `id → key` map (O(1) predecessor lookup). Mirrors one block container's automerge order.
#[derive(Default)]
struct ContainerOrder {
    entries: BTreeSet<OrderKey>,
    by_id: HashMap<Arc<str>, Arc<[u8]>>,
}

impl ContainerOrder {
    fn insert(&mut self, id: Arc<str>, key: Arc<[u8]>) {
        self.remove(&id);
        self.by_id.insert(Arc::clone(&id), Arc::clone(&key));
        self.entries.insert(OrderKey { key, id });
    }

    fn remove(&mut self, id: &str) {
        if let Some((id, key)) = self.by_id.remove_entry(id) {
            self.entries.remove(&OrderKey { key, id });
        }
    }
}

/// A lazily-built, incrementally-maintained index of every touched block container's order.
/// Makes block insertion sub-linear: the naive rescan-and-resort on every insert was O(n log n)
/// each (O(n² log n) to build/import n blocks); with this index a neighbour lookup is O(log n).
///
/// It is a per-[`IdGen`] mirror kept in sync by every order-mutating [`DocMut`] method; a fresh
/// `IdGen` starts empty and rebuilds from the live document on first use, so it can't outlive what it mirrors.
#[derive(Default)]
struct OrderIndex {
    by_container: HashMap<ObjId, ContainerOrder>,
}

/// The last block placed by an insert that did not build the order index:
/// where it went and the next block's key, which bounds the next insert after
/// it too.
struct InsertRun {
    container: ObjId,
    last: String,
    key: Vec<u8>,
    hi: Option<Vec<u8>>,
}

/// Collision-free block id generator.
///
/// Inside an action it mints from the action's deterministic context
/// ([`Self::for_action`]): ids are then a function of the action and the
/// document it applies to, identical on the origin and on every replica
/// replaying it, and minting costs nothing however large the document.
/// Writers outside an action (an importer, an owned [`crate::Document`])
/// count per replica: `{actor_hex}.{counter}`.
pub struct IdGen {
    actor_hex: String,
    next: Option<u64>,
    /// The action minting, when writing inside one.
    action: Option<tilo_resource::ActionCx>,
    /// The key an insert just placed, and its upper bound, until the block
    /// is recorded (see [`DocMut::order_after_in`]).
    placing: Option<(ObjId, Vec<u8>, Option<Vec<u8>>)>,
    /// The last block inserted without the order index, so the next insert
    /// after it (a paste, a table's cells) needs no scan.
    run: Option<InsertRun>,
    /// In-memory block-order index (see [`OrderIndex`]); private bookkeeping carried on `IdGen` since it threads through every `DocMut`.
    order: OrderIndex,
    /// Blocks and marks buffered by an open [`DocMut::bulk`] scope.
    bulk: bulk::Bulk,
    /// Bulk writes may commit (see [`Self::allow_commits`]).
    commits_allowed: bool,
    /// Made by [`Self::new`] rather than seeded from a document: its caller is creating content,
    /// not editing a document it read ids from. See [`DocMut::bulk`].
    unseeded: bool,
}

/// Clone for transactional staging against a cloned document, with a fresh
/// derived ordering cache. On success the candidate document and generator must
/// replace their originals together; on failure discard both candidates.
///
/// The original and clone share an actor and next counter. They must never be
/// used as two concurrently allocating live generators: that would reuse durable
/// identifiers. Independent replicas require distinct actor identities.
impl Clone for IdGen {
    fn clone(&self) -> Self {
        Self {
            actor_hex: self.actor_hex.clone(),
            // Deleted IDs remain part of durable history. A staged edit must
            // inherit the counter rather than reseeding from visible objects.
            next: self.next,
            action: None,
            placing: None,
            run: None,
            order: OrderIndex::default(),
            bulk: bulk::Bulk::default(),
            commits_allowed: false,
            unseeded: false,
        }
    }
}

/// The finite per-actor block-id namespace has been consumed.
///
/// tilo-doc identifiers are durable collaboration keys, so reusing the final
/// counter (or wrapping to zero) would alias existing content. Callers must
/// create a new replica/document instead of continuing this namespace.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdGenError {
    #[error("tilo-doc id namespace for actor {actor_hex} is exhausted")]
    Exhausted { actor_hex: String },
}

impl IdGen {
    pub fn new(actor_hex: String) -> Self {
        Self {
            actor_hex,
            next: Some(0),
            action: None,
            placing: None,
            run: None,
            order: OrderIndex::default(),
            bulk: bulk::Bulk::default(),
            commits_allowed: false,
            unseeded: true,
        }
    }
    /// Start the counter at `next` — used to seed past ids that already exist in a loaded/opened
    /// document (otherwise a fresh generator re-mints existing ids and overwrites those blocks).
    pub fn with_counter(actor_hex: String, next: u64) -> Self {
        Self {
            actor_hex,
            next: Some(next),
            action: None,
            placing: None,
            run: None,
            order: OrderIndex::default(),
            bulk: bulk::Bulk::default(),
            commits_allowed: false,
            unseeded: false,
        }
    }

    /// Mint ids from an applying action's context: deterministic under
    /// replay, and independent of the document's size (no scan of existing
    /// ids — an action's context is unique to the action and the version it
    /// applies to, so its ids cannot collide with ids minted elsewhere).
    pub fn for_action(cx: tilo_resource::ActionCx) -> Self {
        Self {
            actor_hex: String::new(),
            next: None,
            action: Some(cx),
            placing: None,
            run: None,
            order: OrderIndex::default(),
            bulk: bulk::Bulk::default(),
            commits_allowed: false,
            unseeded: false,
        }
    }

    /// Declare that the document this generator serves may be committed by bulk writes: a
    /// [`DocMut::bulk`] scope then commits whatever is pending and writes its blocks and marks as
    /// new changes, which is several times faster for mark-heavy imports. [`crate::Document`]
    /// sets this. A document being created — nothing but the schema in it, written through a
    /// generator from [`Self::new`] — gets the same treatment without it (see [`DocMut::bulk`]),
    /// so a host opening a file into a fresh document need not set it.
    pub fn allow_commits(&mut self) {
        self.commits_allowed = true;
    }
    /// Mint a durable identifier, failing closed once this actor has used all
    /// `u64` counter values.
    ///
    /// # Errors
    ///
    /// Returns [`IdGenError::Exhausted`] after the final counter has been
    /// issued, so a durable identifier can never be reused.
    pub fn try_mint(&mut self) -> Result<String, IdGenError> {
        if let Some(action) = &mut self.action {
            return Ok(action.id().as_str().to_owned());
        }
        let counter = self.next.ok_or_else(|| IdGenError::Exhausted {
            actor_hex: self.actor_hex.clone(),
        })?;
        let id = format!("{}.{}", self.actor_hex, counter);
        self.next = counter.checked_add(1);
        Ok(id)
    }

    /// Compatibility helper for infallible document mutators.
    ///
    /// New code that can surface an error should use [`Self::try_mint`]. The
    /// legacy mutator APIs return raw identifiers, so exhaustion aborts rather
    /// than silently colliding with a durable block id.
    #[allow(
        clippy::expect_used,
        reason = "the legacy API cannot return an error; panic is the explicit fail-closed behavior"
    )]
    pub fn mint(&mut self) -> String {
        self.try_mint()
            .expect("tilo-doc id namespace exhausted; refusing to reuse an identifier")
    }
}

/// The next free id counter for `actor_hex`, scanning the existing block ids so a
/// generator created over an already-populated document won't collide.
///
/// # Errors
///
/// Returns [`IdGenError::Exhausted`] when the document already holds this
/// actor's final possible identifier.
pub fn next_counter_for(
    doc: &AutoCommit,
    c: &Containers,
    actor_hex: &str,
) -> Result<u64, IdGenError> {
    let prefix = format!("{actor_hex}.");
    let mut next = Some(0u64);
    let mut bump = |id: &str| {
        if let Some(rest) = id.strip_prefix(&prefix)
            && let Ok(n) = rest.parse::<u64>()
        {
            next = match (next, n.checked_add(1)) {
                (Some(current), Some(candidate)) => Some(current.max(candidate)),
                (_, None) | (None, _) => None,
            };
        }
    };
    // Every block object counts, ordered or not: seeding only has to stay clear of minted ids, and
    // listing keys skips reading each block's fields.
    for id in block_ids(doc, &c.blocks) {
        bump(&id);
    }
    // Header/footer blocks, note ids, and note-body blocks draw from the same per-replica
    // generator, so a seeded generator must scan them too or it could re-mint an existing one.
    for region in [Region::Header, Region::Footer] {
        for id in block_ids(doc, region_obj(c, region)) {
            bump(&id);
        }
    }
    for nid in note_ids(doc, c) {
        bump(&nid);
        for bid in note_blocks_in_order(doc, c, &nid) {
            bump(&bid);
        }
    }
    // Comment ids + comment-body block ids draw from the same generator too, for the same reason.
    let mut replies = comment_replies_by_parent(doc, c);
    for cid in comment_ids(doc, c) {
        bump(&cid);
        for bid in comment_blocks_in_order(doc, c, &cid) {
            bump(&bid);
        }
        for rid in replies.remove(&cid).unwrap_or_default() {
            bump(&rid);
            for bid in comment_reply_blocks_in_order(doc, c, &cid, &rid) {
                bump(&bid);
            }
        }
    }
    // Tracked-change ids draw from the same generator too, for the same reason.
    for chid in change_ids(doc, c) {
        bump(&chid);
    }
    next.ok_or_else(|| IdGenError::Exhausted {
        actor_hex: actor_hex.to_owned(),
    })
}

/// The keys of every block object (map-valued entry) in a block container, unordered.
fn block_ids(doc: &AutoCommit, container: &ObjId) -> Vec<String> {
    doc.map_range(container, ..)
        .filter(|item| matches!(item.value, ValueRef::Object(ObjType::Map)))
        .map(|item| item.key.into_owned())
        .collect()
}

/// Try to construct an [`IdGen`] for `doc`'s current actor, seeded past
/// existing block ids.
///
/// # Errors
///
/// Returns [`IdGenError::Exhausted`] when a persisted document already
/// contains this actor's final possible identifier.
pub fn try_seeded_id_gen(doc: &AutoCommit, c: &Containers) -> Result<IdGen, IdGenError> {
    let actor = doc.get_actor().to_hex_string();
    let next = next_counter_for(doc, c, &actor)?;
    Ok(IdGen::with_counter(actor, next))
}

/// An [`IdGen`] for `doc`'s current actor, seeded past existing block ids.
///
/// Compatibility helper for existing infallible host paths. New load and
/// persistence boundaries should call [`try_seeded_id_gen`] to return the
/// exhaustion error to their caller.
///
/// # Panics
///
/// Panics when the persisted document already used this actor's final
/// possible identifier; continuing would otherwise reuse durable state.
#[allow(
    clippy::expect_used,
    reason = "the established compatibility API cannot return an error; panic is explicit fail-closed behavior"
)]
pub fn seeded_id_gen(doc: &AutoCommit, c: &Containers) -> IdGen {
    try_seeded_id_gen(doc, c)
        .expect("tilo-doc id namespace exhausted; refusing to seed a colliding identifier")
}

/// The actor that writes every document's schema.
///
/// Nobody edits as this actor; it is not a replica. It exists so the ops that
/// create the container maps are *the same ops* on every machine — same actor,
/// same counters, therefore the same `ObjId`s and the same change hash.
///
/// Minting the schema per replica cannot work. `put_object(root, "blocks", Map)`
/// creates a new object and points the key at it, so two replicas that each run
/// it have written the same key concurrently with no ancestor between them.
/// After a merge `root.blocks` has two candidates and automerge keeps exactly
/// one — the op with the greater `(counter, actor)`. The counters are equal
/// (both ran the identical sequence), so the winner is decided by comparing two
/// random actor ids, and the loser's whole block tree stays in the change graph
/// unreachable from the root, which reads exactly like it was never written.
/// Deriving the schema from one genesis removes the conflict rather than
/// resolving it: both replicas hold the *same change*, automerge deduplicates
/// it, and `root.blocks` has one candidate however many participants there are
/// and whoever imported first.
const GENESIS_ACTOR: &[u8] = b"qdoc.schema.genesis.v1";

/// Whether `doc` has no committed history beyond the schema [`init`] adopts: nothing written to it
/// yet that anything could have seen or bracketed an undo step against.
pub(crate) fn holds_only_schema(doc: &AutoCommit) -> bool {
    static GENESIS_HEADS: std::sync::OnceLock<Vec<automerge::ChangeHash>> =
        std::sync::OnceLock::new();
    let heads = doc.base_heads();
    heads.is_empty()
        || heads
            == *GENESIS_HEADS.get_or_init(|| {
                AutoCommit::load(genesis())
                    .map(|mut d| d.get_heads())
                    .unwrap_or_default()
            })
}

/// The canonical schema change, built once per process.
///
/// A fixed actor replaying a fixed op sequence over an empty document, so every
/// machine produces byte-identical output. The bytes are only a carrier for a
/// change hash that has to agree across the room.
fn genesis() -> &'static [u8] {
    static GENESIS: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    GENESIS.get_or_init(|| {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(GENESIS_ACTOR));
        write_schema(&mut doc, &ROOT);
        doc.commit();
        doc.save()
    })
}

/// Give `doc` the canonical schema under `root`, and resolve the containers.
///
/// At the document root the schema is *adopted* from the shared `genesis`
/// rather than minted, so two replicas that each open the same file are editing
/// one document and not two that merge lossily (see `GENESIS_ACTOR`).
/// Idempotent: a document that already carries the schema is returned as it
/// stands, because minting a second set of containers is the same conflict
/// arrived at from one process instead of two.
///
/// A **nested** root (the FS layer embeds a document inside a namespace node)
/// cannot use the genesis: its parent object id is minted by whichever replica
/// created the node, so no pre-baked change can name it. Those documents are
/// created once and then synced rather than created twice and merged, so the
/// schema is written in place.
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> Containers {
    if let Ok(existing) = open(doc, root) {
        return existing;
    }
    if root == &ROOT {
        // The genesis has no dependencies, so it applies to any document, and a
        // replica already holding it is unaffected: same hash, one copy kept.
        if doc.load_incoming(genesis()).is_ok()
            && let Ok(adopted) = open(doc, root)
        {
            return adopted;
        }
        // Falling back to a private schema would reintroduce exactly the
        // silent-loss conflict the genesis exists to prevent, so say so.
        tracing::error!(
            "tilo-doc: could not adopt the shared schema genesis; this document's \
             containers are private to this replica and merging it with another \
             replica's copy will drop one side's content"
        );
    }
    write_schema(doc, root)
}

/// Write schema scalars + create the container maps under `root`, in place.
///
/// This op sequence *is* the schema, and [`genesis`] replays it under a fixed
/// actor. Adding, removing or reordering an op changes the genesis hash, which
/// is a schema migration for every document in the world, not a local edit.
///
/// # Panics
/// Panics if `root` names an object that doesn't exist in `doc`. Every caller resolves
/// `root` (`automerge::ROOT`, or a namespace node it just created) immediately before
/// calling this, so the object is always live; [`init`]'s own callers across the
/// workspace depend on its infallible `Containers` return, so surfacing this as a
/// `Result` here would be a public-API break out of scope for a lint-only pass.
#[allow(
    clippy::unwrap_used,
    reason = "root is always a live object at every call site (see the panics note above); init()'s widely-depended-upon infallible signature rules out propagating a Result"
)]
fn write_schema(doc: &mut AutoCommit, root: &ObjId) -> Containers {
    doc.put(root, KEY_SCHEMA, SCHEMA_ID).unwrap();
    doc.put(root, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
    let blocks = doc.put_object(root, KEY_BLOCKS, ObjType::Map).unwrap();
    let styles = doc.put_object(root, KEY_STYLES, ObjType::Map).unwrap();
    let preserved = doc.put_object(root, KEY_PRESERVED, ObjType::Map).unwrap();
    let meta = doc.put_object(root, KEY_META, ObjType::Map).unwrap();
    let blobs = doc.put_object(root, KEY_BLOBS, ObjType::Map).unwrap();
    // Every container is created here rather than on first write. A lazily
    // created map is a per-replica object, so two people who concurrently add a
    // footnote — or a comment, a tracked change, a page header — would create
    // two containers and a merge would keep one, which is the same silent loss
    // the genesis exists to prevent. Present-and-empty is indistinguishable from absent to every reader, and
    // both office codecs decide a document *has* a header from whether the
    // region materializes blocks, so an empty one exports nothing.
    let notes = doc.put_object(root, KEY_NOTES, ObjType::Map).unwrap();
    let comments = doc.put_object(root, KEY_COMMENTS, ObjType::Map).unwrap();
    let changes = doc.put_object(root, KEY_CHANGES, ObjType::Map).unwrap();
    let header = doc.put_object(root, KEY_HEADER, ObjType::Map).unwrap();
    let footer = doc.put_object(root, KEY_FOOTER, ObjType::Map).unwrap();
    Containers {
        root: root.clone(),
        blocks,
        styles,
        preserved,
        meta,
        blobs,
        header,
        footer,
        notes,
        comments,
        changes,
    }
}

/// Whether `actor` wrote the schema rather than any of the document's content.
///
/// Nobody edits as the genesis actor, so a change it authored is not a revision:
/// a version history that lists it attributes one to a person who does not exist.
pub fn is_schema_genesis_actor(actor: &automerge::ActorId) -> bool {
    actor.to_bytes() == GENESIS_ACTOR
}

/// Whether `version` is the schema genesis rather than something a person
/// wrote. See [`is_schema_genesis_actor`].
pub fn is_schema_genesis(version: &tilo_resource::history::Version) -> bool {
    version.actor == schema_genesis_actor_hex()
}

/// The schema genesis actor as hex, for comparing against a recorded
/// actor. See [`is_schema_genesis_actor`].
pub fn schema_genesis_actor_hex() -> String {
    automerge::ActorId::from(GENESIS_ACTOR).to_hex_string()
}

/// The keys a document is expected to reach exactly one object through.
const CONTAINER_KEYS: &[&str] = &[
    KEY_BLOCKS,
    KEY_STYLES,
    KEY_PRESERVED,
    KEY_META,
    KEY_BLOBS,
    KEY_HEADER,
    KEY_FOOTER,
    KEY_NOTES,
    KEY_COMMENTS,
    KEY_CHANGES,
];

/// Container keys under `root` that resolve to **more than one** object.
///
/// A non-empty result means content sits in the change graph that no read of
/// this document will ever return: automerge picked one candidate and everything
/// under the others is unreachable. It is the signature of two replicas having
/// minted their own schema — a document written before the genesis existed
/// meeting one written after it. One `get_all` per key, so a caller that merges
/// foreign documents can afford to check and then refuse, or warn, instead of
/// showing someone a document that quietly lost half of itself.
pub fn shadowed_containers(doc: &AutoCommit, root: &ObjId) -> Vec<String> {
    CONTAINER_KEYS
        .iter()
        .filter(|key| doc.get_all(root, **key).is_ok_and(|found| found.len() > 1))
        .map(|key| (*key).to_string())
        .collect()
}

/// Resolve existing containers under `root`.
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<Containers, DocError> {
    // One walk over `root` finds every container: a record holding rich text
    // in each of its items opens one per item, and a lookup per key seeks
    // into the operations again each time.
    let mut maps: HashMap<&'static str, ObjId> = HashMap::with_capacity(CONTAINER_KEYS.len());
    for item in doc.map_range(root, ..) {
        if matches!(item.value, ValueRef::Object(ObjType::Map))
            && let Some(key) = CONTAINER_KEYS.iter().find(|key| **key == item.key.as_ref())
        {
            maps.insert(*key, item.id());
        }
    }
    let mut take = |key: &str| {
        maps.remove(key)
            .ok_or_else(|| DocError::Invalid(format!("root.{key} is not a map")))
    };
    Ok(Containers {
        root: root.clone(),
        blocks: take(KEY_BLOCKS)?,
        styles: take(KEY_STYLES)?,
        preserved: take(KEY_PRESERVED)?,
        meta: take(KEY_META)?,
        blobs: take(KEY_BLOBS)?,
        header: take(KEY_HEADER)?,
        footer: take(KEY_FOOTER)?,
        notes: take(KEY_NOTES)?,
        comments: take(KEY_COMMENTS)?,
        changes: take(KEY_CHANGES)?,
    })
}

pub(crate) fn required_map(doc: &AutoCommit, root: &ObjId, key: &str) -> Result<ObjId, DocError> {
    match doc.get(root, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}

/// A region's block container object.
fn region_obj(c: &Containers, region: Region) -> &ObjId {
    match region {
        Region::Body => &c.blocks,
        Region::Header => &c.header,
        Region::Footer => &c.footer,
    }
}

/// A region block's `(container, block)` objects.
fn region_block(
    doc: &AutoCommit,
    c: &Containers,
    region: Region,
    id: &str,
) -> Option<(ObjId, ObjId)> {
    let container = region_obj(c, region).clone();
    let obj = block_obj(doc, &container, id)?;
    Some((container, obj))
}

/// The block object of `id` in `region`.
pub fn region_block_obj(
    doc: &AutoCommit,
    c: &Containers,
    region: Region,
    id: &str,
) -> Option<ObjId> {
    region_block(doc, c, region, id).map(|(_, obj)| obj)
}

/// The length in characters of block `id`'s text in `region`, or `None` when `id` is not a block
/// of that region as a snapshot would show it (a map carrying a byte order key). A block without a
/// text object has length 0. A point lookup: nothing else in the document is read, so an editor can
/// validate a keystroke against it without materializing the region.
pub fn region_block_len(
    doc: &AutoCommit,
    c: &Containers,
    region: Region,
    id: &str,
) -> Option<usize> {
    let block = region_block_obj(doc, c, region, id)?;
    match doc.get(&block, BLOCK_ORDER) {
        Ok(Some((Value::Scalar(value), _))) if matches!(value.as_ref(), ScalarValue::Bytes(_)) => {}
        _ => return None,
    }
    let len = match doc.get(&block, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), text))) => {
            doc.text(&text).map_or(0, |t| t.chars().count())
        }
        _ => 0,
    };
    Some(len)
}

/// The ordered block ids of a region (empty when the region is absent).
pub fn region_blocks_in_order(doc: &AutoCommit, c: &Containers, region: Region) -> Vec<String> {
    ordered_keys(doc, region_obj(c, region))
}

/// Materialize a region's blocks into [`BlockView`]s (empty when absent).
pub fn materialize_region(doc: &AutoCommit, c: &Containers, region: Region) -> Vec<BlockView> {
    ordered_views(doc, region_obj(c, region))
}

/// Resolve a single note's sub-map object by id.
fn note_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let notes = &c.notes;
    map_at(doc, notes, id)
}

/// Resolve a note's block container (`note.blocks`) sub-map object.
fn note_blocks_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let note = note_obj(doc, c, id)?;
    map_at(doc, &note, NOTE_BLOCKS)
}

/// The note ids present in the document, in a stable (sorted) order so two replicas converge
/// byte-identically; citation order in the body text is recovered separately from `note-ref` marks.
pub fn note_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let notes = &c.notes;
    let mut ids: Vec<String> = doc
        .keys(notes)
        .filter(|id| {
            matches!(
                doc.get(notes, id),
                Ok(Some((Value::Object(ObjType::Map), _)))
            )
        })
        .collect();
    ids.compact_sort();
    ids
}

/// A single note's kind (`Footnote`/`Endnote`), or `None` if the id is unknown.
pub fn note_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<NoteKind> {
    let note = note_obj(doc, c, id)?;
    str_at(doc, &note, NOTE_KIND)
        .as_deref()
        .map(NoteKind::from_str)
}

/// Materialize a note's block sequence (empty if the note has no blocks / is unknown).
pub fn materialize_note_blocks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<BlockView> {
    match note_blocks_obj(doc, c, id) {
        Some(obj) => ordered_views(doc, &obj),
        None => Vec::new(),
    }
}

/// The ordered block ids of a note's content (empty if absent).
pub fn note_blocks_in_order(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<String> {
    match note_blocks_obj(doc, c, id) {
        Some(obj) => ordered_keys(doc, &obj),
        None => Vec::new(),
    }
}

/// Materialize every note into a [`crate::snapshot::NoteView`] (sorted by note id).
pub fn materialize_notes(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::NoteView> {
    note_ids(doc, c)
        .into_iter()
        .map(|id| crate::snapshot::NoteView {
            kind: note_kind(doc, c, &id)
                .unwrap_or(NoteKind::Footnote)
                .as_str()
                .to_string(),
            blocks: materialize_note_blocks(doc, c, &id),
            id,
        })
        .collect()
}

/// A grow-only resolution marker. Unlike deleting the comment map entry alone,
/// this remains observable when a divergent replica concurrently authors a
/// reply against its still-live copy of the thread.
pub fn comment_is_tombstoned(doc: &AutoCommit, c: &Containers, id: &str) -> bool {
    let comments = &c.comments;
    let key = format!("{COMMENT_TOMBSTONE_PREFIX}{id}");
    matches!(doc.get(comments, key), Ok(Some((value, _))) if value.as_bool() == Some(true))
}

/// Resolve a single comment's sub-map object by id.
fn comment_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    if comment_is_tombstoned(doc, c, id) {
        return None;
    }
    let comments = &c.comments;
    map_at(doc, comments, id)
}

/// Resolve a comment's block container (`comment.blocks`) sub-map object.
fn comment_blocks_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let comment = comment_obj(doc, c, id)?;
    map_at(doc, &comment, COMMENT_BLOCKS)
}

fn comment_reply_obj(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Option<ObjId> {
    let comments = &c.comments;
    match doc.get(comments, reply_id) {
        Ok(Some((Value::Object(ObjType::Map), reply))) => {
            match doc.get(&reply, COMMENT_REPLY_PARENT) {
                Ok(Some((parent, _))) if parent.as_str() == Some(comment_id) => Some(reply),
                _ => None,
            }
        }
        _ => None,
    }
}

fn comment_reply_blocks_obj(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Option<ObjId> {
    let reply = comment_reply_obj(doc, c, comment_id, reply_id)?;
    map_at(doc, &reply, COMMENT_REPLY_BLOCKS)
}

/// The comment ids present in the document, in a stable (sorted) order so two replicas that merged
/// in different orders materialize byte-identically; anchored ranges are recovered from the `comment` marks.
pub fn comment_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let comments = &c.comments;
    let mut ids: Vec<String> = doc
        .keys(comments)
        .filter(|id| {
            !comment_is_tombstoned(doc, c, id)
                && matches!(doc.get(comments, id), Ok(Some((Value::Object(ObjType::Map), oid))) if !matches!(doc.get(&oid, COMMENT_REPLY_PARENT), Ok(Some(_))))
        })
        .collect();
    ids.compact_sort();
    ids
}

/// A comment's author display name (empty if unset / unknown id).
pub fn comment_author(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let comment = comment_obj(doc, c, id)?;
    str_at(doc, &comment, COMMENT_AUTHOR)
}

/// A comment's date / timestamp string (empty if unset / unknown id).
pub fn comment_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let comment = comment_obj(doc, c, id)?;
    str_at(doc, &comment, COMMENT_DATE)
}

/// The ordered block ids of a comment's content (empty if absent).
pub fn comment_blocks_in_order(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<String> {
    match comment_blocks_obj(doc, c, id) {
        Some(obj) => ordered_keys(doc, &obj),
        None => Vec::new(),
    }
}

/// Materialize a comment's block sequence (empty if the comment has no blocks / is unknown).
pub fn materialize_comment_blocks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<BlockView> {
    match comment_blocks_obj(doc, c, id) {
        Some(obj) => ordered_views(doc, &obj),
        None => Vec::new(),
    }
}

/// Stable ids of every reply belonging to a comment. Missing thread maps are
/// treated as empty to keep old documents readable without migration writes.
///
/// Replies are records in the shared comments map, so this reads the whole map; to list the
/// replies of every comment, use [`comment_replies_by_parent`] once instead.
pub fn comment_reply_ids(doc: &AutoCommit, c: &Containers, comment_id: &str) -> Vec<String> {
    comment_replies_by_parent(doc, c)
        .remove(comment_id)
        .unwrap_or_default()
}

/// Every reply id in the comments map, grouped by the comment it answers, each group sorted: the
/// answer [`comment_reply_ids`] gives for every comment, from one pass over the map.
pub fn comment_replies_by_parent(doc: &AutoCommit, c: &Containers) -> HashMap<String, Vec<String>> {
    let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
    let comments = &c.comments;
    for item in doc.map_range(comments, ..) {
        if !matches!(item.value, ValueRef::Object(ObjType::Map)) {
            continue;
        }
        if let Ok(Some((parent, _))) = doc.get(item.id(), COMMENT_REPLY_PARENT)
            && let Some(parent) = parent.as_str()
        {
            by_parent
                .entry(parent.to_owned())
                .or_default()
                .push(item.key.into_owned());
        }
    }
    for ids in by_parent.values_mut() {
        ids.compact_sort();
    }
    by_parent
}

pub fn comment_reply_author(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Option<String> {
    let reply = comment_reply_obj(doc, c, comment_id, reply_id)?;
    str_at(doc, &reply, COMMENT_REPLY_AUTHOR)
}

pub fn comment_reply_date(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Option<String> {
    let reply = comment_reply_obj(doc, c, comment_id, reply_id)?;
    str_at(doc, &reply, COMMENT_REPLY_DATE)
}

pub fn comment_reply_blocks_in_order(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Vec<String> {
    comment_reply_blocks_obj(doc, c, comment_id, reply_id)
        .map_or_else(Vec::new, |blocks| ordered_keys(doc, &blocks))
}

pub fn materialize_comment_reply_blocks(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_id: &str,
) -> Vec<BlockView> {
    let Some(blocks) = comment_reply_blocks_obj(doc, c, comment_id, reply_id) else {
        return Vec::new();
    };
    ordered_views(doc, &blocks)
}

pub fn materialize_comment_replies(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
) -> Vec<crate::snapshot::CommentReplyView> {
    reply_views(doc, c, comment_id, comment_reply_ids(doc, c, comment_id))
}

fn reply_views(
    doc: &AutoCommit,
    c: &Containers,
    comment_id: &str,
    reply_ids: Vec<String>,
) -> Vec<crate::snapshot::CommentReplyView> {
    reply_ids
        .into_iter()
        .map(|id| crate::snapshot::CommentReplyView {
            author: comment_reply_author(doc, c, comment_id, &id).unwrap_or_default(),
            date: comment_reply_date(doc, c, comment_id, &id).unwrap_or_default(),
            blocks: materialize_comment_reply_blocks(doc, c, comment_id, &id),
            id,
        })
        .collect()
}

/// The anchored body range `(block_id, start, end)` of a comment, recovered from the `comment` mark
/// covering text valued with the comment id. `None` if absent (e.g. the commented text was deleted).
pub fn comment_range(doc: &AutoCommit, c: &Containers, id: &str) -> Option<(String, usize, usize)> {
    for bid in blocks_in_order(doc, c) {
        for m in marks(doc, c, &bid) {
            if m.name == MARK_COMMENT && m.value == crate::document::MarkValue::Str(id.to_string())
            {
                return Some((bid, m.start, m.end));
            }
        }
    }
    None
}

/// Materialize every comment into a [`crate::snapshot::CommentView`] (sorted by id), recovering each comment's anchored body range.
pub fn materialize_comments(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::CommentView> {
    let ids = comment_ids(doc, c);
    if ids.is_empty() {
        return Vec::new();
    }
    let ranges = AnchorRanges::of(&ordered_views(doc, &c.blocks));
    comments_with_ranges(doc, c, ids, &ranges)
}

fn comments_with_ranges(
    doc: &AutoCommit,
    c: &Containers,
    ids: Vec<String>,
    ranges: &AnchorRanges,
) -> Vec<crate::snapshot::CommentView> {
    let mut replies = comment_replies_by_parent(doc, c);
    ids.into_iter()
        .map(|id| crate::snapshot::CommentView {
            author: comment_author(doc, c, &id).unwrap_or_default(),
            date: comment_date(doc, c, &id).unwrap_or_default(),
            blocks: materialize_comment_blocks(doc, c, &id),
            replies: reply_views(doc, c, &id, replies.remove(&id).unwrap_or_default()),
            range: ranges.get(MARK_COMMENT, &id),
            id,
        })
        .collect()
}

/// The first body range each comment / tracked-change anchor mark covers, keyed by `(mark name, id)`:
/// "first" in block order, then in canonical mark order — what [`comment_range`] / [`change_range`]
/// find, but for every id from one pass over already-materialized body blocks.
struct AnchorRanges(HashMap<(&'static str, String), (String, usize, usize)>);

impl AnchorRanges {
    fn of(blocks: &[BlockView]) -> Self {
        let mut out = HashMap::new();
        for b in blocks {
            for m in &b.marks {
                let Some(name) = [MARK_COMMENT, MARK_INS, MARK_DEL]
                    .into_iter()
                    .find(|n| *n == m.name)
                else {
                    continue;
                };
                if let crate::document::MarkValue::Str(v) = &m.value {
                    out.entry((name, v.clone()))
                        .or_insert_with(|| (b.id.clone(), m.start, m.end));
                }
            }
        }
        Self(out)
    }

    fn get(&self, name: &'static str, id: &str) -> Option<(String, usize, usize)> {
        self.0.get(&(name, id.to_string())).cloned()
    }
}

/// Every bookmark among already-materialized body blocks (see [`bookmarks`]).
fn bookmarks_in(blocks: &[BlockView]) -> Vec<crate::snapshot::BookmarkView> {
    let mut out: Vec<crate::snapshot::BookmarkView> = Vec::new();
    for b in blocks {
        for m in &b.marks {
            if m.name == MARK_BOOKMARK
                && let crate::document::MarkValue::Str(name) = &m.value
            {
                out.push(crate::snapshot::BookmarkView {
                    name: name.clone(),
                    range: (b.id.clone(), m.start, m.end),
                });
            }
        }
    }
    out.compact_sort_by(|a, b| {
        (&a.name, &a.range.0, a.range.1, a.range.2)
            .cmp(&(&b.name, &b.range.0, b.range.1, b.range.2))
    });
    out
}

/// Every bookmark in the body, recovered from the `bookmark` marks. Sorted by `(name, block, start)`
/// so replicas that merged in different orders materialize byte-identically; a name may repeat if the source document duplicated it.
pub fn bookmarks(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::BookmarkView> {
    bookmarks_in(&ordered_views(doc, &c.blocks))
}

/// The auto/derived reference id of a heading block (so a cross-reference can target a heading with no
/// explicit bookmark): [`HEADING_REF_PREFIX`] + the block id. Stable across save/load, distinct from any user bookmark name.
pub fn heading_ref_id(block_id: &str) -> String {
    format!("{HEADING_REF_PREFIX}{block_id}")
}

/// Resolve a single change's sub-map object by id.
fn change_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let changes = &c.changes;
    map_at(doc, changes, id)
}

/// The change ids present in the document, in a stable (sorted) order so replicas converge byte-identically;
/// anchored ranges are recovered separately from the body `ins`/`del` marks.
pub fn change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let changes = &c.changes;
    let mut ids: Vec<String> = doc
        .keys(changes)
        .filter(|id| {
            matches!(
                doc.get(changes, id),
                Ok(Some((Value::Object(ObjType::Map), _)))
            )
        })
        .collect();
    ids.compact_sort();
    ids
}

/// A change's author display name (empty if unset / unknown id).
pub fn change_author(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let change = change_obj(doc, c, id)?;
    str_at(doc, &change, CHANGE_AUTHOR)
}

/// A change's date / timestamp string (empty if unset / unknown id).
pub fn change_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let change = change_obj(doc, c, id)?;
    str_at(doc, &change, CHANGE_DATE)
}

/// A change's kind (`Ins`/`Del`), or `None` if the id is unknown.
pub fn change_kind(
    doc: &AutoCommit,
    c: &Containers,
    id: &str,
) -> Option<crate::snapshot::ChangeKind> {
    let change = change_obj(doc, c, id)?;
    str_at(doc, &change, CHANGE_KIND)
        .as_deref()
        .map(crate::snapshot::ChangeKind::from_str)
}

/// The mark name a change of `kind` anchors with (`ins`/`del`).
fn change_mark_name(kind: crate::snapshot::ChangeKind) -> &'static str {
    match kind {
        crate::snapshot::ChangeKind::Ins => MARK_INS,
        crate::snapshot::ChangeKind::Del => MARK_DEL,
    }
}

/// The anchored body range `(block_id, start, end)` of a change, recovered from the `ins`/`del` mark
/// (matching the change's kind) valued with the change id. `None` if absent (e.g. accepted/rejected already).
pub fn change_range(doc: &AutoCommit, c: &Containers, id: &str) -> Option<(String, usize, usize)> {
    let kind = change_kind(doc, c, id)?;
    let name = change_mark_name(kind);
    for bid in blocks_in_order(doc, c) {
        for m in marks(doc, c, &bid) {
            if m.name == name && m.value == crate::document::MarkValue::Str(id.to_string()) {
                return Some((bid, m.start, m.end));
            }
        }
    }
    None
}

/// Materialize every tracked change into a [`crate::snapshot::ChangeView`] (sorted by id), recovering each change's anchored body range.
pub fn materialize_changes(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::ChangeView> {
    let ids = change_ids(doc, c);
    if ids.is_empty() {
        return Vec::new();
    }
    let ranges = AnchorRanges::of(&ordered_views(doc, &c.blocks));
    changes_with_ranges(doc, c, ids, &ranges)
}

fn changes_with_ranges(
    doc: &AutoCommit,
    c: &Containers,
    ids: Vec<String>,
    ranges: &AnchorRanges,
) -> Vec<crate::snapshot::ChangeView> {
    ids.into_iter()
        .map(|id| {
            let recorded = change_kind(doc, c, &id);
            let kind = recorded.unwrap_or(crate::snapshot::ChangeKind::Ins);
            crate::snapshot::ChangeView {
                kind,
                author: change_author(doc, c, &id).unwrap_or_default(),
                date: change_date(doc, c, &id).unwrap_or_default(),
                // `change_range` has no range for a change without a recorded kind.
                range: recorded.and_then(|k| ranges.get(change_mark_name(k), &id)),
                id,
            }
        })
        .collect()
}

/// A blob's descriptor — what the document holds *instead of* its bytes.
/// `None` when the document doesn't reference that hash at all, distinct from referencing it without having the content yet.
pub fn blob_meta(doc: &AutoCommit, c: &Containers, hash: &str) -> Option<ContentMeta> {
    let blobs = &c.blobs;
    match doc.get(blobs, hash) {
        Ok(Some((Value::Object(ObjType::Map), d))) => {
            let str_at = |k: &str| match doc.get(&d, k) {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::Str(v) => v.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            let int_at = |k: &str| match doc.get(&d, k) {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::Int(v) => *v,
                    ScalarValue::Uint(v) => i64::try_from(*v).unwrap_or(i64::MAX),
                    _ => 0,
                },
                _ => 0,
            };
            Some(ContentMeta {
                mime: str_at(BLOB_MIME),
                len: u64::try_from(int_at(BLOB_LEN).max(0)).unwrap_or(0),
                w: u32::try_from(int_at(BLOB_W).max(0)).unwrap_or(u32::MAX),
                h: u32::try_from(int_at(BLOB_H).max(0)).unwrap_or(u32::MAX),
            })
        }
        _ => None,
    }
}

/// Every blob hash this document references, whether or not the content is here yet.
pub fn blob_hashes(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    doc.keys(&c.blobs).collect()
}

/// Every content address the document refers to, in any of its versions: image descriptors and
/// preserved package parts alike (both are recorded in the grow-only blobs map). The set a
/// content store must retain for this document.
pub fn referenced_content(doc: &AutoCommit, c: &Containers) -> BTreeSet<String> {
    blob_hashes(doc, c).into_iter().collect()
}

fn block_obj(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<ObjId> {
    map_at(doc, container, id)
}

thread_local! {
    /// Per-thread count of full [`ordered_keys`] container scans (each an O(n) read-and-sort). The in-memory
    /// order index means a *bulk* insert (build/import of n blocks) triggers only O(1) of these — one per
    /// container to seed the index — instead of one per insert. The perf test asserts on this delta directly,
    /// so a regression back to per-insert scanning is caught mode-independently (no wall-clock dependence).
    static ORDERED_KEYS_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The number of full container scans this thread has performed (see `ORDERED_KEYS_SCANS`). Sample it
/// around a bulk insert to prove the order index kept work near-constant rather than O(n).
pub fn ordered_keys_scan_count() -> u64 {
    ORDERED_KEYS_SCANS.with(std::cell::Cell::get)
}

/// A block map's fields, read in one pass over its entries. Automerge answers each `get` with a fresh
/// seek into the op set, so reading a block field by field cost one seek per field; a single
/// `map_range` walk visits every field in order for the price of one.
#[derive(Default)]
struct BlockFields {
    order: Option<Vec<u8>>,
    kind: Option<String>,
    text: Option<ObjId>,
    attrs: Option<ObjId>,
    children: Option<ObjId>,
}

fn block_fields(doc: &AutoCommit, obj: &ObjId) -> BlockFields {
    let mut f = BlockFields::default();
    for item in doc.map_range(obj, ..) {
        match (item.key.as_ref(), &item.value) {
            (BLOCK_ORDER, ValueRef::Scalar(ScalarValueRef::Bytes(b))) => {
                f.order = Some(b.to_vec());
            }
            (BLOCK_KIND, ValueRef::Scalar(ScalarValueRef::Str(s))) => {
                f.kind = Some(s.to_string());
            }
            (BLOCK_TEXT, ValueRef::Object(ObjType::Text)) => f.text = Some(item.id()),
            (BLOCK_ATTRS, ValueRef::Object(ObjType::Map)) => f.attrs = Some(item.id()),
            (BLOCK_CHILDREN, ValueRef::Object(ObjType::Map)) => f.children = Some(item.id()),
            _ => {}
        }
    }
    f
}

/// One block of a container, as found by [`ordered_blocks`].
struct OrderedBlock {
    order: Vec<u8>,
    id: String,
    /// The block's map.
    obj: ObjId,
    fields: BlockFields,
}

/// Every block of `container` that carries an order key, sorted into document order (fractional key via
/// [`tilo_resource::order::bytes::cmp`], then id as a tiebreak). One walk over the container and one over each block.
fn ordered_blocks(doc: &AutoCommit, container: &ObjId) -> Vec<OrderedBlock> {
    ORDERED_KEYS_SCANS.with(|c| c.set(c.get().saturating_add(1)));
    let mut v: Vec<OrderedBlock> = doc
        .map_range(container, ..)
        .filter(|item| matches!(item.value, ValueRef::Object(ObjType::Map)))
        .filter_map(|item| {
            let obj = item.id();
            let mut fields = block_fields(doc, &obj);
            let order = fields.order.take()?;
            Some(OrderedBlock {
                order,
                id: item.key.into_owned(),
                obj,
                fields,
            })
        })
        .collect();
    v.compact_sort_by(|a, b| {
        tilo_resource::order::bytes::cmp(&a.order, &b.order).then_with(|| a.id.cmp(&b.id))
    });
    v
}

fn ordered_keys(doc: &AutoCommit, container: &ObjId) -> Vec<String> {
    ordered_blocks(doc, container)
        .into_iter()
        .map(|b| b.id)
        .collect()
}

/// Materialize every block of `container` in document order.
fn ordered_views(doc: &AutoCommit, container: &ObjId) -> Vec<BlockView> {
    ordered_blocks(doc, container)
        .into_iter()
        .map(|b| block_view(doc, b.id, b.fields))
        .collect()
}

pub fn blocks_in_order(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    ordered_keys(doc, &c.blocks)
}

/// One ordered body block's place in the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpineEntry {
    pub order: Vec<u8>,
    pub id: String,
    pub kind: String,
}

/// The objects a body block is stored in: its map, and the text and
/// attributes inside it when it has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockObjects {
    pub block: ObjId,
    pub text: Option<ObjId>,
    pub attrs: Option<ObjId>,
}

/// One ordered body block as [`read_body`] found it.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyBlock {
    pub entry: SpineEntry,
    /// Where the block is stored.
    pub objects: BlockObjects,
    /// The block materialised, when the caller asked for its kind.
    pub view: Option<BlockView>,
}

/// The body's spine, materialising the blocks whose kind `view` picks, in
/// one walk over the body: each block's fields are read once, whether it
/// becomes a spine entry, a view or both, so no picked block is read twice
/// or looked up by id again.
pub fn read_body(
    doc: &AutoCommit,
    c: &Containers,
    mut view: impl FnMut(&str) -> bool,
) -> Vec<BodyBlock> {
    ordered_blocks(doc, &c.blocks)
        .into_iter()
        .map(|b| {
            let kind = b.fields.kind.clone().unwrap_or_default();
            let objects = BlockObjects {
                block: b.obj,
                text: b.fields.text.clone(),
                attrs: b.fields.attrs.clone(),
            };
            let view = view(&kind).then(|| block_view(doc, b.id.clone(), b.fields));
            BodyBlock {
                entry: SpineEntry {
                    order: b.order,
                    id: b.id,
                    kind,
                },
                objects,
                view,
            }
        })
        .collect()
}

/// One body block's spine entry; `None` when the block is gone or has no
/// order key (and so is not part of the document).
pub fn body_spine_entry(doc: &AutoCommit, c: &Containers, id: &str) -> Option<SpineEntry> {
    let obj = block_obj(doc, &c.blocks, id)?;
    let fields = block_fields(doc, &obj);
    Some(SpineEntry {
        order: fields.order?,
        id: id.to_owned(),
        kind: fields.kind.unwrap_or_default(),
    })
}

/// Materialize exactly the requested body blocks, in the supplied order.
///
/// This is deliberately unlike [`materialize`] and [`materialize_region`]: it
/// does not enumerate the body container first. Callers that already maintain a
/// canonical order index (for example a viewport renderer) can therefore fetch
/// a bounded visible window without allocating views for an entire imported
/// document. Missing ids are ignored; that makes a window assembled immediately
/// before a concurrent deletion safe to consume without fabricating a block.
pub fn materialize_blocks(doc: &AutoCommit, c: &Containers, ids: &[String]) -> Vec<BlockView> {
    ids.iter()
        .filter_map(|id| {
            let obj = block_obj(doc, &c.blocks, id)?;
            Some(block_view(doc, id.clone(), block_fields(doc, &obj)))
        })
        .collect()
}

/// The byte length of every body block's fractional order key. [`tilo_resource::order::bytes::between`] keeps these
/// logarithmic in a run of sequential appends; a scheme that grows keys linearly makes every comparison O(n).
/// The perf test asserts the max stays small — a direct, mode-independent guard on key growth.
pub fn block_order_key_lengths(doc: &AutoCommit, c: &Containers) -> Vec<usize> {
    ordered_blocks(doc, &c.blocks)
        .into_iter()
        .map(|b| b.order.len())
        .collect()
}

/// Read a document-level meta string (e.g. the serialized stylesheet).
pub fn meta_str(doc: &AutoCommit, c: &Containers, key: &str) -> Option<String> {
    str_at(doc, &c.meta, key)
}

/// The document-wide multi-column layout: `(column_count, optional_gap_px)`. Reads the `meta.columns` /
/// `meta.column_gap` registers, defaulting to a single column ([`DEFAULT_COLUMNS`]) with no gap when unset
/// so documents predating this setting read as single-column. A stored count of zero is clamped up to one.
pub fn columns(doc: &AutoCommit, c: &Containers) -> (u32, Option<f64>) {
    let count = match doc.get(&c.meta, META_COLUMNS) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Uint(u) => u32::try_from(*u).unwrap_or(u32::MAX),
            ScalarValue::Int(i) if *i > 0 => u32::try_from(*i).unwrap_or(u32::MAX),
            _ => DEFAULT_COLUMNS,
        },
        _ => DEFAULT_COLUMNS,
    }
    .max(1);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a page column gap in px; realistic values are far below 2^53 so f64 loses no precision that matters here"
    )]
    let gap = match doc.get(&c.meta, META_COLUMN_GAP) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::F64(f) => Some(*f),
            ScalarValue::Int(i) => Some(*i as f64),
            ScalarValue::Uint(u) => Some(*u as f64),
            _ => None,
        },
        _ => None,
    };
    (count, gap)
}

/// The document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`), read from `meta.language`.
/// `None` for documents that declare no default language; the consumer then falls back to its own locale.
/// Round-trips through the `.odt` default-paragraph-style `fo:language`/`fo:country` and `.docx` `docDefaults` `w:lang`.
pub fn language(doc: &AutoCommit, c: &Containers) -> Option<String> {
    str_at(doc, &c.meta, META_LANGUAGE).filter(|s| !s.is_empty())
}

/// Resolve the document-properties sub-map under `meta` (`meta.props`), if it exists (created lazily on first [`DocMut::set_doc_prop`]).
fn props_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    map_at(doc, &c.meta, KEY_PROPS)
}

/// Read a single document property (`title`/`author`/… — see [`DOC_PROP_KEYS`](crate::schema::DOC_PROP_KEYS)), or any other string property name a codec stashed. `None` if unset.
pub fn doc_prop(doc: &AutoCommit, c: &Containers, name: &str) -> Option<String> {
    let props = props_obj(doc, c)?;
    str_at(doc, &props, name)
}

/// All document properties (`name → value`) as a sorted map, empty if none are set.
pub fn doc_props(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, String> {
    props_obj(doc, c).map_or_else(BTreeMap::new, |props| str_entries(doc, &props))
}

pub fn block_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    str_at(doc, &obj, BLOCK_KIND)
}

pub fn block_text(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    block_text_on(doc, &obj)
}

// ---- paragraph text storage ------------------------------------------------
//
// A block's `text` is one Automerge `Text` object: a character-level sequence
// CRDT whose inline formatting lives as marks on it, so concurrent edits in one
// paragraph merge per character. Every block constructor below creates that
// object in the same change as the block itself.

fn canonical_text(doc: &AutoCommit, block_obj: &ObjId) -> Option<ObjId> {
    match doc.get(block_obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => None,
    }
}

/// The characters a block shows. `None` when the block stores no text.
pub fn block_text_on(doc: &AutoCommit, block_obj: &ObjId) -> Option<String> {
    doc.text(&canonical_text(doc, block_obj)?).ok()
}

/// Resolve the block's Text object for a write, creating it when missing.
pub fn ensure_block_text(doc: &mut AutoCommit, block_obj: &ObjId) -> Option<ObjId> {
    if let Some(id) = canonical_text(doc, block_obj) {
        return Some(id);
    }
    doc.put_object(block_obj, BLOCK_TEXT, ObjType::Text).ok()
}

/// Create a new block's (empty) Text object in the same change as the block,
/// so concurrent first edits on other replicas address one shared sequence.
fn create_block_text(doc: &mut AutoCommit, block_obj: &ObjId) {
    let _ = doc.put_object(block_obj, BLOCK_TEXT, ObjType::Text);
}

fn marks_on(doc: &AutoCommit, block_obj: &ObjId) -> Vec<MarkSpan> {
    let Some(tid) = canonical_text(doc, block_obj) else {
        return vec![];
    };
    text_and_marks(doc, &tid).1
}

/// A text object's content and its marks, from one walk over its spans (`text` and `marks` would each
/// walk every character). Runs are merged exactly as automerge's own `marks` merges them — a run
/// extends the previous run of the same name when it carries the same value and starts where that
/// one ended — and removed (null-valued) marks are dropped. Marks come back in canonical order so
/// replicas that merged in different orders produce byte-identical materializations (the mark *set*
/// is order-independent); value is part of the key so concurrent valued marks sort deterministically.
fn text_and_marks(doc: &AutoCommit, tid: &ObjId) -> (String, Vec<MarkSpan>) {
    #[cfg(test)]
    MARK_READS.with(|count| count.set(count.get().saturating_add(1)));
    let Ok(spans) = doc.spans(tid) else {
        return (String::new(), Vec::new());
    };
    let mut text = String::new();
    let mut runs: BTreeMap<String, Vec<(usize, usize, ScalarValue)>> = BTreeMap::new();
    let mut index = 0usize;
    for span in spans {
        let piece = span.as_str();
        let len = piece.chars().count();
        text.push_str(piece);
        if let automerge::Span::Text {
            marks: Some(marks), ..
        } = &span
            && len > 0
        {
            for (name, value) in marks.iter() {
                let named = runs.entry(name.to_string()).or_default();
                match named.last_mut() {
                    Some((start, run_len, v))
                        if v == value && start.saturating_add(*run_len) == index =>
                    {
                        *run_len = run_len.saturating_add(len);
                    }
                    _ => named.push((index, len, value.clone())),
                }
            }
        }
        index = index.saturating_add(len);
    }
    let mut marks: Vec<MarkSpan> = runs
        .into_iter()
        .flat_map(|(name, named)| {
            named
                .into_iter()
                .filter(|(_, _, value)| !value.is_null())
                .map(move |(start, len, value)| MarkSpan {
                    start,
                    end: start.saturating_add(len),
                    name: name.clone(),
                    value: (&value).into(),
                })
        })
        .collect();
    marks.compact_sort_by(|a, b| {
        (a.start, a.end, &a.name, &a.value).cmp(&(b.start, b.end, &b.name, &b.value))
    });
    (text, marks)
}

pub fn marks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<MarkSpan> {
    match block_obj(doc, &c.blocks, id) {
        Some(obj) => marks_on(doc, &obj),
        None => vec![],
    }
}

/// Whether a mark named `name` fully covers `[start, end)` of block `id`.
pub fn has_mark_over(
    doc: &AutoCommit,
    c: &Containers,
    id: &str,
    start: usize,
    end: usize,
    name: &str,
) -> bool {
    marks(doc, c, id)
        .iter()
        .any(|m| m.name == name && m.start <= start && m.end >= end)
}

/// All preserved parts (the lossless preservation layer — verbatim bytes a codec stashed for content it
/// doesn't model), as `(key, bytes)`, each reconstructed exactly by concatenating its per-atom segments
/// in stored order.
///
/// A part preserved as a content reference is read from `content`; one whose content is missing there
/// is left out (and logged), since there are no bytes to re-emit.
pub fn preserved_parts(
    doc: &AutoCommit,
    c: &Containers,
    content: &dyn ContentSource,
) -> Vec<(String, Vec<u8>)> {
    doc.keys(&c.preserved)
        .filter_map(|k| preserved_part(doc, c, &k, content).map(|b| (k, b)))
        .collect()
}

/// How a preserved part is stored: verbatim in the document, or as a reference to a binary input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreservedPart {
    /// The part's bytes, reassembled from its atoms.
    Bytes(Vec<u8>),
    /// The content address of the part, kept in the caller's content store.
    Content(String),
}

/// One preserved part as stored, without resolving content references.
pub fn preserved_entry(doc: &AutoCommit, c: &Containers, key: &str) -> Option<PreservedPart> {
    if let Ok(Some((Value::Scalar(s), _))) = doc.get(&c.preserved, key)
        && let ScalarValue::Str(hash) = s.as_ref()
    {
        return Some(PreservedPart::Content(hash.to_string()));
    }
    preserved_inline(doc, c, key).map(PreservedPart::Bytes)
}

/// Reconstruct one preserved part's verbatim bytes by key: inline parts from the document, content
/// references from `content`.
pub fn preserved_part(
    doc: &AutoCommit,
    c: &Containers,
    key: &str,
    content: &dyn ContentSource,
) -> Option<Vec<u8>> {
    match preserved_entry(doc, c, key)? {
        PreservedPart::Bytes(bytes) => Some(bytes),
        PreservedPart::Content(hash) => {
            let bytes = content.content(&hash).map(std::borrow::Cow::into_owned);
            if bytes.is_none() {
                tracing::warn!(%key, %hash, "preserved part's content is not available; leaving it out");
            }
            bytes
        }
    }
}

/// A preserved part stored in the document: its atoms concatenated in order.
fn preserved_inline(doc: &AutoCommit, c: &Containers, key: &str) -> Option<Vec<u8>> {
    preserved_segments(doc, c, key).map(|segs| segs.into_iter().flat_map(|(_, b)| b).collect())
}

/// The per-atom segment ids of a preserved part, in source order, paired with each atom's verbatim bytes.
/// Empty for a part that is not stored inline. Lets a codec or test address an individual atom by its stable id.
pub fn preserved_atoms(doc: &AutoCommit, c: &Containers, key: &str) -> Vec<(String, Vec<u8>)> {
    preserved_segments(doc, c, key).unwrap_or_default()
}

/// A part's `Map<segment_id, {ord: Bytes, bytes: Bytes}>`, sorted by `ord` (segment id breaks ties), or
/// `None` when `key` is not an inline part. Each segment is its own automerge object, so edits to
/// different atoms of one part merge rather than clobber each other.
fn preserved_segments(
    doc: &AutoCommit,
    c: &Containers,
    key: &str,
) -> Option<Vec<(String, Vec<u8>)>> {
    let Ok(Some((Value::Object(ObjType::Map), part_obj))) = doc.get(&c.preserved, key) else {
        return None;
    };
    let bytes_at = |obj: &ObjId, prop: &str| doc.bytes_at(obj, prop);
    let mut segs: Vec<(Vec<u8>, String, Vec<u8>)> = doc
        .keys(&part_obj)
        .filter_map(|sid| {
            let Ok(Some((Value::Object(ObjType::Map), seg_obj))) = doc.get(&part_obj, &sid) else {
                return None;
            };
            let ord = bytes_at(&seg_obj, PRESERVED_SEG_ORD).unwrap_or_default();
            let bytes = bytes_at(&seg_obj, PRESERVED_SEG_BYTES)?;
            Some((ord, sid, bytes))
        })
        .collect();
    segs.compact_sort_by(|a, b| tilo_resource::order::bytes::cmp(&a.0, &b.0).then(a.1.cmp(&b.1)));
    Some(segs.into_iter().map(|(_, sid, b)| (sid, b)).collect())
}

/// A stable, replica-independent id for a preserved atom: its source-order index plus a short content hash.
/// The index disambiguates repeated identical atoms; the content hash keeps the id meaningful across re-imports.
/// The same source bytes split into the same ids on every replica, so the same atom converges to one automerge object.
fn preserved_atom_id(idx: usize, atom: &[u8]) -> String {
    let h = blake3::hash(atom).to_hex();
    // blake3's hex digest is always 64 ASCII hex chars, so a 16-byte prefix is always a char boundary.
    let prefix = h.as_str().get(..16).unwrap_or(h.as_str());
    format!("{idx:06}-{prefix}")
}

/// A block's (or style's) scalar properties as [`AttrVal`]s, read in one walk over the map.
fn scalar_props(doc: &AutoCommit, obj: &ObjId) -> BTreeMap<String, AttrVal> {
    let mut out = BTreeMap::new();
    for item in doc.map_range(obj, ..) {
        let ValueRef::Scalar(s) = &item.value else {
            continue;
        };
        let val = match s {
            ScalarValueRef::Int(i) => AttrVal::Int(*i),
            ScalarValueRef::Uint(u) => AttrVal::Int(i64::try_from(*u).unwrap_or(i64::MAX)),
            ScalarValueRef::Boolean(b) => AttrVal::Bool(*b),
            ScalarValueRef::Str(st) => AttrVal::Str(st.to_string()),
            ScalarValueRef::F64(f) => AttrVal::Float(*f),
            _ => continue,
        };
        out.insert(item.key.into_owned(), val);
    }
    out
}

/// The rich-text document a peer's saved `bytes` hold, read without
/// keeping them: `None` when they are not a document, or not a rich-text
/// one.
pub fn snapshot_of(bytes: &[u8]) -> Option<DocSnapshot> {
    let doc = AutoCommit::load(bytes).ok()?;
    let c = open(&doc, &ROOT).ok()?;
    Some(materialize(&doc, &c))
}

pub fn materialize(doc: &AutoCommit, c: &Containers) -> DocSnapshot {
    let blocks = ordered_views(doc, &c.blocks);
    let ranges = AnchorRanges::of(&blocks);
    let comments = comments_with_ranges(doc, c, comment_ids(doc, c), &ranges);
    let changes = changes_with_ranges(doc, c, change_ids(doc, c), &ranges);
    let bookmarks = bookmarks_in(&blocks);
    let (columns, column_gap) = columns(doc, c);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a page column gap in px; document-realistic values fit f32 exactly"
    )]
    let column_gap = column_gap.map(|g| g as f32);
    let mut snap = DocSnapshot {
        schema: SCHEMA_ID.to_string(),
        schema_version: SCHEMA_VERSION,
        blocks,
        styles: all_styles(doc, c),
        properties: doc_props(doc, c),
        header: materialize_region(doc, c, Region::Header),
        footer: materialize_region(doc, c, Region::Footer),
        notes: materialize_notes(doc, c),
        comments,
        changes,
        bookmarks,
        columns,
        column_gap,
        language: language(doc, c),
    };
    project_blob_meta(doc, c, &mut snap);
    snap
}

/// Copy each referenced blob's descriptor onto the image blocks that reference it, as `blob_mime` / `blob_bytes` /
/// `blob_w` / `blob_h`. The descriptor is stored once per hash (deduplicated with the content) but read per block,
/// since a renderer already reads the `blob` attr off the block — so an image's intrinsic size is available even
/// before its bytes arrive. See [`ATTR_BLOB_W`] for why that's not the existing `width`/`height`.
fn project_blob_meta(doc: &AutoCommit, c: &Containers, snap: &mut DocSnapshot) {
    fn walk(
        doc: &AutoCommit,
        c: &Containers,
        cache: &mut BTreeMap<String, ContentMeta>,
        blocks: &mut [BlockView],
    ) {
        for b in blocks.iter_mut() {
            let hash = match b.attrs.get("blob") {
                Some(AttrVal::Str(h)) if !h.is_empty() => h.clone(),
                _ => {
                    walk(doc, c, cache, &mut b.children);
                    continue;
                }
            };
            let meta = if let Some(m) = cache.get(&hash) {
                m.clone()
            } else {
                let m = blob_meta(doc, c, &hash).unwrap_or_default();
                cache.insert(hash.clone(), m.clone());
                m
            };
            if !meta.mime.is_empty() {
                b.attrs
                    .insert(ATTR_BLOB_MIME.to_string(), AttrVal::Str(meta.mime.clone()));
            }
            b.attrs.insert(
                ATTR_BLOB_BYTES.to_string(),
                AttrVal::Int(i64::try_from(meta.len).unwrap_or(i64::MAX)),
            );
            if meta.w > 0 && meta.h > 0 {
                b.attrs
                    .insert(ATTR_BLOB_W.to_string(), AttrVal::Int(i64::from(meta.w)));
                b.attrs
                    .insert(ATTR_BLOB_H.to_string(), AttrVal::Int(i64::from(meta.h)));
            }
            walk(doc, c, cache, &mut b.children);
        }
    }
    let mut cache: BTreeMap<String, ContentMeta> = BTreeMap::new();
    walk(doc, c, &mut cache, &mut snap.blocks);
    walk(doc, c, &mut cache, &mut snap.header);
    walk(doc, c, &mut cache, &mut snap.footer);
    for n in &mut snap.notes {
        walk(doc, c, &mut cache, &mut n.blocks);
    }
    for cm in &mut snap.comments {
        walk(doc, c, &mut cache, &mut cm.blocks);
    }
}

/// Style ids defined in the document's styles container (e.g. `body`, `heading1`, or an imported ODF named-style name).
pub fn style_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    doc.keys(&c.styles)
        .filter(|id| {
            !style_is_tombstoned(doc, c, id)
                && matches!(
                    doc.get(&c.styles, id),
                    Ok(Some((Value::Object(ObjType::Map), _)))
                )
        })
        .collect()
}

pub fn style_is_tombstoned(doc: &AutoCommit, c: &Containers, id: &str) -> bool {
    let key = format!("{STYLE_TOMBSTONE_PREFIX}{id}");
    matches!(doc.get(&c.styles, key), Ok(Some((value, _))) if value.as_bool() == Some(true))
}

/// The scalar properties of a single style (`name`/`font`/`size`/… → value), or an empty map if the style id is unknown.
pub fn style_props(doc: &AutoCommit, c: &Containers, id: &str) -> BTreeMap<String, AttrVal> {
    match doc.get(&c.styles, id) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => scalar_props(doc, &obj),
        _ => BTreeMap::new(),
    }
}

/// Every style in the container (`style_id → (property → value)`).
pub fn all_styles(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, BTreeMap<String, AttrVal>> {
    let mut out = BTreeMap::new();
    for id in style_ids(doc, c) {
        let props = style_props(doc, c, &id);
        out.insert(id, props);
    }
    out
}

#[cfg(test)]
fn materialize_block(doc: &AutoCommit, container: &ObjId, id: &str) -> BlockView {
    let fields = match block_obj(doc, container, id) {
        Some(obj) => block_fields(doc, &obj),
        None => BlockFields::default(),
    };
    block_view(doc, id.to_string(), fields)
}

/// Build a block's view from its already-read fields (see [`block_fields`]).
fn block_view(doc: &AutoCommit, id: String, fields: BlockFields) -> BlockView {
    let (text, marks) = match &fields.text {
        Some(tid) => text_and_marks(doc, tid),
        None => (String::new(), Vec::new()),
    };
    BlockView {
        id,
        kind: fields.kind.unwrap_or_default(),
        text,
        marks,
        attrs: fields
            .attrs
            .map(|a| scalar_props(doc, &a))
            .unwrap_or_default(),
        children: fields
            .children
            .map(|cid| ordered_views(doc, &cid))
            .unwrap_or_default(),
    }
}

/// A mutable view of a canonical document living in a borrowed `AutoCommit`; used by the editor (over `ui::State.doc`) and by codecs.
pub struct DocMut<'a> {
    pub doc: &'a mut AutoCommit,
    pub c: &'a Containers,
    pub ids: &'a mut IdGen,
}

/// A read-only view, used by codec `export`.
pub struct DocRef<'a> {
    pub doc: &'a AutoCommit,
    pub c: &'a Containers,
}

impl DocRef<'_> {
    pub fn materialize(&self) -> DocSnapshot {
        materialize(self.doc, self.c)
    }
}

// The `put`/`put_object` calls below all target either `self.c`'s cached container ids (resolved
// once by `open`/`init` and never touched by anything but this single-threaded, synchronous API) or
// an object this same method just created a line above — never anything derived from untrusted
// document *content* (text, attrs, offsets). Automerge only errors these ops when the target object
// id has been deleted, which cannot happen mid-call here. Threading `Result` through every mutator
// would also break the many external callers (apps/word-processor, apps/agent, fs, …) that already
// depend on `DocMut`'s infallible method signatures — out of scope for a lint-only pass.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "every put/put_object here targets a container id this API itself resolved or just created; see the note above for why threading Result through is out of scope"
)]
impl DocMut<'_> {
    /// A fractional key ordered strictly after block `after` (or at the front when `after` is `None`) within
    /// `container`, for block `id`. Uses the in-memory order index so the lookup is O(log n), not an O(n log n)
    /// rescan. The key is salted by `id`, so two replicas placing blocks in one gap at once give them distinct
    /// keys, and a later block still fits between them.
    fn order_after_in(&mut self, container: &ObjId, after: Option<&str>, id: &str) -> Vec<u8> {
        let salt = order_salt(id);
        // An action's insert after a known block needs only that block's
        // successor. Finding it reads each block's order key once; building
        // the index sorts them all, and an action's generator lives for one
        // action. Staged (bulk) inserts and long-lived generators use the
        // index.
        if self.ids.action.is_some()
            && !self.bulk_active()
            && !self.ids.order.by_container.contains_key(container)
            && let Some((key, hi)) = self.order_after_scanning(container, after, salt)
        {
            self.ids.placing = Some((container.clone(), key.clone(), hi));
            return key;
        }
        self.ensure_order_cache(container);
        let co = self
            .ids
            .order
            .by_container
            .get(container)
            .expect("order cache just built");
        let (lo, hi) = match after {
            // Insert at the front: bounded above by the first block's key.
            None => (None, co.entries.iter().next().map(|k| k.key.clone())),
            Some(id) => match co.by_id.get_key_value(id) {
                // `after` exists: lo is its key, hi is the next block's key.
                Some((id, after_key)) => {
                    let probe = OrderKey {
                        key: Arc::clone(after_key),
                        id: Arc::clone(id),
                    };
                    let hi = co
                        .entries
                        .range((Bound::Excluded(probe), Bound::Unbounded))
                        .next()
                        .map(|k| k.key.clone());
                    (Some(Arc::clone(after_key)), hi)
                }
                // `after` unknown → append at the end (mirrors the old `unwrap_or(len)`).
                None => (co.entries.iter().next_back().map(|k| k.key.clone()), None),
            },
        };
        tilo_resource::order::bytes::between_salted(lo.as_deref(), hi.as_deref(), salt)
    }

    /// The key for a block inserted after `after` in `container`, and the
    /// key of the block that follows it, without building the order index:
    /// from the run of inserts this generator is making, or one pass over
    /// the container's order keys. `None` when `after` has no order key (the
    /// caller then appends via the index).
    fn order_after_scanning(
        &self,
        container: &ObjId,
        after: Option<&str>,
        salt: u16,
    ) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        use tilo_resource::order::bytes::{between_salted, cmp};
        let after = after?;
        if let Some(run) = &self.ids.run
            && &run.container == container
            && run.last == after
        {
            let hi = run.hi.clone();
            return Some((between_salted(Some(&run.key), hi.as_deref(), salt), hi));
        }
        let order_of = |doc: &AutoCommit, obj: &ObjId| doc.bytes_at(obj, BLOCK_ORDER);
        let after_key = order_of(self.doc, &block_obj(self.doc, container, after)?)?;
        let mut next: Option<(Vec<u8>, String)> = None;
        for item in self.doc.map_range(container, ..) {
            if !matches!(item.value, ValueRef::Object(ObjType::Map)) {
                continue;
            }
            let Some(key) = order_of(self.doc, &item.id()) else {
                continue;
            };
            let id = item.key.as_ref();
            let past_after = cmp(&key, &after_key).then_with(|| id.cmp(after)).is_gt();
            let before_next = next.as_ref().is_none_or(|(next_key, next_id)| {
                cmp(&key, next_key)
                    .then_with(|| id.cmp(next_id.as_str()))
                    .is_lt()
            });
            if past_after && before_next {
                next = Some((key, id.to_owned()));
            }
        }
        let hi = next.map(|(key, _)| key);
        Some((between_salted(Some(&after_key), hi.as_deref(), salt), hi))
    }

    fn order_after(&mut self, after: Option<&str>, id: &str) -> Vec<u8> {
        let container = self.c.blocks.clone();
        self.order_after_in(&container, after, id)
    }

    /// Build the order index for `container` on first use (one O(n log n) scan), then keep it incrementally.
    fn ensure_order_cache(&mut self, container: &ObjId) {
        if self.ids.order.by_container.contains_key(container) {
            return;
        }
        let mut co = ContainerOrder::default();
        for b in ordered_blocks(self.doc, container) {
            co.insert(b.id.into(), b.order.into());
        }
        self.ids.order.by_container.insert(container.clone(), co);
    }

    /// Record (or re-key) a block's order in the index. Called right after the block is written to the
    /// document, so the index stays in lock-step with it.
    fn record_order(&mut self, container: &ObjId, id: &str, key: &[u8]) {
        if let Some(co) = self.ids.order.by_container.get_mut(container) {
            co.insert(id.into(), key.into());
        }
        self.ids.run = match self.ids.placing.take() {
            Some((placed, placed_key, hi)) if &placed == container && placed_key == key => {
                Some(InsertRun {
                    container: placed,
                    last: id.to_owned(),
                    key: placed_key,
                    hi,
                })
            }
            _ => None,
        };
    }

    /// Drop a block from the order index (on delete). A no-op if the container's index hasn't been built
    /// yet — it will rebuild from the (already-updated) document on next use.
    fn forget_order(&mut self, container: &ObjId, id: &str) {
        if let Some(co) = self.ids.order.by_container.get_mut(container) {
            co.remove(id);
        }
    }

    // NOTE: DocMut does NOT commit the automerge transaction — the host owns the transaction boundary.
    // The owned `Document` commits per op (for fine-grained history); a host like `ui::State` commits once
    // per user action (so its snapshot-based undo records each action). Reads always observe pending ops.

    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        self.insert_block_in(Region::Body, after, kind)
    }

    /// Insert a block into a page region (`Body`/`Header`/`Footer`). Header/footer containers are created
    /// lazily on first insert. Block ids are minted from the same per-replica generator as the body, so they never collide across regions.
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_block_in(&mut self, region: Region, after: Option<&str>, kind: &str) -> String {
        let container = region_obj(self.c, region).clone();
        if self.bulk_active() {
            return self.stage_block(container, after, kind);
        }
        let id = self.ids.mint();
        let key = self.order_after_in(&container, after, &id);
        let obj = self
            .doc
            .put_object(&container, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc
            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
            .unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc
            .put_object(&obj, BLOCK_ATTRS, ObjType::Map)
            .unwrap();
        create_block_text(self.doc, &obj);
        self.record_order(&container, &id, &key);
        id
    }

    /// Insert a block after `after` and before `next` — the block known to
    /// follow it — reading only those two blocks' order keys. Falls back to
    /// [`Self::insert_block_in`] when `next` is unknown, gone, or not after
    /// `after`.
    ///
    /// # Panics
    /// As [`Self::insert_block_in`]: when the region's container cannot be
    /// written, which the containers this view resolved rule out.
    pub fn insert_block_before_in(
        &mut self,
        region: Region,
        after: &str,
        next: Option<&str>,
        kind: &str,
    ) -> String {
        let container = region_obj(self.c, region).clone();
        let key_of = |doc: &AutoCommit, id: &str| {
            let obj = block_obj(doc, &container, id)?;
            doc.bytes_at(&obj, BLOCK_ORDER)
        };
        let bounds = match (self.bulk_active(), next) {
            (false, Some(next)) => key_of(self.doc, after).zip(key_of(self.doc, next)),
            _ => None,
        };
        let Some((low, high)) =
            bounds.filter(|(low, high)| tilo_resource::order::bytes::cmp(low, high).is_lt())
        else {
            return self.insert_block_in(region, Some(after), kind);
        };
        let id = self.ids.mint();
        let key =
            tilo_resource::order::bytes::between_salted(Some(&low), Some(&high), order_salt(&id));
        let obj = self
            .doc
            .put_object(&container, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc
            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
            .unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc
            .put_object(&obj, BLOCK_ATTRS, ObjType::Map)
            .unwrap();
        self.record_order(&container, &id, &key);
        id
    }

    /// Splice text into a block living in `region`.
    pub fn splice_text_in(
        &mut self,
        region: Region,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
        if self.bulk_splice(block_id, idx, del, ins) {
            return;
        }
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    /// Add an inline mark over a block living in `region`.
    pub fn add_mark_in(
        &mut self,
        region: Region,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<ScalarValue>,
    ) {
        if self.bulk_active() {
            let container = region_obj(self.c, region).clone();
            self.stage_mark(container, block_id, start, end, name, value.into());
            return;
        }
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Mark `[start, end)` of a block living in `region` as written by `author`
    /// ([`MARK_AUTHOR`](crate::schema::MARK_AUTHOR)), as
    /// [`tilo_automerge::mark_text_author`] marks plain text: the mark covers exactly that text
    /// and does not expand, so nothing inserted beside it later — by anyone — is taken in. Every
    /// write names its own author.
    ///
    /// That also keeps typing cheap. An expanding end takes in the next key, so a mark per key
    /// nests inside the one before and every earlier mark stays open at the caret: each key
    /// would pay for all the keys typed before it.
    pub fn mark_author_in(
        &mut self,
        region: Region,
        block_id: &str,
        start: usize,
        end: usize,
        author: &str,
    ) {
        self.flush_bulk();
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let mark = Mark::new(crate::schema::MARK_AUTHOR.to_owned(), author, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::None);
        }
    }

    /// Clear the mark `name` over `[start, end)` of a block living in `region` (the region
    /// counterpart of [`Self::remove_mark`]).
    pub fn remove_mark_in(
        &mut self,
        region: Region,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
    ) {
        self.flush_bulk();
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let _ = self.doc.unmark(&tid, name, start, end, ExpandMark::After);
        }
    }

    /// Set a scalar attribute on a block living in `region`.
    pub fn set_attr_in(
        &mut self,
        region: Region,
        block_id: &str,
        key: &str,
        value: impl Into<ScalarValue>,
    ) {
        let container = region_obj(self.c, region).clone();
        let Some(value) = self.stage_attr(&container, block_id, key, value.into()) else {
            return;
        };
        let Some((_, obj)) = region_block(self.doc, self.c, region, block_id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) else {
            return;
        };
        let _ = self.doc.put(&attrs, key, value);
    }

    /// Remove a block attribute in `region` (a list item that stops being a
    /// task, a code line that becomes prose). Absent is the attribute's
    /// default, so readers need no tombstone value.
    pub fn remove_attr_in(&mut self, region: Region, block_id: &str, key: &str) {
        let container = region_obj(self.c, region).clone();
        if self.unstage_attr(&container, block_id, key) {
            return;
        }
        let Some(obj) = block_obj(self.doc, &container, block_id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) else {
            return;
        };
        if matches!(self.doc.get(&attrs, key), Ok(Some(_))) {
            let _ = self.doc.delete(&attrs, key);
        }
    }

    /// Insert a page-number field at `idx` in a region block: splices the
    /// placeholder character and covers it with a `field` mark valued
    /// `page-number`. The field then merges with concurrent text edits and
    /// round-trips through the codecs.
    pub fn insert_page_number_field(&mut self, region: Region, block_id: &str, idx: usize) {
        self.splice_text_in(region, block_id, idx, 0, &FIELD_PLACEHOLDER.to_string());
        self.add_mark_in(
            region,
            block_id,
            idx,
            idx.saturating_add(1),
            MARK_FIELD,
            FIELD_PAGE_NUMBER,
        );
    }

    /// Create a new note (footnote/endnote) with an empty block sequence and return its freshly-minted id
    /// (filled in via [`Self::insert_note_block`] + [`Self::splice_note_text`]); id shares the body's generator so it never collides.
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_note(&mut self, kind: NoteKind) -> String {
        let notes = self.c.notes.clone();
        let id = self.ids.mint();
        let note = self
            .doc
            .put_object(&notes, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc.put(&note, NOTE_KIND, kind.as_str()).unwrap();
        self.doc
            .put_object(&note, NOTE_BLOCKS, ObjType::Map)
            .unwrap();
        id
    }

    /// Place a note reference marker at `idx` in a body block: splice the placeholder character and cover it
    /// with a `note-ref` mark valued with the note id (inverse of import, which re-creates both the note and its marker).
    pub fn insert_note_ref(&mut self, block_id: &str, idx: usize, note_id: &str) {
        self.splice_text(block_id, idx, 0, &FIELD_PLACEHOLDER.to_string());
        self.add_mark(block_id, idx, idx.saturating_add(1), MARK_NOTE_REF, note_id);
    }

    /// Convenience: create a footnote, place its reference marker at `idx` in `block_id`, and return the new note id.
    pub fn insert_footnote(&mut self, block_id: &str, idx: usize) -> String {
        let id = self.insert_note(NoteKind::Footnote);
        self.insert_note_ref(block_id, idx, &id);
        id
    }

    /// Convenience: create an endnote, place its reference marker at `idx` in `block_id`, and return the new note id.
    pub fn insert_endnote(&mut self, block_id: &str, idx: usize) -> String {
        let id = self.insert_note(NoteKind::Endnote);
        self.insert_note_ref(block_id, idx, &id);
        id
    }

    /// Resolve-or-create a note's block container (`note.blocks`).
    fn note_blocks_container(&mut self, note_id: &str) -> Option<ObjId> {
        let note = note_obj(self.doc, self.c, note_id)?;
        match self.doc.get(&note, NOTE_BLOCKS) {
            Ok(Some((Value::Object(ObjType::Map), bid))) => Some(bid),
            _ => self.doc.put_object(&note, NOTE_BLOCKS, ObjType::Map).ok(),
        }
    }

    /// Insert a block into a note's content (created lazily); returns its id, or `None` if the note id is unknown.
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_note_block(
        &mut self,
        note_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let container = self.note_blocks_container(note_id)?;
        if self.bulk_active() {
            return Some(self.stage_block(container, after, kind));
        }
        let id = self.ids.mint();
        let key = self.order_after_in(&container, after, &id);
        let obj = self
            .doc
            .put_object(&container, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc
            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
            .unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc
            .put_object(&obj, BLOCK_ATTRS, ObjType::Map)
            .unwrap();
        create_block_text(self.doc, &obj);
        self.record_order(&container, &id, &key);
        Some(id)
    }

    /// Resolve-or-create the Text object of a note's block.
    fn note_text_obj(&mut self, note_id: &str, block_id: &str) -> Option<ObjId> {
        let container = self.note_blocks_container(note_id)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        ensure_block_text(self.doc, &obj)
    }

    pub fn splice_note_text(
        &mut self,
        note_id: &str,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
        if self.bulk_splice(block_id, idx, del, ins) {
            return;
        }
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    /// Add an inline mark over a note's block text.
    pub fn add_note_mark(
        &mut self,
        note_id: &str,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<ScalarValue>,
    ) {
        if self.bulk_active() {
            if let Some(container) = self.note_blocks_container(note_id) {
                self.stage_mark(container, block_id, start, end, name, value.into());
            }
            return;
        }
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Permanently resolve a comment id. The grow-only marker gives delete-wins
    /// semantics when a reply is authored against a divergent copy of the
    /// thread and merged later.
    pub fn tombstone_comment(&mut self, comment_id: &str) {
        let comments = self.c.comments.clone();
        let key = format!("{COMMENT_TOMBSTONE_PREFIX}{comment_id}");
        let _ = self.doc.put(&comments, key, true);
    }

    /// Create a new comment with an `author`, a `date` string, and an empty block sequence; returns its
    /// freshly-minted id (shares the body's generator so it never collides). Content is filled via
    /// [`Self::insert_comment_block`] + [`Self::splice_comment_text`]; the body anchor via [`Self::add_comment_mark`].
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_comment(&mut self, author: &str, date: &str) -> String {
        let comments = self.c.comments.clone();
        let id = self.ids.mint();
        let comment = self
            .doc
            .put_object(&comments, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc.put(&comment, COMMENT_AUTHOR, author).unwrap();
        self.doc.put(&comment, COMMENT_DATE, date).unwrap();
        self.doc
            .put_object(&comment, COMMENT_BLOCKS, ObjType::Map)
            .unwrap();
        id
    }

    /// Anchor a comment to the body range `[start, end)` of `block_id`: cover it with a `comment` mark
    /// valued with the comment id. The mark merges with concurrent text edits and round-trips through the codecs.
    pub fn add_comment_mark(&mut self, block_id: &str, start: usize, end: usize, comment_id: &str) {
        self.add_mark(block_id, start, end, MARK_COMMENT, comment_id);
    }

    /// Convenience: create a comment authored by `author` with body `text` (a single paragraph), anchor it to
    /// `[start, end)` of `block_id`, and return the new comment id. For multi-paragraph content, use [`Self::insert_comment`] directly.
    pub fn add_comment(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        author: &str,
        text: &str,
    ) -> String {
        let id = self.insert_comment(author, "");
        if let Some(b) = self.insert_comment_block(&id, None, "paragraph") {
            self.splice_comment_text(&id, &b, 0, 0, text);
        }
        self.add_comment_mark(block_id, start, end, &id);
        id
    }

    /// Create one durable reply below `comment_id`. The reply and its blocks
    /// live in the already-eager comments root, making ids stable under action
    /// replay and collision-free under concurrent authors (including legacy
    /// comments that predate threaded replies).
    pub fn insert_comment_reply(
        &mut self,
        comment_id: &str,
        author: &str,
        date: &str,
    ) -> Option<String> {
        // Refuse a stale/deleted parent before minting an id. The comments root
        // is shared from schema creation onward, so concurrent reply inserts
        // never contend on a lazily-created nested object.
        comment_obj(self.doc, self.c, comment_id)?;
        let replies = self.c.comments.clone();
        let id = self.ids.mint();
        let reply = self
            .doc
            .put_object(&replies, id.as_str(), ObjType::Map)
            .ok()?;
        self.doc.put(&reply, COMMENT_REPLY_AUTHOR, author).ok()?;
        self.doc.put(&reply, COMMENT_REPLY_DATE, date).ok()?;
        self.doc
            .put(&reply, COMMENT_REPLY_PARENT, comment_id)
            .ok()?;
        self.doc
            .put_object(&reply, COMMENT_REPLY_BLOCKS, ObjType::Map)
            .ok()?;
        Some(id)
    }

    /// Resolve-or-create the block map for a reply. As with comment bodies,
    /// this accepts older/incomplete data and only writes when editing it.
    fn comment_reply_blocks_container(
        &mut self,
        comment_id: &str,
        reply_id: &str,
    ) -> Option<ObjId> {
        let reply = comment_reply_obj(self.doc, self.c, comment_id, reply_id)?;
        match self.doc.get(&reply, COMMENT_REPLY_BLOCKS) {
            Ok(Some((Value::Object(ObjType::Map), blocks))) => Some(blocks),
            _ => self
                .doc
                .put_object(&reply, COMMENT_REPLY_BLOCKS, ObjType::Map)
                .ok(),
        }
    }

    /// Insert a rich-text block into a reply, returning `None` only when the
    /// addressed comment/reply no longer exists.
    pub fn insert_comment_reply_block(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let container = self.comment_reply_blocks_container(comment_id, reply_id)?;
        let id = self.ids.mint();
        let key = self.order_after_in(&container, after, &id);
        let obj = self
            .doc
            .put_object(&container, id.as_str(), ObjType::Map)
            .ok()?;
        self.doc
            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
            .ok()?;
        self.doc.put(&obj, BLOCK_KIND, kind).ok()?;
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).ok()?;
        create_block_text(self.doc, &obj);
        self.record_order(&container, &id, &key);
        Some(id)
    }

    fn comment_reply_text_obj(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        block_id: &str,
    ) -> Option<ObjId> {
        let container = self.comment_reply_blocks_container(comment_id, reply_id)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        ensure_block_text(self.doc, &obj)
    }

    pub fn splice_comment_reply_text(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
        if let Some(text) = self.comment_reply_text_obj(comment_id, reply_id, block_id) {
            let _ = self.doc.splice_text(&text, idx, del, ins);
        }
    }

    /// Set a scalar attribute (heading `level`, list `ordered`, …) of one
    /// reply block.
    pub fn set_comment_reply_block_attr(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        block_id: &str,
        key: &str,
        value: impl Into<ScalarValue>,
    ) {
        let Some(container) = self.comment_reply_blocks_container(comment_id, reply_id) else {
            return;
        };
        let Some(obj) = block_obj(self.doc, &container, block_id) else {
            return;
        };
        if let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) {
            let _ = self.doc.put(&attrs, key, value);
        }
    }

    /// Mark characters `[start, end)` of one reply block.
    pub fn add_comment_reply_mark(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<ScalarValue>,
    ) {
        if let Some(text) = self.comment_reply_text_obj(comment_id, reply_id, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&text, mark, ExpandMark::After);
        }
    }

    /// Add a one-paragraph reply, the common authoring path. Multi-paragraph
    /// replies can use `insert_comment_reply` plus the block methods above.
    pub fn add_comment_reply(
        &mut self,
        comment_id: &str,
        author: &str,
        date: &str,
        text: &str,
    ) -> Option<String> {
        let id = self.insert_comment_reply(comment_id, author, date)?;
        let block = self.insert_comment_reply_block(comment_id, &id, None, "paragraph")?;
        self.splice_comment_reply_text(comment_id, &id, &block, 0, 0, text);
        Some(id)
    }

    /// Resolve-or-create a comment's block container (`comment.blocks`).
    fn comment_blocks_container(&mut self, comment_id: &str) -> Option<ObjId> {
        let comment = comment_obj(self.doc, self.c, comment_id)?;
        match self.doc.get(&comment, COMMENT_BLOCKS) {
            Ok(Some((Value::Object(ObjType::Map), bid))) => Some(bid),
            _ => self
                .doc
                .put_object(&comment, COMMENT_BLOCKS, ObjType::Map)
                .ok(),
        }
    }

    /// Insert a block into a comment's content (created lazily); returns its id, or `None` if the comment id is unknown.
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_comment_block(
        &mut self,
        comment_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let container = self.comment_blocks_container(comment_id)?;
        let id = self.ids.mint();
        let key = self.order_after_in(&container, after, &id);
        let obj = self
            .doc
            .put_object(&container, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc
            .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
            .unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc
            .put_object(&obj, BLOCK_ATTRS, ObjType::Map)
            .unwrap();
        create_block_text(self.doc, &obj);
        self.record_order(&container, &id, &key);
        Some(id)
    }

    /// Resolve-or-create the Text object of a comment's block.
    fn comment_text_obj(&mut self, comment_id: &str, block_id: &str) -> Option<ObjId> {
        let container = self.comment_blocks_container(comment_id)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        ensure_block_text(self.doc, &obj)
    }

    pub fn splice_comment_text(
        &mut self,
        comment_id: &str,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
        if let Some(tid) = self.comment_text_obj(comment_id, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    /// Create a tracked-change record of `kind` with an `author` + `date`; returns its freshly-minted id (shares
    /// the body's generator). The body anchor is placed separately via [`Self::add_change_mark`] — callers usually go through [`Self::track_insert`]/[`Self::track_delete`].
    ///
    /// # Panics
    /// Panics if `self.c`'s cached container ids no longer resolve in `self.doc` — see the note on `impl DocMut` above.
    pub fn insert_change(
        &mut self,
        kind: crate::snapshot::ChangeKind,
        author: &str,
        date: &str,
    ) -> String {
        let changes = self.c.changes.clone();
        let id = self.ids.mint();
        let change = self
            .doc
            .put_object(&changes, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc.put(&change, CHANGE_KIND, kind.as_str()).unwrap();
        self.doc.put(&change, CHANGE_AUTHOR, author).unwrap();
        self.doc.put(&change, CHANGE_DATE, date).unwrap();
        id
    }

    /// Anchor a tracked change to the body range `[start, end)` of `block_id`: cover it with the `ins`/`del`
    /// mark matching the change's kind, valued with the change id.
    pub fn add_change_mark(
        &mut self,
        kind: crate::snapshot::ChangeKind,
        block_id: &str,
        start: usize,
        end: usize,
        change_id: &str,
    ) {
        self.add_mark(block_id, start, end, change_mark_name(kind), change_id);
    }

    /// Track an **insertion**: splice `text` into `block_id` at `char_idx`, then flag it with an `ins` mark
    /// valued with a fresh change id. The text is real content — accept keeps it, reject removes it.
    pub fn track_insert(
        &mut self,
        block_id: &str,
        char_idx: usize,
        text: &str,
        author: &str,
        date: &str,
    ) -> String {
        let id = self.insert_change(crate::snapshot::ChangeKind::Ins, author, date);
        let n = text.chars().count();
        self.splice_text(block_id, char_idx, 0, text);
        if n > 0 {
            self.add_change_mark(
                crate::snapshot::ChangeKind::Ins,
                block_id,
                char_idx,
                char_idx.saturating_add(n),
                &id,
            );
        }
        id
    }

    /// Track a **deletion**: flag the existing body range `[start, end)` with a `del` mark valued with a fresh
    /// change id. The text is *retained* (struck, not removed) so reviewers can see it; accept removes it, reject keeps it.
    pub fn track_delete(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        author: &str,
        date: &str,
    ) -> String {
        let id = self.insert_change(crate::snapshot::ChangeKind::Del, author, date);
        if end > start {
            self.add_change_mark(crate::snapshot::ChangeKind::Del, block_id, start, end, &id);
        }
        id
    }

    /// Accept a tracked change, applying it permanently: an **insertion** keeps the text and drops its `ins`
    /// mark; a **deletion** removes the struck text (and its `del` mark). The change record is removed from
    /// the `changes` container either way. A no-op for an unknown id.
    pub fn accept_change(&mut self, change_id: &str) {
        self.flush_bulk();
        let Some(kind) = change_kind(self.doc, self.c, change_id) else {
            return;
        };
        let range = change_range(self.doc, self.c, change_id);
        match kind {
            crate::snapshot::ChangeKind::Ins => {
                // Keep the text; just remove the revision mark over its range.
                if let Some((b, start, end)) = range
                    && end > start
                {
                    self.remove_mark(&b, start, end, MARK_INS);
                }
            }
            crate::snapshot::ChangeKind::Del => {
                // Remove the struck text (the `del` mark goes with it).
                if let Some((b, start, end)) = range
                    && end > start
                {
                    let del_len = isize::try_from(end.saturating_sub(start)).unwrap_or(isize::MAX);
                    self.splice_text(&b, start, del_len, "");
                }
            }
        }
        self.remove_change_record(change_id);
    }

    /// Reject a tracked change, reverting it (inverse of [`Self::accept_change`]): an **insertion** removes the
    /// inserted text (and its `ins` mark); a **deletion** keeps the struck text and drops its `del` mark. The
    /// change record is removed from the `changes` container either way. A no-op for an unknown id.
    pub fn reject_change(&mut self, change_id: &str) {
        self.flush_bulk();
        let Some(kind) = change_kind(self.doc, self.c, change_id) else {
            return;
        };
        let range = change_range(self.doc, self.c, change_id);
        match kind {
            crate::snapshot::ChangeKind::Ins => {
                // Undo the insertion: delete the inserted text.
                if let Some((b, start, end)) = range
                    && end > start
                {
                    let del_len = isize::try_from(end.saturating_sub(start)).unwrap_or(isize::MAX);
                    self.splice_text(&b, start, del_len, "");
                }
            }
            crate::snapshot::ChangeKind::Del => {
                // Keep the text; just drop the deletion mark over its range.
                if let Some((b, start, end)) = range
                    && end > start
                {
                    self.remove_mark(&b, start, end, MARK_DEL);
                }
            }
        }
        self.remove_change_record(change_id);
    }

    /// Delete a change's record from the `changes` container (no-op if absent).
    fn remove_change_record(&mut self, change_id: &str) {
        let _ = self.doc.delete(&self.c.changes, change_id);
    }

    /// Anchor a **bookmark** (named anchor) over `[start, end)` of `block_id`: cover it with a `bookmark` mark
    /// valued with `name`. A zero-width (point) bookmark is widened to one character when the block has text
    /// after the anchor (a mark needs a non-empty range to live on); an end-of-block point sits over the last character.
    pub fn add_bookmark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        let (mut s, mut e) = (start, end);
        if e <= s {
            // Widen a point bookmark to one character so the mark has somewhere to live.
            let blen = self.block_text_len(block_id);
            if s < blen {
                e = s.saturating_add(1);
            } else if blen > 0 {
                // End-of-block point: sit over the last character.
                s = blen.saturating_sub(1);
                e = blen;
            }
        }
        if e > s {
            self.add_mark(block_id, s, e, MARK_BOOKMARK, name);
        }
    }

    /// Anchor a **cross-reference** (internal link) over `[start, end)` of `block_id`: cover it with a `link`
    /// mark valued `#<target>`, where `target` is a bookmark name or a heading's derived id (see [`heading_ref_id`]).
    /// The leading `#` distinguishes an internal reference from an external URL; a target already starting with `#` is used verbatim.
    pub fn add_cross_ref(&mut self, block_id: &str, start: usize, end: usize, target: &str) {
        if end <= start {
            return;
        }
        let value = if target.starts_with(CROSS_REF_PREFIX) {
            target.to_string()
        } else {
            format!("{CROSS_REF_PREFIX}{target}")
        };
        self.add_mark(block_id, start, end, MARK_LINK, value.as_str());
    }

    /// Remove every block from a region (clearing a header/footer).
    pub fn clear_region(&mut self, region: Region) {
        self.flush_bulk();
        let container = region_obj(self.c, region).clone();
        for id in ordered_keys(self.doc, &container) {
            let _ = self.doc.delete(&container, &id);
        }
        // Drop the (now-empty) container's cached order; it rebuilds lazily.
        self.ids.order.by_container.remove(&container);
    }

    /// Resolve-or-create the Text object of a region block.
    fn region_text_obj(&mut self, region: Region, block_id: &str) -> Option<ObjId> {
        let (_, obj) = region_block(self.doc, self.c, region, block_id)?;
        ensure_block_text(self.doc, &obj)
    }

    pub fn remove_block(&mut self, id: &str) {
        let container = self.c.blocks.clone();
        if self.unstage_block(&container, id) {
            return;
        }
        self.flush_bulk();
        let _ = self.doc.delete(&self.c.blocks, id);
        let container = self.c.blocks.clone();
        self.forget_order(&container, id);
    }

    /// Remove block `id` from `region` (the region counterpart of [`Self::remove_block`]).
    pub fn remove_block_in(&mut self, region: Region, id: &str) {
        if region == Region::Body {
            self.remove_block(id);
            return;
        }
        self.flush_bulk();
        // The block may live in a concurrently created candidate container
        // (see "conflicting region containers").
        let Some((container, _)) = region_block(self.doc, self.c, region, id) else {
            return;
        };
        let _ = self.doc.delete(&container, id);
        self.forget_order(&container, id);
    }

    /// Remove block `id` from note `note_id` (the note counterpart of
    /// [`Self::remove_block_in`]).
    pub fn remove_note_block(&mut self, note_id: &str, id: &str) {
        self.flush_bulk();
        let Some(container) = note_blocks_obj(self.doc, self.c, note_id) else {
            return;
        };
        let _ = self.doc.delete(&container, id);
        self.forget_order(&container, id);
    }

    /// Remove one structural block and leave a grow-only semantic tombstone in
    /// the same region container. Action replay uses this for delete-wins
    /// behavior against divergent edits of the removed object.
    pub fn tombstone_block_in(&mut self, region: Region, id: &str) {
        self.flush_bulk();
        let container = region_obj(self.c, region).clone();
        let key = format!("{BLOCK_TOMBSTONE_PREFIX}{id}");
        let _ = self.doc.put(&container, key, true);
        let _ = self.doc.delete(&container, id);
        self.forget_order(&container, id);
    }

    /// Move block `id` to a new position, ordered directly after `after` (or first, when `after` is `None`).
    ///
    /// # Panics
    /// Panics if `self.c.blocks` no longer resolves in `self.doc` — see the note on `impl DocMut` above.
    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        self.flush_bulk();
        // Compute the target key with the block still at its old position (mirrors the pre-index behaviour).
        let key = self.order_after(after, id);
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, id) {
            self.doc
                .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key.clone()))
                .unwrap();
            let container = self.c.blocks.clone();
            self.record_order(&container, id, &key); // re-keys the moved block in the index
        }
    }

    fn text_obj(&mut self, block_id: &str) -> Option<ObjId> {
        let obj = block_obj(self.doc, &self.c.blocks, block_id)?;
        ensure_block_text(self.doc, &obj)
    }

    pub fn splice_text(&mut self, block_id: &str, idx: usize, del: isize, ins: &str) {
        if self.bulk_splice(block_id, idx, del, ins) {
            return;
        }
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    pub fn add_mark(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<ScalarValue>,
    ) {
        if self.bulk_active() {
            let container = self.c.blocks.clone();
            self.stage_mark(container, block_id, start, end, name, value.into());
            return;
        }
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Set a scalar attribute (e.g. heading `level`, list `ordered`) in the block's `attrs` map.
    pub fn set_attr(&mut self, block_id: &str, key: &str, value: impl Into<ScalarValue>) {
        let container = self.c.blocks.clone();
        let Some(value) = self.stage_attr(&container, block_id, key, value.into()) else {
            return;
        };
        let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) else {
            return;
        };
        let _ = self.doc.put(&attrs, key, value);
    }

    /// Set a document-level meta string (e.g. the serialized stylesheet).
    pub fn set_meta_str(&mut self, key: &str, value: &str) {
        let _ = self.doc.put(&self.c.meta, key, value);
    }

    /// Set the document-wide multi-column layout: `count` newspaper-style columns for the body, with an
    /// optional `gap` (px). Each is its own scalar register under `meta`, so replicas changing different
    /// aspects field-level merge. A `count` of one with no gap clears the setting. Round-trips through `.odt`/`.docx`.
    pub fn set_columns(&mut self, count: u32, gap: Option<f64>) {
        let count = count.max(1);
        if count <= 1 {
            let _ = self.doc.delete(&self.c.meta, META_COLUMNS);
        } else {
            let _ = self.doc.put(&self.c.meta, META_COLUMNS, u64::from(count));
        }
        match gap {
            Some(g) => {
                let _ = self.doc.put(&self.c.meta, META_COLUMN_GAP, g);
            }
            None => {
                let _ = self.doc.delete(&self.c.meta, META_COLUMN_GAP);
            }
        }
    }

    /// Set the document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`), stored as a single `Str`
    /// register under `meta` so replicas changing the language field-level merge rather than clobber. An empty
    /// `code` clears the setting. Round-trips through `.odt`/`.docx`; supports spell-check locale and a11y language.
    pub fn set_language(&mut self, code: &str) {
        if code.is_empty() {
            let _ = self.doc.delete(&self.c.meta, META_LANGUAGE);
        } else {
            let _ = self.doc.put(&self.c.meta, META_LANGUAGE, code);
        }
    }

    /// Resolve-or-create the `meta.props` document-properties sub-map (created lazily so old documents migrate transparently).
    fn props_map(&mut self) -> ObjId {
        match self.doc.get(&self.c.meta, KEY_PROPS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => self
                .doc
                .put_object(&self.c.meta, KEY_PROPS, ObjType::Map)
                .unwrap(),
        }
    }

    /// Set a document property (`title`/`author`/`subject`/`keywords`/`created`/`modified`/`generator` — see
    /// [`DOC_PROP_KEYS`](crate::schema::DOC_PROP_KEYS)). Each is its own scalar register in `meta.props`, so replicas editing different properties field-level merge.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        let props = self.props_map();
        let _ = self.doc.put(&props, name, value);
    }

    /// Upsert a named style into the styles container. Each property is written as its own scalar register
    /// *into the existing sub-map*, so replicas editing different properties of the same style field-level merge.
    ///
    /// # Panics
    /// Panics if `self.c.styles` no longer resolves in `self.doc` — see the note on `impl DocMut` above.
    pub fn set_style(&mut self, id: &str, props: &BTreeMap<String, AttrVal>) {
        let obj = match self.doc.get(&self.c.styles, id) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => self
                .doc
                .put_object(&self.c.styles, id, ObjType::Map)
                .unwrap(),
        };
        for (k, v) in props {
            let _ = match v {
                AttrVal::Int(n) => self.doc.put(&obj, k.as_str(), *n),
                AttrVal::Bool(b) => self.doc.put(&obj, k.as_str(), *b),
                AttrVal::Str(s) => self.doc.put(&obj, k.as_str(), s.as_str()),
                AttrVal::Float(f) => self.doc.put(&obj, k.as_str(), *f),
            };
        }
    }

    /// Remove a named style from the styles container.
    pub fn remove_style(&mut self, id: &str) {
        let _ = self.doc.delete(&self.c.styles, id);
    }

    /// Permanently delete a custom style id across divergent replicas.
    pub fn tombstone_style(&mut self, id: &str) {
        let key = format!("{STYLE_TOMBSTONE_PREFIX}{id}");
        let _ = self.doc.put(&self.c.styles, key, true);
        self.remove_style(id);
    }

    /// Change a block's kind (e.g. `paragraph` ↔ `heading` ↔ `list_item`).
    pub fn set_kind(&mut self, block_id: &str, kind: &str) {
        let container = self.c.blocks.clone();
        if self.stage_kind(&container, block_id, kind) {
            return;
        }
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) {
            let _ = self.doc.put(&obj, BLOCK_KIND, kind);
        }
    }

    /// Set the kind of a block living in `region` (the region counterpart of [`Self::set_kind`]).
    pub fn set_kind_in(&mut self, region: Region, block_id: &str, kind: &str) {
        if region == Region::Body {
            self.set_kind(block_id, kind);
            return;
        }
        self.flush_bulk();
        if let Some(obj) = region_block_obj(self.doc, self.c, region, block_id) {
            let _ = self.doc.put(&obj, BLOCK_KIND, kind);
        }
    }

    /// Set the kind of a block living in note `note_id` (the note counterpart
    /// of [`Self::set_kind_in`]).
    pub fn set_note_kind(&mut self, note_id: &str, block_id: &str, kind: &str) {
        self.flush_bulk();
        let Some(container) = note_blocks_obj(self.doc, self.c, note_id) else {
            return;
        };
        if let Some(obj) = block_obj(self.doc, &container, block_id) {
            let _ = self.doc.put(&obj, BLOCK_KIND, kind);
        }
    }

    /// Set an attribute of a block living in note `note_id` (the note
    /// counterpart of [`Self::set_attr_in`]).
    pub fn set_note_attr(
        &mut self,
        note_id: &str,
        block_id: &str,
        key: &str,
        value: impl Into<ScalarValue>,
    ) {
        self.flush_bulk();
        let Some(container) = note_blocks_obj(self.doc, self.c, note_id) else {
            return;
        };
        let Some(value) = self.stage_attr(&container, block_id, key, value.into()) else {
            return;
        };
        let Some(obj) = block_obj(self.doc, &container, block_id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) else {
            return;
        };
        let _ = self.doc.put(&attrs, key, value);
    }

    /// Remove an attribute of a block living in note `note_id` (the note
    /// counterpart of [`Self::remove_attr_in`]).
    pub fn remove_note_attr(&mut self, note_id: &str, block_id: &str, key: &str) {
        self.flush_bulk();
        let Some(container) = note_blocks_obj(self.doc, self.c, note_id) else {
            return;
        };
        if self.unstage_attr(&container, block_id, key) {
            return;
        }
        let Some(obj) = block_obj(self.doc, &container, block_id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), attrs))) = self.doc.get(&obj, BLOCK_ATTRS) else {
            return;
        };
        if matches!(self.doc.get(&attrs, key), Ok(Some(_))) {
            let _ = self.doc.delete(&attrs, key);
        }
    }

    /// Remove a mark named `name` over `[start, end)` of a note block's text
    /// (the note counterpart of [`Self::remove_mark_in`]).
    pub fn remove_note_mark(
        &mut self,
        note_id: &str,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
    ) {
        self.flush_bulk();
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let _ = self.doc.unmark(&tid, name, start, end, ExpandMark::After);
        }
    }

    /// Remove a mark named `name` over `[start, end)` of the block's text.
    pub fn remove_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        self.flush_bulk();
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.unmark(&tid, name, start, end, ExpandMark::After);
        }
    }

    /// Stash a verbatim part in the preservation layer (e.g. an unmodeled ODF part), keyed by `key` and already
    /// split into ordered atoms (verbatim byte slices that concatenate back to the whole part); re-emitted unchanged
    /// on export. How a part splits is the format's business (the codecs split XML at its first branching element). Each atom becomes an independent automerge object keyed by a stable id, so distinct atoms merge across replicas. Replaces any existing part under `key`.
    ///
    /// # Panics
    /// Panics if `self.c.preserved` no longer resolves in `self.doc` — see the note on `impl DocMut` above.
    pub fn set_preserved_atoms(&mut self, key: &str, atoms: &[Vec<u8>]) {
        // Replace the part wholesale: a fresh map keyed by per-atom stable ids.
        let part = self
            .doc
            .put_object(&self.c.preserved, key, ObjType::Map)
            .unwrap();
        for (idx, atom) in atoms.iter().enumerate() {
            let id = preserved_atom_id(idx, atom);
            let seg = self
                .doc
                .put_object(&part, id.as_str(), ObjType::Map)
                .unwrap();
            // Order key: the atom's source position, big-endian, so segments sort back into source order deterministically.
            // A document has nowhere near u32::MAX top-level XML atoms; clamping is a formality, not a real limit.
            let ord = u32::try_from(idx)
                .unwrap_or(u32::MAX)
                .to_be_bytes()
                .to_vec();
            let _ = self
                .doc
                .put(&seg, PRESERVED_SEG_ORD, ScalarValue::Bytes(ord));
            let _ = self
                .doc
                .put(&seg, PRESERVED_SEG_BYTES, ScalarValue::Bytes(atom.clone()));
        }
    }

    /// Overwrite a single preserved atom's verbatim bytes in place, by its stable segment id, without disturbing
    /// the part's other atoms. No-op if the part isn't segmented or the segment id is absent.
    pub fn set_preserved_atom_bytes(&mut self, key: &str, segment_id: &str, bytes: &[u8]) {
        let Ok(Some((Value::Object(ObjType::Map), part))) = self.doc.get(&self.c.preserved, key)
        else {
            return;
        };
        if let Ok(Some((Value::Object(ObjType::Map), seg))) = self.doc.get(&part, segment_id) {
            let _ = self.doc.put(
                &seg,
                PRESERVED_SEG_BYTES,
                ScalarValue::Bytes(bytes.to_vec()),
            );
        }
    }

    /// Take `bytes` as a binary input of this document: add them to `inputs` (which the caller persists
    /// in its content store) and record their descriptor. Returns the content address an `image` block's
    /// `blob` attr names.
    pub fn attach(&mut self, bytes: &[u8], inputs: &mut ContentInputs) -> String {
        let meta = tilo_content::sniff(bytes);
        let hash = inputs.add(bytes);
        self.reference_content(&hash, &meta);
        hash
    }

    /// Record that this document refers to the content at `hash`, described by `meta` (what a
    /// renderer needs to reserve space before the bytes arrive). The bytes themselves are the
    /// caller's to keep in its content store. Idempotent. The descriptor is never removed, so the map names every input any version refers to.
    ///
    /// # Panics
    /// Panics if `self.c.root` no longer resolves in `self.doc` — see the note on `impl DocMut` above.
    pub fn reference_content(&mut self, hash: &str, meta: &ContentMeta) {
        let blobs = self.c.blobs.clone();
        // Idempotent by construction: the key is the content hash, so identical bytes address the same descriptor
        // and re-putting it would be pure tombstone churn for no change in meaning.
        if matches!(
            self.doc.get(&blobs, hash),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) {
            return;
        }
        if let Ok(d) = self.doc.put_object(&blobs, hash, ObjType::Map) {
            let _ = self
                .doc
                .put(&d, BLOB_MIME, ScalarValue::Str(meta.mime.as_str().into()));
            let _ = self.doc.put(
                &d,
                BLOB_LEN,
                ScalarValue::Int(i64::try_from(meta.len).unwrap_or(i64::MAX)),
            );
            let _ = self
                .doc
                .put(&d, BLOB_W, ScalarValue::Int(i64::from(meta.w)));
            let _ = self
                .doc
                .put(&d, BLOB_H, ScalarValue::Int(i64::from(meta.h)));
        }
    }

    /// Preserve the part `key` as a reference to content the caller keeps in its content store (a
    /// binary package part: an image, a font, an embedded object) rather than as bytes in the
    /// document. Replaces any existing part under `key`; the address is also recorded with
    /// [`Self::reference_content`], so it stays among the document's [`referenced_content`].
    ///
    /// # Panics
    /// Panics if `self.c.preserved` no longer resolves in `self.doc` — see the note on `impl DocMut` above.
    pub fn set_preserved_content(&mut self, key: &str, hash: &str, meta: &ContentMeta) {
        self.reference_content(hash, meta);
        self.doc
            .put(&self.c.preserved, key, ScalarValue::Str(hash.into()))
            .unwrap();
    }
}

#[cfg(test)]
mod snapshot_anchor_tests {
    use super::*;
    use crate::snapshot::ChangeKind;

    #[test]
    fn snapshot_reads_body_marks_once_even_without_annotations() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let mut dm = DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let first = dm.insert_block(None, "paragraph");
        dm.splice_text(&first, 0, 0, "Aé🦀e\u{301}");
        let second = dm.insert_block(Some(&first), "heading");
        dm.splice_text(&second, 0, 0, "Second block");
        MARK_READS.with(|count| count.set(0));
        let snap = materialize(&doc, &c);
        assert_eq!(snap.blocks.len(), 2);
        assert_eq!(
            MARK_READS.with(std::cell::Cell::get),
            2,
            "the bookmark projection must reuse marks already read for the body"
        );
    }

    #[test]
    fn snapshot_anchors_match_standalone_readers_after_merge_and_pending_edits() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let first;
        {
            let mut dm = DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            first = dm.insert_block(None, "paragraph");
            dm.splice_text(&first, 0, 0, "Aé🦀e\u{301} first text");
            let second = dm.insert_block(Some(&first), "heading");
            dm.splice_text(&second, 0, 0, "Second block");
            let comment = dm.add_comment(&second, 1, 4, "Ada", "Comment body");
            dm.add_comment_mark(&first, 1, 4, &comment);
            let insertion = dm.insert_change(ChangeKind::Ins, "Ada", "today");
            dm.add_change_mark(ChangeKind::Ins, &second, 1, 3, &insertion);
            dm.add_change_mark(ChangeKind::Ins, &first, 2, 4, &insertion);
            let deletion = dm.insert_change(ChangeKind::Del, "Bob", "today");
            dm.add_change_mark(ChangeKind::Del, &first, 5, 7, &deletion);
            dm.insert_comment("Unanchored", "today");
            dm.insert_change(ChangeKind::Ins, "Unanchored", "today");
            dm.add_bookmark(&first, 0, 1, "same-name");
            dm.add_bookmark(&second, 0, 2, "same-name");
        }
        doc.commit();
        let mut remote = doc.fork();
        remote.set_actor(automerge::ActorId::from(vec![97]));
        let mut remote_ids = IdGen::new(remote.get_actor().to_hex_string());
        DocMut {
            doc: &mut remote,
            c: &c,
            ids: &mut remote_ids,
        }
        .add_bookmark(&first, 8, 10, "remote");
        remote.commit();
        doc.merge_incoming(&mut remote).unwrap();
        DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        }
        .splice_text(&first, 0, 0, "pending ");
        let pending = doc.pending_ops();
        let heads = doc.base_heads();
        MARK_READS.with(|count| count.set(0));
        let snap = materialize(&doc, &c);
        assert_eq!(
            MARK_READS.with(std::cell::Cell::get),
            3,
            "two body blocks and one comment body are each read once"
        );
        assert_eq!(snap.comments, materialize_comments(&doc, &c));
        assert_eq!(snap.changes, materialize_changes(&doc, &c));
        assert_eq!(snap.bookmarks, bookmarks(&doc, &c));
        assert_eq!(doc.pending_ops(), pending);
        assert_eq!(doc.base_heads(), heads);
        assert!(snap.comments.iter().any(|comment| comment.range.is_none()));
        assert!(snap.changes.iter().any(|change| change.range.is_none()));
        assert_eq!(
            snap.bookmarks
                .iter()
                .filter(|bookmark| bookmark.name == "same-name")
                .count(),
            2
        );
    }

    #[test]
    fn snapshot_anchors_preserve_top_level_scope_and_malformed_change_kind() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (body, comment, change);
        {
            let mut dm = DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            body = dm.insert_block(None, "paragraph");
            dm.splice_text(&body, 0, 0, "body");
            comment = dm.insert_comment("Ada", "today");
            change = dm.insert_change(ChangeKind::Ins, "Ada", "today");
            dm.add_change_mark(ChangeKind::Ins, &body, 0, 2, &change);
            dm.add_mark(&body, 2, 4, MARK_BOOKMARK, true);
            let header = dm.insert_block_in(Region::Header, None, "paragraph");
            dm.splice_text_in(Region::Header, &header, 0, 0, "header");
            dm.add_mark_in(
                Region::Header,
                &header,
                0,
                2,
                MARK_COMMENT,
                comment.as_str(),
            );
            dm.add_mark_in(Region::Header, &header, 0, 2, MARK_BOOKMARK, "header-only");
        }
        let parent = block_obj(&doc, &c.blocks, &body).unwrap();
        let children = doc
            .put_object(&parent, BLOCK_CHILDREN, ObjType::Map)
            .unwrap();
        let child = doc.put_object(&children, "child", ObjType::Map).unwrap();
        doc.put(&child, BLOCK_KIND, "paragraph").unwrap();
        doc.put(&child, BLOCK_ORDER, ScalarValue::Bytes(vec![128]))
            .unwrap();
        let text = doc.put_object(&child, BLOCK_TEXT, ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "child").unwrap();
        doc.mark(
            &text,
            Mark::new(MARK_COMMENT.into(), comment.as_str(), 0, 2),
            ExpandMark::After,
        )
        .unwrap();
        doc.mark(
            &text,
            Mark::new(MARK_BOOKMARK.into(), "child-only", 0, 2),
            ExpandMark::After,
        )
        .unwrap();
        let change_obj = change_obj(&doc, &c, &change).unwrap();
        doc.delete(&change_obj, CHANGE_KIND).unwrap();
        let snap = materialize(&doc, &c);
        assert_eq!(snap.comments, materialize_comments(&doc, &c));
        assert_eq!(snap.changes, materialize_changes(&doc, &c));
        assert_eq!(snap.bookmarks, bookmarks(&doc, &c));
        assert!(snap.comments[0].range.is_none());
        assert!(snap.changes[0].range.is_none());
        assert!(snap.bookmarks.is_empty());
        assert!(
            !snap.blocks[0].children[0].marks.is_empty(),
            "nested marks are still rendered"
        );
        assert!(
            !snap.header[0].marks.is_empty(),
            "header marks are still rendered"
        );
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use crate::schema::*;
    use automerge::{AutoCommit, ROOT};

    fn props(name: &str, size: f64, bold: bool) -> BTreeMap<String, AttrVal> {
        let mut m = BTreeMap::new();
        m.insert(STYLE_NAME.to_string(), AttrVal::Str(name.into()));
        m.insert(STYLE_SIZE.to_string(), AttrVal::Float(size));
        m.insert(STYLE_BOLD.to_string(), AttrVal::Bool(bold));
        m.insert(STYLE_COLOR.to_string(), AttrVal::Str("#112233".into()));
        m
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn styles_and_doc_props_upsert_snapshot_and_open_old_documents() {
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_style("body", &props("Body", 16.5, false));
                dm.set_style("h1", &props("Heading 1", 30.0, true));
            }

            let mut got = style_ids(&doc, &c);
            got.compact_sort();
            assert_eq!(got, vec!["body".to_string(), "h1".to_string()]);

            let bp = style_props(&doc, &c, "body");
            assert_eq!(
                bp.get(STYLE_SIZE),
                Some(&AttrVal::Float(16.5)),
                "fractional size stored as F64"
            );
            assert_eq!(bp.get(STYLE_NAME), Some(&AttrVal::Str("Body".into())));
            assert_eq!(bp.get(STYLE_BOLD), Some(&AttrVal::Bool(false)));

            let snap = materialize(&doc, &c);
            assert_eq!(snap.styles.len(), 2);
            assert_eq!(
                snap.styles["h1"].get(STYLE_BOLD),
                Some(&AttrVal::Bool(true))
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            // No props map yet → empty reads, no panic, open still works.
            assert!(doc_props(&doc, &c).is_empty());
            assert_eq!(doc_prop(&doc, &c, PROP_TITLE), None);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_doc_prop(PROP_TITLE, "Hello");
                dm.set_doc_prop(PROP_AUTHOR, "Ada");
            }
            assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Hello"));
            assert_eq!(doc_prop(&doc, &c, PROP_AUTHOR).as_deref(), Some("Ada"));
            let all = doc_props(&doc, &c);
            assert_eq!(all.get("title").map(String::as_str), Some("Hello"));
            assert_eq!(all.get("author").map(String::as_str), Some("Ada"));
            let snap = materialize(&doc, &c);
            assert_eq!(
                snap.properties.get("title").map(String::as_str),
                Some("Hello")
            );
            // Field-level upsert: setting one property leaves the other intact.
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_doc_prop(PROP_TITLE, "Changed");
            }
            assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Changed"));
            assert_eq!(
                doc_prop(&doc, &c, PROP_AUTHOR).as_deref(),
                Some("Ada"),
                "untouched prop preserved"
            );
        }
        {
            // A document whose meta map predates the props sub-map must open + read as having no properties.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            // `init` does not create meta.props; confirm reads degrade gracefully.
            assert!(doc_props(&doc, &c).is_empty(), "no props map → empty");
            assert_eq!(doc_prop(&doc, &c, PROP_GENERATOR), None);
            let bytes = doc.save();
            let reloaded = AutoCommit::load(&bytes).unwrap();
            let c2 = open(&reloaded, &ROOT).unwrap();
            assert!(
                doc_props(&reloaded, &c2).is_empty(),
                "reload still opens with no props"
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_style("h1", &props("Heading 1", 30.0, true));
                // Touch only one property; the others must survive (field-level merge).
                let mut one = BTreeMap::new();
                one.insert(STYLE_SIZE.to_string(), AttrVal::Float(40.0));
                dm.set_style("h1", &one);
            }
            let h1 = style_props(&doc, &c, "h1");
            assert_eq!(
                h1.get(STYLE_SIZE),
                Some(&AttrVal::Float(40.0)),
                "updated property"
            );
            assert_eq!(
                h1.get(STYLE_BOLD),
                Some(&AttrVal::Bool(true)),
                "untouched property preserved"
            );
            assert_eq!(h1.get(STYLE_NAME), Some(&AttrVal::Str("Heading 1".into())));

            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.remove_style("h1");
            }
            assert!(style_ids(&doc, &c).is_empty(), "style removed");
        }
    }
}

#[cfg(test)]
mod genesis_detection_tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjType, ROOT};

    #[test]
    fn genesis_changes_are_told_apart_from_authored_and_shadowed_ones() {
        // Phase `the_genesis_change_is_told_apart_from_an_authored_one`.
        // The two helpers a caller uses to tell a schema change from a person's edit have no consumer in this
        // crate — the version view that filters on them lives elsewhere — so without these tests they can drift unnoticed.
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            doc.splice_text(&c.blocks, 0, 0, "").ok();
            doc.put(&ROOT, "title", "an edit somebody made").unwrap();
            doc.commit();

            let changes = doc.get_changes(&[]);
            let genesis: Vec<_> = changes
                .iter()
                .filter(|ch| is_schema_genesis_actor(ch.actor_id()))
                .collect();
            let has_authored = changes
                .iter()
                .any(|ch| !is_schema_genesis_actor(ch.actor_id()));

            assert!(
                !genesis.is_empty(),
                "the schema has to be written by the genesis actor"
            );
            assert!(
                has_authored,
                "the edit has to be attributed to this replica"
            );
            assert_eq!(
                genesis[0].actor_id().to_hex_string(),
                schema_genesis_actor_hex(),
                "the hex form must match what a recorded actor_hex is compared against"
            );
        }
        // Phase `a_second_minting_of_the_schema_is_reported_as_shadowed`.
        // A document created before the genesis existed, merged with one created after it, is the case the genesis
        // was introduced to prevent: two candidates per container key, one of them unreachable for good.
        {
            let mut shared = AutoCommit::new();
            init(&mut shared, &ROOT);
            assert!(
                shadowed_containers(&shared, &ROOT).is_empty(),
                "a document written with the genesis reaches one object per key"
            );

            // What a replica that minted its own schema produces: the same keys,
            // written under its own actor.
            let mut own = AutoCommit::new();
            own.put_object(&ROOT, KEY_BLOCKS, ObjType::List).unwrap();
            own.put_object(&ROOT, KEY_STYLES, ObjType::Map).unwrap();
            own.commit();

            let mut merged = shared.clone();
            merged.merge_incoming(&mut own).unwrap();
            let shadowed = shadowed_containers(&merged, &ROOT);
            assert!(
                shadowed.contains(&KEY_BLOCKS.to_string()),
                "two candidates for {KEY_BLOCKS:?} must be reported, got {shadowed:?}"
            );
        }
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;

    use automerge::{AutoCommit, ROOT};

    #[test]
    fn headers_and_footers_are_shared_and_hold_fields() {
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            // The regions come with the schema rather than being minted on first
            // write, so two people who each add a page header cannot end up with two
            // `root.header` maps and lose one — see `GENESIS_ACTOR`.
            assert!(region_blocks_in_order(&doc, &c, Region::Header).is_empty());
            assert!(materialize_region(&doc, &c, Region::Footer).is_empty());
            let bytes = doc.save();
            let reloaded = AutoCommit::load(&bytes).unwrap();
            let c2 = open(&reloaded, &ROOT).unwrap();
            assert!(region_blocks_in_order(&reloaded, &c2, Region::Header).is_empty());
            assert!(materialize_region(&reloaded, &c2, Region::Footer).is_empty());
        }
        // Phase `concurrent_headers_on_two_replicas_both_survive`.
        // The reason the regions are not lazily created: two replicas each adding a
        // page header must end up with both, not with whichever one won a
        // comparison of their actor ids.
        {
            for _ in 0..32 {
                let mut a = AutoCommit::new();
                let ca = init(&mut a, &ROOT);
                let mut ids_a = IdGen::new(a.get_actor().to_hex_string());
                let mut b = AutoCommit::new();
                let cb = init(&mut b, &ROOT);
                let mut ids_b = IdGen::new(b.get_actor().to_hex_string());
                {
                    let mut dm = DocMut {
                        doc: &mut a,
                        c: &ca,
                        ids: &mut ids_a,
                    };
                    let x = dm.insert_block_in(Region::Header, None, "paragraph");
                    dm.splice_text_in(Region::Header, &x, 0, 0, "left");
                }
                {
                    let mut dm = DocMut {
                        doc: &mut b,
                        c: &cb,
                        ids: &mut ids_b,
                    };
                    let y = dm.insert_block_in(Region::Header, None, "paragraph");
                    dm.splice_text_in(Region::Header, &y, 0, 0, "right");
                }
                a.merge_incoming(&mut b).unwrap();
                let merged = open(&a, &ROOT).unwrap();
                let texts: Vec<String> = materialize_region(&a, &merged, Region::Header)
                    .into_iter()
                    .map(|b| b.text)
                    .collect();
                assert!(
                    texts.iter().any(|t| t == "left") && texts.iter().any(|t| t == "right"),
                    "both replicas' headers survive the merge; got {texts:?}"
                );
            }
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (hid, fid);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                hid = dm.insert_block_in(Region::Header, None, "paragraph");
                dm.splice_text_in(Region::Header, &hid, 0, 0, "Title");
                fid = dm.insert_block_in(Region::Footer, None, "paragraph");
                dm.splice_text_in(Region::Footer, &fid, 0, 0, "Page ");
                dm.insert_page_number_field(Region::Footer, &fid, 5);
            }
            let header = materialize_region(&doc, &c, Region::Header);
            assert_eq!(header.len(), 1);
            assert_eq!(header[0].text, "Title");
            assert!(blocks_in_order(&doc, &c).is_empty(), "body stays empty");

            let footer = materialize_region(&doc, &c, Region::Footer);
            assert_eq!(footer.len(), 1);
            assert_eq!(footer[0].text, format!("Page {FIELD_PLACEHOLDER}"));
            let field = footer[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_FIELD)
                .expect("page-number field mark present");
            assert_eq!(
                (field.start, field.end),
                (5, 6),
                "field covers exactly the placeholder"
            );
            assert_eq!(
                field.value,
                crate::document::MarkValue::Str(FIELD_PAGE_NUMBER.into())
            );

            let snap = materialize(&doc, &c);
            assert_eq!(snap.header[0].text, "Title");
            assert_eq!(snap.footer.len(), 1);
        }
    }

    #[test]
    fn notes_anchor_their_content_and_never_remint_ids() {
        {
            // A fresh doc has an (empty) notes container — created eagerly so concurrent
            // footnote inserts converge on one shared object.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            assert!(note_ids(&doc, &c).is_empty(), "but empty");
            assert!(materialize_notes(&doc, &c).is_empty());
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, nid, nb);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "see ref");
                nid = dm.insert_footnote(&pid, 4);
                nb = dm.insert_note_block(&nid, None, "paragraph").unwrap();
                dm.splice_note_text(&nid, &nb, 0, 0, "the note");
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            assert_eq!(
                p.text,
                format!("see {FIELD_PLACEHOLDER}ref"),
                "placeholder spliced inline"
            );
            let nref = p
                .marks
                .iter()
                .find(|m| m.name == MARK_NOTE_REF)
                .expect("note-ref mark present");
            assert_eq!((nref.start, nref.end), (4, 5));
            assert_eq!(nref.value, crate::document::MarkValue::Str(nid.clone()));
            assert_eq!(note_kind(&doc, &c, &nid), Some(NoteKind::Footnote));
            let blocks = materialize_note_blocks(&doc, &c, &nid);
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "the note");
            let snap = materialize(&doc, &c);
            assert_eq!(snap.notes.len(), 1);
            assert_eq!(snap.notes[0].kind, "footnote");
            assert_eq!(snap.notes[0].blocks[0].text, "the note");
        }
        {
            // A generator seeded over a reloaded doc must skip past note + note-block ids, otherwise it would re-mint an existing note.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, nid);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "x");
                nid = dm.insert_footnote(&pid, 1);
                let nb = dm.insert_note_block(&nid, None, "paragraph").unwrap();
                dm.splice_note_text(&nid, &nb, 0, 0, "note");
            }
            // Re-seed over the same doc (as load() does) and add another note.
            let mut ids2 = seeded_id_gen(&doc, &c);
            let nid2;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids2,
                };
                nid2 = dm.insert_footnote(&pid, 0);
                let nb2 = dm.insert_note_block(&nid2, None, "paragraph").unwrap();
                dm.splice_note_text(&nid2, &nb2, 0, 0, "second");
            }
            assert_ne!(nid, nid2, "second note id must not collide with the first");
            let snap = materialize(&doc, &c);
            assert_eq!(
                snap.notes.len(),
                2,
                "both notes survive (no overwrite): {:?}",
                snap.notes
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn comments_anchor_their_content_and_threads_and_never_remint_ids() {
        {
            // A fresh doc has an (empty) comments container — eager, so concurrent comment adds converge on one shared object.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            assert!(comment_ids(&doc, &c).is_empty(), "but empty");
            assert!(materialize_comments(&doc, &c).is_empty());
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, cid);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "please fix this word");
                cid = dm.add_comment(&pid, 11, 20, "Ada", "note: fix");
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            let cm = p
                .marks
                .iter()
                .find(|m| m.name == MARK_COMMENT)
                .expect("comment mark present");
            assert_eq!((cm.start, cm.end), (11, 20));
            assert_eq!(cm.value, crate::document::MarkValue::Str(cid.clone()));
            assert_eq!(comment_author(&doc, &c, &cid).as_deref(), Some("Ada"));
            let blocks = materialize_comment_blocks(&doc, &c, &cid);
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "note: fix");
            let snap = materialize(&doc, &c);
            assert_eq!(snap.comments.len(), 1);
            assert_eq!(snap.comments[0].author, "Ada");
            assert_eq!(snap.comments[0].blocks[0].text, "note: fix");
            assert_eq!(snap.comments[0].range, Some((pid, 11, 20)));
        }
        {
            // A generator seeded over a reloaded doc must skip past comment ids, otherwise it would re-mint an existing comment.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, cid);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "abcdef");
                cid = dm.add_comment(&pid, 0, 3, "A", "first");
            }
            let mut ids2 = seeded_id_gen(&doc, &c);
            let cid2;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids2,
                };
                cid2 = dm.add_comment(&pid, 3, 6, "B", "second");
            }
            assert_ne!(
                cid, cid2,
                "second comment id must not collide with the first"
            );
            let snap = materialize(&doc, &c);
            assert_eq!(
                snap.comments.len(),
                2,
                "both comments survive (no overwrite): {:?}",
                snap.comments
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let mut comments = Vec::new();
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                let paragraph = dm.insert_block(None, "paragraph");
                dm.splice_text(&paragraph, 0, 0, "one two three four five");
                for (i, start) in [0usize, 4, 8, 14].into_iter().enumerate() {
                    let comment = dm.add_comment(&paragraph, start, start + 3, "Ada", "note");
                    // Interleave replies across threads, and leave the last thread without any.
                    for n in 0..(3 - i.min(3)) {
                        dm.add_comment_reply(&comment, "Bea", "2026-09-07", &format!("reply {n}"))
                            .expect("parent comment exists");
                    }
                    comments.push(comment);
                }
                let first = comments[0].clone();
                dm.add_comment_reply(&first, "Cy", "2026-09-08", "late reply")
                    .expect("parent comment exists");
            }
            let by_parent = comment_replies_by_parent(&doc, &c);
            let all = materialize_comments(&doc, &c);
            assert_eq!(all.len(), comments.len());
            for view in &all {
                assert_eq!(
                    comment_reply_ids(&doc, &c, &view.id),
                    by_parent.get(&view.id).cloned().unwrap_or_default()
                );
                assert_eq!(
                    view.replies,
                    materialize_comment_replies(&doc, &c, &view.id),
                    "a comment's replies read the same alone and in the full list"
                );
            }
            let counts: Vec<usize> = comments
                .iter()
                .map(|id| {
                    all.iter()
                        .find(|v| &v.id == id)
                        .map_or(0, |v| v.replies.len())
                })
                .collect();
            assert_eq!(counts, vec![4, 2, 1, 0]);

            let seeded = try_seeded_id_gen(&doc, &c).expect("seeds");
            assert_eq!(seeded.next, ids.next);
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (paragraph, comment, reply_a, reply_b);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                paragraph = dm.insert_block(None, "paragraph");
                dm.splice_text(&paragraph, 0, 0, "review this");
                comment = dm.add_comment(&paragraph, 0, 6, "Ada", "Initial note");
                // A legacy-shaped comment has no `replies` map until the first
                // reply; materializing it must not create one.
                assert!(comment_reply_ids(dm.doc, dm.c, &comment).is_empty());
                reply_a = dm
                    .add_comment_reply(&comment, "Bea", "2026-09-07", "First reply")
                    .expect("parent comment exists");
                reply_b = dm
                    .add_comment_reply(&comment, "Cy", "2026-09-08", "Second reply")
                    .expect("parent comment exists");
            }
            let replies = materialize_comment_replies(&doc, &c, &comment);
            assert_eq!(replies.len(), 2);
            assert_eq!(
                replies
                    .iter()
                    .map(|reply| reply.id.clone())
                    .collect::<Vec<_>>(),
                vec![reply_a, reply_b],
                "reply ids are sorted canonically rather than by merge arrival"
            );
            assert_eq!(replies[0].blocks[0].text, "First reply");
            assert_eq!(replies[1].blocks[0].text, "Second reply");

            let saved = doc.save();
            let reloaded = AutoCommit::load(&saved).expect("reload threaded comments");
            let c2 = open(&reloaded, &ROOT).expect("open reloaded schema");
            assert_eq!(
                materialize_comments(&reloaded, &c2)[0].replies,
                replies,
                "thread survives reload unchanged"
            );

            // A legacy comment is simply a root comment without any reply records;
            // materializing it is empty and performs no migration write.
            let mut old = AutoCommit::new();
            let old_c = init(&mut old, &ROOT);
            let mut old_ids = IdGen::new(old.get_actor().to_hex_string());
            let legacy;
            {
                let mut dm = DocMut {
                    doc: &mut old,
                    c: &old_c,
                    ids: &mut old_ids,
                };
                let p = dm.insert_block(None, "paragraph");
                dm.splice_text(&p, 0, 0, "legacy");
                legacy = dm.add_comment(&p, 0, 6, "D", "old comment");
            }
            assert!(
                materialize_comments(&old, &old_c)
                    .into_iter()
                    .find(|entry| entry.id == legacy)
                    .is_some_and(|entry| entry.replies.is_empty())
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn tracked_changes_anchor_accept_reject_and_never_remint_ids() {
        {
            // A fresh doc has an (empty) changes container — eager, for the same convergence reason as comments/notes.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            assert!(change_ids(&doc, &c).is_empty(), "but empty");
            assert!(materialize_changes(&doc, &c).is_empty());
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, ins_id, del_id);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "hello world");
                ins_id = dm.track_insert(&pid, 5, " there", "Ada", "2021-01-02");
                del_id = dm.track_delete(&pid, 12, 17, "Bob", "2021-03-04");
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            assert_eq!(
                p.text, "hello there world",
                "inserted text is real content, deletion retained"
            );
            let im = p
                .marks
                .iter()
                .find(|m| m.name == MARK_INS)
                .expect("ins mark present");
            assert_eq!((im.start, im.end), (5, 11), "ins covers ' there'");
            assert_eq!(im.value, crate::document::MarkValue::Str(ins_id.clone()));
            let dmrk = p
                .marks
                .iter()
                .find(|m| m.name == MARK_DEL)
                .expect("del mark present");
            assert_eq!((dmrk.start, dmrk.end), (12, 17), "del covers 'world'");
            assert_eq!(dmrk.value, crate::document::MarkValue::Str(del_id.clone()));
            assert_eq!(
                change_kind(&doc, &c, &ins_id),
                Some(crate::snapshot::ChangeKind::Ins)
            );
            assert_eq!(
                change_kind(&doc, &c, &del_id),
                Some(crate::snapshot::ChangeKind::Del)
            );
            assert_eq!(change_author(&doc, &c, &ins_id).as_deref(), Some("Ada"));
            assert_eq!(change_author(&doc, &c, &del_id).as_deref(), Some("Bob"));
            assert_eq!(
                change_date(&doc, &c, &del_id).as_deref(),
                Some("2021-03-04")
            );
            let snap = materialize(&doc, &c);
            assert_eq!(snap.changes.len(), 2);
            let ins = snap.changes.iter().find(|ch| ch.id == ins_id).unwrap();
            assert_eq!(ins.kind, crate::snapshot::ChangeKind::Ins);
            assert_eq!(ins.range, Some((pid.clone(), 5, 11)));
            let del = snap.changes.iter().find(|ch| ch.id == del_id).unwrap();
            assert_eq!(del.kind, crate::snapshot::ChangeKind::Del);
            assert_eq!(del.range, Some((pid, 12, 17)));
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let pid;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "ab");
                let id = dm.track_insert(&pid, 2, "cd", "A", "");
                dm.accept_change(&id);
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            assert_eq!(p.text, "abcd", "accepted insertion keeps the text");
            assert!(
                p.marks.iter().all(|m| m.name != MARK_INS),
                "ins mark dropped on accept"
            );
            assert!(
                materialize_changes(&doc, &c).is_empty(),
                "change record removed on accept"
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let pid;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "keep DROP end");
                let id = dm.track_delete(&pid, 5, 9, "A", "");
                dm.accept_change(&id);
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            assert_eq!(
                p.text, "keep  end",
                "accepted deletion removes the struck text"
            );
            assert!(
                p.marks.iter().all(|m| m.name != MARK_DEL),
                "del mark gone with the text"
            );
            assert!(
                materialize_changes(&doc, &c).is_empty(),
                "change record removed on accept"
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let pid;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "abYZ");
                let ins = dm.track_insert(&pid, 4, "QQ", "A", "");
                dm.reject_change(&ins);
                let del = dm.track_delete(&pid, 2, 4, "A", "");
                dm.reject_change(&del);
            }
            let p = materialize_block(&doc, &c.blocks, &pid);
            assert_eq!(
                p.text, "abYZ",
                "reject-ins removed inserted text; reject-del kept struck text"
            );
            assert!(
                p.marks
                    .iter()
                    .all(|m| m.name != MARK_INS && m.name != MARK_DEL),
                "both marks dropped"
            );
            assert!(
                materialize_changes(&doc, &c).is_empty(),
                "both records removed"
            );
        }
        {
            // A generator seeded over a reloaded doc must skip past change ids, otherwise it would re-mint an existing change.
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (pid, chid);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                pid = dm.insert_block(None, "paragraph");
                dm.splice_text(&pid, 0, 0, "abcdef");
                chid = dm.track_delete(&pid, 0, 3, "A", "");
            }
            let mut ids2 = seeded_id_gen(&doc, &c);
            let chid2;
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids2,
                };
                chid2 = dm.track_delete(&pid, 3, 6, "B", "");
            }
            assert_ne!(
                chid, chid2,
                "second change id must not collide with the first"
            );
            let snap = materialize(&doc, &c);
            assert_eq!(
                snap.changes.len(),
                2,
                "both changes survive (no overwrite): {:?}",
                snap.changes
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn bookmarks_anchor_cross_references_and_converge() {
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (target, src);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                target = dm.insert_block(None, "paragraph");
                dm.splice_text(&target, 0, 0, "the intro section");
                dm.add_bookmark(&target, 4, 9, "intro");
                src = dm.insert_block(Some(&target), "paragraph");
                dm.splice_text(&src, 0, 0, "see above");
                dm.add_cross_ref(&src, 4, 9, "intro");
            }
            let tb = materialize_block(&doc, &c.blocks, &target);
            let bm = tb
                .marks
                .iter()
                .find(|m| m.name == MARK_BOOKMARK)
                .expect("bookmark mark present");
            assert_eq!((bm.start, bm.end), (4, 9));
            assert_eq!(bm.value, crate::document::MarkValue::Str("intro".into()));
            let sb = materialize_block(&doc, &c.blocks, &src);
            let link = sb
                .marks
                .iter()
                .find(|m| m.name == MARK_LINK)
                .expect("cross-ref link mark present");
            assert_eq!((link.start, link.end), (4, 9));
            assert_eq!(link.value, crate::document::MarkValue::Str("#intro".into()));
            let snap = materialize(&doc, &c);
            assert_eq!(snap.bookmarks.len(), 1);
            assert_eq!(snap.bookmarks[0].name, "intro");
            assert_eq!(snap.bookmarks[0].range, (target, 4, 9));
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (h, p);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                h = dm.insert_block(None, "heading");
                dm.set_attr(&h, "level", 1i64);
                dm.splice_text(&h, 0, 0, "Title");
                p = dm.insert_block(Some(&h), "paragraph");
                dm.splice_text(&p, 0, 0, "abc");
                // A point bookmark at idx 1 (start == end) widens to one character.
                dm.add_bookmark(&p, 1, 1, "pt");
                dm.add_cross_ref(&p, 0, 3, &heading_ref_id(&h));
            }
            let snap = materialize(&doc, &c);
            let pt = snap
                .bookmarks
                .iter()
                .find(|b| b.name == "pt")
                .expect("point bookmark present");
            assert_eq!(
                (pt.range.1, pt.range.2),
                (1, 2),
                "point bookmark widened to one char"
            );
            let link = snap
                .blocks
                .iter()
                .find(|b| b.text == "abc")
                .unwrap()
                .marks
                .iter()
                .find(|m| m.name == MARK_LINK)
                .unwrap()
                .clone();
            assert_eq!(
                link.value,
                crate::document::MarkValue::Str(format!("#{HEADING_REF_PREFIX}{h}"))
            );
        }
        {
            // Two replicas each add a bookmark to the same document; after merge both converge with both bookmarks present.
            let mut a = crate::Document::new();
            let p = a.insert_block(None, "paragraph");
            a.splice_text(&p, 0, 0, "shared base text");
            let mut b = a.fork();
            a.add_bookmark(&p, 0, 6, "first");
            b.add_bookmark(&p, 7, 11, "second");
            a.merge(&mut b);
            b.merge(&mut a);
            assert_eq!(
                a.materialize(),
                b.materialize(),
                "replicas converge with bookmarks"
            );
            assert_eq!(
                a.materialize().bookmarks.len(),
                2,
                "both bookmarks present after merge"
            );
        }
    }

    #[test]
    fn regions_clear_and_requested_blocks_materialize_faithfully() {
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                let a = dm.insert_block_in(Region::Header, None, "paragraph");
                dm.splice_text_in(Region::Header, &a, 0, 0, "one");
                dm.clear_region(Region::Header);
            }
            assert!(
                materialize_region(&doc, &c, Region::Header).is_empty(),
                "header cleared"
            );
        }
        {
            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let mut ids = IdGen::new(doc.get_actor().to_hex_string());
            let (first, middle, last);
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                first = dm.insert_block(None, "paragraph");
                dm.splice_text(&first, 0, 0, "first");
                middle = dm.insert_block(Some(&first), "paragraph");
                dm.splice_text(&middle, 0, 0, "middle");
                last = dm.insert_block(Some(&middle), "heading");
                dm.splice_text(&last, 0, 0, "last");
                dm.set_attr(&last, "level", 2_i64);
                dm.add_mark(&last, 0, 4, "strong", true);
            }

            let requested = vec![last.clone(), first.clone(), "deleted-remotely".into()];
            let scans_before = ordered_keys_scan_count();
            let views = materialize_blocks(&doc, &c, &requested);

            assert_eq!(ordered_keys_scan_count(), scans_before, "no body scan");
            assert_eq!(
                views.iter().map(|b| &b.id).collect::<Vec<_>>(),
                vec![&last, &first]
            );
            assert_eq!(views[0].kind, "heading");
            assert_eq!(views[0].text, "last");
            assert_eq!(views[0].attrs.get("level"), Some(&AttrVal::Int(2)));
            assert!(
                views[0]
                    .marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 0 && m.end == 4)
            );
            assert_eq!(views[1].text, "first");
        }
    }
}

#[cfg(test)]
mod preserved_tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};

    fn new_doc() -> (AutoCommit, Containers, IdGen) {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let ids = IdGen::new(doc.get_actor().to_hex_string());
        (doc, c, ids)
    }

    /// A `styles.xml`-shaped part with two independent top-level style atoms — the
    /// canonical "two unmodeled regions in one part" shape that used to clobber.
    const TWO_STYLE_PART: &[u8] = br#"<?xml version="1.0"?>
<office:styles xmlns:office="o" xmlns:style="s">
<style:style style:name="A">alpha</style:style>
<style:style style:name="B">bravo</style:style>
</office:styles>"#;

    /// [`TWO_STYLE_PART`] split the way the ODF codec splits it: one atom per top-level style.
    fn two_style_atoms() -> Vec<Vec<u8>> {
        let at = TWO_STYLE_PART
            .windows(3)
            .rposition(|w| w == b"\n<s")
            .map(|i| i.saturating_add(1))
            .unwrap();
        vec![TWO_STYLE_PART[..at].to_vec(), TWO_STYLE_PART[at..].to_vec()]
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn preserved_parts_round_trip_merge_per_atom_and_never_reuse_ids() {
        {
            let (mut doc, c, mut ids) = new_doc();
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_preserved_atoms("odt/part/styles.xml", &two_style_atoms());
            }
            let got =
                preserved_part(&doc, &c, "odt/part/styles.xml", &tilo_content::NoContent).unwrap();
            assert_eq!(got, TWO_STYLE_PART, "segmented part re-emits verbatim");
            let parts = preserved_parts(&doc, &c, &tilo_content::NoContent);
            let p = parts
                .iter()
                .find(|(k, _)| k == "odt/part/styles.xml")
                .unwrap();
            assert_eq!(p.1, TWO_STYLE_PART);
            assert!(preserved_atoms(&doc, &c, "odt/part/styles.xml").len() >= 2);
        }
        // Phase `different_atom_edits_merge_not_clobber`.
        // THE core win: two replicas import the same part, each rewrites a DIFFERENT
        // unmodeled atom of it, then merge — and BOTH edits survive. Under the old
        // whole-part `Bytes` scalar this was last-writer-wins (one edit clobbered the
        // other); per-atom storage makes the two edits independent automerge objects.
        {
            let (mut doc, c, mut ids) = new_doc();
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_preserved_atoms("odt/part/styles.xml", &two_style_atoms());
            }
            doc.commit();

            // Fork into replica B (distinct actor); A and B share the same per-atom object ids from the common ancestor.
            let mut doc_b = AutoCommit::load(&doc.save()).unwrap();
            doc_b.set_actor(automerge::ActorId::random());
            let c_b = open(&doc_b, &ROOT).unwrap();

            // Replica A rewrites ONLY the alpha atom; replica B rewrites ONLY bravo.
            let atoms_a = preserved_atoms(&doc, &c, "odt/part/styles.xml");
            let (id_alpha, cur_alpha) = find_atom(&atoms_a, b"alpha");
            let (id_bravo, cur_bravo) = find_atom(&atoms_a, b"bravo");
            let atoms_b = preserved_atoms(&doc_b, &c_b, "odt/part/styles.xml");
            assert!(
                atoms_b.iter().any(|(id, _)| id == &id_alpha)
                    && atoms_b.iter().any(|(id, _)| id == &id_bravo),
                "fork shares the same stable atom ids: A={atoms_a:?} B={atoms_b:?}"
            );

            let new_alpha = edit_atom(&cur_alpha, "alpha", "ALPHA-EDITED");
            let new_bravo = edit_atom(&cur_bravo, "bravo", "BRAVO-EDITED");
            {
                let mut dm = DocMut {
                    doc: &mut doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.set_preserved_atom_bytes("odt/part/styles.xml", &id_alpha, &new_alpha);
            }
            doc.commit();
            {
                let mut ids_b = IdGen::new(doc_b.get_actor().to_hex_string());
                let mut dm_b = DocMut {
                    doc: &mut doc_b,
                    c: &c_b,
                    ids: &mut ids_b,
                };
                dm_b.set_preserved_atom_bytes("odt/part/styles.xml", &id_bravo, &new_bravo);
            }
            doc_b.commit();

            let merged_a = merge_into(&mut doc, &mut doc_b);
            let merged_b = merge_into(&mut doc_b, &mut doc);
            doc = merged_a;
            doc_b = merged_b;
            let c = open(&doc, &ROOT).unwrap();
            let c_b = open(&doc_b, &ROOT).unwrap();

            // BOTH edits survive on BOTH replicas — this is what clobbered before.
            for (d, cc, who) in [(&doc, &c, "A"), (&doc_b, &c_b, "B")] {
                let merged =
                    preserved_part(d, cc, "odt/part/styles.xml", &tilo_content::NoContent).unwrap();
                let txt = String::from_utf8_lossy(&merged);
                assert!(
                    txt.contains("ALPHA-EDITED"),
                    "replica {who} kept A's edit: {txt}"
                );
                assert!(
                    txt.contains("BRAVO-EDITED"),
                    "replica {who} kept B's edit: {txt}"
                );
                // Source order is preserved (alpha before bravo) and the leading declaration/root-open atom survives untouched.
                let ai = txt.find("ALPHA-EDITED").unwrap();
                let bi = txt.find("BRAVO-EDITED").unwrap();
                assert!(ai < bi, "replica {who} kept atom order");
                assert!(
                    txt.contains("<office:styles"),
                    "replica {who} kept the root open tag"
                );
            }
            // Convergence: both replicas reconstruct identical bytes.
            assert_eq!(
                preserved_part(&doc, &c, "odt/part/styles.xml", &tilo_content::NoContent),
                preserved_part(
                    &doc_b,
                    &c_b,
                    "odt/part/styles.xml",
                    &tilo_content::NoContent
                ),
                "replicas converge on identical reconstructed bytes"
            );
        }
        {
            let mut ids = IdGen::with_counter("actor".into(), u64::MAX);
            assert_eq!(ids.try_mint(), Ok("actor.18446744073709551615".into()));
            assert_eq!(
                ids.try_mint(),
                Err(IdGenError::Exhausted {
                    actor_hex: "actor".into()
                })
            );
        }
        {
            use automerge::transaction::Transactable;

            let mut doc = AutoCommit::new();
            let c = init(&mut doc, &ROOT);
            let actor = doc.get_actor().to_hex_string();
            let id = format!("{actor}.{max}", max = u64::MAX);
            let block = doc.put_object(&c.blocks, id, ObjType::Map).unwrap();
            doc.put(&block, BLOCK_ORDER, ScalarValue::Bytes(vec![0x80]))
                .unwrap();
            assert_eq!(
                next_counter_for(&doc, &c, &actor),
                Err(IdGenError::Exhausted { actor_hex: actor })
            );
        }
    }

    /// Read-modify-write the one atom containing `needle` (test fixture helper: the fixture
    /// guarantees exactly one match), returning its id and current bytes.
    fn find_atom(atoms: &[(String, Vec<u8>)], needle: &[u8]) -> (String, Vec<u8>) {
        atoms
            .iter()
            .find(|(_, b)| b.windows(needle.len()).any(|w| w == needle))
            .map(|(id, b)| (id.clone(), b.clone()))
            .expect("fixture atom present")
    }

    fn edit_atom(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
        String::from_utf8_lossy(bytes)
            .replace(from, to)
            .into_bytes()
    }

    /// Merge `src` into `dst` via the low-level Automerge (mirroring `Document::merge`, which
    /// sidesteps an automerge-0.8 `AutoCommit::merge` patch-log panic), reloaded from the merged bytes.
    fn merge_into(dst: &mut AutoCommit, src: &mut AutoCommit) -> AutoCommit {
        let mut a = automerge::Automerge::load(&dst.save()).unwrap();
        let mut b = automerge::Automerge::load(&src.save()).unwrap();
        let _ = a.merge(&mut b);
        AutoCommit::load(&a.save()).unwrap()
    }
}

/// The salt a block's order key takes from its id: FNV-1a, folded to 16 bits.
fn order_salt(id: &str) -> u16 {
    let hash = id.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    let [a, b, c, d] = hash.to_le_bytes();
    u16::from_le_bytes([a ^ c, b ^ d])
}
